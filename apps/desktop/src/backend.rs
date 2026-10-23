use anyhow::{Context, Result, bail};
use omdrop_core::{
    DiscoveryEvent, Identity, NearbyReceiver, SendCancellation, SendEvent, SendFormat, SendOptions,
};
use omdrop_service::{
    client::{Client, ClientConfig, ClientErrorKind},
    protocol::Status,
    session::{ClientLease, ReleaseOutcome, SessionKind},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::Sender,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkStatus {
    pub state: String,
    pub port: Option<String>,
    pub channel: u16,
    pub target: Option<String>,
    pub awdl0: bool,
    pub error: Option<String>,
}
impl From<Status> for LinkStatus {
    fn from(status: Status) -> Self {
        Self {
            state: status.state,
            port: status.port,
            channel: status.channel,
            target: status.target,
            awdl0: status.awdl0,
            error: status.error,
        }
    }
}
impl LinkStatus {
    pub fn ready(&self) -> bool {
        self.state == "running" && self.awdl0 && self.error.is_none()
    }
}

pub enum Event {
    FilesValidated(u64, Vec<PathBuf>, Result<u64, String>),
    FilePreview(u64, usize, std::sync::Arc<gpui::RenderImage>),
    Status(Result<LinkStatus, String>),
    Discovery(u64, DiscoveryEvent),
    ScanDone(u64, Result<(), String>),
    Transfer(u64, SendEvent),
    SendDone(u64, Result<(), SendFailure>),
    ReceiverStatus(Result<(), String>),
    ReceiverActivity(omdrop_core::ReceiverEvent),
    HistoryLoaded(Result<Vec<omdrop_host::TransferHistoryRecord>, String>),
    HistoryUpdated(Result<Vec<omdrop_host::TransferHistoryRecord>, String>),
    HistoryCleared(Result<Vec<omdrop_host::TransferHistoryRecord>, String>),
    HistoryFlushed,
    DownloadDirectorySaved(Result<PathBuf, String>),
    RadioChannelChanged(Result<Status, String>),
    Incoming(crate::receiving::IncomingRequest),
    ShutdownDone(Result<bool, String>),
    Firmware(crate::firmware::Event),
    ShellError(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendFailure {
    Declined,
    Cancelled,
    Connection(String),
    Validation(String),
    Failed(String),
}

impl From<anyhow::Error> for SendFailure {
    fn from(error: anyhow::Error) -> Self {
        match error
            .downcast_ref::<omdrop_core::SendError>()
            .map(omdrop_core::SendError::kind)
        {
            Some(omdrop_core::TransferFailureKind::Declined) => Self::Declined,
            Some(omdrop_core::TransferFailureKind::Cancelled) => Self::Cancelled,
            Some(omdrop_core::TransferFailureKind::Connection) => Self::Connection(format!(
                "{}{error:#}",
                crate::i18n::current().strings().err_send_connection
            )),
            Some(omdrop_core::TransferFailureKind::Validation) => Self::Validation(format!(
                "{}{error:#}",
                crate::i18n::current().strings().err_send_validation
            )),
            _ => Self::Failed(format!("{error:#}")),
        }
    }
}

pub fn validate(id: u64, paths: Vec<PathBuf>, tx: Sender<Event>) {
    std::thread::spawn(move || {
        let result = omdrop_core::validate_files(&paths).map_err(|error| error.to_string());
        let preview_paths: Vec<_> = if result.is_ok() {
            paths
                .iter()
                .take(crate::preview::MAX_PREVIEWS)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        if tx.send(Event::FilesValidated(id, paths, result)).is_err() {
            return;
        }
        // Validation is delivered first; a slow shell extension must never
        // delay showing the selection, discovering peers, or navigating back.
        for (index, path) in preview_paths.iter().enumerate() {
            if let Ok(image) = crate::preview::load(path)
                && tx.send(Event::FilePreview(id, index, image)).is_err()
            {
                break;
            }
        }
    });
}

pub struct Monitor {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<ReleaseOutcome, String>>>,
}
impl Monitor {
    pub fn shutdown(&mut self) -> Option<JoinHandle<Result<ReleaseOutcome, String>>> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
        self.worker.take()
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn monitor(tx: Sender<Event>) -> Monitor {
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker = std::thread::spawn(move || {
        let mut lease: Option<ClientLease> = None;
        let client = Client::new(ClientConfig {
            timeout: Duration::from_secs(3),
            ..Default::default()
        })
        .map_err(|error| error.to_string())?;
        while !worker_stop.load(Ordering::Acquire) {
            if lease.as_ref().is_some_and(|lease| !lease.is_alive()) {
                lease.take();
                let _ = tx.send(Event::Status(Err(crate::i18n::current()
                    .strings()
                    .backend_session_lost
                    .into())));
            }
            let status = (|| -> Result<LinkStatus> {
                let text = crate::i18n::current().strings();
                if lease.is_none() {
                    omdrop_service::service_control::start().context(text.backend_start_failed)?;
                    lease = Some(
                        Client::default()
                            .acquire(SessionKind::Client)
                            .context(text.backend_lease_failed)?,
                    );
                }
                Ok(client.status()?.into())
            })()
            .map_err(|error| format!("{error:#}"));
            if status.is_err() {
                lease.take();
            }
            if tx.send(Event::Status(status)).is_err() {
                break;
            }
            std::thread::park_timeout(Duration::from_secs(3));
        }
        match lease {
            Some(lease) => lease.release().map_err(|error| {
                format!(
                    "{}{error}",
                    crate::i18n::current().strings().backend_release_failed
                )
            }),
            None => settle_service_session(),
        }
    });
    Monitor {
        stop,
        worker: Some(worker),
    }
}

pub fn change_radio_channel(
    channel: u16,
    receiver: Option<JoinHandle<()>>,
    tx: Sender<Event>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let result = receiver
            .map(|worker| {
                worker.join().map_err(|_| {
                    crate::i18n::current()
                        .strings()
                        .receiver_worker_crashed
                        .to_owned()
                })
            })
            .transpose()
            .map(|_| ())
            .and_then(|()| {
                Client::default()
                    .set_channel_and_wait(channel, Duration::from_secs(45))
                    .map_err(|error| format!("{error}"))
            });
        let _ = tx.send(Event::RadioChannelChanged(result));
    })
}

pub fn shutdown(
    monitor: Option<Monitor>,
    receiver: Option<JoinHandle<()>>,
    transfers: Vec<JoinHandle<()>>,
    tx: Sender<Event>,
) {
    std::thread::spawn(move || {
        // Keep our client session until all sockets and transfer sessions close.
        // Releasing it last waits for OWL only when no other client still uses it.
        for worker in transfers {
            let _ = worker.join();
        }
        if let Some(worker) = receiver {
            let _ = worker.join();
        }
        let result = if let Some(mut monitor) = monitor {
            monitor
                .shutdown()
                .map(|worker| {
                    worker
                        .join()
                        .map_err(|_| {
                            crate::i18n::current()
                                .strings()
                                .backend_worker_crashed
                                .to_owned()
                        })
                        .and_then(|result| result)
                        .map(|outcome| outcome.remaining_clients > 0 || !outcome.service_stopping)
                })
                .unwrap_or(Ok(false))
        } else {
            // Retry a failed release without stopping somebody else's service.
            settle_service_session()
                .map(|outcome| outcome.remaining_clients > 0 || !outcome.service_stopping)
        };
        let _ = tx.send(Event::ShutdownDone(result));
    });
}

// A failed status request may already have dropped our last lease. Observe the
// ensuing shutdown or release a fresh lease; never infer cleanup from disconnection.
fn settle_service_session() -> Result<ReleaseOutcome, String> {
    use omdrop_service::service_control::ServiceState;
    let deadline = Instant::now() + Duration::from_secs(30);
    let client = Client::new(ClientConfig {
        timeout: Duration::from_secs(3),
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;
    loop {
        match omdrop_service::service_control::status() {
            Ok(ServiceState::Stopped) => break,
            Err(error) if error.raw_os_error() == Some(1060) => break,
            Err(error) => {
                return Err(format!(
                    "{}{error}",
                    crate::i18n::current().strings().backend_status_failed
                ));
            }
            Ok(ServiceState::Running) => {
                match client
                    .acquire(SessionKind::Client)
                    .and_then(ClientLease::release)
                {
                    Ok(outcome) => return Ok(outcome),
                    Err(error)
                        if matches!(
                            error.kind(),
                            ClientErrorKind::Stopping | ClientErrorKind::Connect
                        ) => {}
                    Err(error) => {
                        return Err(format!(
                            "{}{error}",
                            crate::i18n::current().strings().backend_release_retry
                        ));
                    }
                }
            }
            Ok(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(crate::i18n::current()
                .strings()
                .backend_release_pending
                .into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(ReleaseOutcome {
        remaining_clients: 0,
        service_stopping: true,
    })
}

/// Cancels this operation if its service instance disappears. A restarted service
/// cannot authorize work under a lease granted by its predecessor.
pub(super) struct UsageSession {
    lease: Option<Arc<ClientLease>>,
    stop: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
}
impl UsageSession {
    pub(super) fn acquire(kind: SessionKind, cancel: impl Fn() + Send + 'static) -> Result<Self> {
        let lease = Arc::new(Client::default().acquire(kind)?);
        let watched = lease.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let watch_stop = stop.clone();
        let watcher = std::thread::spawn(move || {
            while !watch_stop.load(Ordering::Acquire) {
                if !watched.is_alive() {
                    cancel();
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        Ok(Self {
            lease: Some(lease),
            stop,
            watcher: Some(watcher),
        })
    }
}
impl Drop for UsageSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
        if let Some(lease) = self
            .lease
            .take()
            .and_then(|lease| Arc::try_unwrap(lease).ok())
        {
            let _ = lease.release();
        }
    }
}

pub(super) fn identity(computer_name: &str) -> Result<Identity> {
    omdrop_host::load_identity(computer_name)
}

pub(super) fn prepare_network() -> Result<()> {
    // Sending and receiving may start together, but the service serializes
    // firewall updates. Keep our requests from racing each other.
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = GATE.lock().unwrap_or_else(|error| error.into_inner());
    let text = crate::i18n::current().strings();
    let status: LinkStatus = Client::default()
        .firewall(&std::env::current_exe()?)
        .context(text.network_config_failed)?
        .into();
    if !status.ready() {
        bail!(text.bridge_not_ready);
    }
    Ok(())
}

pub fn scan(
    id: u64,
    sender_name: String,
    cancel: SendCancellation,
    tx: Sender<Event>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            let signal = cancel.clone();
            let _session = UsageSession::acquire(SessionKind::Client, move || signal.cancel())?;
            prepare_network()?;
            let interface = omdrop_host::awdl_interface()?;
            let identity = identity(&sender_name)?;
            omdrop_core::discover_receivers_live_cancellable(
                interface,
                &identity,
                Duration::from_secs(15),
                true,
                false,
                &cancel,
                |event| {
                    let _ = tx.send(Event::Discovery(id, event));
                },
            )?;
            Ok(())
        })();
        let _ = tx.send(Event::ScanDone(id, result.map_err(|e| format!("{e:#}"))));
    })
}

pub fn send(
    id: u64,
    peer: NearbyReceiver,
    files: Vec<PathBuf>,
    sender_name: String,
    cancel: SendCancellation,
    tx: Sender<Event>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            let signal = cancel.clone();
            let _session = UsageSession::acquire(SessionKind::Send, move || signal.cancel())?;
            prepare_network()?;
            let identity = identity(&sender_name)?;
            let options = SendOptions {
                sender_name,
                sender_model: "Windows".into(),
                target_name: peer.name.clone(),
                timeout: Duration::from_secs(15),
                format: SendFormat::Auto,
                ble_wake: true,
                legacy_ble: false,
            };
            let mut last = Instant::now();
            omdrop_core::send_to_receiver_cancellable(
                &peer,
                &identity,
                &files,
                &options,
                &cancel,
                |event| {
                    if let SendEvent::Uploading { sent, total } = &event {
                        if *sent != 0 && sent != total && last.elapsed() < Duration::from_millis(80)
                        {
                            return;
                        }
                        last = Instant::now();
                    }
                    let _ = tx.send(Event::Transfer(id, event));
                },
            )?;
            Ok(())
        })();
        let _ = tx.send(Event::SendDone(id, result.map_err(SendFailure::from)));
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decline_words_and_http_status_in_untyped_errors_do_not_count_as_receiver_refusal() {
        let message = "receiver declined the transfer (HTTP 401)";
        assert_eq!(
            SendFailure::from(anyhow::anyhow!(message)),
            SendFailure::Failed(message.into())
        );
        let network = omdrop_core::SendError::from(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out",
        ));
        assert!(matches!(
            SendFailure::from(anyhow::Error::new(network).context("send failed")),
            SendFailure::Connection(_)
        ));
    }
}
