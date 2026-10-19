//! AirDrop sender flow for an explicitly selected receiver.

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    net::{SocketAddrV6, TcpStream},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use flate2::{write::GzEncoder, Compression};
use omdrop_airdrop::plist::{self, Value};
use omdrop_airdrop::{write_cpio, ArchiveError, DvZipWriter};
use omdrop_net::http::{BodyKind, BodyReader, Input};
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::{verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    ClientConfig, ClientConnection, DigitallySignedStruct, Error as TlsError, SignatureScheme,
    StreamOwned,
};

use crate::{
    cancel::CheckedWriter,
    mdns::{AirDropService, NetworkInterface},
    Identity, SendCancellation, TransferFailureKind,
};

const CONTROL_BODY_LIMIT: usize = 4 * 1024 * 1024;
const UPLOAD_CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendFormat {
    Auto,
    Gzip,
    DvZip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendOptions {
    pub sender_name: String,
    pub sender_model: String,
    pub target_name: String,
    pub timeout: Duration,
    pub format: SendFormat,
    pub ble_wake: bool,
    pub legacy_ble: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReport {
    pub receiver: String,
    pub files: usize,
    pub source_bytes: u64,
    pub archive_bytes: u64,
    pub format: SendFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendEvent {
    Preparing,
    WaitingForAcceptance { receiver: String },
    Uploading { sent: u64, total: u64 },
    WaitingForConfirmation,
    Completed(SendReport),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NearbyReceiver {
    pub name: String,
    pub service: AirDropService,
}

#[derive(Debug)]
pub struct SendError {
    message: String,
    reason: omdrop_board::Reason,
    kind: TransferFailureKind,
}

impl SendError {
    pub fn kind(&self) -> TransferFailureKind {
        self.kind
    }

    pub(crate) fn cancelled() -> Self {
        Self::invalid("operation cancelled").with_kind(TransferFailureKind::Cancelled)
    }

    fn with_kind(mut self, kind: TransferFailureKind) -> Self {
        self.kind = kind;
        self
    }

    /// The receiver explicitly declined the offer or upload.
    pub fn is_declined(&self) -> bool {
        self.reason == omdrop_board::Reason::Declined
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            reason: omdrop_board::Reason::None,
            kind: TransferFailureKind::Validation,
        }
    }

    fn with_reason(mut self, reason: omdrop_board::Reason) -> Self {
        self.reason = reason;
        if reason == omdrop_board::Reason::Declined {
            self.kind = TransferFailureKind::Declined;
        }
        self
    }
}

impl From<io::Error> for SendError {
    fn from(error: io::Error) -> Self {
        Self {
            message: error.to_string(),
            kind: TransferFailureKind::from_io(&error),
            reason: match error.kind() {
                io::ErrorKind::TimedOut => omdrop_board::Reason::Timeout,
                io::ErrorKind::ConnectionAborted
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::NotConnected
                | io::ErrorKind::BrokenPipe => omdrop_board::Reason::Connection,
                _ => omdrop_board::Reason::None,
            },
        }
    }
}

impl From<TlsError> for SendError {
    fn from(error: TlsError) -> Self {
        Self::invalid(format!("TLS: {error}")).with_kind(TransferFailureKind::Connection)
    }
}

impl From<ArchiveError> for SendError {
    fn from(error: ArchiveError) -> Self {
        let kind = match &error {
            ArchiveError::Io(error) => TransferFailureKind::from_io(error),
            _ => TransferFailureKind::Validation,
        };
        Self::invalid(format!("archive: {error}")).with_kind(kind)
    }
}

impl From<plist::PlistError> for SendError {
    fn from(error: plist::PlistError) -> Self {
        Self::invalid(format!("plist: {error}")).with_kind(TransferFailureKind::Validation)
    }
}

impl From<omdrop_net::http::Error> for SendError {
    fn from(error: omdrop_net::http::Error) -> Self {
        match error {
            omdrop_net::http::Error::Io(error) => Self::from(error),
            error => {
                Self::invalid(format!("HTTP: {error}")).with_kind(TransferFailureKind::Validation)
            }
        }
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SendError {}

/// Browse without offering files. Use `discover_receivers_live` for UI events.
pub fn discover_receivers(
    interface: NetworkInterface,
    identity: &Identity,
    timeout: Duration,
    ble_wake: bool,
    legacy_ble: bool,
) -> Result<Vec<NearbyReceiver>, SendError> {
    crate::discovery::discover_receivers_live(
        interface,
        identity,
        timeout,
        ble_wake,
        legacy_ble,
        |_| {},
    )
}

pub fn send_to_name(
    interface: NetworkInterface,
    identity: &Identity,
    files: &[PathBuf],
    options: &SendOptions,
) -> Result<SendReport, SendError> {
    validate_files(files)?;
    let receivers = discover_receivers(
        interface,
        identity,
        options.timeout,
        options.ble_wake,
        options.legacy_ble,
    )?;
    let selected = crate::discovery::receiver_by_name(&receivers, &options.target_name)?;
    send_to_receiver(selected, identity, files, options, |_| {})
}

/// Send only to the chosen service identity/address. A UI must explicitly select it first.
pub fn send_to_receiver(
    selected: &NearbyReceiver,
    identity: &Identity,
    files: &[PathBuf],
    options: &SendOptions,
    notify: impl FnMut(SendEvent),
) -> Result<SendReport, SendError> {
    send_to_receiver_cancellable(
        selected,
        identity,
        files,
        options,
        &SendCancellation::default(),
        notify,
    )
}

/// Like `send_to_receiver`, with cancellation during archive creation, acceptance and upload.
pub fn send_to_receiver_cancellable(
    selected: &NearbyReceiver,
    identity: &Identity,
    files: &[PathBuf],
    options: &SendOptions,
    cancel: &SendCancellation,
    mut notify: impl FnMut(SendEvent),
) -> Result<SendReport, SendError> {
    use omdrop_board::{Phase, Reason, Tracker};
    let mut board = Tracker::begin(
        crate::board_feedback_enabled(),
        0,
        &selected.name,
        files.len(),
    );
    let mut result = (|| {
        let mut report = |event: SendEvent| {
            match &event {
                SendEvent::Preparing => board.phase(Phase::Preparing),
                SendEvent::WaitingForAcceptance { .. } => board.phase(Phase::Waiting),
                SendEvent::Uploading { sent, total } => {
                    board.update(Phase::Transferring, *sent, Some(*total), Reason::None)
                }
                SendEvent::WaitingForConfirmation => board.phase(Phase::Confirming),
                SendEvent::Completed(_) => board.phase(Phase::Complete),
            }
            notify(event);
        };
        cancel.check()?;
        let source_bytes = validate_files(files)?;
        #[cfg(windows)]
        let _advert = if options.ble_wake {
            Some(crate::ble::AirDropAdvert::start(options.legacy_ble)?)
        } else {
            None
        };
        let client = client_config(identity)?;
        let format = match options.format {
            SendFormat::Auto
                if selected
                    .service
                    .flags
                    .is_some_and(|flags| flags & 0x02 != 0) =>
            {
                SendFormat::DvZip
            }
            SendFormat::Auto => SendFormat::Gzip,
            format => format,
        };
        let options = SendOptions {
            target_name: selected.name.clone(),
            format,
            ..options.clone()
        };
        send_offer(
            &selected.service,
            &client,
            files,
            source_bytes,
            &options,
            &mut report,
            cancel,
        )
    })();
    cancel.detach();
    // A verified final confirmation wins over a concurrent cancellation.
    if result.is_err() && cancel.is_cancelled() {
        result = Err(SendError::cancelled());
    }
    if let Err(error) = &result {
        if cancel.is_cancelled() {
            board.phase(Phase::Cancelled);
        } else {
            board.fail(error.reason);
        }
    }
    result
}

/// Validate before browsing or showing a remote offer.
pub fn validate_files(files: &[PathBuf]) -> Result<u64, SendError> {
    if files.is_empty() {
        return Err(SendError::invalid("at least one file is required"));
    }
    let mut total = 0u64;
    let mut names = std::collections::HashSet::new();
    for path in files {
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(SendError::invalid(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| SendError::invalid("file name is not valid UTF-8"))?;
        if !names.insert(name.to_lowercase()) {
            return Err(SendError::invalid(format!(
                "duplicate archive file name: {name}"
            )));
        }
        total = total
            .checked_add(metadata.len())
            .ok_or_else(|| SendError::invalid("total file size overflow"))?;
    }
    Ok(total)
}

pub(crate) fn client_config(identity: &Identity) -> Result<Arc<ClientConfig>, SendError> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let algorithms = provider.signature_verification_algorithms;
    let verifier = Arc::new(AirdropCertificateVerifier { algorithms });
    let builder =
        ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions()?;
    let certificate = CertificateDer::from(identity.certificate_der.clone());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.private_key_der.clone()));
    let mut config = builder
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![certificate], key)?;
    config.alpn_protocols.clear();
    Ok(Arc::new(config))
}

#[derive(Debug)]
struct AirdropCertificateVerifier {
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for AirdropCertificateVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        // AirDrop peers use self-signed certificates and a link-local endpoint. Keep normal
        // handshake signature verification below; only the chain/name check is unavailable.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, certificate, signature, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, certificate, signature, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

pub(crate) fn discover(
    service: &AirDropService,
    client: &Arc<ClientConfig>,
    timeout: Duration,
    cancel: &SendCancellation,
) -> Result<Option<String>, SendError> {
    cancel.check()?;
    let mut connection = connect(service.address, client, timeout)?;
    cancel.attach(connection.get_ref().get_ref())?;
    let body = plist::write(&Value::Dictionary(Vec::new()))?;
    let response = post_bytes(&mut connection, "/Discover", &body)?;
    connection.get_mut().conn.send_close_notify();
    let _ = connection.flush();
    if response.status != 200 {
        return Ok(None);
    }
    let Value::Dictionary(entries) = plist::read(&response.body)? else {
        return Ok(None);
    };
    Ok(entries.into_iter().find_map(|(key, value)| {
        (key == "ReceiverComputerName")
            .then_some(value)
            .and_then(|value| match value {
                Value::String(name) => Some(name),
                _ => None,
            })
    }))
}

fn send_offer(
    service: &AirDropService,
    client: &Arc<ClientConfig>,
    files: &[PathBuf],
    source_bytes: u64,
    options: &SendOptions,
    notify: &mut impl FnMut(SendEvent),
    cancel: &SendCancellation,
) -> Result<SendReport, SendError> {
    notify(SendEvent::Preparing);
    let upload_format = match options.format {
        SendFormat::Auto | SendFormat::Gzip => UploadFormatChoice::Gzip,
        SendFormat::DvZip => UploadFormatChoice::DvZip,
    };
    let mut archive = create_archive_cancellable(files, upload_format, cancel)?;
    let archive_bytes = archive.metadata()?.len();
    let mut connection = connect_for_transfer(service.address, client, cancel)?;
    let transfer_id = transfer_id()?;
    let sender_id = random_hex(6)?;
    let ask = build_outgoing_ask(
        &options.sender_name,
        &options.sender_model,
        &sender_id,
        &transfer_id,
        files,
    )?;
    notify(SendEvent::WaitingForAcceptance {
        receiver: options.target_name.clone(),
    });
    let response = post_bytes(&mut connection, "/Ask", &ask)?;
    if response.status != 200 {
        if matches!(response.status, 401 | 403) {
            return Err(SendError::invalid(format!(
                "receiver \"{}\" declined the transfer (HTTP {})",
                options.target_name, response.status
            ))
            .with_reason(omdrop_board::Reason::Declined));
        }
        return Err(SendError::invalid(format!(
            "receiver \"{}\" could not accept the transfer (HTTP {})",
            options.target_name, response.status
        )));
    }

    if response.close || !connection_open(&mut connection)? {
        connection = connect_for_transfer(service.address, client, cancel)?;
    }
    let content_type = match upload_format {
        UploadFormatChoice::Gzip => "application/x-cpio",
        UploadFormatChoice::DvZip => "application/x-dvzip",
    };
    let response = post_chunked(
        &mut connection,
        content_type,
        &transfer_id,
        source_bytes,
        &mut archive,
        notify,
    )?;
    if response.status != 200 {
        let error = SendError::invalid(format!(
            "receiver refused the upload (HTTP {})",
            response.status
        ));
        return Err(if matches!(response.status, 401 | 403) {
            error.with_reason(omdrop_board::Reason::Declined)
        } else {
            error
        });
    }
    let report = SendReport {
        receiver: options.target_name.clone(),
        files: files.len(),
        source_bytes,
        archive_bytes,
        format: options.format,
    };
    notify(SendEvent::Completed(report.clone()));
    Ok(report)
}

#[derive(Clone, Copy)]
enum UploadFormatChoice {
    Gzip,
    DvZip,
}

#[cfg(test)]
fn create_archive(files: &[PathBuf], format: UploadFormatChoice) -> Result<File, SendError> {
    create_archive_cancellable(files, format, &SendCancellation::default())
}

fn create_archive_cancellable(
    files: &[PathBuf],
    format: UploadFormatChoice,
    cancel: &SendCancellation,
) -> Result<File, SendError> {
    cancel.check()?;
    let mut archive = tempfile::tempfile()?;
    match format {
        UploadFormatChoice::Gzip => {
            let mut encoder =
                GzEncoder::new(CheckedWriter(&mut archive, cancel), Compression::fast());
            write_cpio(CheckedWriter(&mut encoder, cancel), files.iter())?;
            let _ = encoder.finish()?;
        }
        UploadFormatChoice::DvZip => {
            let mut encoder = DvZipWriter::new(CheckedWriter(&mut archive, cancel), UPLOAD_CHUNK)?;
            write_cpio(CheckedWriter(&mut encoder, cancel), files.iter())?;
            let _ = encoder.finish()?;
        }
    }
    archive.flush()?;
    archive.seek(SeekFrom::Start(0))?;
    Ok(archive)
}

fn connect(
    address: SocketAddrV6,
    client: &Arc<ClientConfig>,
    timeout: Duration,
) -> Result<Input<StreamOwned<ClientConnection, TcpStream>>, SendError> {
    let tcp = TcpStream::connect_timeout(&address.into(), timeout)?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(timeout))?;
    tcp.set_write_timeout(Some(timeout))?;
    let server_name = ServerName::try_from("airdrop".to_owned())
        .map_err(|_| SendError::invalid("invalid AirDrop TLS server name"))?;
    let tls = ClientConnection::new(Arc::clone(client), server_name)?;
    Ok(Input::new(StreamOwned::new(tls, tcp)))
}

fn connect_for_transfer(
    address: SocketAddrV6,
    client: &Arc<ClientConfig>,
    cancel: &SendCancellation,
) -> Result<Input<StreamOwned<ClientConnection, TcpStream>>, SendError> {
    cancel.check()?;
    // Bound the one phase that cannot be interrupted with socket shutdown.
    let connection = connect(address, client, Duration::from_secs(3))?;
    let socket = connection.get_ref().get_ref();
    cancel.attach(socket)?;
    socket.set_read_timeout(Some(Duration::from_secs(120)))?;
    socket.set_write_timeout(Some(Duration::from_secs(120)))?;
    Ok(connection)
}

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
    close: bool,
}

fn post_bytes(
    connection: &mut Input<StreamOwned<ClientConnection, TcpStream>>,
    path: &str,
    body: &[u8],
) -> Result<HttpResponse, SendError> {
    write!(
        connection,
        "POST {path} HTTP/1.1\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: keep-alive\r\nAccept: */*\r\nUser-Agent: AirDrop/1.0\r\nAccept-Language: en-us\r\n\r\n",
        body.len()
    )?;
    connection.write_all(body)?;
    connection.flush()?;
    read_final_response(connection)
}

fn post_chunked(
    connection: &mut Input<StreamOwned<ClientConnection, TcpStream>>,
    content_type: &str,
    transfer_id: &str,
    total_bytes: u64,
    body: &mut File,
    notify: &mut impl FnMut(SendEvent),
) -> Result<HttpResponse, SendError> {
    write!(
        connection,
        "POST /Upload HTTP/1.1\r\nContent-Type: {content_type}\r\nTransferID: {transfer_id}\r\nTotalBytes: {total_bytes}\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\nConnection: keep-alive\r\nAccept: */*\r\nUser-Agent: AirDrop/1.0\r\nAccept-Language: en-us\r\n\r\n"
    )?;
    connection.flush()?;
    connection
        .get_ref()
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(2)))?;
    // Wait for HTTP bytes, not encrypted TLS session tickets. No HTTP line is
    // consumed here, so a timeout cannot discard a partially received response.
    let ready = connection.ensure_data();
    connection
        .get_ref()
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(120)))?;
    let has_response = match ready {
        Ok(true) => true,
        Ok(false) => {
            return Err(SendError::invalid(
                "receiver closed before accepting the upload",
            ))
        }
        Err(omdrop_net::http::Error::Io(error)) if is_io_timeout(&error) => false,
        Err(error) => return Err(error.into()),
    };
    if has_response {
        let mut interim_count = 0;
        loop {
            let response = read_response(connection)?;
            if response.status == 100 {
                break;
            }
            if response.status >= 200 {
                if response.status < 300 {
                    return Err(SendError::invalid(
                        "receiver returned success before the upload body was sent",
                    ));
                }
                return Ok(response);
            }
            interim_count += 1;
            if interim_count >= 8 {
                return Err(SendError::invalid("too many interim HTTP responses"));
            }
        }
    }
    let total = body.metadata()?.len();
    write_chunked_body(connection, body, total, notify)?;
    notify(SendEvent::WaitingForConfirmation);
    read_final_response(connection)
}

