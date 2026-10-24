//! Long-lived, versioned usage leases. Dropping a lease closes its pipe.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const VERSION: u32 = 1;
pub const CAPABILITIES: &[&str] = &[
    "sessions",
    "send-receive-exclusive",
    "maintenance",
    "release-after-reap",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionKind {
    Client,
    Send,
    Receive,
    Maintenance,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RunMode {
    OnDemand,
    #[default]
    Resident,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseOutcome {
    pub remaining_clients: usize,
    pub service_stopping: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Hello {
    pub version: u32,
    pub pid: u32,
    pub kind: SessionKind,
}
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Reply {
    pub version: u32,
    pub instance: String,
    pub token: u64,
    pub capabilities: Vec<String>,
    pub outcome: ReleaseOutcome,
    pub error: Option<String>,
    pub code: Option<String>,
}
impl Reply {
    pub(crate) fn error(code: &str, message: impl ToString) -> Self {
        Self {
            version: VERSION,
            error: Some(message.to_string()),
            code: Some(code.into()),
            ..Default::default()
        }
    }
    #[cfg(windows)]
    fn check(self) -> Result<Self, crate::client::ClientError> {
        use crate::client::{ClientError, ClientErrorKind};
        if let Some(error) = &self.error {
            let kind = match self.code.as_deref() {
                Some("busy") => ClientErrorKind::Busy,
                Some("upgrade") => ClientErrorKind::UpgradeRequired,
                Some("stopping") => ClientErrorKind::Stopping,
                _ => ClientErrorKind::Service,
            };
            return Err(ClientError::new(kind, error));
        }
        if self.version != VERSION {
            return Err(ClientError::new(
                ClientErrorKind::UpgradeRequired,
                "service session protocol requires an update",
            ));
        }
        Ok(self)
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Release {
    pub instance: String,
    pub token: u64,
    pub device_ready: bool,
}

/// Single-threaded admission state; the supervisor owns all transitions.
#[derive(Default)]
pub(crate) struct Registry {
    pub entries: HashMap<u64, SessionKind>,
    pub next: u64,
    pub closing: bool,
    pub used: bool,
    pub maintenance_failed: bool,
    pub radio_reconfiguring: bool,
}
impl Registry {
    pub fn acquire(&mut self, kind: SessionKind) -> Result<u64, &'static str> {
        if self.closing {
            return Err("stopping");
        }
        if self.radio_reconfiguring
            && matches!(
                kind,
                SessionKind::Send | SessionKind::Receive | SessionKind::Maintenance
            )
        {
            return Err("busy");
        }
        if self
            .entries
            .values()
            .any(|v| *v == SessionKind::Maintenance)
            || (self.maintenance_failed && matches!(kind, SessionKind::Send | SessionKind::Receive))
            || (kind == SessionKind::Maintenance
                && self
                    .entries
                    .values()
                    .any(|v| matches!(v, SessionKind::Send | SessionKind::Receive)))
            || (matches!(kind, SessionKind::Send | SessionKind::Receive)
                && self.entries.values().any(|v| *v == kind))
        {
            return Err("busy");
        }
        self.next += 1;
        self.entries.insert(self.next, kind);
        self.used = true;
        Ok(self.next)
    }
    pub fn release(&mut self, token: u64, ready: bool, mode: RunMode) -> ReleaseOutcome {
        if self.entries.remove(&token) == Some(SessionKind::Maintenance) {
            self.maintenance_failed = !ready;
        }
        if self.entries.is_empty() && mode == RunMode::OnDemand {
            self.closing = true;
        }
        ReleaseOutcome {
            remaining_clients: self.entries.len(),
            service_stopping: self.closing,
        }
    }
    pub fn maintenance(&self) -> bool {
        self.maintenance_failed
            || self
                .entries
                .values()
                .any(|k| *k == SessionKind::Maintenance)
    }
    pub fn radio_reconfiguration_busy(&self) -> bool {
        self.radio_reconfiguring
            || self.entries.values().any(|kind| {
                matches!(
                    kind,
                    SessionKind::Send | SessionKind::Receive | SessionKind::Maintenance
                )
            })
    }
}

#[cfg(windows)]
mod lease {
    use super::*;
    use crate::client::{self, Client, ClientError, ClientErrorKind};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    type Action = (bool, mpsc::SyncSender<Result<ReleaseOutcome, ClientError>>);
    pub struct ClientLease {
        actions: Option<tokio::sync::mpsc::UnboundedSender<Action>>,
        worker: Option<std::thread::JoinHandle<()>>,
        alive: Arc<AtomicBool>,
    }
    impl std::fmt::Debug for ClientLease {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ClientLease")
                .field("alive", &self.is_alive())
                .finish()
        }
    }
    impl Client {
        pub fn acquire(&self, kind: SessionKind) -> Result<ClientLease, ClientError> {
            let config = self.config.clone();
            let (ready, received) = mpsc::sync_channel(1);
            let (actions, mut action_rx) = tokio::sync::mpsc::unbounded_channel::<Action>();
            let alive = Arc::new(AtomicBool::new(true));
            let thread_alive = alive.clone();
            let worker = std::thread::spawn(move || {
                let result = client::runtime().map(|runtime| runtime.block_on(async {
                    let acquired = tokio::time::timeout(config.timeout, async {
                        let name = format!("{}.sessions",config.pipe_name);
                        let mut pipe = match client::connect(&name).await {
                            Ok(pipe) => pipe,
                            Err(e) if e.kind() == ClientErrorKind::Connect => {
                                // An absent control pipe is an unavailable service; an
                                // existing old control pipe requires a coordinated update.
                                let control = Client::new(config.clone())?;
                                return match control.status_async().await {
                                    Ok(_) => Err(ClientError::new(ClientErrorKind::UpgradeRequired, "service lacks session support; update the installed service")),
                                    Err(_) => Err(e),
                                };
                            }
                            Err(e) => return Err(e),
                        };
                        pipe.write_all(&client::encode(&Hello { version:VERSION,pid:std::process::id(),kind })?).await.map_err(client::io_error)?;
                        let reply = client::read_json::<Reply>(&mut pipe).await?.check()?;
                        if !CAPABILITIES.iter().all(|expected| reply.capabilities.iter().any(|c| c == expected)) { return Err(ClientError::new(ClientErrorKind::UpgradeRequired,"service lacks required session capabilities")); }
                        Ok((pipe,reply))
                    }).await.unwrap_or_else(|_| Err(ClientError::new(ClientErrorKind::Timeout,"session acquisition timed out")));
                    let (mut pipe,reply) = match acquired { Ok(value)=>{ let _=ready.send(Ok(())); value }, Err(e)=>{ let _=ready.send(Err(e)); return; } };
                    let mut closed = [0u8];
                    tokio::select! {
                        _ = pipe.read(&mut closed) => {},
                        action = action_rx.recv() => {
                            if let Some((device_ready, result)) = action {
                                let released = tokio::time::timeout(config.timeout,async {
                                    pipe.write_all(&client::encode(&Release { instance:reply.instance,token:reply.token,device_ready })?).await.map_err(client::io_error)?;
                                    let reply = client::read_json::<Reply>(&mut pipe).await?.check()?;
                                    // The server waits for this acknowledgment before it tears down the pipe.
                                    pipe.write_all(&[0xff]).await.map_err(client::io_error)?;
                                    Ok(reply.outcome)
                                }).await.unwrap_or_else(|_| Err(ClientError::new(ClientErrorKind::Timeout,"waiting for OWL cleanup timed out")));
                                let _=result.send(released);
                            }
                        }
                    }
                }));
                if result.is_err() { /* ready sender closes and acquisition reports an I/O failure. */
                }
                thread_alive.store(false, Ordering::Release);
            });
            match received
                .recv()
                .map_err(|_| ClientError::new(ClientErrorKind::Io, "session worker stopped"))?
            {
                Ok(()) => Ok(ClientLease {
                    actions: Some(actions),
                    worker: Some(worker),
                    alive,
                }),
                Err(e) => {
                    let _ = worker.join();
                    Err(e)
                }
            }
        }
        pub async fn acquire_async(&self, kind: SessionKind) -> Result<ClientLease, ClientError> {
            let client = self.clone();
            tokio::task::spawn_blocking(move || client.acquire(kind))
                .await
                .map_err(|e| ClientError::new(ClientErrorKind::Io, e))?
        }
    }
    impl ClientLease {
        pub fn is_alive(&self) -> bool {
            self.alive.load(Ordering::Acquire)
        }
        pub fn release(self) -> Result<ReleaseOutcome, ClientError> {
            self.finish_maintenance(false)
        }
        pub fn finish_maintenance(
            mut self,
            device_ready: bool,
        ) -> Result<ReleaseOutcome, ClientError> {
            let (response, result) = mpsc::sync_channel(1);
            self.actions
                .take()
                .ok_or_else(|| {
                    ClientError::new(ClientErrorKind::Connect, "session already closed")
                })?
                .send((device_ready, response))
                .map_err(|_| {
                    ClientError::new(ClientErrorKind::Connect, "service session disconnected")
                })?;
            let result = result.recv().map_err(|_| {
                ClientError::new(ClientErrorKind::Connect, "service session disconnected")
            })?;
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            result
        }
        pub async fn release_async(self) -> Result<ReleaseOutcome, ClientError> {
            self.finish_maintenance_async(false).await
        }
        pub async fn finish_maintenance_async(
            self,
            ready: bool,
        ) -> Result<ReleaseOutcome, ClientError> {
            tokio::task::spawn_blocking(move || self.finish_maintenance(ready))
                .await
                .map_err(|e| ClientError::new(ClientErrorKind::Io, e))?
        }
    }
    impl Drop for ClientLease {
        fn drop(&mut self) {
            self.actions.take();
        }
    }
}
#[cfg(windows)]
pub use lease::ClientLease;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lanes_and_maintenance_are_exclusive() {
        let mut r = Registry::default();
        let a = r.acquire(SessionKind::Client).unwrap();
        let b = r.acquire(SessionKind::Client).unwrap();
        assert!(!r.radio_reconfiguration_busy());
        r.radio_reconfiguring = true;
        assert!(r.radio_reconfiguration_busy());
        assert_eq!(r.acquire(SessionKind::Send), Err("busy"));
        assert_eq!(r.acquire(SessionKind::Receive), Err("busy"));
        assert_eq!(r.acquire(SessionKind::Maintenance), Err("busy"));
        let client = r.acquire(SessionKind::Client).unwrap();
        r.release(client, false, RunMode::Resident);
        r.radio_reconfiguring = false;
        let s = r.acquire(SessionKind::Send).unwrap();
        assert!(r.radio_reconfiguration_busy());
        let recv = r.acquire(SessionKind::Receive).unwrap();
        assert_eq!(r.acquire(SessionKind::Send), Err("busy"));
        assert_eq!(r.acquire(SessionKind::Receive), Err("busy"));
        assert_eq!(r.acquire(SessionKind::Maintenance), Err("busy"));
        r.release(s, false, RunMode::Resident);
        r.release(recv, false, RunMode::Resident);
        assert!(!r.radio_reconfiguration_busy());
        let m = r.acquire(SessionKind::Maintenance).unwrap();
        assert!(r.radio_reconfiguration_busy());
        assert_eq!(r.acquire(SessionKind::Send), Err("busy"));
        r.release(m, false, RunMode::Resident);
        assert!(r.maintenance());
        assert_eq!(r.acquire(SessionKind::Receive), Err("busy"));
        let retry = r.acquire(SessionKind::Maintenance).unwrap();
        r.release(retry, true, RunMode::Resident);
        assert!(!r.maintenance());
        assert!(!r.release(a, false, RunMode::OnDemand).service_stopping);
        assert!(r.release(b, false, RunMode::OnDemand).service_stopping);
        assert_eq!(r.acquire(SessionKind::Client), Err("stopping"));
    }
    #[test]
    fn resident_remains_available_after_last_client() {
        let mut r = Registry::default();
        let id = r.acquire(SessionKind::Client).unwrap();
        assert!(!r.release(id, false, RunMode::Resident).service_stopping);
        assert!(r.acquire(SessionKind::Client).is_ok());
    }
}
