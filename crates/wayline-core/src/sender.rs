//! Application-facing sender adapter for Owly's AirDrop state machine.
use crate::{AirDropService, Identity, NetworkInterface, SendCancellation, TransferFailureKind};
use owly_airdrop::{
    driver::sender::prepare_cancellable,
    proto::{
        archive::compress::Compressor,
        sender::{self as proto, Action, Outcome, ResponseReader, Transfer},
    },
};
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use std::{
    fs,
    io::{self, Read, Write},
    net::{SocketAddrV6, TcpStream},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

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
    ModeSelected(SendFormat),
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
    kind: TransferFailureKind,
}
impl SendError {
    pub fn kind(&self) -> TransferFailureKind {
        self.kind
    }
    pub fn is_declined(&self) -> bool {
        self.kind == TransferFailureKind::Declined
    }
    pub(crate) fn cancelled() -> Self {
        Self {
            message: "operation cancelled".into(),
            kind: TransferFailureKind::Cancelled,
        }
    }
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: TransferFailureKind::Validation,
        }
    }
}
impl From<io::Error> for SendError {
    fn from(e: io::Error) -> Self {
        Self {
            message: e.to_string(),
            kind: TransferFailureKind::from_io(&e),
        }
    }
}
impl From<rustls::Error> for SendError {
    fn from(e: rustls::Error) -> Self {
        Self {
            message: e.to_string(),
            kind: TransferFailureKind::Connection,
        }
    }
}
impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SendError {}

