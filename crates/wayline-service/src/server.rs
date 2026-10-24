use crate::{
    firewall,
    protocol::{Request, Status},
};
use std::{
    io,
    path::Path,
    ptr,
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    sync::{mpsc, oneshot, watch, Semaphore},
    task::JoinSet,
    time::timeout,
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    },
};

pub const MAX_REQUEST: usize = 8192;
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);
pub type Snapshot = Arc<RwLock<Status>>;
pub struct Control {
    pub request: Request,
    pub response: oneshot::Sender<Status>,
}

#[derive(Clone)]
pub struct Handler {
    pub commands: mpsc::Sender<Control>,
    pub status: Snapshot,
    pub firewall_gate: Arc<Semaphore>,
}
impl Handler {
    pub fn snapshot(&self) -> Status {
        self.status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    async fn handle(&self, request: Request) -> Status {
        if request.cmd == "status" {
            return self.snapshot();
        }
        if request.cmd == "firewall" {
            let Ok(_permit) = self.firewall_gate.try_acquire() else {
                return self
                    .snapshot()
                    .with_error("firewall update already in progress");
            };
            let Some(exe) = request.exe else {
                return self.snapshot().with_error("firewall requires exe");
            };
            return match firewall::ensure(Path::new(&exe)).await {
                Ok(()) => self.snapshot(),
                Err(e) => self.snapshot().with_error(e),
            };
        }
        let (response, result) = oneshot::channel();
        if self
            .commands
            .try_send(Control { request, response })
            .is_err()
        {
            return self
                .snapshot()
                .with_error("service command queue unavailable");
        }
        match timeout(Duration::from_secs(2), result).await {
            Ok(Ok(status)) => status,
            _ => self.snapshot().with_error("service command timed out"),
        }
    }
}

pub fn pipe_path(name: &str) -> io::Result<String> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pipe name must be 1-128 ASCII letters, digits, dots, dashes or underscores",
        ));
    }
    Ok(format!(r"\\.\pipe\{name}"))
}

pub fn listen(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    // Authenticated users can read/write, but cannot create a server instance.
    // GENERIC_WRITE would also grant FILE_CREATE_PIPE_INSTANCE (bit 0x4).
    // OW grants the foreground process's owner the same creation rights as SYSTEM.
    let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;OW)(A;;0x12019b;;;AU)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut raw: PSECURITY_DESCRIPTOR = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut raw,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    struct Descriptor(PSECURITY_DESCRIPTOR);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    let descriptor = Descriptor(raw);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .in_buffer_size(MAX_REQUEST as u32)
            .out_buffer_size(MAX_REQUEST as u32)
            .create_with_security_attributes_raw(
                pipe_path(name)?,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}

pub async fn serve(
    name: String,
    mut listener: NamedPipeServer,
    handler: Handler,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let mut clients = JoinSet::new();
    let capacity = Arc::new(Semaphore::new(32));
    let result = loop {
        if *shutdown.borrow() {
            break Ok(());
        }
        tokio::select! {
            _ = shutdown.changed() => break Ok(()),
            Some(_) = clients.join_next(), if !clients.is_empty() => {},
            connected = listener.connect() => {
                // A client may vanish between opening and connecting; keep the
                // listener alive so a malformed/disconnected client is not fatal.
                if connected.is_err() { let _ = listener.disconnect(); continue; }
                let next = match listen(&name, false) { Ok(next) => next, Err(e) => break Err(e) };
                let client = std::mem::replace(&mut listener, next);
                if let Ok(permit) = capacity.clone().try_acquire_owned() {
                    let handler = handler.clone();
                    clients.spawn(async move { let _permit = permit; let _ = serve_client(client, handler).await; });
                } else {
                    let _ = client.disconnect();
                }
            }
        }
    };
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    let _ = listener.disconnect();
    result
}

async fn serve_client(pipe: NamedPipeServer, handler: Handler) -> io::Result<()> {
    // Closing a server handle alone can leave the named pipe alive while a stale
    // client holds its handle. Disconnect on every path, including task abort, so
    // the next service instance can acquire FILE_FLAG_FIRST_PIPE_INSTANCE.
    struct Connection(NamedPipeServer);
    impl Drop for Connection {
        fn drop(&mut self) {
            let _ = self.0.disconnect();
        }
    }
    let mut connection = Connection(pipe);
    let pipe = &mut connection.0;
    let mut line = Vec::new();
    let read = timeout(
        CLIENT_TIMEOUT,
        BufReader::new((&mut *pipe).take((MAX_REQUEST + 1) as u64)).read_until(b'\n', &mut line),
    )
    .await;
    match read {
        Ok(Ok(0)) | Err(_) => return Ok(()),
        Ok(Err(e)) => return Err(e),
        Ok(Ok(_)) => {}
    }
    let status = if line.len() > MAX_REQUEST || line.last() != Some(&b'\n') {
        handler
            .snapshot()
            .with_error("request must be a JSON line of at most 8192 bytes")
    } else {
        let data = line.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&line);
        match serde_json::from_slice::<Request>(data) {
            Ok(request) => handler.handle(request).await,
            Err(e) => handler
                .snapshot()
                .with_error(format!("invalid request: {e}")),
        }
    };
    let mut response = serde_json::to_vec(&status)?;
    response.push(b'\n');
    timeout(CLIENT_TIMEOUT, pipe.write_all(&response))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "pipe response timed out"))??;
    // Keep the server handle until the client closes (or its deadline expires).
    // DisconnectNamedPipe immediately after WriteFile can discard unread bytes.
    // No blocking FlushFileBuffers: an idle client must never hold up shutdown.
    let mut byte = [0];
    let _ = timeout(CLIENT_TIMEOUT, pipe.read(&mut byte)).await;
    Ok(())
}
