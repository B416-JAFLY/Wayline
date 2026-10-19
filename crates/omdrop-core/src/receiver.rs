//! Transport-independent AirDrop receiver state machine.
//!
//! TLS, TCP accept loops, UI prompts, and filesystem policy remain outside this module. This core
//! owns the Apple request sequence and makes receiver behavior testable with a
//! plain in-memory stream: `/Discover`, accepted `/Ask`, then `/Upload` tied by `TransferID`.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use omdrop_airdrop::plist::Value;
use omdrop_board::{Phase, Reason, Tracker};
use omdrop_net::http::{self, BodyReader, Input, RequestHead};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::airdrop::{
    build_discover_reply, parse_ask, Error as AirDropError, IncomingTransfer, PreparedUpload,
};
use crate::mdns::{MdnsResponder, NetworkInterface};

#[derive(Debug, Clone)]
pub struct ReceiverOptions {
    pub computer_name: String,
    pub model_name: String,
    pub save_dir: PathBuf,
    pub accept_all: bool,
    pub max_control_body: usize,
    pub retired_service_ids: Vec<String>,
    /// Some Apple senders wait for the final response at a validated DVZip/CPIO
    /// boundary without sending HTTP's zero chunk. Generic receivers stay strict.
    pub allow_dvzip_archive_boundary: bool,
}

impl ReceiverOptions {
    pub fn new(
        computer_name: impl Into<String>,
        model_name: impl Into<String>,
        save_dir: impl AsRef<Path>,
    ) -> Self {
        Self {
            computer_name: computer_name.into(),
            model_name: model_name.into(),
            save_dir: save_dir.as_ref().to_path_buf(),
            accept_all: false,
            max_control_body: 4 * 1024 * 1024,
            retired_service_ids: Vec::new(),
            allow_dvzip_archive_boundary: false,
        }
    }
    /// Explicit policy for unattended receivers and local test fixtures.
    pub fn with_accept_all(mut self) -> Self {
        self.accept_all = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiverEvent {
    Listening,
    Discovered,
    ConnectionFailed {
        peer: std::net::SocketAddr,
        error: String,
        kind: crate::TransferFailureKind,
    },
    Asked(IncomingTransfer),
    /// Encoded HTTP body bytes; chunked uploads have no known total.
    Receiving {
        transfer_id: String,
        received: u64,
        total: Option<u64>,
    },
    Confirming {
        transfer_id: String,
    },
    /// An enabled AirDrop compatibility policy finished a fully validated archive
    /// at a chunk boundary; the HTTP zero chunk was not received.
    ArchiveBoundary {
        transfer_id: String,
    },
    TransferFailed {
        transfer_id: String,
        error: String,
        kind: crate::TransferFailureKind,
    },
    Received {
        transfer: IncomingTransfer,
        files: Vec<PathBuf>,
    },
    Rejected {
        path: String,
        status: u16,
    },
}

#[derive(Debug)]
pub enum ReceiverError {
    Io(io::Error),
    Http(http::Error),
    AirDrop(AirDropError),
    Tls(String),
    Invalid(&'static str),
    UploadContext { context: String, source: Box<Self> },
}

impl ReceiverError {
    pub fn kind(&self) -> crate::TransferFailureKind {
        use crate::TransferFailureKind;
        match self {
            Self::Io(error) | Self::Http(http::Error::Io(error)) => {
                TransferFailureKind::from_io(error)
            }
            Self::AirDrop(error) => error.kind(),
            Self::Tls(_) => TransferFailureKind::Connection,
            Self::Http(_) => TransferFailureKind::Validation,
            Self::Invalid(_) => TransferFailureKind::Other,
            Self::UploadContext { source, .. } => source.kind(),
        }
    }

    fn upload_context(
        self,
        stage: &str,
        received: u64,
        total: Option<u64>,
        content_type: &str,
        state: http::BodyReadState,
    ) -> Self {
        Self::UploadContext {
            context: format!(
                "upload stage={stage}; received={received}; total={}; content-type={content_type:?}; HTTP={state:?}",
                total.map_or_else(|| "unknown".into(), |value| value.to_string())
            ),
            source: Box::new(self),
        }
    }
}

impl From<io::Error> for ReceiverError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<http::Error> for ReceiverError {
    fn from(error: http::Error) -> Self {
        Self::Http(error)
    }
}

impl From<AirDropError> for ReceiverError {
    fn from(error: AirDropError) -> Self {
        Self::AirDrop(error)
    }
}

impl std::fmt::Display for ReceiverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Http(error) => write!(f, "HTTP: {error}"),
            Self::AirDrop(error) => write!(f, "AirDrop: {error}"),
            Self::Tls(error) => write!(f, "TLS: {error}"),
            Self::Invalid(message) => f.write_str(message),
            Self::UploadContext { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl std::error::Error for ReceiverError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UploadContext { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

pub struct ReceiverCore {
    options: ReceiverOptions,
    state: Mutex<ReceiverState>,
    decide: Option<Arc<DecisionHandler>>,
}

type DecisionHandler = dyn Fn(&IncomingTransfer) -> bool + Send + Sync;

#[derive(Default)]
struct ReceiverState {
    accepted: HashMap<String, Accepted>,
    last_accepted: Option<String>,
    uploading: usize,
}

struct ActiveUpload<'a>(&'a Mutex<ReceiverState>);
impl Drop for ActiveUpload<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            state.uploading -= 1;
        }
    }
}

struct Accepted {
    transfer: IncomingTransfer,
    board: Tracker,
    at: Instant,
}

/// Close persistent /Discover connections when replacing the listener. Otherwise
/// detached TLS workers can keep answering with the previous receiver name.
struct ReceiverWorkers {
    shutdown: Arc<AtomicBool>,
    active: Vec<(TcpStream, thread::JoinHandle<()>)>,
}

impl ReceiverWorkers {
    fn reap(&mut self) {
        self.active.retain(|(_, worker)| !worker.is_finished());
    }
}

impl Drop for ReceiverWorkers {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        for (stream, _) in &self.active {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        for (_, worker) in self.active.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Tracks only TLS record framing, leaving all authentication/decryption to rustls.
/// An incomplete encrypted record must prevent an early archive-boundary response,
/// even when it has not produced plaintext and the socket currently has no bytes.
#[derive(Default)]
struct TlsRecordBoundary {
    header: [u8; 5],
    header_len: usize,
    payload_remaining: usize,
}

impl TlsRecordBoundary {
    fn observe(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            if self.payload_remaining > 0 {
                let count = bytes.len().min(self.payload_remaining);
                self.payload_remaining -= count;
                bytes = &bytes[count..];
            } else {
                let count = bytes.len().min(self.header.len() - self.header_len);
                self.header[self.header_len..self.header_len + count]
                    .copy_from_slice(&bytes[..count]);
                self.header_len += count;
                bytes = &bytes[count..];
                if self.header_len == self.header.len() {
                    self.payload_remaining =
                        u16::from_be_bytes([self.header[3], self.header[4]]) as usize;
                    self.header_len = 0;
                }
            }
        }
    }

