//! Application lifecycle and blocking Windows workers around Owly's receiver.
use crate::{
    mdns::{MdnsResponder, NetworkInterface},
    IncomingTransfer, TransferFailureKind,
};
use omdrop_board::{Phase, Reason, Tracker};
use owly_airdrop::{
    driver::staging::{rollback, Staging},
    proto::receiver::{self as proto, Connection, Event},
};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::Sender,
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
#[derive(Debug, Clone)]
pub struct ReceiverOptions {
    pub computer_name: String,
    pub model_name: String,
    pub save_dir: PathBuf,
    pub accept_all: bool,
    pub decision_timeout: Duration,
    pub max_control_body: usize,
    pub retired_service_ids: Vec<String>,
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
            save_dir: save_dir.as_ref().to_owned(),
            accept_all: false,
            decision_timeout: Duration::from_secs(60),
            max_control_body: 4 * 1024 * 1024,
            retired_service_ids: Vec::new(),
            allow_dvzip_archive_boundary: false,
        }
    }
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
        kind: TransferFailureKind,
    },
    Asked(IncomingTransfer),
    ModeSelected(TransferFormat),
    Receiving {
        transfer_id: String,
        received: u64,
        total: Option<u64>,
    },
    Confirming {
        transfer_id: String,
    },
    ArchiveBoundary {
        transfer_id: String,
    },
    TransferFailed {
        transfer_id: String,
        error: String,
        kind: TransferFailureKind,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferFormat {
    Gzip,
    DvZip,
}
#[derive(Debug)]
pub struct ReceiverError(io::Error);
impl ReceiverError {
    pub fn kind(&self) -> TransferFailureKind {
        TransferFailureKind::from_io(&self.0)
    }
}
impl From<io::Error> for ReceiverError {
    fn from(e: io::Error) -> Self {
        Self(e)
    }
}
impl std::fmt::Display for ReceiverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for ReceiverError {}
type DecisionHandler = dyn Fn(&IncomingTransfer) -> bool + Send + Sync;
pub struct ReceiverCore {
    options: ReceiverOptions,
    active: AtomicUsize,
    decide: Option<Arc<DecisionHandler>>,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}
struct Published {
    paths: Vec<PathBuf>,
    confirmed: bool,
}
impl Drop for Published {
    fn drop(&mut self) {
        if !self.confirmed {
            rollback(&self.paths);
        }
    }
}
impl ReceiverCore {
    pub fn new(options: ReceiverOptions) -> Self {
        Self {
            options,
            active: AtomicUsize::new(0),
            decide: None,
        }
    }
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
    pub fn has_active_transfers(&self) -> bool {
        self.active.load(Ordering::Acquire) > 0
    }
    pub fn serve_tls_connection(
        stream: TcpStream,
        config: Arc<ServerConfig>,
        core: Arc<Self>,
    ) -> Result<Vec<ReceiverEvent>, ReceiverError> {
        let mut events = Vec::new();
        Self::serve_tls_connection_with_events(stream, config, core, |e| events.push(e))?;
        Ok(events)
    }
    pub fn serve_tls_connection_with_events(
        stream: TcpStream,
        config: Arc<ServerConfig>,
        core: Arc<Self>,
        mut notify: impl FnMut(ReceiverEvent),
    ) -> Result<(), ReceiverError> {
        Self::serve_with_stop(stream, config, core, &AtomicBool::new(false), &mut notify)
    }
    fn serve_with_stop(
        stream: TcpStream,
        config: Arc<ServerConfig>,
        core: Arc<Self>,
        stop: &AtomicBool,
        mut notify: impl FnMut(ReceiverEvent),
    ) -> Result<(), ReceiverError> {
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        let session = ServerConnection::new(config)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut tls = StreamOwned::new(session, TrackedSocket::new(stream));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let mut config = proto::Config::new(&core.options.computer_name);
        config.model_name = core.options.model_name.clone();
        config.ask_timeout = core.options.decision_timeout;
        config.limits.max_buffered = core.options.max_control_body;
        config.limits.plist.max_bytes = core.options.max_control_body;
        config.allow_dvzip_archive_boundary = core.options.allow_dvzip_archive_boundary;
        let mut connection = Connection::new(Arc::new(config));
        let mut incoming: Option<IncomingTransfer> = None;
        let mut board: Option<Tracker> = None;
        let mut active = None;
        let mut staging: Option<Staging> = None;
        let mut published = Published {
            paths: Vec::new(),
            confirmed: false,
        };
        let mut failure = None;
        let mut deadline = None;
        let mut last_progress = None;
        let mut last_update = Instant::now();
        let mut idle = Instant::now() + Duration::from_secs(120);
        let result = (|| -> io::Result<()> {
            let mut buffer = [0; 64 << 10];
            loop {
                if stop.load(Ordering::Acquire) {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "receiver stopped",
                    ));
                }
                while let Some(event) = connection.poll() {
                    if let (Some(transfer), Some(progress)) =
                        (&incoming, connection.upload_progress())
                    {
                        let first_bytes =
                            last_progress.is_some_and(|(n, _)| n == 0) && progress.0 > 0;
                        let terminal =
                            matches!(&event, Event::UploadComplete | Event::UploadFailed(_));
                        if last_progress != Some(progress)
                            && (last_progress.is_none()
                                || first_bytes
                                || terminal
                                || last_update.elapsed() >= Duration::from_millis(100)
                                || progress.1 == Some(progress.0))
                        {
                            last_progress = Some(progress);
                            last_update = Instant::now();
                            if let Some(b) = &mut board {
                                b.update(Phase::Transferring, progress.0, progress.1, Reason::None);
                            }
                            notify(ReceiverEvent::Receiving {
                                transfer_id: transfer.id.clone(),
                                received: progress.0,
                                total: progress.1,
                            });
                        }
                    }
                    match event {
                        Event::Transmit(bytes) => {
                            tls.write_all(&bytes)?;
                            tls.flush()?;
                            if !published.paths.is_empty() && bytes.starts_with(b"HTTP/1.1 200") {
                                published.confirmed = true;
                                if let Some(b) = &mut board {
                                    b.phase(Phase::Complete);
                                }
                                notify(ReceiverEvent::Received {
                                    transfer: incoming.clone().unwrap(),
                                    files: published.paths.clone(),
                                });
                                active = None;
                            }
                        }
                        Event::SetTimer(after) => deadline = after.map(|d| Instant::now() + d),
                        Event::Discovered(_) => notify(ReceiverEvent::Discovered),
                        Event::Ask(ask) => {
                            let transfer = IncomingTransfer::from_ask(&ask);
                            core.active.fetch_add(1, Ordering::AcqRel);
                            active = Some(Active(&core.active));
                            board = Some(Tracker::begin(
                                crate::board_feedback_enabled(),
                                1,
                                &transfer.sender_name,
                                transfer.files.len(),
                            ));
                            let before = Instant::now();
                            let accepted = core
                                .decide
                                .as_ref()
                                .map_or(core.options.accept_all, |decide| decide(&transfer))
                                && before.elapsed() < core.options.decision_timeout;
                            incoming = Some(transfer.clone());
                            if accepted {
                                connection.accept();
                                if let Some(b) = &mut board {
                                    b.phase(Phase::Waiting);
                                }
                                notify(ReceiverEvent::Asked(transfer));
                            } else {
                                connection.decline();
                                active = None;
                                if let Some(b) = &mut board {
                                    b.fail(Reason::Declined);
                                }
                                notify(ReceiverEvent::Rejected {
                                    path: "/Ask".into(),
                                    status: 403,
                                });
                            }
                        }
                        Event::AskWithdrawn => {
                            active = None;
                            if let Some(b) = &mut board {
                                b.fail(Reason::Interrupted);
                            }
                        }
                        Event::Urls(_) => {
                            if let Some(b) = &mut board {
                                b.phase(Phase::Complete);
                            }
                            notify(ReceiverEvent::Received {
                                transfer: incoming.clone().unwrap(),
                                files: Vec::new(),
                            });
                            active = None;
                        }
                        Event::UploadStarted { compressor } => {
                            notify(ReceiverEvent::ModeSelected(match compressor {
                                owly_airdrop::proto::archive::compress::Compressor::Gzip => {
                                    TransferFormat::Gzip
                                }
                                owly_airdrop::proto::archive::compress::Compressor::DvZip => {
                                    TransferFormat::DvZip
                                }
                            }));
                            staging = Some(runtime.block_on(Staging::new(&core.options.save_dir))?)
                        }
                        Event::CreateDir(path) => {
                            runtime.block_on(staging.as_mut().unwrap().create_dir(&path))?
                        }
                        Event::CreateFile(path) => {
                            runtime.block_on(staging.as_mut().unwrap().create_file(&path))?
                        }
                        Event::Data(data) => {
                            runtime.block_on(staging.as_mut().unwrap().write(&data))?
                        }
                        Event::EndFile => runtime.block_on(staging.as_mut().unwrap().end_file())?,
                        Event::UploadComplete => {
                            let id = incoming.as_ref().unwrap().id.clone();
                            if connection.archive_boundary() {
                                notify(ReceiverEvent::ArchiveBoundary {
                                    transfer_id: id.clone(),
                                });
                            }
                            notify(ReceiverEvent::Confirming { transfer_id: id });
                            if let Some(b) = &mut board {
                                b.phase(Phase::Confirming);
                            }
                            match runtime.block_on(staging.take().unwrap().finish()) {
                                Ok(paths) => {
                                    published.paths = paths;
                                    connection.finish_upload(Ok(()));
                                }
                                Err(e) => {
                                    connection.finish_upload(Err(e.to_string()));
                                    failure = Some(e);
                                }
                            }
                        }
                        Event::UploadFailed(message) => {
                            if let Some(mut stage) = staging.take() {
                                let _ = runtime.block_on(stage.end_file());
                            }
                            failure = Some(io::Error::new(io::ErrorKind::InvalidData, message));
                        }
                        Event::Close => {
                            if active.is_some() && !published.confirmed && failure.is_none() {
                                return Err(io::Error::new(
                                    io::ErrorKind::UnexpectedEof,
                                    "sender closed before transfer confirmation",
                                ));
                            }
                            return match failure.take() {
                                Some(e) => Err(e),
                                None => Ok(()),
                            };
                        }
                    }
                }
                if connection.is_closed() {
                    return Ok(());
                }
                if deadline.is_some_and(|d| Instant::now() >= d) {
                    deadline = None;
                    connection.timeout();
                    continue;
                }
                if connection.upload_progress().is_some()
                    && core.options.allow_dvzip_archive_boundary
                    && !tls_has_pending_data(&mut &mut tls)?
                    && connection.finish_validated_dvzip_boundary()
                {
                    continue;
                }
                match tls.read(&mut buffer) {
                    Ok(0) => connection.receive_eof(),
                    Ok(n) => {
                        idle = Instant::now() + Duration::from_secs(120);
                        connection.receive(&buffer[..n]);
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                        ) && Instant::now() < idle =>
                    {
                        continue
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
        })();
        if let Some(mut stage) = staging {
            let _ = runtime.block_on(stage.end_file());
        }
        if let Err(e) = &result {
            if let Some(b) = &mut board {
                b.fail(Reason::Interrupted);
            }
            if let Some(transfer) = &incoming {
                notify(ReceiverEvent::TransferFailed {
                    transfer_id: transfer.id.clone(),
                    error: e.to_string(),
                    kind: TransferFailureKind::from_io(e),
                });
            }
        }
        drop(active);
        result.map_err(ReceiverError::from)
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
                    if let Err(error) = Self::serve_with_stop(
                        stream,
                        worker_config,
                        worker_core,
                        &worker_shutdown,
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