pub fn discover_receivers(
    interface: NetworkInterface,
    identity: &Identity,
    timeout: Duration,
    ble_wake: bool,
    legacy_ble: bool,
) -> Result<Vec<NearbyReceiver>, SendError> {
    crate::discover_receivers_live(interface, identity, timeout, ble_wake, legacy_ble, |_| {})
}
pub fn send_to_name(
    interface: NetworkInterface,
    identity: &Identity,
    files: &[PathBuf],
    options: &SendOptions,
) -> Result<SendReport, SendError> {
    validate_files(files)?;
    let found = discover_receivers(
        interface,
        identity,
        options.timeout,
        options.ble_wake,
        options.legacy_ble,
    )?;
    let selected = crate::receiver_by_name(&found, &options.target_name)?;
    send_to_receiver(selected, identity, files, options, |_| {})
}
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
pub fn send_to_receiver_cancellable(
    selected: &NearbyReceiver,
    identity: &Identity,
    files: &[PathBuf],
    options: &SendOptions,
    cancel: &SendCancellation,
    mut notify: impl FnMut(SendEvent),
) -> Result<SendReport, SendError> {
    use wayline_board::{Phase, Reason, Tracker};
    let mut board = Tracker::begin(
        crate::board_feedback_enabled(),
        0,
        &selected.name,
        files.len(),
    );
    let result = (|| {
        cancel.check()?;
        let source_bytes = validate_files(files)?;
        #[cfg(windows)]
        let _advert = if options.ble_wake {
            Some(crate::ble::AirDropAdvert::start(options.legacy_ble)?)
        } else {
            None
        };
        let mut event = |e: SendEvent| {
            match &e {
                SendEvent::Preparing => board.phase(Phase::Preparing),
                SendEvent::ModeSelected(_) => {}
                SendEvent::WaitingForAcceptance { .. } => board.phase(Phase::Waiting),
                SendEvent::Uploading { sent, total } => {
                    board.update(Phase::Transferring, *sent, Some(*total), Reason::None)
                }
                SendEvent::WaitingForConfirmation => board.phase(Phase::Confirming),
                SendEvent::Completed(_) => board.phase(Phase::Complete),
            }
            notify(e);
        };
        event(SendEvent::Preparing);
        let format = select_format(options.format, selected.service.flags);
        event(SendEvent::ModeSelected(format));
        let compressor = if format == SendFormat::DvZip {
            Compressor::DvZip
        } else {
            Compressor::Gzip
        };
        let (offer, mut archive, len) = prepare_cancellable(files, compressor, &|| cancel.check())?;
        let mut cfg = proto::Config::new(&options.sender_name, identity.service_id());
        cfg.model_name = options.sender_model.clone();
        cfg.ask_timeout = options.timeout.max(Duration::from_secs(65));
        let mut id = [0; 16];
        getrandom::fill(&mut id).map_err(|e| SendError::invalid(e.to_string()))?;
        id[6] = id[6] & 15 | 64;
        id[8] = id[8] & 63 | 128;
        let hex = hex::encode_upper(id);
        let id = format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        );
        let mut transfer = Transfer::new(&cfg, &id, &offer, Some((compressor, len)));
        let client = client_config(identity)?;
        let mut tls = connect(selected.service.address, &client, options.timeout, cancel)?;
        event(SendEvent::WaitingForAcceptance {
            receiver: selected.name.clone(),
        });
        let mut deadline = None;
        let mut buffer = [0; 64 << 10];
        loop {
            while let Some(action) = transfer.poll() {
                match action {
                    Action::Done(Outcome::Delivered) => {
                        let report = SendReport {
                            receiver: selected.name.clone(),
                            files: files.len(),
                            source_bytes,
                            archive_bytes: len,
                            format,
                        };
                        event(SendEvent::Completed(report.clone()));
                        return Ok(report);
                    }
                    Action::Done(Outcome::Declined) => {
                        return Err(SendError {
                            message: "receiver declined".into(),
                            kind: TransferFailureKind::Declined,
                        })
                    }
                    Action::Done(Outcome::Failed(message)) => {
                        return Err(SendError {
                            message,
                            kind: TransferFailureKind::Connection,
                        })
                    }
                    Action::Transmit(bytes) => {
                        cancel.check()?;
                        tls.write_all(&bytes)?;
                        tls.flush()?;
                    }
                    Action::SetTimer(after) => deadline = after.map(|d| Instant::now() + d),
                    Action::Accepted => {}
                    Action::Reconnect => {
                        tls = connect(selected.service.address, &client, options.timeout, cancel)?
                    }
                    Action::SendUpload(head) => {
                        cancel.check()?;
                        tls.write_all(&head)?;
                        tls.flush()?;
                        let mut sent = 0;
                        let mut updated = Instant::now();
                        event(SendEvent::Uploading { sent, total: len });
                        loop {
                            cancel.check()?;
                            let n = archive.read(&mut buffer)?;
                            if n == 0 {
                                break;
                            }
                            tls.write_all(&buffer[..n])?;
                            tls.flush()?;
                            sent += n as u64;
                            if sent == n as u64
                                || sent == len
                                || updated.elapsed() >= Duration::from_millis(100)
                            {
                                event(SendEvent::Uploading { sent, total: len });
                                updated = Instant::now();
                            }
                        }
                        if sent != len {
                            return Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "archive size changed",
                            )
                            .into());
                        }
                        event(SendEvent::WaitingForConfirmation);
                        transfer.upload_sent();
                    }
                }
            }
            cancel.check()?;
            if deadline.is_some_and(|at| Instant::now() >= at) {
                deadline = None;
                transfer.timeout();
                continue;
            }
            match tls.read(&mut buffer) {
                Ok(0) => transfer.receive_eof(),
                Ok(n) => transfer.receive(&buffer[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
    })();
    cancel.detach();
    if let Err(e) = &result {
        if cancel.is_cancelled() {
            board.phase(Phase::Cancelled);
            return Err(SendError::cancelled());
        }
        board.fail(if e.is_declined() {
            Reason::Declined
        } else {
            Reason::Connection
        });
    }
    result
}

pub fn validate_files(files: &[PathBuf]) -> Result<u64, SendError> {
    if files.is_empty() {
        return Err(SendError::invalid("at least one file is required"));
    }
    let mut names = std::collections::HashSet::new();
    let mut total = 0u64;
    let mut count = 0usize;
    fn visit(
        path: &std::path::Path,
        depth: usize,
        total: &mut u64,
        count: &mut usize,
    ) -> Result<(), SendError> {
        if depth > 64 || *count >= 100_000 {
            return Err(SendError::invalid("directory tree exceeds transfer limits"));
        }
        *count += 1;
        let meta = fs::symlink_metadata(path)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Err(SendError::invalid("reparse points are not transferred"));
            }
        }
        if meta.file_type().is_symlink() {
            return Err(SendError::invalid("symbolic links are not transferred"));
        }
        if meta.is_file() {
            if meta.len() > owly_airdrop::proto::archive::cpio::ODC_MAX_SIZE {
                return Err(SendError::invalid("file exceeds CPIO size limit"));
            }
            *total = total
                .checked_add(meta.len())
                .ok_or_else(|| SendError::invalid("file sizes overflow"))?;
        } else if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                visit(&entry?.path(), depth + 1, total, count)?;
            }
        } else {
            return Err(SendError::invalid("unsupported file type"));
        }
        Ok(())
    }
    for path in files {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| SendError::invalid("file name is not UTF-8"))?;
        owly_airdrop::proto::archive::extract::RelPath::file_name(name)
            .map_err(|e| SendError::invalid(e.to_string()))?;
        if !names.insert(name.to_lowercase()) {
            return Err(SendError::invalid("duplicate file name"));
        }
        visit(path, 0, &mut total, &mut count)?;
    }
    Ok(total)
}