    fn partial(&self) -> bool {
        self.header_len != 0 || self.payload_remaining != 0
    }
}

struct TrackedSocket {
    socket: TcpStream,
    records: TlsRecordBoundary,
}

impl TrackedSocket {
    fn new(socket: TcpStream) -> Self {
        Self {
            socket,
            records: TlsRecordBoundary::default(),
        }
    }
}

impl Read for TrackedSocket {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.socket.read(output)?;
        self.records.observe(&output[..count]);
        Ok(count)
    }
}

impl Write for TrackedSocket {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.socket.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

fn tls_has_pending_data(
    tls: &mut &mut StreamOwned<ServerConnection, TrackedSocket>,
) -> io::Result<bool> {
    // Borrow plaintext without consuming it. A closed TLS stream is also pending:
    // strict HTTP reading must diagnose its missing terminator instead of treating
    // disconnection as permission to complete an upload.
    match tls.conn.reader().into_first_chunk() {
        Ok(_) => return Ok(true),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
        Err(error) => return Err(error),
    }
    if tls.sock.records.partial() {
        return Ok(true);
    }
    // Only this worker performs socket I/O; the listener's clone is used for
    // shutdown. Restore blocking mode on every result before normal TLS I/O resumes.
    tls.sock.socket.set_nonblocking(true)?;
    let pending = loop {
        match tls.sock.socket.peek(&mut [0]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => break result,
        }
    };
    tls.sock.socket.set_nonblocking(false)?;
    match pending {
        Ok(_) => Ok(true), // Bytes or TCP EOF both require strict processing.
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
        Err(error) => Err(error),
    }
}

/// Counts HTTP body bytes, not advertised source bytes (which may be compressed).
struct ProgressReader<'a, R> {
    inner: R,
    board: &'a mut Tracker,
    transfer_id: &'a str,
    notify: &'a mut dyn FnMut(ReceiverEvent),
    done: u64,
    total: Option<u64>,
    last: Instant,
}

impl<R: Read> Read for ProgressReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buffer)?;
        self.done = self.done.saturating_add(n as u64);
        if n == 0 || self.last.elapsed() >= Duration::from_millis(100) {
            self.board
                .update(Phase::Transferring, self.done, self.total, Reason::None);
            (self.notify)(ReceiverEvent::Receiving {
                transfer_id: self.transfer_id.to_owned(),
                received: self.done,
                total: self.total,
            });
            self.last = Instant::now();
        }
        Ok(n)
    }
}

impl ReceiverCore {
    pub fn new(options: ReceiverOptions) -> Self {
        Self {
            options,
            state: Mutex::new(ReceiverState::default()),
            decide: None,
        }
    }

    /// Ask the host UI before authorizing an upload. The callback runs on the
    /// connection worker, without holding the receiver state lock.
    pub fn with_decision(
        mut self,
        decide: impl Fn(&IncomingTransfer) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.decide = Some(Arc::new(decide));
        self
    }

    pub fn options(&self) -> &ReceiverOptions {
        &self.options
    }

    /// Includes accepted offers and uploads until validation and the final response
    /// finish. Presentation timeouts must not authorize maintenance during a transfer.
    pub fn has_active_transfers(&self) -> bool {
        self.state.lock().map_or(true, |state| {
            !state.accepted.is_empty() || state.uploading > 0
        })
    }

    /// Retire offers whose sender never started uploading. Call from listener maintenance.
    pub fn expire_offers(&self) {
        self.expire_offers_with_events(&mut |_| {});
    }

    fn expire_offers_with_events(&self, notify: &mut dyn FnMut(ReceiverEvent)) {
        let mut expired = Vec::new();
        if let Ok(mut state) = self.state.lock() {
            let ids: Vec<_> = state
                .accepted
                .iter()
                .filter(|(_, accepted)| accepted.at.elapsed() >= Duration::from_secs(120))
                .map(|(id, _)| id.clone())
                .collect();
            for id in ids {
                if let Some(accepted) = state.accepted.remove(&id) {
                    expired.push(accepted);
                }
            }
            if state
                .last_accepted
                .as_ref()
                .is_some_and(|id| !state.accepted.contains_key(id))
            {
                state.last_accepted = None;
            }
        }
        // UI callbacks and board delivery must not run while holding the state lock.
        for mut accepted in expired {
            accepted.board.fail(Reason::Timeout);
            notify(ReceiverEvent::TransferFailed {
                transfer_id: accepted.transfer.id,
                error: "sender did not start uploading within 120 seconds".into(),
                kind: crate::TransferFailureKind::Connection,
            });
        }
    }

    /// Process one request from a connection. Returns `true` when the connection should close.
    pub fn handle<R: Read + Write>(
        &self,
        input: &mut Input<R>,
    ) -> Result<Option<ReceiverEvent>, ReceiverError> {
        self.handle_with_events(input, &mut |_| {})
    }

    fn handle_with_events<R: Read + Write>(
        &self,
        input: &mut Input<R>,
        notify: &mut dyn FnMut(ReceiverEvent),
    ) -> Result<Option<ReceiverEvent>, ReceiverError> {
        let Some(head) = RequestHead::read(input)? else {
            return Ok(None);
        };
        if head
            .header("expect")
            .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
        {
            if let Err(error) = http::write_continue(input) {
                let error = ReceiverError::from(error);
                // A failed provisional reply still ends the authorized upload.
                if head.method == "POST" && head.path == "/Upload" {
                    if let Some(mut accepted) = self.take_offer(&head)? {
                        accepted.board.fail(Reason::Interrupted);
                        Self::notify_failure(&accepted.transfer.id, &error, notify);
                    }
                }
                return Err(error);
            }
        }

        self.handle_head_with_events(head, input, notify)
    }

    /// Handle a request head that has already been read. The split is used by the TLS listener so
    /// it can send `100 Continue` before consuming a potentially large upload body.
    pub fn handle_head<R: Read + Write>(
        &self,
        head: RequestHead,
        input: &mut Input<R>,
    ) -> Result<Option<ReceiverEvent>, ReceiverError> {
        self.handle_head_with_events(head, input, &mut |_| {})
    }

