//! Bounded JSON-line control client shared by desktop and CLI.
use crate::protocol::{Request, Status};
use std::{fmt, path::Path, time::Duration};

pub const MAX_MESSAGE: usize = 8192;
#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub pipe_name: String,
    pub timeout: Duration,
}
impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            pipe_name: "omdrop-owl".into(),
            timeout: Duration::from_secs(25),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientErrorKind {
    Connect,
    Timeout,
    InvalidResponse,
    Service,
    Io,
    InvalidRequest,
    Busy,
    UpgradeRequired,
    Stopping,
}
#[derive(Debug, Clone)]
pub struct ClientError {
    kind: ClientErrorKind,
    message: String,
}
impl ClientError {
    pub fn new(kind: ClientErrorKind, message: impl ToString) -> Self {
        Self {
            kind,
            message: message.to_string(),
        }
    }
    pub fn kind(&self) -> ClientErrorKind {
        self.kind
    }
}
impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ClientError {}
#[derive(Clone, Debug, Default)]
pub struct Client {
    pub(crate) config: ClientConfig,
}
impl Client {
    pub fn new(config: ClientConfig) -> Result<Self, ClientError> {
        pipe_path(&config.pipe_name)?;
        if config.timeout.is_zero() {
            return Err(ClientError::new(
                ClientErrorKind::InvalidRequest,
                "request timeout must be nonzero",
            ));
        }
        Ok(Self { config })
    }
    pub fn status(&self) -> Result<Status, ClientError> {
        self.request(&Request::new("status"))
    }
    pub fn firewall(&self, exe: &Path) -> Result<Status, ClientError> {
        let mut request = Request::new("firewall");
        request.exe = Some(
            exe.to_str()
                .ok_or_else(|| {
                    ClientError::new(
                        ClientErrorKind::InvalidRequest,
                        "executable path is not Unicode",
                    )
                })?
                .into(),
        );
        self.request(&request)
    }
    #[cfg(windows)]
    pub fn request(&self, request: &Request) -> Result<Status, ClientError> {
        let client = self.clone();
        let request = request.clone();
        // A private thread also makes this safe when called inside a Tokio runtime.
        std::thread::spawn(move || runtime()?.block_on(client.request_async(&request)))
            .join()
            .map_err(|_| ClientError::new(ClientErrorKind::Io, "service client thread failed"))?
    }
    #[cfg(not(windows))]
    pub fn request(&self, _: &Request) -> Result<Status, ClientError> {
        Err(ClientError::new(
            ClientErrorKind::Connect,
            "service client requires Windows",
        ))
    }
    #[cfg(windows)]
    pub async fn status_async(&self) -> Result<Status, ClientError> {
        self.request_async(&Request::new("status")).await
    }
    #[cfg(windows)]
    pub async fn firewall_async(&self, exe: &Path) -> Result<Status, ClientError> {
        let mut request = Request::new("firewall");
        request.exe = Some(
            exe.to_str()
                .ok_or_else(|| {
                    ClientError::new(
                        ClientErrorKind::InvalidRequest,
                        "executable path is not Unicode",
                    )
                })?
                .into(),
        );
        self.request_async(&request).await
    }
    #[cfg(windows)]
    pub async fn request_async(&self, request: &Request) -> Result<Status, ClientError> {
        use tokio::io::AsyncWriteExt;
        let bytes = encode(request)?;
        tokio::time::timeout(self.config.timeout, async {
            let mut pipe = connect(&self.config.pipe_name).await?;
            pipe.write_all(&bytes).await.map_err(io_error)?;
            let status: Status = read_json(&mut pipe).await?;
            if request.cmd != "status" {
                if let Some(error) = &status.error {
                    return Err(ClientError::new(ClientErrorKind::Service, error));
                }
            }
            Ok(status)
        })
        .await
        .map_err(|_| ClientError::new(ClientErrorKind::Timeout, "service request timed out"))?
    }
}
pub(crate) fn pipe_path(name: &str) -> Result<String, ClientError> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    {
        return Err(ClientError::new(
            ClientErrorKind::InvalidRequest,
            "invalid local pipe name",
        ));
    }
    Ok(format!(r"\\.\pipe\{name}"))
}
pub(crate) fn encode(value: &impl serde::Serialize) -> Result<Vec<u8>, ClientError> {
    let mut bytes = serde_json::to_vec(value)
        .map_err(|e| ClientError::new(ClientErrorKind::InvalidRequest, e))?;
    bytes.push(b'\n');
    if bytes.len() > MAX_MESSAGE {
        return Err(ClientError::new(
            ClientErrorKind::InvalidRequest,
            "request exceeds 8192 bytes",
        ));
    }
    Ok(bytes)
}
#[cfg(windows)]
pub(crate) fn runtime() -> Result<tokio::runtime::Runtime, ClientError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(io_error)
}
#[cfg(windows)]
pub(crate) fn io_error(error: std::io::Error) -> ClientError {
    ClientError::new(ClientErrorKind::Io, error)
}
#[cfg(windows)]
pub(crate) async fn connect(
    name: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, ClientError> {
    loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_path(name)?) {
            Ok(pipe) => return Ok(pipe),
            Err(e) if e.raw_os_error() == Some(231) => {
                tokio::time::sleep(Duration::from_millis(20)).await
            }
            Err(e) => {
                return Err(ClientError::new(
                    ClientErrorKind::Connect,
                    format!("cannot connect to service: {e}"),
                ))
            }
        }
    }
}
#[cfg(windows)]
pub(crate) async fn read_json<T: serde::de::DeserializeOwned>(
    pipe: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<T, ClientError> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let mut bytes = Vec::new();
    let read = BufReader::new(pipe.take((MAX_MESSAGE + 1) as u64))
        .read_until(b'\n', &mut bytes)
        .await;
    if let Err(error) = read {
        if !matches!(
            error.kind(),
            std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
        ) && !matches!(error.raw_os_error(), Some(109 | 232 | 233))
        {
            return Err(io_error(error));
        }
    }
    if bytes.len() > MAX_MESSAGE || bytes.last() != Some(&b'\n') {
        return Err(ClientError::new(
            ClientErrorKind::InvalidResponse,
            "service returned an incomplete or oversized JSON line",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| ClientError::new(ClientErrorKind::InvalidResponse, e))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    fn name() -> String {
        static N: AtomicU64 = AtomicU64::new(0);
        format!(
            "omdrop-client-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        )
    }
    fn client(name: String, timeout: Duration) -> Client {
        Client::new(ClientConfig {
            pipe_name: name,
            timeout,
        })
        .unwrap()
    }
    fn status_bytes(error: Option<&str>) -> Vec<u8> {
        encode(&Status {
            state: "running".into(),
            port: Some("COM123".into()),
            channel: 6,
            awdl0: true,
            error: error.map(str::to_owned),
            enabled: true,
        })
        .unwrap()
    }
    async fn fixture(
        mut pipe: tokio::net::windows::named_pipe::NamedPipeServer,
        bytes: Vec<u8>,
        close: bool,
        delay: Duration,
    ) {
        pipe.connect().await.unwrap();
        let mut line = String::new();
        BufReader::new(&mut pipe)
            .read_line(&mut line)
            .await
            .unwrap();
        tokio::time::sleep(delay).await;
        if pipe.write_all(&bytes).await.is_ok() {
            if close {
                tokio::time::sleep(Duration::from_millis(30)).await;
            } else {
                let mut byte = [0];
                let _ = tokio::time::timeout(Duration::from_secs(2), pipe.read(&mut byte)).await;
            }
        }
        let _ = pipe.disconnect();
    }
    async fn response(
        bytes: Vec<u8>,
        request: Request,
        close: bool,
    ) -> Result<Status, ClientError> {
        let name = name();
        let server = crate::server::listen(&name, true).unwrap();
        let task = tokio::spawn(fixture(server, bytes, close, Duration::ZERO));
        let result = client(name, Duration::from_secs(2))
            .request_async(&request)
            .await;
        task.await.unwrap();
        result
    }
    #[tokio::test]
    async fn preserves_fault_snapshot_but_rejects_control_errors() {
        assert!(
            response(status_bytes(None), Request::new("status"), false)
                .await
                .unwrap()
                .awdl0
        );
        assert_eq!(
            response(
                status_bytes(Some("radio fault")),
                Request::new("status"),
                false
            )
            .await
            .unwrap()
            .error
            .as_deref(),
            Some("radio fault")
        );
        assert_eq!(
            response(status_bytes(Some("denied")), Request::new("start"), false)
                .await
                .unwrap_err()
                .kind(),
            ClientErrorKind::Service
        );
    }
    #[tokio::test]
    async fn rejects_oversized_truncated_and_invalid_json() {
        for (bytes, close) in [
            (vec![b'x'; MAX_MESSAGE + 1], false),
            (b"{}".to_vec(), true),
            (b"{bad}\n".to_vec(), false),
            (b"{}\n".to_vec(), false),
        ] {
            assert_eq!(
                response(bytes, Request::new("status"), close)
                    .await
                    .unwrap_err()
                    .kind(),
                ClientErrorKind::InvalidResponse
            );
        }
    }
    #[tokio::test]
    async fn absent_and_timeout_have_distinct_categories() {
        assert_eq!(
            client(name(), Duration::from_secs(1))
                .status_async()
                .await
                .unwrap_err()
                .kind(),
            ClientErrorKind::Connect
        );
        let name = name();
        let server = crate::server::listen(&name, true).unwrap();
        let task = tokio::spawn(fixture(
            server,
            status_bytes(None),
            false,
            Duration::from_millis(150),
        ));
        assert_eq!(
            client(name, Duration::from_millis(30))
                .status_async()
                .await
                .unwrap_err()
                .kind(),
            ClientErrorKind::Timeout
        );
        task.await.unwrap();
    }
    #[tokio::test]
    async fn requires_session_upgrade_only_when_control_service_exists() {
        use crate::session::SessionKind;
        let absent = client(name(), Duration::from_secs(1));
        assert_eq!(
            absent
                .acquire_async(SessionKind::Client)
                .await
                .unwrap_err()
                .kind(),
            ClientErrorKind::Connect
        );
        let name = name();
        let server = crate::server::listen(&name, true).unwrap();
        let task = tokio::spawn(fixture(server, status_bytes(None), false, Duration::ZERO));
        assert_eq!(
            client(name, Duration::from_secs(2))
                .acquire_async(SessionKind::Client)
                .await
                .unwrap_err()
                .kind(),
            ClientErrorKind::UpgradeRequired
        );
        task.await.unwrap();
    }
    #[tokio::test]
    async fn waits_for_busy_pipe_then_completes() {
        let name = name();
        let first = crate::server::listen(&name, true).unwrap();
        let occupied = connect(&name).await.unwrap();
        first.connect().await.unwrap();
        let fixture_name = name.clone();
        let task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            let next = crate::server::listen(&fixture_name, false).unwrap();
            let _ = first.disconnect();
            drop(first);
            fixture(next, status_bytes(None), false, Duration::ZERO).await;
        });
        let start = std::time::Instant::now();
        assert!(
            client(name, Duration::from_secs(2))
                .status_async()
                .await
                .unwrap()
                .awdl0
        );
        assert!(start.elapsed() >= Duration::from_millis(70));
        drop(occupied);
        task.await.unwrap();
    }
    #[test]
    fn validates_config_and_request_bounds_before_connecting() {
        assert_eq!(
            Client::new(ClientConfig {
                pipe_name: "../unsafe".into(),
                timeout: Duration::from_secs(1)
            })
            .unwrap_err()
            .kind(),
            ClientErrorKind::InvalidRequest
        );
        assert_eq!(
            Client::default()
                .request(&Request::new("x".repeat(MAX_MESSAGE)))
                .unwrap_err()
                .kind(),
            ClientErrorKind::InvalidRequest
        );
    }
}