fn select_format(requested: SendFormat, receiver_flags: Option<u32>) -> SendFormat {
    match requested {
        SendFormat::Auto if receiver_flags.unwrap_or(0) & 2 != 0 => SendFormat::DvZip,
        SendFormat::Auto => SendFormat::Gzip,
        format => format,
    }
}

pub(crate) fn client_config(identity: &Identity) -> Result<Arc<ClientConfig>, SendError> {
    crate::tls::install_provider();
    Ok(owly_airdrop::driver::tls::client_config(Some(
        &crate::tls::upstream_identity(identity),
    ))?)
}
fn connect(
    address: SocketAddrV6,
    config: &Arc<ClientConfig>,
    timeout: Duration,
    cancel: &SendCancellation,
) -> Result<StreamOwned<ClientConnection, TcpStream>, SendError> {
    cancel.check()?;
    let socket = TcpStream::connect_timeout(
        &address.into(),
        timeout
            .min(Duration::from_secs(3))
            .max(Duration::from_millis(1)),
    )?;
    cancel.attach(&socket)?;
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(200)))?;
    socket.set_write_timeout(Some(Duration::from_secs(30)))?;
    let connection =
        ClientConnection::new(config.clone(), owly_airdrop::driver::tls::server_name())?;
    let mut tls = StreamOwned::new(connection, socket);
    let deadline = Instant::now() + timeout;
    while tls.conn.is_handshaking() {
        cancel.check()?;
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out").into());
        }
        match tls.conn.complete_io(&mut tls.sock) {
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(tls)
}
pub(crate) fn discover(
    service: &AirDropService,
    client: &Arc<ClientConfig>,
    timeout: Duration,
    cancel: &SendCancellation,
) -> Result<Option<String>, SendError> {
    let mut tls = connect(service.address, client, timeout, cancel)?;
    tls.write_all(&proto::discover_request())?;
    tls.flush()?;
    let mut parser = ResponseReader::new();
    let mut buf = [0; 16384];
    let until = Instant::now() + timeout;
    loop {
        cancel.check()?;
        if Instant::now() >= until {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "Discover timed out").into());
        }
        let response = match tls.read(&mut buf) {
            Ok(0) => parser.receive_eof().map(Some),
            Ok(n) => parser.receive(&buf[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(e) => return Err(e.into()),
        }
        .map_err(|e| SendError::invalid(e.to_string()))?;
        if let Some(response) = response {
            return Ok(proto::parse_discover_response(&response).map(|r| r.computer_name));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_format_uses_receiver_dvzip_capability_and_falls_back_to_gzip() {
        assert_eq!(select_format(SendFormat::Auto, Some(2)), SendFormat::DvZip);
        assert_eq!(select_format(SendFormat::Auto, Some(0)), SendFormat::Gzip);
        assert_eq!(select_format(SendFormat::Auto, None), SendFormat::Gzip);
        assert_eq!(select_format(SendFormat::Gzip, Some(2)), SendFormat::Gzip);
        assert_eq!(select_format(SendFormat::DvZip, Some(0)), SendFormat::DvZip);
    }
}