fn write_chunked_body<W: Write, R: Read>(
    output: &mut W,
    input: &mut R,
    total: u64,
    notify: &mut impl FnMut(SendEvent),
) -> Result<(), SendError> {
    // Reserve a prefix for the largest hexadecimal usize plus CRLF. Read the
    // payload straight into its final position so each chunk is one TLS write,
    // without allocating a header or copying the payload on every iteration.
    const PREFIX: usize = 2 * std::mem::size_of::<usize>() + 2;
    let mut buffer = vec![0u8; PREFIX + UPLOAD_CHUNK + 2];
    let mut sent = 0;
    notify(SendEvent::Uploading { sent, total });
    loop {
        let length = match input.read(&mut buffer[PREFIX..PREFIX + UPLOAD_CHUNK]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if length == 0 {
            break;
        }
        let mut header = [0; PREFIX];
        let mut remaining = &mut header[..];
        write!(remaining, "{length:x}\r\n")?;
        let header_len = PREFIX - remaining.len();
        let start = PREFIX - header_len;
        buffer[start..PREFIX].copy_from_slice(&header[..header_len]);
        buffer[PREFIX + length..PREFIX + length + 2].copy_from_slice(b"\r\n");
        output.write_all(&buffer[start..PREFIX + length + 2])?;
        output.flush()?;
        sent += length as u64;
        notify(SendEvent::Uploading { sent, total });
    }
    output.write_all(b"0\r\n\r\n")?;
    output.flush()?;
    Ok(())
}

fn read_response<R: Read>(input: &mut Input<R>) -> Result<HttpResponse, SendError> {
    let line = input.read_line()?.ok_or_else(|| {
        SendError::invalid("connection closed before the HTTP response")
            .with_kind(TransferFailureKind::Connection)
    })?;
    let mut parts = line.split_whitespace();
    let version = parts.next().unwrap_or_default();
    let status = parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| SendError::invalid("malformed HTTP status line"))?;
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") || !(100..600).contains(&status) {
        return Err(SendError::invalid("unsupported HTTP response version"));
    }
    let mut content_length = None;
    let mut chunked = false;
    let mut close = false;
    let mut header_count = 0;
    loop {
        let header = input
            .read_line()?
            .ok_or_else(|| SendError::invalid("truncated HTTP response headers"))?;
        if header.is_empty() {
            break;
        }
        header_count += 1;
        if header_count > 128 {
            return Err(SendError::invalid("too many response headers"));
        }
        let (name, value) = header
            .split_once(':')
            .ok_or_else(|| SendError::invalid("malformed HTTP response header"))?;
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .trim()
                    .parse::<u64>()
                    .map_err(|_| SendError::invalid("invalid response Content-Length"))?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("chunked"));
        } else if name.eq_ignore_ascii_case("connection") {
            close = value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("close"));
        }
    }
    let kind = if chunked {
        BodyKind::Chunked
    } else if let Some(length) = content_length {
        BodyKind::Length(length)
    } else {
        BodyKind::Empty
    };
    let body = BodyReader::new(input, kind).read_to_end_limited(CONTROL_BODY_LIMIT)?;
    Ok(HttpResponse {
        status,
        body,
        close,
    })
}