    fn take_offer(&self, head: &RequestHead) -> Result<Option<Accepted>, ReceiverError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ReceiverError::Invalid("receiver state lock poisoned"))?;
        let key = head
            .header("transferid")
            .map(str::to_owned)
            .or_else(|| state.last_accepted.clone());
        let transfer = key.and_then(|key| state.accepted.remove(&key));
        state.last_accepted = None;
        Ok(transfer)
    }

    fn notify_failure(
        transfer_id: &str,
        error: &ReceiverError,
        notify: &mut dyn FnMut(ReceiverEvent),
    ) {
        notify(ReceiverEvent::TransferFailed {
            transfer_id: transfer_id.to_owned(),
            error: error.to_string(),
            kind: error.kind(),
        });
    }

    fn handle_head_with_events<R: Read + Write>(
        &self,
        head: RequestHead,
        input: &mut Input<R>,
        notify: &mut dyn FnMut(ReceiverEvent),
    ) -> Result<Option<ReceiverEvent>, ReceiverError> {
        self.expire_offers_with_events(notify);
        match (head.method.as_str(), head.path.as_str()) {
            ("POST", "/Discover") => {
                let _ = BodyReader::new(input, head.body)
                    .read_to_end_limited(self.options.max_control_body)?;
                let body =
                    build_discover_reply(&self.options.computer_name, &self.options.model_name)?;
                http::write_response(input, 200, &body, Some("application/octet-stream"), false)?;
                Ok(Some(ReceiverEvent::Discovered))
            }
            ("POST", "/Ask") => {
                let body = BodyReader::new(input, head.body)
                    .read_to_end_limited(self.options.max_control_body)?;
                let transfer = parse_ask(&body)?;
                let mut board = Tracker::begin(
                    crate::board_feedback_enabled(),
                    1,
                    &transfer.sender_name,
                    transfer.files.len(),
                );
                board.phase(Phase::Waiting);
                let accepted = self
                    .decide
                    .as_ref()
                    .map_or(self.options.accept_all, |decide| decide(&transfer));
                if !accepted {
                    board.fail(Reason::Declined);
                    http::write_response(input, 403, &[], None, false)?;
                    return Ok(Some(ReceiverEvent::Rejected {
                        path: "/Ask".into(),
                        status: 403,
                    }));
                }
                let reply = Value::Dictionary(vec![
                    (
                        "ReceiverModelName".into(),
                        Value::String(self.options.model_name.clone()),
                    ),
                    (
                        "ReceiverComputerName".into(),
                        Value::String(self.options.computer_name.clone()),
                    ),
                ]);
                let result = (|| -> Result<(), ReceiverError> {
                    let body = omdrop_airdrop::plist::write(&reply)
                        .map_err(|error| ReceiverError::AirDrop(AirDropError::Plist(error)))?;
                    http::write_response(
                        input,
                        200,
                        &body,
                        Some("application/octet-stream"),
                        false,
                    )?;
                    let mut state = self
                        .state
                        .lock()
                        .map_err(|_| ReceiverError::Invalid("receiver state lock poisoned"))?;
                    state.last_accepted = Some(transfer.id.clone());
                    state.accepted.insert(
                        transfer.id.clone(),
                        Accepted {
                            transfer: transfer.clone(),
                            board,
                            at: Instant::now(),
                        },
                    );
                    Ok(())
                })();
                if let Err(error) = result {
                    Self::notify_failure(&transfer.id, &error, notify);
                    return Err(error);
                }
                Ok(Some(ReceiverEvent::Asked(transfer)))
            }
            ("POST", "/Upload") => {
                let transfer = {
                    let mut state = self
                        .state
                        .lock()
                        .map_err(|_| ReceiverError::Invalid("receiver state lock poisoned"))?;
                    let key = head
                        .header("transferid")
                        .map(str::to_owned)
                        .or_else(|| state.last_accepted.clone());
                    let transfer = key.and_then(|key| state.accepted.remove(&key));
                    if transfer.is_some() {
                        state.uploading += 1;
                    }
                    state.last_accepted = None;
                    transfer
                };
                let Some(Accepted {
                    transfer,
                    mut board,
                    ..
                }) = transfer
                else {
                    let _ = BodyReader::new(input, head.body)
                        .read_to_end_limited(self.options.max_control_body);
                    http::write_response(input, 403, &[], None, true)?;
                    return Ok(Some(ReceiverEvent::Rejected {
                        path: "/Upload".into(),
                        status: 403,
                    }));
                };
                let _active_upload = ActiveUpload(&self.state);
                let content_type = head.header("content-type").unwrap_or("");
                let total = match head.body {
                    http::BodyKind::Length(n) => Some(n),
                    _ => None,
                };
                board.update(Phase::Transferring, 0, total, Reason::None);
                notify(ReceiverEvent::Receiving {
                    transfer_id: transfer.id.clone(),
                    received: 0,
                    total,
                });
                let (result, done, body_state) = {
                    let mut body = ProgressReader {
                        inner: BodyReader::new(input, head.body),
                        board: &mut board,
                        transfer_id: &transfer.id,
                        notify,
                        done: 0,
                        total,
                        last: Instant::now(),
                    };
                    let mut stage = "archive decoding";
                    let allow_boundary = self.options.allow_dvzip_archive_boundary
                        && content_type.eq_ignore_ascii_case("application/x-dvzip");
                    let result = PreparedUpload::read_with_dvzip_boundary(
                        content_type,
                        &mut body,
                        &self.options.save_dir,
                        |body| {
                            if allow_boundary {
                                body.inner.can_finish_archive_boundary()
                            } else {
                                Ok(false)
                            }
                        },
                    )
                    .and_then(|pending| {
                        if pending.archive_boundary {
                            (body.notify)(ReceiverEvent::ArchiveBoundary {
                                transfer_id: transfer.id.clone(),
                            });
                        } else {
                            // Strict uploads validate the complete HTTP framing.
                            stage = "HTTP body completion";
                            io::copy(&mut body, &mut io::sink())?;
                        }
                        (body.notify)(ReceiverEvent::Confirming {
                            transfer_id: transfer.id.clone(),
                        });
                        body.board
                            .update(Phase::Confirming, body.done, total, Reason::None);
                        stage = "file commit";
                        pending.commit()
                    });
                    let state = body.inner.state();
                    let result = result.map_err(|error| {
                        ReceiverError::AirDrop(error).upload_context(
                            stage,
                            body.done,
                            total,
                            content_type,
                            state,
                        )
                    });
                    (result, body.done, state)
                };
                match result {
                    Ok(files) => {
                        board.update(Phase::Confirming, done, total, Reason::None);
                        if let Err(error) = http::write_response(input, 200, &[], None, true) {
                            for file in files {
                                let _ = std::fs::remove_file(file);
                            }
                            let error = ReceiverError::from(error).upload_context(
                                "final HTTP response",
                                done,
                                total,
                                content_type,
                                body_state,
                            );
                            board.fail(Reason::Interrupted);
                            Self::notify_failure(&transfer.id, &error, notify);
                            return Err(error);
                        }
                        board.phase(Phase::Complete);
                        Ok(Some(ReceiverEvent::Received { transfer, files }))
                    }
                    Err(error) => {
                        board.fail(Reason::None);
                        Self::notify_failure(&transfer.id, &error, notify);
                        let _ = http::write_response(input, 400, &[], None, true);
                        Err(error)
                    }
                }
            }
            _ => {
                let _ = BodyReader::new(input, head.body)
                    .read_to_end_limited(self.options.max_control_body);
                http::write_response(input, 404, &[], None, true)?;
                Ok(Some(ReceiverEvent::Rejected {
                    path: head.path,
                    status: 404,
                }))
            }
        }
    }

    /// Serve requests from one TLS-wrapped TCP connection. The receiver owns an internal state
    /// lock, so accepted connections can run in parallel without losing TransferID authorization.
    pub fn serve_tls_connection(
        stream: TcpStream,
        config: Arc<ServerConfig>,
        core: Arc<Self>,
    ) -> Result<Vec<ReceiverEvent>, ReceiverError> {
        let mut events = Vec::new();
        Self::serve_tls_connection_with_events(stream, config, core, |event| events.push(event))?;
        Ok(events)
    }

    /// Deliver events as each request completes, even on a persistent TLS connection.
    pub fn serve_tls_connection_with_events(
        stream: TcpStream,
        config: Arc<ServerConfig>,
        core: Arc<Self>,
        mut notify: impl FnMut(ReceiverEvent),
    ) -> Result<(), ReceiverError> {
        // Winsock accept inherits FIONBIO from the discovery listener. TLS runs
        // on a dedicated blocking worker; WouldBlock must not abort a handshake.
        stream.set_nonblocking(false)?;
        let connection =
            ServerConnection::new(config).map_err(|error| ReceiverError::Tls(error.to_string()))?;
        let mut tls = StreamOwned::new(connection, TrackedSocket::new(stream));
        let mut input = Input::new(&mut tls).with_read_probe(tls_has_pending_data);
        loop {
            let event = core.handle_with_events(&mut input, &mut notify)?;
            let Some(event) = event else {
                break;
            };
            let terminal = matches!(
                event,
                ReceiverEvent::Received { .. }
                    | ReceiverEvent::Rejected {
                        status: 404 | 403 | 400,
                        ..
                    }
            );
            notify(event);
            if terminal {
                break;
            }
        }
        Ok(())
    }

    /// Run an explicit TCP diagnostic listener, closing and joining every worker on stop.
    pub fn run_tcp_listener(
        listener: std::net::TcpListener,
        config: Arc<ServerConfig>,
        core: Arc<Self>,
        shutdown: Arc<AtomicBool>,
        events: Sender<ReceiverEvent>,
    ) -> io::Result<()> {
        listener.set_nonblocking(true)?;
        let mut workers = ReceiverWorkers {
            shutdown: shutdown.clone(),
            active: Vec::new(),
        };
        let _ = events.send(ReceiverEvent::Listening);
        while !shutdown.load(Ordering::Acquire) {
            workers.reap();
            core.expire_offers_with_events(&mut |event| {
                let _ = events.send(event);
            });
            Self::accept_worker(&listener, &config, &core, &shutdown, &events, &mut workers)?;
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    fn accept_worker(
        listener: &std::net::TcpListener,
        config: &Arc<ServerConfig>,
        core: &Arc<Self>,
        shutdown: &Arc<AtomicBool>,
        events: &Sender<ReceiverEvent>,
        workers: &mut ReceiverWorkers,
    ) -> io::Result<()> {
        match listener.accept() {
            Ok((stream, peer)) => {
                // Bound idle peers; /Ask authorization has its own bounded UI queue.
                if workers.active.len() >= 32 {
                    return Ok(());
                }
                stream.set_read_timeout(Some(Duration::from_secs(120)))?;
                stream.set_write_timeout(Some(Duration::from_secs(120)))?;
                let worker_config = Arc::clone(config);
                let worker_core = Arc::clone(core);
                let worker_events = events.clone();
                let worker_shutdown = shutdown.clone();
                let retained_stream = stream.try_clone()?;
                let worker = thread::spawn(move || {
                    if let Err(error) = Self::serve_tls_connection_with_events(
                        stream,
                        worker_config,
                        worker_core,
                        |event| {
                            let _ = worker_events.send(event);
                        },
                    ) {
                        if !worker_shutdown.load(Ordering::Acquire) {
                            let _ = worker_events.send(ReceiverEvent::ConnectionFailed {
                                peer,
                                error: error.to_string(),
                                kind: error.kind(),
                            });
                        }
                    }
                });
                workers.active.push((retained_stream, worker));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    /// Run the IPv6 listener and mDNS responder until `shutdown` is set.
    pub fn run_listener(
        interface: NetworkInterface,
        port: u16,
        service_id: &str,
        config: Arc<ServerConfig>,
        core: Arc<Self>,
        shutdown: Arc<AtomicBool>,
        events: Sender<ReceiverEvent>,
    ) -> io::Result<()> {
        let listener = std::net::TcpListener::bind(interface.endpoint(port))?;
        listener.set_nonblocking(true)?;
        let responder = MdnsResponder::bind(interface, service_id, port, 137)?;
        let mut workers = ReceiverWorkers {
            shutdown: shutdown.clone(),
            active: Vec::new(),
        };
        responder.announce_once()?;
        let _ = events.send(ReceiverEvent::Listening);
        let mut next_announce = Instant::now() + Duration::from_millis(16);
        let mut announcements = 63u8;
        let mut next_cycle = Instant::now() + Duration::from_millis(1050);
        let retirement_started = Instant::now();
        let mut next_retirement = retirement_started;

        while !shutdown.load(Ordering::Acquire) {
            workers.reap();
            core.expire_offers_with_events(&mut |event| {
                let _ = events.send(event);
            });
            Self::accept_worker(&listener, &config, &core, &shutdown, &events, &mut workers)?;

            // A deleted/recreated adapter needs a new socket. Malformed packets
            // are unrelated traffic, but actual network errors must reach the host.
            if let Err(error) = responder.poll_query_retiring(&core.options.retired_service_ids) {
                if error.kind() != io::ErrorKind::InvalidData {
                    return Err(error);
                }
            }
            let now = Instant::now();
            if now >= next_retirement && retirement_started.elapsed() < Duration::from_secs(120) {
                responder.retire_services_once(&core.options.retired_service_ids)?;
                let interval = if retirement_started.elapsed() < Duration::from_secs(2) {
                    100
                } else {
                    1000
                };
                next_retirement = now + Duration::from_millis(interval);
            }
            if announcements > 0 && now >= next_announce {
                responder.announce_once()?;
                announcements -= 1;
                next_announce = now + Duration::from_millis(16);
            } else if announcements == 0 && now >= next_cycle {
                announcements = 64;
                next_announce = now;
                next_cycle = now + Duration::from_millis(1050 + 3000);
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = responder.goodbye_once();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::airdrop::{build_ask, encode_upload, UploadFormat};
    use std::fs;
    use std::io::{Cursor, Read, Write};

    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Read for Duplex {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.input.read(buffer)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn options(dir: &Path) -> ReceiverOptions {
        ReceiverOptions::new("receiver", "omdrop", dir).with_accept_all()
    }

    #[test]
    fn tls_record_tracker_preserves_partial_headers_and_payloads() {
        let wire = [23, 3, 3, 0, 3, 1, 2, 3, 23, 3, 3, 0, 0, 23, 3, 3, 0, 1, 4];
        for cut in 0..=wire.len() {
            let mut records = TlsRecordBoundary::default();
            for byte in &wire[..cut] {
                records.observe(std::slice::from_ref(byte));
            }
            assert_eq!(
                records.partial(),
                ![0, 8, 13, 19].contains(&cut),
                "cut={cut}"
            );
            records.observe(&wire[cut..]);
            assert!(!records.partial());
        }
    }

    #[test]
    fn tls_probe_preserves_plaintext_and_detects_buffered_partial_encrypted_records() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::Identity::load_or_create(dir.path(), "receiver").unwrap();
        let server = crate::server_config(&identity).unwrap();
        let client = crate::sender::client_config(&identity).unwrap();
        for partial in [0, 1, 2, 3, 4, 5, 7] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let (ready, ready_rx) = std::sync::mpsc::channel();
            let (length, length_rx) = std::sync::mpsc::channel();
            let server = server.clone();
            let worker = thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let connection = ServerConnection::new(server).unwrap();
                let mut tls = StreamOwned::new(connection, TrackedSocket::new(socket));
                while tls.conn.is_handshaking() {
                    tls.conn.complete_io(&mut tls.sock).unwrap();
                }
                ready.send(()).unwrap();
                let expected = length_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                let mut received = 0;
                while received < expected {
                    let count = tls.conn.read_tls(&mut tls.sock).unwrap();
                    assert!(count > 0);
                    received += count;
                    tls.conn.process_new_packets().unwrap();
                }
                // The complete application record and partial following record have
                // both entered rustls, leaving no bytes available to socket.peek.
                assert_eq!(received, expected);
                assert!(tls_has_pending_data(&mut &mut tls).unwrap());
                assert_eq!(tls.conn.reader().into_first_chunk().unwrap(), b"abc");
                let mut plaintext = [0; 3];
                tls.conn.reader().read_exact(&mut plaintext).unwrap();
                assert_eq!(&plaintext, b"abc");
                assert_eq!(tls.sock.records.partial(), partial != 0);
                assert_eq!(tls_has_pending_data(&mut &mut tls).unwrap(), partial != 0);
            });
            let socket = TcpStream::connect(address).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let connection =
                rustls::ClientConnection::new(client.clone(), "airdrop".try_into().unwrap())
                    .unwrap();
            let mut tls = StreamOwned::new(connection, socket);
            tls.flush().unwrap();
            ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            tls.conn.writer().write_all(b"abc").unwrap();
            let mut wire = Vec::new();
            while tls.conn.wants_write() {
                tls.conn.write_tls(&mut wire).unwrap();
            }
            tls.conn.writer().write_all(b"0\r\n\r\n").unwrap();
            let mut tail = Vec::new();
            while tls.conn.wants_write() {
                tls.conn.write_tls(&mut tail).unwrap();
            }
            wire.extend_from_slice(&tail[..partial]);
            tls.sock.write_all(&wire).unwrap();
            length.send(wire.len()).unwrap();
            worker.join().unwrap();
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum DvzipTlsCase {
        ArchiveBoundary,
        StandardTerminator,
        StrictMissingTerminator,
        BadChecksum,
        BadTrailingBlock,
        PartialChunkHeader,
        MissingChunkDelimiter,
        WrongChunkLength,
        TruncatedContentLength,
        GzipMissingTerminator,
    }

    fn check_dvzip_tls_upload(case: DvzipTlsCase, contents: &[u8]) {
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("received.bin");
        fs::write(&source, contents).unwrap();
        let destination = tempfile::tempdir().unwrap();
        let mut settings = options(destination.path());
        settings.allow_dvzip_archive_boundary =
            !matches!(case, DvzipTlsCase::StrictMissingTerminator);
        let core = Arc::new(ReceiverCore::new(settings));
        let ask = build_ask("sender", "iPhone", "boundary", &[&source], None).unwrap();
        let mut offer = Input::new(Duplex {
            input: Cursor::new(
                [
                    format!(
                        "POST /Ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                        ask.len()
                    )
                    .into_bytes(),
                    ask,
                ]
                .concat(),
            ),
            output: Vec::new(),
        });
        core.handle(&mut offer).unwrap();
        let format = if matches!(case, DvzipTlsCase::GzipMissingTerminator) {
            UploadFormat::GzipCpio
        } else {
            UploadFormat::DvZip
        };
        let mut archive = encode_upload(&[&source], format).unwrap();
        if matches!(case, DvzipTlsCase::BadChecksum) {
            *archive.last_mut().unwrap() ^= 1;
        }
        let declared = archive.len() + usize::from(matches!(case, DvzipTlsCase::WrongChunkLength));
        let mut wire = if matches!(case, DvzipTlsCase::TruncatedContentLength) {
            format!(
                "POST /Upload HTTP/1.1\r\nTransferID: boundary\r\nContent-Type: application/x-dvzip\r\nContent-Length: {}\r\n\r\n", archive.len() + 1
            ).into_bytes()
        } else {
            let content_type = if format == UploadFormat::GzipCpio {
                "application/x-cpio"
            } else {
                "application/x-dvzip"
            };
            format!(
                "POST /Upload HTTP/1.1\r\nTransferID: boundary\r\nContent-Type: {content_type}\r\nTransfer-Encoding: chunked\r\n\r\n{declared:x}\r\n"
            ).into_bytes()
        };
        wire.extend_from_slice(&archive);
        if !matches!(
            case,
            DvzipTlsCase::MissingChunkDelimiter | DvzipTlsCase::TruncatedContentLength
        ) {
            wire.extend_from_slice(b"\r\n");
        }
        match case {
            DvzipTlsCase::StandardTerminator | DvzipTlsCase::BadChecksum => {
                wire.extend_from_slice(b"0\r\n\r\n");
            }
            DvzipTlsCase::BadTrailingBlock => wire.extend_from_slice(b"1\r\n\x80\r\n0\r\n\r\n"),
            DvzipTlsCase::PartialChunkHeader => wire.push(b'1'),
            _ => {}
        }
        if matches!(case, DvzipTlsCase::StandardTerminator) {
            assert!(
                wire.len() < 16 * 1024,
                "the zero chunk shares the payload's TLS record"
            );
        }
        let identity = crate::Identity::load_or_create(source_dir.path(), "receiver").unwrap();
        let server = crate::server_config(&identity).unwrap();
        let client = crate::sender::client_config(&identity).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (events, received_events) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            ReceiverCore::serve_tls_connection_with_events(stream, server, core, |event| {
                events.send(event).unwrap();
            })
        });
        let socket = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let connection =
            rustls::ClientConnection::new(client, "airdrop".try_into().unwrap()).unwrap();
        let mut tls = StreamOwned::new(connection, socket);
        // A single write includes any standard zero chunk in the same TLS plaintext
        // record for small cases. The large compatibility case leaves the connection
        // open and waits for 200 without ever sending the zero chunk or TCP EOF.
        tls.write_all(&wire).unwrap();
        tls.flush().unwrap();
        let mut input = Input::new(tls);
        let response = input.read_line().unwrap().unwrap();
        let result = worker.join().unwrap();
        let events: Vec<_> = received_events.into_iter().collect();
        let success = matches!(
            case,
            DvzipTlsCase::ArchiveBoundary | DvzipTlsCase::StandardTerminator
        );
        assert_eq!(
            response,
            if success {
                "HTTP/1.1 200 OK"
            } else {
                "HTTP/1.1 400 Bad Request"
            },
            "{case:?}"
        );
        assert_eq!(result.is_ok(), success, "{case:?}: {result:?}");
        assert_eq!(
            events
                .iter()
                .any(|event| matches!(event, ReceiverEvent::ArchiveBoundary { .. })),
            matches!(case, DvzipTlsCase::ArchiveBoundary),
            "{case:?}: {events:?}"
        );
        if success {
            let Some(ReceiverEvent::Received { files, .. }) = events.last() else {
                panic!("{case:?}: no successful terminal event: {events:?}");
            };
            assert_eq!(files.len(), 1);
            assert_eq!(fs::read(&files[0]).unwrap(), contents);
        } else {
            assert!(
                matches!(events.last(), Some(ReceiverEvent::TransferFailed { .. })),
                "{case:?}: {events:?}"
            );
            assert!(
                fs::read_dir(destination.path()).unwrap().next().is_none(),
                "{case:?}"
            );
        }
    }

    #[test]
    fn tls_six_mib_dvzip_archive_can_finish_without_zero_chunk_or_peer_eof() {
        let mut seed = 0x97ac_15d3u32;
        let contents: Vec<u8> = (0..6 * 1024 * 1024)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        check_dvzip_tls_upload(DvzipTlsCase::ArchiveBoundary, &contents);
    }

    #[test]
    fn tls_archive_boundary_policy_preserves_standard_framing_and_rejects_corruption() {
        let contents = b"checksums and HTTP framing still matter".repeat(100);
        for case in [
            DvzipTlsCase::StandardTerminator,
            DvzipTlsCase::StrictMissingTerminator,
            DvzipTlsCase::BadChecksum,
            DvzipTlsCase::BadTrailingBlock,
            DvzipTlsCase::PartialChunkHeader,
            DvzipTlsCase::MissingChunkDelimiter,
            DvzipTlsCase::WrongChunkLength,
            DvzipTlsCase::TruncatedContentLength,
            DvzipTlsCase::GzipMissingTerminator,
        ] {
            check_dvzip_tls_upload(case, &contents);
        }
    }

    #[test]
    fn archive_boundary_option_without_a_transport_probe_remains_strict() {
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("strict.txt");
        fs::write(&source, b"no probe must not guess whether more bytes exist").unwrap();
        let destination = tempfile::tempdir().unwrap();
        let mut settings = options(destination.path());
        settings.allow_dvzip_archive_boundary = true;
        let core = ReceiverCore::new(settings);
        let ask = build_ask("sender", "iPhone", "strict", &[&source], None).unwrap();
        let mut input = Input::new(Duplex {
            input: Cursor::new(
                [
                    format!(
                        "POST /Ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                        ask.len()
                    )
                    .into_bytes(),
                    ask,
                ]
                .concat(),
            ),
            output: Vec::new(),
        });
        core.handle(&mut input).unwrap();
        let archive = encode_upload(&[&source], UploadFormat::DvZip).unwrap();
        let mut input = Input::new(Duplex {
            input: Cursor::new([
                format!("POST /Upload HTTP/1.1\r\nTransferID: strict\r\nContent-Type: application/x-dvzip\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n", archive.len()).into_bytes(),
                archive,
                b"\r\n".to_vec(),
            ].concat()),
            output: Vec::new(),
        });
        let mut events = Vec::new();
        assert!(core
            .handle_with_events(&mut input, &mut |event| events.push(event))
            .is_err());
        assert!(matches!(
            events.last(),
            Some(ReceiverEvent::TransferFailed { .. })
        ));
        assert!(!events.iter().any(|event| matches!(
            event,
            ReceiverEvent::ArchiveBoundary { .. } | ReceiverEvent::Received { .. }
        )));
        assert!(fs::read_dir(destination.path()).unwrap().next().is_none());
    }

    #[test]
    fn replacing_listener_closes_old_connections_and_stops_pending_decisions() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let retained = stream.try_clone().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker = thread::spawn(move || {
            assert!(!matches!(stream.read(&mut [0; 1]), Ok(1..)));
        });
        let workers = ReceiverWorkers {
            shutdown: shutdown.clone(),
            active: vec![(retained, worker)],
        };
        let start = Instant::now();
        drop(workers);
        assert!(shutdown.load(Ordering::Acquire));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(!matches!(peer.read(&mut [0; 1]), Ok(1..)));
    }

    #[test]
    fn tls_worker_handles_a_nonblocking_accepted_socket_and_delayed_request() {
        let dir = tempfile::tempdir().unwrap();
        let identity = crate::Identity::load_or_create(dir.path(), "receiver").unwrap();
        let server = crate::server_config(&identity).unwrap();
        let client = crate::sender::client_config(&identity).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let core = Arc::new(ReceiverCore::new(options(dir.path())));
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // Windows accepts inherit the listener's nonblocking mode.
            stream.set_nonblocking(true).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            ReceiverCore::serve_tls_connection(stream, server, core)
        });
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let connection =
            rustls::ClientConnection::new(client, "airdrop".try_into().unwrap()).unwrap();
        let mut tls = rustls::StreamOwned::new(connection, stream);
        // The worker must wait for TLS/HTTP bytes rather than close on WouldBlock.
        thread::sleep(Duration::from_millis(100));
        tls.write_all(b"POST /Discover HTTP/1.1\r\nHost: airdrop\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        tls.flush().unwrap();
        let mut input = Input::new(tls);
        assert_eq!(
            input.read_line().unwrap().as_deref(),
            Some("HTTP/1.1 200 OK")
        );
        input.get_mut().conn.send_close_notify();
        input.get_mut().flush().unwrap();
        assert!(matches!(
            worker.join().unwrap().unwrap().as_slice(),
            [ReceiverEvent::Discovered]
        ));
    }

    #[test]
    fn tls_progress_is_delivered_before_upload_finishes_and_success_follows_response() {
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("live.txt");
        fs::write(
            &source,
            b"progress reaches the caller while the socket is still open",
        )
        .unwrap();
        let destination = tempfile::tempdir().unwrap();
        let core = Arc::new(ReceiverCore::new(options(destination.path())));
        let ask = build_ask("sender", "iPhone", "live", &[&source], None).unwrap();
        let mut offer = Input::new(Duplex {
            input: Cursor::new(
                [
                    format!(
                        "POST /Ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                        ask.len()
                    )
                    .into_bytes(),
                    ask,
                ]
                .concat(),
            ),
            output: Vec::new(),
        });
        core.handle(&mut offer).unwrap();
        let archive = encode_upload(&[&source], UploadFormat::GzipCpio).unwrap();
        let identity = crate::Identity::load_or_create(source_dir.path(), "receiver").unwrap();
        let server = crate::server_config(&identity).unwrap();
        let client = crate::sender::client_config(&identity).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (events, received_events) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            ReceiverCore::serve_tls_connection_with_events(stream, server, core, |event| {
                events.send(event).unwrap();
            })
        });
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(3)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let connection =
            rustls::ClientConnection::new(client, "airdrop".try_into().unwrap()).unwrap();
        let mut tls = StreamOwned::new(connection, stream);
        write!(tls, "POST /Upload HTTP/1.1\r\nTransferID: live\r\nContent-Type: application/x-cpio\r\nContent-Length: {}\r\n\r\n", archive.len()).unwrap();
        tls.flush().unwrap();
        assert_eq!(
            received_events
                .recv_timeout(Duration::from_secs(3))
                .unwrap(),
            ReceiverEvent::Receiving {
                transfer_id: "live".into(),
                received: 0,
                total: Some(archive.len() as u64),
            }
        );
        assert!(
            received_events.try_recv().is_err(),
            "there is no terminal event before the body arrives"
        );
        tls.write_all(&archive).unwrap();
        tls.flush().unwrap();
        let mut input = Input::new(tls);
        assert_eq!(
            input.read_line().unwrap().as_deref(),
            Some("HTTP/1.1 200 OK")
        );
        worker.join().unwrap().unwrap();
        let events: Vec<_> = received_events.into_iter().collect();
        let progress_count = events.len() - 2;
        assert!(progress_count >= 1);
        assert_eq!(
            events[progress_count - 1],
            ReceiverEvent::Receiving {
                transfer_id: "live".into(),
                received: archive.len() as u64,
                total: Some(archive.len() as u64)
            }
        );
        assert_eq!(
            events[progress_count],
            ReceiverEvent::Confirming {
                transfer_id: "live".into()
            }
        );
        assert!(
            matches!(&events[progress_count + 1], ReceiverEvent::Received { transfer, files } if transfer.id == "live" && files.len() == 1)
        );
    }

    #[test]
    fn failed_acceptance_and_continue_responses_notify_the_authorized_transfer() {
        struct Disconnected {
            input: Cursor<Vec<u8>>,
        }
        impl Read for Disconnected {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.input.read(bytes)
            }
        }
        impl Write for Disconnected {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "peer disconnected",
                ))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("source.txt");
        fs::write(&source, b"source").unwrap();
        let destination = tempfile::tempdir().unwrap();
        let core = ReceiverCore::new(options(destination.path()));
        let ask = build_ask("sender", "iPhone", "disconnected", &[&source], None).unwrap();
        let ask = [
            format!(
                "POST /Ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                ask.len()
            )
            .into_bytes(),
            ask,
        ]
        .concat();
        let mut input = Input::new(Disconnected {
            input: Cursor::new(ask.clone()),
        });
        let mut events = Vec::new();
        assert!(core
            .handle_with_events(&mut input, &mut |event| events.push(event))
            .is_err());
        assert!(
            matches!(events.as_slice(), [ReceiverEvent::TransferFailed { transfer_id, kind: crate::TransferFailureKind::Connection, .. }] if transfer_id == "disconnected")
        );
        assert!(!core.has_active_transfers());

        let mut input = Input::new(Duplex {
            input: Cursor::new(ask),
            output: Vec::new(),
        });
        core.handle(&mut input).unwrap();
        assert!(core.has_active_transfers());
        events.clear();
        let request = b"POST /Upload HTTP/1.1\r\nTransferID: disconnected\r\nExpect: 100-continue\r\nContent-Type: application/x-cpio\r\nContent-Length: 100\r\n\r\n";
        let mut input = Input::new(Disconnected {
            input: Cursor::new(request.to_vec()),
        });
        assert!(core
            .handle_with_events(&mut input, &mut |event| events.push(event))
            .is_err());
        assert!(
            matches!(events.as_slice(), [ReceiverEvent::TransferFailed { transfer_id, kind: crate::TransferFailureKind::Connection, .. }] if transfer_id == "disconnected")
        );
        assert!(!core.has_active_transfers());
        assert!(fs::read_dir(destination.path()).unwrap().next().is_none());
    }

    #[test]
    fn host_decision_is_required_before_upload_and_declines_do_not_save_files() {
        for accept in [false, true] {
            let source_dir = tempfile::tempdir().unwrap();
            let source = source_dir.path().join("consent.txt");
            fs::write(&source, b"only after acceptance").unwrap();
            let save_dir = tempfile::tempdir().unwrap();
            let ask = build_ask("iPhone", "iPhone", "consent", &[&source], None).unwrap();
            let upload = encode_upload(&[&source], UploadFormat::GzipCpio).unwrap();
            let decisions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counted = decisions.clone();
            let mut config = options(save_dir.path());
            // The callback must override both accept-all and reject-all defaults.
            config.accept_all = !accept;
            let receiver = ReceiverCore::new(config).with_decision(move |transfer| {
                assert_eq!(transfer.sender_name, "iPhone");
                assert_eq!(transfer.files[0].name, "consent.txt");
                counted.fetch_add(1, Ordering::SeqCst);
                accept
            });
            let mut ask_input = Input::new(Duplex {
                input: Cursor::new(
                    [
                        format!(
                            "POST /Ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                            ask.len()
                        )
                        .into_bytes(),
                        ask,
                    ]
                    .concat(),
                ),
                output: Vec::new(),
            });
            let event = receiver.handle(&mut ask_input).unwrap().unwrap();
            assert_eq!(decisions.load(Ordering::SeqCst), 1);
            assert_eq!(matches!(event, ReceiverEvent::Asked(_)), accept);
            assert!(fs::read_dir(save_dir.path()).unwrap().next().is_none());
            let mut upload_input = Input::new(Duplex {
                input: Cursor::new([format!("POST /Upload HTTP/1.1\r\nTransferID: consent\r\nContent-Type: application/x-cpio\r\nContent-Length: {}\r\n\r\n", upload.len()).into_bytes(), upload].concat()),
                output: Vec::new(),
            });
            let event = receiver.handle(&mut upload_input).unwrap().unwrap();
            if accept {
                let ReceiverEvent::Received { files, .. } = event else {
                    panic!("expected received file")
                };
                assert_eq!(fs::read(&files[0]).unwrap(), b"only after acceptance");
            } else {
                assert!(matches!(event, ReceiverEvent::Rejected { status: 403, .. }));
                assert!(fs::read_dir(save_dir.path()).unwrap().next().is_none());
            }
        }
    }

    #[test]
    fn default_policy_rejects_before_creating_any_files() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("input.txt");
        fs::write(&source, b"private").unwrap();
        let destination = root.path().join("received");
        let receiver = ReceiverCore::new(ReceiverOptions::new("receiver", "Windows", &destination));
        let ask = build_ask("sender", "phone", "denied", &[&source], None).unwrap();
        let mut input = Input::new(Duplex {
            input: Cursor::new(
                [
                    format!(
                        "POST /Ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                        ask.len()
                    )
                    .into_bytes(),
                    ask,
                ]
                .concat(),
            ),
            output: Vec::new(),
        });
        assert!(matches!(
            receiver.handle(&mut input).unwrap(),
            Some(ReceiverEvent::Rejected { status: 403, .. })
        ));
        assert!(receiver.state.lock().unwrap().accepted.is_empty());
        assert!(!destination.exists());
    }

    #[test]
    fn malformed_http_trailer_does_not_publish_valid_archive_files() {
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("payload.txt");
        fs::write(&file, b"complete archive but incomplete HTTP").unwrap();
        let save = tempfile::tempdir().unwrap();
        let receiver = ReceiverCore::new(options(save.path()));
        let ask = build_ask("sender", "phone", "truncated", &[&file], None).unwrap();
        let mut input = Input::new(Duplex {
            input: Cursor::new(
                [
                    format!(
                        "POST /Ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                        ask.len()
                    )
                    .into_bytes(),
                    ask,
                ]
                .concat(),
            ),
            output: Vec::new(),
        });
        receiver.handle(&mut input).unwrap();
        let archive = encode_upload(&[&file], UploadFormat::GzipCpio).unwrap();
        let mut bytes = format!("POST /Upload HTTP/1.1\r\nTransferID: truncated\r\nContent-Type: application/x-cpio\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n", archive.len()).into_bytes();
        bytes.extend_from_slice(&archive);
        bytes.extend_from_slice(b"\r\n0\r\nmissing-newline");
        let mut input = Input::new(Duplex {
            input: Cursor::new(bytes),
            output: Vec::new(),
        });
        let mut events = Vec::new();
        assert!(receiver
            .handle_with_events(&mut input, &mut |event| events.push(event))
            .is_err());
        assert!(fs::read_dir(save.path()).unwrap().next().is_none());
        assert!(!input.get_ref().output.starts_with(b"HTTP/1.1 200"));
        assert!(matches!(
            events.last(),
            Some(ReceiverEvent::TransferFailed { transfer_id, .. }) if transfer_id == "truncated"
        ));
        assert!(!events.iter().any(|event| matches!(
            event,
            ReceiverEvent::Confirming { .. } | ReceiverEvent::Received { .. }
        )));
    }

    #[test]
    fn upload_timeouts_report_archive_and_http_stages_without_losing_connection_kind() {
        struct StalledDuplex(Cursor<Vec<u8>>);
        impl Read for StalledDuplex {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                match self.0.read(bytes)? {
                    0 => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "socket idle timeout",
                    )),
                    count => Ok(count),
                }
            }
        }
        impl Write for StalledDuplex {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("complete-payload.txt");
        fs::write(
            &source,
            b"the file can be complete before the upload framing is complete",
        )
        .unwrap();
        for (format, missing_gzip_footer, expected_stage) in [
            (UploadFormat::GzipCpio, false, "stage=HTTP body completion"),
            (UploadFormat::DvZip, false, "dvzip final blocks/checksum"),
            (UploadFormat::GzipCpio, true, "gzip final checksum/trailer"),
        ] {
            let destination = tempfile::tempdir().unwrap();
            let core = ReceiverCore::new(options(destination.path()));
            let ask = build_ask("sender", "iPhone", "stalled", &[&source], None).unwrap();
            let mut offer = Input::new(Duplex {
                input: Cursor::new(
                    [
                        format!(
                            "POST /Ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                            ask.len()
                        )
                        .into_bytes(),
                        ask,
                    ]
                    .concat(),
                ),
                output: Vec::new(),
            });
            core.handle(&mut offer).unwrap();
            let archive = if missing_gzip_footer {
                // Padding lets CPIO finish before gzip reaches its final checksum.
                let mut cpio = encode_upload(&[&source], UploadFormat::Cpio).unwrap();
                cpio.resize(cpio.len() + 32768, 0);
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
                encoder.write_all(&cpio).unwrap();
                encoder.finish().unwrap()
            } else {
                encode_upload(&[&source], format).unwrap()
            };
            let content_type = if format == UploadFormat::DvZip {
                "application/x-dvzip"
            } else {
                "application/x-cpio"
            };
            let (framing, body, received, http_stage, total) = if missing_gzip_footer {
                (
                    format!("Content-Length: {}", archive.len()),
                    archive[..archive.len() - 8].to_vec(),
                    archive.len() - 8,
                    "stage: Data",
                    archive.len().to_string(),
                )
            } else {
                (
                    "Transfer-Encoding: chunked".into(),
                    [
                        format!("{:x}\r\n", archive.len()).into_bytes(),
                        archive.clone(),
                        b"\r\n".to_vec(),
                    ]
                    .concat(),
                    archive.len(),
                    "stage: ChunkHeader",
                    "unknown".into(),
                )
            };
            let request = format!("POST /Upload HTTP/1.1\r\nTransferID: stalled\r\nContent-Type: {content_type}\r\n{framing}\r\n\r\n");
            let mut input = Input::new(StalledDuplex(Cursor::new(
                [request.into_bytes(), body].concat(),
            )));
            let mut events = Vec::new();
            let error = core
                .handle_with_events(&mut input, &mut |event| events.push(event))
                .unwrap_err();
            assert_eq!(error.kind(), crate::TransferFailureKind::Connection);
            let message = error.to_string();
            for expected in [
                expected_stage.to_owned(),
                http_stage.to_owned(),
                format!("received={received}"),
                format!("total={total}"),
                content_type.to_owned(),
                "socket idle timeout".into(),
            ] {
                assert!(
                    message.contains(&expected),
                    "missing {expected:?}: {message}"
                );
            }
            assert!(
                matches!(events.last(), Some(ReceiverEvent::TransferFailed { transfer_id, kind: crate::TransferFailureKind::Connection, .. }) if transfer_id == "stalled")
            );
            assert!(fs::read_dir(destination.path()).unwrap().next().is_none());
            assert!(!core.has_active_transfers());
        }
    }

    #[test]
    fn chunked_progress_counts_payload_without_inventing_total() {
        let mut input = Input::new(Cursor::new(b"3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"));
        let mut board = Tracker::begin(false, 1, "sender", 1);
        let mut events = Vec::new();
        let mut notify = |event| events.push(event);
        let mut reader = ProgressReader {
            inner: BodyReader::new(&mut input, http::BodyKind::Chunked),
            board: &mut board,
            transfer_id: "chunked",
            notify: &mut notify,
            done: 0,
            total: None,
            last: Instant::now(),
        };
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"abcde");
        assert_eq!(reader.done, 5);
        assert_eq!(reader.total, None);
        assert_eq!(
            events.last(),
            Some(&ReceiverEvent::Receiving {
                transfer_id: "chunked".into(),
                received: 5,
                total: None,
            })
        );
    }

    #[test]
    fn progress_is_reported_during_reads_before_eof() {
        let mut board = Tracker::begin(false, 1, "sender", 1);
        let (events, received) = std::sync::mpsc::channel();
        let mut notify = |event| events.send(event).unwrap();
        let mut reader = ProgressReader {
            inner: Cursor::new(b"abcdefgh"),
            board: &mut board,
            transfer_id: "streaming",
            notify: &mut notify,
            done: 0,
            total: Some(8),
            last: Instant::now() - Duration::from_millis(101),
        };
        assert_eq!(reader.read(&mut [0; 3]).unwrap(), 3);
        assert_eq!(
            received.try_recv().unwrap(),
            ReceiverEvent::Receiving {
                transfer_id: "streaming".into(),
                received: 3,
                total: Some(8),
            }
        );
        assert_eq!(
            reader.inner.position(),
            3,
            "progress does not wait for the rest of the body"
        );
    }

    #[test]
    fn expired_offer_is_not_authorized_to_upload() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, b"payload").unwrap();
        let transfer =
            parse_ask(&build_ask("sender", "iPhone", "old", &[&source], None).unwrap()).unwrap();
        let receiver = ReceiverCore::new(options(dir.path()));
        {
            let mut state = receiver.state.lock().unwrap();
            state.last_accepted = Some("old".into());
            state.accepted.insert(
                "old".into(),
                Accepted {
                    transfer,
                    board: Tracker::begin(false, 1, "sender", 1),
                    at: Instant::now() - Duration::from_secs(121),
                },
            );
        }
        let mut events = Vec::new();
        receiver.expire_offers_with_events(&mut |event| {
            assert!(
                !receiver.has_active_transfers(),
                "callbacks run outside the state lock"
            );
            events.push(event);
        });
        receiver.expire_offers_with_events(&mut |event| events.push(event));
        assert!(matches!(events.as_slice(), [ReceiverEvent::TransferFailed {
            transfer_id, kind: crate::TransferFailureKind::Connection, ..
        }] if transfer_id == "old"));
        let state = receiver.state.lock().unwrap();
        assert!(state.accepted.is_empty());
        assert!(state.last_accepted.is_none());
    }

    #[test]
    fn discover_ask_upload_sequence_is_stateful() {
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("hello.txt");
        fs::write(&source, b"hello").unwrap();
        let save_dir = tempfile::tempdir().unwrap();
        let ask = build_ask("sender", "iPhone", "TRANSFER-1", &[&source], None).unwrap();
        let upload = encode_upload(&[&source], UploadFormat::GzipCpio).unwrap();
        let discover_request = b"POST /Discover HTTP/1.1\r\nContent-Length: 0\r\n\r\n";
        let ask_request = format!(
            "POST /Ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            ask.len()
        );
        let upload_request = format!("POST /Upload HTTP/1.1\r\nTransferID: TRANSFER-1\r\nContent-Type: application/x-cpio\r\nContent-Length: {}\r\n\r\n", upload.len());

        let receiver = ReceiverCore::new(options(save_dir.path()));
        let mut discover_input = Input::new(Duplex {
            input: Cursor::new(discover_request.to_vec()),
            output: Vec::new(),
        });
        assert_eq!(
            receiver.handle(&mut discover_input).unwrap(),
            Some(ReceiverEvent::Discovered)
        );
        assert!(discover_input
            .get_ref()
            .output
            .starts_with(b"HTTP/1.1 200 OK"));

        let mut ask_bytes = ask_request.into_bytes();
        ask_bytes.extend_from_slice(&ask);
        let mut ask_input = Input::new(Duplex {
            input: Cursor::new(ask_bytes),
            output: Vec::new(),
        });
        assert!(matches!(
            receiver.handle(&mut ask_input).unwrap(),
            Some(ReceiverEvent::Asked(_))
        ));

        let mut upload_bytes = upload_request.into_bytes();
        upload_bytes.extend_from_slice(&upload);
        let mut upload_input = Input::new(Duplex {
            input: Cursor::new(upload_bytes),
            output: Vec::new(),
        });
        let event = receiver.handle(&mut upload_input).unwrap().unwrap();
        let ReceiverEvent::Received { files, .. } = event else {
            panic!("expected received event");
        };
        assert_eq!(files.len(), 1);
        assert_eq!(fs::read(&files[0]).unwrap(), b"hello");
        assert!(upload_input
            .get_ref()
            .output
            .starts_with(b"HTTP/1.1 200 OK"));
    }

    #[test]
    fn active_transfers_cover_upload_validation_and_response_but_expire_idle_offers() {
        use std::sync::mpsc;

        struct PausedUpload {
            body: Cursor<Vec<u8>>,
            started: Option<mpsc::SyncSender<()>>,
            resume: mpsc::Receiver<()>,
            core: Arc<ReceiverCore>,
            fail_response: bool,
        }
        impl Read for PausedUpload {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if let Some(started) = self.started.take() {
                    started.send(()).unwrap();
                    self.resume.recv_timeout(Duration::from_secs(3)).unwrap();
                }
                self.body.read(bytes)
            }
        }
        impl Write for PausedUpload {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                assert!(
                    self.core.has_active_transfers(),
                    "final response is still part of the transfer"
                );
                if self.fail_response {
                    Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "peer disconnected",
                    ))
                } else {
                    Ok(bytes.len())
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("active.txt");
        fs::write(
            &source,
            b"keep maintenance blocked until this transfer finishes",
        )
        .unwrap();
        let ask = build_ask("sender", "iPhone", "active", &[&source], None).unwrap();
        let ask_bytes = [
            format!(
                "POST /Ask HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
                ask.len()
            )
            .into_bytes(),
            ask,
        ]
        .concat();
        let archive = encode_upload(&[&source], UploadFormat::GzipCpio).unwrap();

        for (truncated, fail_response) in [(false, false), (true, false), (false, true)] {
            let destination = tempfile::tempdir().unwrap();
            let core = Arc::new(ReceiverCore::new(options(destination.path())));
            assert!(!core.has_active_transfers());
            let mut offer = Input::new(Duplex {
                input: Cursor::new(ask_bytes.clone()),
                output: Vec::new(),
            });
            assert!(matches!(
                core.handle(&mut offer).unwrap(),
                Some(ReceiverEvent::Asked(_))
            ));
            assert!(
                core.has_active_transfers(),
                "accepted offers reserve the receiver"
            );

            let request = format!("POST /Upload HTTP/1.1\r\nTransferID: active\r\nContent-Type: application/x-cpio\r\nContent-Length: {}\r\n\r\n", archive.len());
            let head = RequestHead::read(&mut Input::new(Cursor::new(request.into_bytes())))
                .unwrap()
                .unwrap();
            let (started, reading) = mpsc::sync_channel(1);
            let (resume, resumed) = mpsc::channel();
            let worker_core = core.clone();
            let mut body = archive.clone();
            if truncated {
                body.truncate(body.len() / 2);
            }
            let worker = thread::spawn(move || {
                let mut input = Input::new(PausedUpload {
                    body: Cursor::new(body),
                    started: Some(started),
                    resume: resumed,
                    core: worker_core.clone(),
                    fail_response,
                });
                let mut events = Vec::new();
                let result = worker_core
                    .handle_head_with_events(head, &mut input, &mut |event| events.push(event));
                (result, events)
            });
            reading.recv_timeout(Duration::from_secs(3)).unwrap();
            assert!(core.has_active_transfers());
            assert!(core.state.lock().unwrap().accepted.is_empty());
            core.expire_offers();
            assert!(
                core.has_active_transfers(),
                "offer expiration cannot release an upload"
            );
            resume.send(()).unwrap();
            let (result, events) = worker.join().unwrap();
            assert!(
                !core.has_active_transfers(),
                "every upload exit releases its reservation"
            );
            if truncated || fail_response {
                assert!(result.is_err());
                assert!(fs::read_dir(destination.path()).unwrap().next().is_none());
                assert!(
                    matches!(events.last(), Some(ReceiverEvent::TransferFailed { transfer_id, .. }) if transfer_id == "active")
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| matches!(event, ReceiverEvent::TransferFailed { .. }))
                        .count(),
                    1
                );
                assert_eq!(
                    events
                        .iter()
                        .any(|event| matches!(event, ReceiverEvent::Confirming { .. })),
                    fail_response
                );
            } else {
                assert!(matches!(
                    result.unwrap(),
                    Some(ReceiverEvent::Received { .. })
                ));
                assert_eq!(
                    fs::read(destination.path().join("active.txt")).unwrap(),
                    fs::read(&source).unwrap()
                );
                assert!(
                    matches!(events.last(), Some(ReceiverEvent::Confirming { transfer_id }) if transfer_id == "active")
                );
            }
            assert!(!events
                .iter()
                .any(|event| matches!(event, ReceiverEvent::Received { .. })));

            let mut offer = Input::new(Duplex {
                input: Cursor::new(ask_bytes.clone()),
                output: Vec::new(),
            });
            core.handle(&mut offer).unwrap();
            assert!(core.has_active_transfers());
            core.state
                .lock()
                .unwrap()
                .accepted
                .get_mut("active")
                .unwrap()
                .at = Instant::now() - Duration::from_secs(121);
            core.expire_offers();
            assert!(
                !core.has_active_transfers(),
                "expired idle offers no longer reserve maintenance"
            );
        }
    }
}