fn read_final_response<R: Read>(input: &mut Input<R>) -> Result<HttpResponse, SendError> {
    for _ in 0..8 {
        let response = read_response(input)?;
        if response.status >= 200 {
            return Ok(response);
        }
    }
    Err(SendError::invalid("too many interim HTTP responses"))
}

fn connection_open(
    connection: &mut Input<StreamOwned<ClientConnection, TcpStream>>,
) -> Result<bool, SendError> {
    let tls = connection.get_mut();
    if tls.conn.process_new_packets()?.peer_has_closed() {
        return Ok(false);
    }
    tls.sock.set_nonblocking(true)?;
    let result = (|| {
        for _ in 0..16 {
            match tls.conn.read_tls(&mut tls.sock) {
                Ok(0) => return Ok(false),
                Ok(_) => {
                    if tls.conn.process_new_packets()?.peer_has_closed() {
                        return Ok(false);
                    }
                }
                Err(error) if is_io_timeout(&error) => return Ok(true),
                Err(error) if error.kind() == io::ErrorKind::ConnectionReset => return Ok(false),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(true)
    })();
    tls.sock.set_nonblocking(false)?;
    result
}

fn is_io_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn build_outgoing_ask(
    sender_name: &str,
    sender_model: &str,
    sender_id: &str,
    transfer_id: &str,
    files: &[PathBuf],
) -> Result<Vec<u8>, SendError> {
    let mut file_values = Vec::with_capacity(files.len());
    for path in files {
        let metadata = fs::metadata(path)?;
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| SendError::invalid("file name is not valid UTF-8"))?;
        let size = i64::try_from(metadata.len())
            .map_err(|_| SendError::invalid("file is too large for the AirDrop plist"))?;
        file_values.push(Value::Dictionary(vec![
            ("FileName".into(), Value::String(name.into())),
            ("FileType".into(), Value::String(uti(path).into())),
            ("FileSize".into(), Value::Integer(size)),
            ("FileBomPath".into(), Value::String(format!("./{name}"))),
            ("FileIsDirectory".into(), Value::Bool(false)),
            ("ShouldConvertMediaFormats".into(), Value::Bool(false)),
        ]));
    }
    plist::write(&Value::Dictionary(vec![
        (
            "TransferID".into(),
            Value::Dictionary(vec![("id".into(), Value::String(transfer_id.into()))]),
        ),
        (
            "TransferType".into(),
            Value::Dictionary(vec![("files".into(), Value::Dictionary(Vec::new()))]),
        ),
        ("SenderID".into(), Value::String(sender_id.into())),
        ("BundleID".into(), Value::String("com.apple.finder".into())),
        (
            "SenderComputerName".into(),
            Value::String(sender_name.into()),
        ),
        ("SenderModelName".into(), Value::String(sender_model.into())),
        ("Items".into(), Value::Array(Vec::new())),
        ("Files".into(), Value::Array(file_values)),
        ("ConvertMediaFormats".into(), Value::Bool(false)),
    ]))
    .map_err(SendError::from)
}

fn uti(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "public.jpeg",
        "png" => "public.png",
        "heic" => "public.heic",
        "gif" => "com.compuserve.gif",
        "mov" => "com.apple.quicktime-movie",
        "mp4" => "public.mpeg-4",
        "m4a" => "com.apple.m4a-audio",
        "mp3" => "public.mp3",
        "pdf" => "com.adobe.pdf",
        "txt" => "public.plain-text",
        "zip" => "public.zip-archive",
        "vcf" => "public.vcard",
        _ => "public.data",
    }
}

fn random_hex(bytes: usize) -> Result<String, SendError> {
    let mut random = vec![0u8; bytes];
    getrandom::fill(&mut random).map_err(|error| SendError::invalid(error.to_string()))?;
    Ok(hex::encode(random))
}

fn transfer_id() -> Result<String, SendError> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|error| SendError::invalid(error.to_string()))?;
    random[6] = (random[6] & 0x0f) | 0x40;
    random[8] = (random[8] & 0x3f) | 0x80;
    let hex = hex::encode_upper(random);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{server_config, ReceiverCore, ReceiverEvent, ReceiverOptions};
    use omdrop_net::http::{write_continue, write_response, RequestHead};
    use std::net::{Ipv6Addr, TcpListener};

    #[test]
    fn outgoing_ask_has_transfer_metadata_and_file_size() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("photo.jpg");
        fs::write(&file, b"photo bytes").unwrap();
        let id = "01234567-89AB-4CDE-8FAB-0123456789AB";
        let ask = build_outgoing_ask("sender", "Windows", "aabbccddeeff", id, &[file]).unwrap();
        let Value::Dictionary(entries) = plist::read(&ask).unwrap() else {
            panic!("outgoing /Ask must be a dictionary");
        };
        let value = |name: &str| entries.iter().find(|(key, _)| key == name).unwrap();
        let Value::Dictionary(transfer) = &value("TransferID").1 else {
            panic!("TransferID must be a dictionary");
        };
        assert!(transfer.contains(&("id".into(), Value::String(id.into()))));
        let Value::Array(files) = &value("Files").1 else {
            panic!("Files must be an array");
        };
        let Value::Dictionary(file) = &files[0] else {
            panic!("file entry must be a dictionary");
        };
        assert!(file.contains(&("FileType".into(), Value::String("public.jpeg".into()))));
        assert!(file.contains(&("FileSize".into(), Value::Integer(11))));
    }

    #[test]
    fn transfer_id_is_uppercase_rfc4122_v4() {
        let id = transfer_id().unwrap();
        assert_eq!(id.len(), 36);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'));
        assert_eq!(id.as_bytes()[14], b'4');
        assert!(matches!(id.as_bytes()[19], b'8' | b'9' | b'A' | b'B'));
        assert!(random_hex(6).unwrap().len() == 12);
    }

    #[test]
    fn archives_stream_through_gzip_and_dvzip_files() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("payload.txt");
        fs::write(&file, vec![b'x'; 96 * 1024]).unwrap();
        for format in [UploadFormatChoice::Gzip, UploadFormatChoice::DvZip] {
            let mut archive = create_archive(std::slice::from_ref(&file), format).unwrap();
            let len = archive.metadata().unwrap().len();
            assert!(len > 4);
            let mut content = Vec::new();
            archive.read_to_end(&mut content).unwrap();
            if matches!(format, UploadFormatChoice::Gzip) {
                assert!(content.starts_with(&[0x1f, 0x8b]));
            } else {
                assert!(content.len() > 4);
            }
        }
    }

    #[test]
    fn sends_offer_and_chunked_upload_to_rust_receiver() {
        for (format, flags, filename) in [
            (SendFormat::Gzip, Some(0), "payload-gzip.txt"),
            (SendFormat::DvZip, Some(0x02), "payload-dvzip.txt"),
        ] {
            round_trip(format, flags, filename);
        }
    }

    fn round_trip(format: SendFormat, flags: Option<u32>, filename: &str) {
        let directory = tempfile::tempdir().unwrap();
        let incoming = directory.path().join("incoming");
        fs::create_dir(&incoming).unwrap();
        let source = directory.path().join(filename);
        // Incompressible data exercises many upload chunks in both formats.
        let mut state = 0x12345678u32;
        let contents: Vec<u8> = (0..1024 * 1024 + 7)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as u8
            })
            .collect();
        fs::write(&source, &contents).unwrap();
        let unicode = directory.path().join("照片说明.txt");
        let caption = "照片的说明\n".as_bytes();
        fs::write(&unicode, caption).unwrap();
        let empty = directory.path().join("empty.txt");
        fs::write(&empty, b"").unwrap();
        let files = vec![source, unicode, empty];

        let receiver_identity =
            Identity::load_or_create(directory.path().join("receiver-id"), "receiver").unwrap();
        let receiver_tls = server_config(&receiver_identity).unwrap();
        let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
        let address = match listener.local_addr().unwrap() {
            std::net::SocketAddr::V6(address) => address,
            _ => unreachable!(),
        };
        let receiver = Arc::new(ReceiverCore::new(
            ReceiverOptions::new("receiver", "omdrop", &incoming).with_accept_all(),
        ));
        let server_receiver = Arc::clone(&receiver);
        let server = std::thread::spawn(move || {
            let mut events = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                events.extend(
                    ReceiverCore::serve_tls_connection(
                        stream,
                        Arc::clone(&receiver_tls),
                        Arc::clone(&server_receiver),
                    )
                    .unwrap(),
                );
            }
            events
        });

        let sender_identity =
            Identity::load_or_create(directory.path().join("sender-id"), "sender").unwrap();
        let client = client_config(&sender_identity).unwrap();
        let service = AirDropService {
            instance: "receiver._airdrop._tcp.local".into(),
            host: "receiver.local".into(),
            port: address.port(),
            address,
            flags,
        };
        assert_eq!(
            discover(
                &service,
                &client,
                Duration::from_secs(5),
                &SendCancellation::default()
            )
            .unwrap()
            .as_deref(),
            Some("receiver")
        );
        let mut events = Vec::new();
        let cancel = SendCancellation::default();
        let report = send_to_receiver_cancellable(
            &NearbyReceiver {
                name: "receiver".into(),
                service,
            },
            &sender_identity,
            &files,
            &SendOptions {
                sender_name: "sender".into(),
                sender_model: "Windows".into(),
                target_name: "receiver".into(),
                timeout: Duration::from_secs(5),
                format: SendFormat::Auto,
                ble_wake: false,
                legacy_ble: false,
            },
            &cancel,
            |event| {
                if matches!(event, SendEvent::Completed(_)) {
                    cancel.cancel();
                }
                events.push(event);
            },
        )
        .unwrap();
        assert!(matches!(events.first(), Some(SendEvent::Preparing)));
        assert!(matches!(events.last(), Some(SendEvent::Completed(_))));
        assert!(events.iter().any(|e| matches!(e, SendEvent::Uploading { sent, total } if sent == total && *sent == report.archive_bytes)));
        assert_eq!(report.files, 3);
        assert_eq!(report.format, format);
        assert_eq!(report.source_bytes, (contents.len() + caption.len()) as u64);
        assert_eq!(fs::read(incoming.join(filename)).unwrap(), contents);
        assert_eq!(fs::read(incoming.join("照片说明.txt")).unwrap(), caption);
        assert!(fs::read(incoming.join("empty.txt")).unwrap().is_empty());
        let progress: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                SendEvent::Uploading { sent, total } => Some((*sent, *total)),
                _ => None,
            })
            .collect();
        assert!(progress.len() > 16);
        assert_eq!(progress[0].0, 0);
        assert!(progress
            .windows(2)
            .all(|pair| pair[0].0 < pair[1].0 && pair[0].1 == pair[1].1));
        assert!(matches!(
            events[events.len() - 2],
            SendEvent::WaitingForConfirmation
        ));
        assert!(server.join().unwrap().iter().any(|event| matches!(
            event,
            ReceiverEvent::Received { transfer, .. }
                if transfer.sender_name == "sender" && transfer.files.len() == 3
        )));
    }

    #[test]
    fn chunked_upload_coalesces_writes_and_preserves_framing() {
        #[derive(Default)]
        struct Sink {
            bytes: Vec<u8>,
            writes: usize,
            max_write: usize,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.writes += 1;
                let count = bytes.len().min(self.max_write);
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for length in [
            0,
            1,
            UPLOAD_CHUNK - 1,
            UPLOAD_CHUNK,
            UPLOAD_CHUNK + 1,
            2 * UPLOAD_CHUNK + 17,
        ] {
            let payload: Vec<_> = (0..length).map(|i| (i * 131) as u8).collect();
            for max_write in [usize::MAX, 7] {
                let mut output = Sink {
                    max_write,
                    ..Sink::default()
                };
                let mut progress = Vec::new();
                write_chunked_body(
                    &mut output,
                    &mut io::Cursor::new(&payload),
                    length as u64,
                    &mut |event| {
                        if let SendEvent::Uploading { sent, total } = event {
                            progress.push((sent, total));
                        }
                    },
                )
                .unwrap();
                if max_write == usize::MAX {
                    assert_eq!(output.writes, length.div_ceil(UPLOAD_CHUNK) + 1);
                }
                assert_eq!(progress.first(), Some(&(0, length as u64)));
                assert_eq!(progress.last(), Some(&(length as u64, length as u64)));
                output.bytes.extend_from_slice(b"HTTP/1.1 200 OK\r\n");
                let mut input = Input::new(io::Cursor::new(output.bytes));
                let decoded = BodyReader::new(&mut input, BodyKind::Chunked)
                    .read_to_end_limited(length)
                    .unwrap();
                assert_eq!(decoded, payload);
                assert_eq!(
                    input.read_line().unwrap().as_deref(),
                    Some("HTTP/1.1 200 OK")
                );
            }
        }
    }

    #[test]
    fn chunked_upload_retries_interrupted_reads_and_propagates_write_errors() {
        struct Source(bool);
        impl Read for Source {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if !self.0 {
                    self.0 = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                out[0] = 42;
                Ok(1)
            }
        }
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                panic!("failed chunk must not be flushed")
            }
        }
        let mut progress = 0;
        assert!(
            write_chunked_body(&mut Broken, &mut Source(false), 1, &mut |_| progress += 1).is_err()
        );
        assert_eq!(progress, 1, "failed write must not advance progress");
    }

    #[test]
    fn validates_files_before_remote_offer() {
        let directory = tempfile::tempdir().unwrap();
        assert!(validate_files(&[]).is_err());
        assert!(validate_files(&[directory.path().into()]).is_err());
        let first = directory.path().join("photo.png");
        fs::write(&first, b"a").unwrap();
        let sub = directory.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let second = sub.join("PHOTO.PNG");
        fs::write(&second, b"b").unwrap();
        assert!(validate_files(&[first, second])
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
    }

    #[test]
    fn response_parser_skips_late_continue_and_checks_final_framing() {
        let mut input = Input::new(io::Cursor::new(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive, Close\r\n\r\n2\r\nok\r\n0\r\n\r\n"));
        let response = read_final_response(&mut input).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"ok");
        assert!(response.close);
        for bytes in [
            &b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nab"[..],
            &b"HTTP/1.1 000 OK\r\n\r\n"[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 99999999\r\n\r\n"[..],
        ] {
            assert!(read_final_response(&mut Input::new(io::Cursor::new(bytes))).is_err());
        }
        let interim = b"HTTP/1.1 100 Continue\r\n\r\n".repeat(9);
        assert!(read_final_response(&mut Input::new(io::Cursor::new(interim))).is_err());
    }

    #[derive(Clone, Copy, Debug)]
    enum Reply {
        AskStatus(u16),
        RejectUpload,
        PrematureSuccess,
        DisconnectAfterUpload,
        Reconnect,
        DelayedContinue,
    }

    #[test]
    fn upload_requires_acceptance_and_final_confirmation_and_supports_reconnection() {
        for reply in [
            Reply::AskStatus(401),
            Reply::AskStatus(403),
            Reply::AskStatus(500),
            Reply::RejectUpload,
            Reply::PrematureSuccess,
            Reply::DisconnectAfterUpload,
            Reply::Reconnect,
            Reply::DelayedContinue,
        ] {
            scripted_peer(reply);
        }
    }

    fn scripted_peer(reply: Reply) {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("test.txt");
        fs::write(&source, b"test content").unwrap();
        let identity = Identity::load_or_create(directory.path().join("id"), "test").unwrap();
        let tls = server_config(&identity).unwrap();
        let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).unwrap();
        let std::net::SocketAddr::V6(address) = listener.local_addr().unwrap() else {
            unreachable!()
        };
        let server = std::thread::spawn(move || {
            let accept = || {
                let (tcp, _) = listener.accept().unwrap();
                tcp.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
                tcp.set_write_timeout(Some(Duration::from_secs(8))).unwrap();
                Input::new(StreamOwned::new(
                    rustls::ServerConnection::new(Arc::clone(&tls)).unwrap(),
                    tcp,
                ))
            };
            let mut connection = accept();
            let ask = RequestHead::read(&mut connection).unwrap().unwrap();
            assert_eq!(ask.path, "/Ask");
            let body = BodyReader::new(&mut connection, ask.body)
                .read_to_end_limited(CONTROL_BODY_LIMIT)
                .unwrap();
            let Value::Dictionary(fields) = plist::read(&body).unwrap() else {
                panic!()
            };
            let Value::Dictionary(transfer) = &fields
                .iter()
                .find(|(key, _)| key == "TransferID")
                .unwrap()
                .1
            else {
                panic!()
            };
            let Value::String(id) = &transfer[0].1 else {
                panic!()
            };
            if let Reply::AskStatus(status) = reply {
                write_response(&mut connection, status, b"", None, true).unwrap();
                connection.get_mut().conn.send_close_notify();
                connection.flush().unwrap();
                // The sender must stop here without offering /Upload.
                assert!(RequestHead::read(&mut connection).ok().flatten().is_none());
                return 0;
            }
            write_response(
                &mut connection,
                200,
                b"",
                None,
                matches!(reply, Reply::Reconnect),
            )
            .unwrap();
            if matches!(reply, Reply::Reconnect) {
                connection.get_mut().conn.send_close_notify();
                connection.flush().unwrap();
                drop(connection);
                connection = accept();
            }
            let upload = RequestHead::read(&mut connection).unwrap().unwrap();
            assert_eq!(upload.path, "/Upload");
            assert_eq!(upload.header("transferid"), Some(id.as_str()));
            assert_eq!(upload.body, BodyKind::Chunked);
            if matches!(reply, Reply::RejectUpload | Reply::PrematureSuccess) {
                let status = if matches!(reply, Reply::RejectUpload) {
                    403
                } else {
                    200
                };
                write_response(&mut connection, status, b"", None, true).unwrap();
                return 0;
            }
            if matches!(reply, Reply::DelayedContinue) {
                // First read the body: the client must fall back after its Expect timeout.
                let body = BodyReader::new(&mut connection, upload.body)
                    .read_to_end_limited(CONTROL_BODY_LIMIT)
                    .unwrap();
                write_continue(&mut connection).unwrap();
                write_response(&mut connection, 200, b"", None, true).unwrap();
                return body.len();
            }
            write_continue(&mut connection).unwrap();
            let body = BodyReader::new(&mut connection, upload.body)
                .read_to_end_limited(CONTROL_BODY_LIMIT)
                .unwrap();
            if !matches!(reply, Reply::DisconnectAfterUpload) {
                write_response(&mut connection, 200, b"", None, true).unwrap();
            }
            connection.get_mut().conn.send_close_notify();
            connection.flush().unwrap();
            body.len()
        });
        let selected = NearbyReceiver {
            name: "test".into(),
            service: AirDropService {
                instance: "test._airdrop._tcp.local".into(),
                host: "test.local".into(),
                port: address.port(),
                address,
                flags: Some(0),
            },
        };
        let mut events = Vec::new();
        let result = send_to_receiver(
            &selected,
            &identity,
            &[source],
            &SendOptions {
                sender_name: "test".into(),
                sender_model: "Windows".into(),
                target_name: "test".into(),
                timeout: Duration::from_secs(5),
                format: SendFormat::Auto,
                ble_wake: false,
                legacy_ble: false,
            },
            |event| events.push(event),
        );
        let received = server.join().unwrap();
        let success = matches!(reply, Reply::Reconnect | Reply::DelayedContinue);
        assert_eq!(result.is_ok(), success, "{reply:?}: {result:?}");
        assert_eq!(
            result.as_ref().err().is_some_and(SendError::is_declined),
            matches!(reply, Reply::AskStatus(401 | 403) | Reply::RejectUpload),
            "{reply:?}: {result:?}"
        );
        assert_eq!(
            events
                .iter()
                .any(|event| matches!(event, SendEvent::Completed(_))),
            success
        );
        if matches!(
            reply,
            Reply::AskStatus(_) | Reply::RejectUpload | Reply::PrematureSuccess
        ) {
            assert_eq!(received, 0);
            assert!(!events
                .iter()
                .any(|event| matches!(event, SendEvent::Uploading { .. })));
        } else {
            assert!(received > 0);
            assert!(events
                .iter()
                .any(|event| matches!(event, SendEvent::WaitingForConfirmation)));
        }
    }
}
