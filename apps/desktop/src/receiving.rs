//! Background receiver owned by the desktop, including while its window is hidden.
use crate::backend::{self, Event};
use anyhow::Result;
use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use wayline_core::{IncomingTransfer, ReceiverCore, ReceiverEvent, ReceiverOptions};
use wayline_host::{
    TransferDirection, TransferFailureReason, TransferFormat, TransferHistoryRecord,
    TransferOutcome,
};

pub struct IncomingRequest {
    pub transfer: IncomingTransfer,
    pub save_dir: PathBuf,
    pub started_at_unix_ms: u64,
    pub decision: Sender<bool>,
    pub accepting: Arc<AtomicBool>,
    pub expires: Instant,
    pub stopped: Arc<AtomicBool>,
}
impl IncomingRequest {
    pub fn active(&self) -> bool {
        !self.stopped.load(Ordering::Acquire) && Instant::now() < self.expires
    }
}

pub fn save_directory() -> Result<PathBuf> {
    wayline_host::download_dir()
}

/// Only a successful event accepted by the receive model may open Explorer.
pub fn auto_open_location(
    event: &ReceiverEvent,
    applied: bool,
    enabled: bool,
    fallback: Option<&std::path::Path>,
) -> Option<PathBuf> {
    if !applied || !enabled {
        return None;
    }
    match event {
        ReceiverEvent::Received { files, .. } => files
            .first()
            .cloned()
            .or_else(|| fallback.map(std::path::Path::to_path_buf)),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Receiving { received: u64, total: Option<u64> },
    Confirming,
}

pub struct Transfer {
    pub id: String,
    pub sender: String,
    pub files: usize,
    pub file_names: Vec<String>,
    pub save_dir: PathBuf,
    pub phase: Phase,
    pub started_at: Option<Instant>,
    pub started_at_unix_ms: u64,
    pub file_bytes: Option<u64>,
    pub transferred_bytes: u64,
    pub channel: Option<u16>,
    pub format: Option<TransferFormat>,
}

impl Transfer {
    pub fn elapsed(&self, now: Instant) -> Option<Duration> {
        self.started_at
            .map(|started| now.saturating_duration_since(started))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeArea {
    Banner,
    Details,
}

pub struct Notice {
    pub id: u64,
    pub message: String,
    pub full_text: String,
    pub kind: NoticeKind,
    pub details_visible: bool,
    pub copied: bool,
    expires_at: Instant,
    banner_hovered: bool,
    details_hovered: bool,
    leave_at: Option<Instant>,
    copied_until: Option<Instant>,
}

const NOTICE_DURATION: Duration = Duration::from_secs(8);
const NOTICE_HOVER_GRACE: Duration = Duration::from_millis(350);
const NOTICE_COPIED_DURATION: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct Model {
    pub active: Option<Transfer>,
    pub notice: Option<Notice>,
    interrupted: Option<Transfer>,
    notice_generation: u64,
    pub speed_bytes_per_second: Option<u64>,
    speed: crate::transfer_speed::TransferSpeed,
    finished_elapsed: Option<Duration>,
    channel: Option<u16>,
}

impl Model {
    pub fn show_notice(&mut self, message: impl Into<String>, kind: NoticeKind) {
        self.show_notice_at(message.into(), kind, Instant::now());
    }

    fn show_notice_at(&mut self, message: String, kind: NoticeKind, now: Instant) {
        self.notice_generation = self.notice_generation.wrapping_add(1);
        self.notice = Some(Notice {
            id: self.notice_generation,
            full_text: message.clone(),
            message,
            kind,
            details_visible: false,
            copied: false,
            expires_at: now + NOTICE_DURATION,
            banner_hovered: false,
            details_hovered: false,
            leave_at: None,
            copied_until: None,
        });
    }

    pub fn show_notice_with_details(
        &mut self,
        message: impl Into<String>,
        full_text: impl Into<String>,
        kind: NoticeKind,
    ) {
        self.show_notice(message, kind);
        if let Some(notice) = &mut self.notice {
            notice.full_text = full_text.into();
        }
    }

    pub fn dismiss_notice(&mut self) -> bool {
        self.notice.take().is_some()
    }

    pub fn expire_notice(&mut self, now: Instant) -> bool {
        let Some(notice) = self.notice.as_mut() else {
            return false;
        };
        let mut changed = false;
        if notice.copied_until.is_some_and(|deadline| now >= deadline) {
            notice.copied = false;
            notice.copied_until = None;
            changed = true;
        }
        if notice.leave_at.is_some_and(|deadline| now >= deadline)
            && !notice.banner_hovered
            && !notice.details_hovered
        {
            notice.leave_at = None;
            notice.details_visible = false;
            notice.expires_at = now + NOTICE_DURATION;
            changed = true;
        }
        if !notice.details_visible
            && !notice.banner_hovered
            && !notice.details_hovered
            && notice.leave_at.is_none()
            && now >= notice.expires_at
        {
            return self.dismiss_notice();
        }
        changed
    }

    pub fn set_notice_hovered(&mut self, id: u64, area: NoticeArea, hovered: bool) -> bool {
        self.set_notice_hovered_at(id, area, hovered, Instant::now())
    }

    fn set_notice_hovered_at(
        &mut self,
        id: u64,
        area: NoticeArea,
        hovered: bool,
        now: Instant,
    ) -> bool {
        let Some(notice) = self.notice.as_mut().filter(|notice| notice.id == id) else {
            return false;
        };
        let current = match area {
            NoticeArea::Banner => &mut notice.banner_hovered,
            NoticeArea::Details => &mut notice.details_hovered,
        };
        if *current == hovered {
            return false;
        }
        *current = hovered;
        if hovered {
            notice.details_visible = true;
            notice.leave_at = None;
        } else if !notice.banner_hovered && !notice.details_hovered {
            notice.leave_at = Some(now + NOTICE_HOVER_GRACE);
        }
        true
    }

    pub fn copy_notice(&mut self, id: u64) -> Option<String> {
        self.copy_notice_at(id, Instant::now())
    }

    fn copy_notice_at(&mut self, id: u64, now: Instant) -> Option<String> {
        let notice = self.notice.as_mut().filter(|notice| notice.id == id)?;
        notice.copied = true;
        notice.copied_until = Some(now + NOTICE_COPIED_DURATION);
        Some(notice.full_text.clone())
    }

    pub fn reset_notice_hover(&mut self) -> bool {
        self.reset_notice_hover_at(Instant::now())
    }

    fn reset_notice_hover_at(&mut self, now: Instant) -> bool {
        let Some(notice) = self.notice.as_mut() else {
            return false;
        };
        let paused = notice.details_visible
            || notice.banner_hovered
            || notice.details_hovered
            || notice.leave_at.is_some();
        if paused {
            notice.banner_hovered = false;
            notice.details_hovered = false;
            notice.details_visible = false;
            notice.leave_at = None;
            notice.expires_at = now + NOTICE_DURATION;
        }
        paused
    }

    pub fn begin(&mut self, transfer: &IncomingTransfer, save_dir: PathBuf) {
        let file_bytes = offered_file_bytes(transfer);
        self.interrupted = None;
        self.finished_elapsed = None;
        self.active = Some(Transfer {
            id: transfer.id.clone(),
            sender: transfer.sender_name.clone(),
            files: transfer.files.len(),
            file_names: transfer
                .files
                .iter()
                .map(|file| file.name.clone())
                .collect(),
            save_dir,
            phase: Phase::Waiting,
            started_at: None,
            started_at_unix_ms: crate::history::unix_time_ms(std::time::SystemTime::now()),
            file_bytes,
            transferred_bytes: 0,
            channel: self.channel,
            format: None,
        });
        self.reset_speed();
        self.notice = None;
    }

    pub fn interrupt(&mut self, message: String) -> bool {
        let Some(active) = self.active.take() else {
            return false;
        };
        self.finished_elapsed = active.elapsed(Instant::now());
        self.reset_speed();
        self.interrupted = Some(active);
        self.show_notice(message, NoticeKind::Error);
        true
    }

    pub fn interrupt_with_details(&mut self, message: String, detail: String) -> bool {
        if !self.interrupt(message.clone()) {
            return false;
        }
        if let Some(notice) = &mut self.notice {
            notice.full_text = format!("{message}\n{detail}");
        }
        true
    }

    /// Only transfer-scoped events can finish the current receive. Discovery
    /// connections can fail independently while the upload is still healthy.
    #[cfg(test)]
    pub fn apply(&mut self, event: &ReceiverEvent) -> bool {
        self.apply_at(
            event,
            Instant::now(),
            crate::history::unix_time_ms(std::time::SystemTime::now()),
        )
    }

    pub(crate) fn apply_at(
        &mut self,
        event: &ReceiverEvent,
        now: Instant,
        now_unix_ms: u64,
    ) -> bool {
        // Radio polling and transfer callbacks use different queues. A terminal
        // transfer event can arrive after a status interrupt moved it aside.
        if self.active.is_none() {
            if let ReceiverEvent::Receiving {
                transfer_id,
                received,
                ..
            } = event
                && let Some(interrupted) = self.interrupted.as_mut()
                && transfer_id == &interrupted.id
            {
                // Account for queued progress without undoing the disconnect UI.
                if interrupted.started_at.is_none() {
                    interrupted.started_at = Some(now);
                    interrupted.started_at_unix_ms = now_unix_ms;
                }
                interrupted.transferred_bytes = *received;
            }
            let terminal_id = match event {
                ReceiverEvent::Received { transfer, .. } => Some(transfer.id.as_str()),
                ReceiverEvent::TransferFailed { transfer_id, .. } => Some(transfer_id.as_str()),
                _ => None,
            };
            if terminal_id
                .is_some_and(|id| self.interrupted.as_ref().is_some_and(|old| old.id == id))
            {
                self.active = self.interrupted.take();
            }
        }
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        match event {
            ReceiverEvent::ModeSelected(format) => {
                active.format = Some(match format {
                    wayline_core::TransferFormat::Gzip => TransferFormat::Gzip,
                    wayline_core::TransferFormat::DvZip => TransferFormat::DvZip,
                });
            }
            ReceiverEvent::Receiving {
                transfer_id,
                received,
                total,
            } if transfer_id == &active.id => {
                if active.started_at.is_none() {
                    active.started_at = Some(now);
                    active.started_at_unix_ms = now_unix_ms;
                }
                self.speed.update(now, *received);
                self.speed_bytes_per_second = self
                    .speed
                    .available()
                    .then(|| self.speed.bytes_per_second());
                active.transferred_bytes = *received;
                active.phase = Phase::Receiving {
                    received: *received,
                    total: *total,
                };
            }
            ReceiverEvent::Confirming { transfer_id } if transfer_id == &active.id => {
                self.speed = crate::transfer_speed::TransferSpeed::default();
                self.speed_bytes_per_second = None;
                active.phase = Phase::Confirming;
            }
            ReceiverEvent::Received { transfer, files } if transfer.id == active.id => {
                let message = crate::i18n::current().received_notice(
                    &active.sender,
                    files.len(),
                    &active.save_dir.display().to_string(),
                );
                self.finished_elapsed = active.elapsed(now);
                self.active = None;
                self.reset_speed();
                self.show_notice(message, NoticeKind::Success);
            }
            ReceiverEvent::TransferFailed {
                transfer_id,
                kind,
                error,
            } if transfer_id == &active.id => {
                let text = crate::i18n::current().strings();
                let message = match kind {
                    wayline_core::TransferFailureKind::Cancelled => text.receive_cancelled,
                    wayline_core::TransferFailureKind::Declined => text.receive_declined_notice,
                    wayline_core::TransferFailureKind::Validation => text.receive_validation,
                    wayline_core::TransferFailureKind::Connection => text.receive_connection,
                    _ => text.receive_failed,
                };
                self.finished_elapsed = active.elapsed(now);
                self.active = None;
                self.reset_speed();
                // Keep the banner short; full diagnostics remain available on demand.
                self.show_notice_with_details(
                    message,
                    format!("{message}\n{error}"),
                    NoticeKind::Error,
                );
            }
            _ => return false,
        }
        true
    }

    pub fn history_record_for_event(
        &self,
        event: &ReceiverEvent,
        finished_at: Instant,
        finished_at_unix_ms: u64,
    ) -> Option<TransferHistoryRecord> {
        let (transfer_id, outcome, failure_reason) = match event {
            ReceiverEvent::Received { transfer, .. } => {
                (&transfer.id, TransferOutcome::Completed, None)
            }
            ReceiverEvent::TransferFailed {
                transfer_id, kind, ..
            } => {
                let (outcome, reason) = match kind {
                    wayline_core::TransferFailureKind::Declined => {
                        (TransferOutcome::Declined, None)
                    }
                    wayline_core::TransferFailureKind::Cancelled => {
                        (TransferOutcome::Cancelled, None)
                    }
                    wayline_core::TransferFailureKind::Connection => (
                        TransferOutcome::Failed,
                        Some(TransferFailureReason::Connection),
                    ),
                    wayline_core::TransferFailureKind::Validation => (
                        TransferOutcome::Failed,
                        Some(TransferFailureReason::Validation),
                    ),
                    wayline_core::TransferFailureKind::Other => {
                        (TransferOutcome::Failed, Some(TransferFailureReason::Other))
                    }
                };
                (transfer_id, outcome, reason)
            }
            _ => return None,
        };
        let transfer = self
            .active
            .as_ref()
            .or(self.interrupted.as_ref())
            .filter(|transfer| &transfer.id == transfer_id)?;
        Some(TransferHistoryRecord {
            started_at_unix_ms: transfer.started_at_unix_ms,
            finished_at_unix_ms: finished_at_unix_ms.max(transfer.started_at_unix_ms),
            direction: TransferDirection::Receive,
            peer: transfer.sender.clone(),
            file_count: transfer.files,
            file_names: transfer.file_names.clone(),
            file_paths: match event {
                ReceiverEvent::Received { files, .. } => files.clone(),
                _ => Vec::new(),
            },
            channel: transfer.channel,
            format: transfer.format,
            file_bytes: transfer.file_bytes,
            transferred_bytes: transfer.transferred_bytes,
            duration_ms: transfer
                .elapsed(finished_at)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64,
            outcome,
            failure_reason,
            destination: (outcome == TransferOutcome::Completed).then(|| transfer.save_dir.clone()),
        })
    }

    pub fn receive_elapsed(&self, now: Instant) -> Option<Duration> {
        self.active
            .as_ref()
            .and_then(|transfer| transfer.elapsed(now))
            .or(self.finished_elapsed)
    }

    pub fn set_channel(&mut self, channel: Option<u16>) {
        self.channel = channel.filter(|channel| matches!(channel, 6 | 44 | 149));
    }

    pub fn transfer_metadata(&self) -> (Option<u16>, Option<TransferFormat>) {
        self.active
            .as_ref()
            .map(|transfer| (transfer.channel, transfer.format))
            .unwrap_or((self.channel, None))
    }

    pub fn expire_speed(&mut self, now: Instant) -> bool {
        if !matches!(
            self.active.as_ref().map(|active| &active.phase),
            Some(Phase::Receiving { .. })
        ) {
            return false;
        }
        if self.speed.expire(now) {
            self.speed_bytes_per_second = Some(0);
            true
        } else {
            false
        }
    }

    fn reset_speed(&mut self) {
        self.speed = crate::transfer_speed::TransferSpeed::default();
        self.speed_bytes_per_second = None;
    }
}

fn offered_file_bytes(transfer: &IncomingTransfer) -> Option<u64> {
    if transfer.files.is_empty() {
        return None;
    }
    transfer
        .files
        .iter()
        .try_fold(0u64, |total, file| total.checked_add(file.size?))
}

pub fn declined_history_record(
    request: &IncomingRequest,
    finished_at_unix_ms: u64,
    _finished_at: Instant,
) -> TransferHistoryRecord {
    let file_bytes = offered_file_bytes(&request.transfer);
    TransferHistoryRecord {
        started_at_unix_ms: request.started_at_unix_ms,
        finished_at_unix_ms: finished_at_unix_ms.max(request.started_at_unix_ms),
        direction: TransferDirection::Receive,
        peer: request.transfer.sender_name.clone(),
        file_count: request.transfer.files.len(),
        file_names: request
            .transfer
            .files
            .iter()
            .map(|file| file.name.clone())
            .collect(),
        file_paths: Vec::new(),
        channel: None,
        format: None,
        file_bytes,
        transferred_bytes: 0,
        duration_ms: 0,
        outcome: TransferOutcome::Declined,
        failure_reason: None,
        destination: None,
    }
}

enum Command {
    Configure(Option<String>),
    ReloadDirectory,
}

pub struct ReceiverHost {
    commands: Option<Sender<Command>>,
    configured: Option<String>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    core: Arc<Mutex<Weak<ReceiverCore>>>,
}
struct Running {
    stop: Arc<AtomicBool>,
    worker: JoinHandle<Result<(), String>>,
}
impl Running {
    fn stop(self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.worker.join();
    }
}

impl ReceiverHost {
    pub fn new(events: Sender<Event>) -> Self {
        let (commands, rx) = mpsc::channel::<Command>();
        let stop = Arc::new(AtomicBool::new(false));
        let host_stop = stop.clone();
        let core = Arc::new(Mutex::new(Weak::new()));
        let host_core = core.clone();
        let worker = thread::spawn(move || {
            let mut desired = None;
            let mut running: Option<Running> = None;
            let mut retry_at = Instant::now();
            while !host_stop.load(Ordering::Acquire) {
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(Command::Configure(command)) => {
                        if command != desired {
                            if let Some(previous) = running.take() {
                                previous.stop();
                            }
                            desired = command;
                            retry_at = Instant::now();
                            let _ = events.send(Event::ReceiverStatus(Err(crate::i18n::current()
                                .strings()
                                .receiver_waiting_radio
                                .into())));
                        }
                    }
                    Ok(Command::ReloadDirectory) => {
                        if let Some(previous) = running.take() {
                            previous.stop();
                        }
                        retry_at = Instant::now();
                        let _ = events.send(Event::ReceiverStatus(Err(crate::i18n::current()
                            .strings()
                            .receiver_updating_dir
                            .into())));
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if running
                    .as_ref()
                    .is_some_and(|task| task.worker.is_finished())
                {
                    let task = running.take().unwrap();
                    let text = crate::i18n::current().strings();
                    let error = match task.worker.join() {
                        Ok(Err(error)) => error,
                        Ok(Ok(())) => text.receiver_stopped.into(),
                        Err(_) => text.receiver_crashed.into(),
                    };
                    log(&error);
                    let _ = events.send(Event::ReceiverStatus(Err(error)));
                    retry_at = Instant::now() + Duration::from_secs(5);
                }
                if running.is_none()
                    && Instant::now() >= retry_at
                    && let Some(name) = desired.clone()
                {
                    let stop = Arc::new(AtomicBool::new(false));
                    let worker_stop = stop.clone();
                    let worker_events = events.clone();
                    let worker_core = host_core.clone();
                    let worker = thread::spawn(move || {
                        run(&name, worker_stop, worker_events, worker_core)
                            .map_err(|error| format!("{error:#}"))
                    });
                    running = Some(Running { stop, worker });
                }
            }
            if let Some(task) = running {
                task.stop();
            }
        });
        Self {
            commands: Some(commands),
            configured: None,
            stop,
            worker: Some(worker),
            core,
        }
    }

    pub fn configure(&mut self, name: Option<String>) {
        if name != self.configured {
            self.configured = name.clone();
            if let Some(commands) = &self.commands {
                let _ = commands.send(Command::Configure(name));
            }
        }
    }

    pub fn reload_directory(&self) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(Command::ReloadDirectory);
        }
    }

    pub fn shutdown(&mut self) -> Option<JoinHandle<()>> {
        self.stop.store(true, Ordering::Release);
        self.commands.take();
        self.worker.take()
    }

    pub fn has_active_transfers(&self) -> bool {
        self.core.lock().map_or(true, |core| {
            core.upgrade()
                .is_some_and(|core| core.has_active_transfers())
        })
    }
}

impl Drop for ReceiverHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(
    name: &str,
    stop: Arc<AtomicBool>,
    events: Sender<Event>,
    shared_core: Arc<Mutex<Weak<ReceiverCore>>>,
) -> Result<()> {
    let signal = stop.clone();
    let _session = backend::UsageSession::acquire(
        wayline_service::session::SessionKind::Receive,
        move || signal.store(true, Ordering::Release),
    )?;
    backend::prepare_network()?;
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let interface = wayline_host::awdl_interface()?;
    let identity = backend::identity(name)?;
    let advertisement = wayline_host::receiver_advertisement(&identity, name)?;
    let service_id = advertisement.service_id;
    let config = wayline_core::server_config(&identity)?;
    let directory = save_directory()?;
    std::fs::create_dir_all(&directory)?;
    let mut options = ReceiverOptions::new(name, "Windows", &directory);
    options.accept_all = false;
    options.allow_dvzip_archive_boundary = true;
    options.retired_service_ids = advertisement.retired_service_ids;
    let prompt_events = events.clone();
    let prompt_stop = stop.clone();
    let core = Arc::new(ReceiverCore::new(options).with_decision(move |transfer| {
        if prompt_stop.load(Ordering::Acquire) || transfer.files.is_empty() {
            return false;
        }
        let (decision, answer) = mpsc::channel();
        let accepting = Arc::new(AtomicBool::new(false));
        let started_at = Instant::now();
        let started_at_unix_ms = crate::history::unix_time_ms(std::time::SystemTime::now());
        let expires = started_at + Duration::from_secs(45);
        if prompt_events
            .send(Event::Incoming(IncomingRequest {
                transfer: transfer.clone(),
                save_dir: directory.clone(),
                started_at_unix_ms,
                decision,
                accepting: accepting.clone(),
                expires,
                stopped: prompt_stop.clone(),
            }))
            .is_err()
        {
            return false;
        }
        let accepted = wait_for_decision(answer, &prompt_stop, expires);
        if !accepted && accepting.load(Ordering::Acquire) {
            // Sending a positive answer does not guarantee the worker observed it
            // before expiry. End the UI's waiting state even in that boundary race.
            let _ = prompt_events.send(
                Event::ReceiverActivity(ReceiverEvent::TransferFailed {
                    transfer_id: transfer.id.clone(),
                    error: crate::i18n::current().strings().incoming_expired.into(),
                    kind: wayline_core::TransferFailureKind::Cancelled,
                })
                .timestamped(),
            );
        }
        accepted
    }));
    *shared_core
        .lock()
        .map_err(|_| anyhow::anyhow!(crate::i18n::current().strings().receiver_state_lock))? =
        Arc::downgrade(&core);
    let (activity, activity_rx) = mpsc::channel();
    let advertised_name = name.to_owned();
    let advertised_id = service_id.clone();
    let activity_worker = thread::spawn(move || {
        for event in activity_rx {
            let at = Instant::now();
            let unix_ms = crate::history::unix_time_ms(std::time::SystemTime::now());
            match &event {
                ReceiverEvent::Listening => {
                    log(&format!(
                        "listening and advertising {advertised_name:?} ({advertised_id}) on awdl0:8771"
                    ));
                    let _ = events.send(Event::ReceiverStatus(Ok(())));
                }
                ReceiverEvent::Discovered => log("remote /Discover answered"),
                ReceiverEvent::ConnectionFailed { peer, error, .. } => {
                    log(&format!("connection from {peer} failed: {error}"))
                }
                ReceiverEvent::Asked(_) => log("incoming offer accepted"),
                ReceiverEvent::ModeSelected(_) => log("transfer mode selected"),
                ReceiverEvent::Receiving { .. } => {}
                ReceiverEvent::ArchiveBoundary { transfer_id } => log(&format!(
                    "transfer {transfer_id}: responding at validated AirDrop DVZip archive boundary"
                )),
                ReceiverEvent::Confirming { transfer_id } => {
                    log(&format!("confirming transfer {transfer_id}"))
                }
                ReceiverEvent::TransferFailed {
                    transfer_id, error, ..
                } => log(&format!("transfer {transfer_id} failed: {error}")),
                ReceiverEvent::Received { files, .. } => {
                    log(&format!("received {} files", files.len()))
                }
                ReceiverEvent::Rejected { path, status } => {
                    log(&format!("rejected {path}: {status}"))
                }
            }
            if events
                .send(Event::Timestamped(
                    Box::new(Event::ReceiverActivity(event)),
                    at,
                    unix_ms,
                ))
                .is_err()
            {
                break;
            }
        }
    });
    let result =
        ReceiverCore::run_listener(interface, 8771, &service_id, config, core, stop, activity);
    // Drain the listener's terminal events before reporting a host restart/error.
    let _ = activity_worker.join();
    result?;
    Ok(())
}

fn wait_for_decision(answer: mpsc::Receiver<bool>, stop: &AtomicBool, expires: Instant) -> bool {
    while Instant::now() < expires && !stop.load(Ordering::Acquire) {
        match answer.recv_timeout(
            Duration::from_millis(100).min(expires.saturating_duration_since(Instant::now())),
        ) {
            Ok(accepted) => {
                return accepted && !stop.load(Ordering::Acquire) && Instant::now() < expires;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    false
}

fn log(message: &str) {
    if let Some(directory) = std::env::var_os("LOCALAPPDATA") {
        let path = PathBuf::from(directory)
            .join("wayline")
            .join("desktop-receiver.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let seconds = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let _ = writeln!(file, "{seconds} {message}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transfer(id: &str) -> IncomingTransfer {
        IncomingTransfer {
            id: id.into(),
            sender_name: "iPhone".into(),
            sender_model: None,
            files: vec![],
            urls: vec![],
            icon: None,
            sender_record_data: None,
        }
    }

    #[test]
    fn auto_open_requires_matching_completion_and_respects_the_preference() {
        let directory = PathBuf::from("chosen folder");
        let mut model = Model::default();
        model.begin(&transfer("current"), directory.clone());
        for event in [
            ReceiverEvent::Receiving {
                transfer_id: "current".into(),
                received: 100,
                total: Some(100),
            },
            ReceiverEvent::Confirming {
                transfer_id: "current".into(),
            },
            ReceiverEvent::Received {
                transfer: transfer("stale"),
                files: vec![directory.join("stale.txt")],
            },
        ] {
            let applied = model.apply(&event);
            assert!(auto_open_location(&event, applied, true, Some(&directory)).is_none());
        }
        let event = ReceiverEvent::Received {
            transfer: transfer("current"),
            files: vec![directory.join("photo.jpg"), directory.join("other.txt")],
        };
        let applied = model.apply(&event);
        assert!(applied);
        assert!(auto_open_location(&event, applied, false, Some(&directory)).is_none());
        assert_eq!(
            auto_open_location(&event, applied, true, Some(&directory)),
            Some(directory.join("photo.jpg"))
        );
        assert!(auto_open_location(&event, model.apply(&event), true, Some(&directory)).is_none());

        model.begin(&transfer("empty"), directory.clone());
        let empty = ReceiverEvent::Received {
            transfer: transfer("empty"),
            files: vec![],
        };
        let applied = model.apply(&empty);
        assert_eq!(
            auto_open_location(&empty, applied, true, Some(&directory)),
            Some(directory.clone())
        );
        assert!(auto_open_location(&empty, applied, true, None).is_none());

        model.begin(&transfer("failed"), directory.clone());
        let failed = ReceiverEvent::TransferFailed {
            transfer_id: "failed".into(),
            error: "cancelled".into(),
            kind: wayline_core::TransferFailureKind::Cancelled,
        };
        assert!(
            auto_open_location(&failed, model.apply(&failed), true, Some(&directory)).is_none()
        );
    }

    #[test]
    fn receive_progress_waits_for_confirmation_and_completion_clears_the_guard() {
        let offer = transfer("current");
        let mut model = Model::default();
        model.begin(&offer, PathBuf::from("chosen folder"));
        assert_eq!(model.active.as_ref().unwrap().phase, Phase::Waiting);
        assert!(model.apply(&ReceiverEvent::Receiving {
            transfer_id: offer.id.clone(),
            received: 512,
            total: None,
        }));
        assert_eq!(
            model.active.as_ref().unwrap().phase,
            Phase::Receiving {
                received: 512,
                total: None
            }
        );
        assert!(model.apply(&ReceiverEvent::Receiving {
            transfer_id: offer.id.clone(),
            received: 1024,
            total: Some(1024),
        }));
        assert!(model.active.is_some(), "100% is not a completed receive");
        assert!(model.apply(&ReceiverEvent::Confirming {
            transfer_id: offer.id.clone()
        }));
        assert_eq!(model.active.as_ref().unwrap().phase, Phase::Confirming);
        assert!(model.apply(&ReceiverEvent::Received {
            transfer: offer,
            files: vec!["chosen folder/photo.jpg".into()]
        }));
        assert!(model.active.is_none());
        assert!(
            model
                .notice
                .as_ref()
                .unwrap()
                .message
                .contains("chosen folder")
        );
        assert!(!model.apply(&ReceiverEvent::Receiving {
            transfer_id: "current".into(),
            received: 1024,
            total: Some(1024),
        }));
    }

    #[test]
    fn only_matching_failure_ends_waiting_or_uploading() {
        for progress in [false, true] {
            let mut model = Model::default();
            model.begin(&transfer("current"), "saved".into());
            if progress {
                model.apply(&ReceiverEvent::Receiving {
                    transfer_id: "current".into(),
                    received: 10,
                    total: None,
                });
            }
            assert!(!model.apply(&ReceiverEvent::ConnectionFailed {
                peer: "[::1]:8771".parse().unwrap(),
                error: "unrelated discovery".into(),
                kind: wayline_core::TransferFailureKind::Connection,
            }));
            assert!(!model.apply(&ReceiverEvent::TransferFailed {
                transfer_id: "old".into(),
                error: "old error".into(),
                kind: wayline_core::TransferFailureKind::Connection,
            }));
            assert!(model.active.is_some());
            assert!(model.apply(&ReceiverEvent::TransferFailed {
                transfer_id: "current".into(),
                error: "upload stage=archive decoding; internal diagnostic detail".repeat(1000),
                kind: wayline_core::TransferFailureKind::Connection,
            }));
            assert!(model.active.is_none());
            let notice = model.notice.as_ref().unwrap();
            assert_eq!(notice.kind, NoticeKind::Error);
            assert!(notice.message.chars().count() < 40);
            assert!(!notice.message.contains("upload stage="));
        }
    }

    #[test]
    fn completed_receive_wins_a_disconnect_race_but_cannot_replace_a_new_transfer() {
        let mut model = Model::default();
        model.begin(&transfer("first"), "saved".into());
        assert!(model.interrupt("无线连接中断".into()));
        assert!(model.active.is_none());
        assert!(!model.apply(&ReceiverEvent::Receiving {
            transfer_id: "first".into(),
            received: 10,
            total: None
        }));
        assert!(model.apply(&ReceiverEvent::Received {
            transfer: transfer("first"),
            files: vec![]
        }));
        assert_eq!(model.notice.as_ref().unwrap().kind, NoticeKind::Success);

        model.begin(&transfer("second"), "saved".into());
        model.interrupt("无线连接中断".into());
        model.begin(&transfer("third"), "saved".into());
        assert!(!model.apply(&ReceiverEvent::Received {
            transfer: transfer("second"),
            files: vec![]
        }));
        assert_eq!(model.active.as_ref().unwrap().id, "third");
    }

    #[test]
    fn receive_history_uses_only_matching_terminal_events_and_transfer_counters() {
        let offer = transfer("history-transfer");
        let mut model = Model::default();
        model.begin(&offer, PathBuf::from("chosen folder"));
        model.apply(&ReceiverEvent::Receiving {
            transfer_id: offer.id.clone(),
            received: 4096,
            total: Some(8192),
        });
        let now = Instant::now();
        assert!(
            model
                .history_record_for_event(
                    &ReceiverEvent::TransferFailed {
                        transfer_id: "old-transfer".into(),
                        error: "ignored".into(),
                        kind: wayline_core::TransferFailureKind::Connection,
                    },
                    now,
                    100,
                )
                .is_none()
        );
        let failed = model
            .history_record_for_event(
                &ReceiverEvent::TransferFailed {
                    transfer_id: offer.id.clone(),
                    error: "private diagnostic detail".into(),
                    kind: wayline_core::TransferFailureKind::Connection,
                },
                now,
                200,
            )
            .unwrap();
        assert_eq!(failed.outcome, TransferOutcome::Failed);
        assert_eq!(failed.transferred_bytes, 4096);
        assert_eq!(
            failed.failure_reason,
            Some(TransferFailureReason::Connection)
        );
        assert!(failed.destination.is_none());
    }

    #[test]
    fn receive_elapsed_excludes_acceptance_wait_and_freezes_when_finished() {
        let mut offer = transfer("timed");
        offer.files.push(wayline_core::IncomingFile {
            name: "photo.png".into(),
            kind: "public.png".into(),
            size: Some(100),
        });
        let mut model = Model::default();
        model.begin(&offer, PathBuf::from("chosen folder"));
        let now = Instant::now();
        assert_eq!(model.receive_elapsed(now + Duration::from_secs(30)), None);
        assert!(!model.apply_at(
            &ReceiverEvent::Receiving {
                transfer_id: "old".into(),
                received: 0,
                total: Some(100)
            },
            now,
            1000,
        ));
        assert_eq!(model.receive_elapsed(now), None);
        model.apply_at(
            &ReceiverEvent::Receiving {
                transfer_id: offer.id.clone(),
                received: 0,
                total: Some(100),
            },
            now + Duration::from_secs(30),
            31_000,
        );
        model.apply_at(
            &ReceiverEvent::Receiving {
                transfer_id: offer.id.clone(),
                received: 100,
                total: Some(100),
            },
            now + Duration::from_secs(32),
            33_000,
        );
        model.apply_at(
            &ReceiverEvent::Confirming {
                transfer_id: offer.id.clone(),
            },
            now + Duration::from_secs(32),
            33_000,
        );
        assert_eq!(
            model.receive_elapsed(now + Duration::from_secs(34)),
            Some(Duration::from_secs(4))
        );
        let completed = ReceiverEvent::Received {
            transfer: offer,
            files: vec![PathBuf::from("saved-photo.png")],
        };
        let record = model
            .history_record_for_event(&completed, now + Duration::from_secs(35), 36_000)
            .unwrap();
        assert_eq!(record.started_at_unix_ms, 31_000);
        assert_eq!(record.duration_ms, 5000);
        assert_eq!(record.file_names, vec!["photo.png"]);
        assert_eq!(record.file_paths, vec![PathBuf::from("saved-photo.png")]);
        model.apply_at(&completed, now + Duration::from_secs(35), 36_000);
        assert_eq!(
            model.receive_elapsed(now + Duration::from_secs(60)),
            Some(Duration::from_secs(5))
        );
        model.begin(&transfer("next"), PathBuf::from("chosen folder"));
        assert_eq!(model.receive_elapsed(now + Duration::from_secs(60)), None);
    }

    #[test]
    fn receive_failure_before_data_has_no_duration_or_file_paths() {
        let offer = transfer("waiting");
        let mut model = Model::default();
        model.begin(&offer, PathBuf::from("chosen folder"));
        let event = ReceiverEvent::TransferFailed {
            transfer_id: offer.id,
            error: "cancelled".into(),
            kind: wayline_core::TransferFailureKind::Cancelled,
        };
        let record = model
            .history_record_for_event(&event, Instant::now() + Duration::from_secs(60), u64::MAX)
            .unwrap();
        assert_eq!(record.duration_ms, 0);
        assert!(record.file_paths.is_empty());
    }

    #[test]
    fn interrupted_receive_preserves_queued_progress_for_late_completion() {
        let offer = transfer("interrupted");
        let mut model = Model::default();
        model.begin(&offer, PathBuf::from("chosen folder"));
        model.interrupt("disconnected".into());
        let started = Instant::now();
        for (seconds, received) in [(0, 0), (2, 1024)] {
            assert!(!model.apply_at(
                &ReceiverEvent::Receiving {
                    transfer_id: offer.id.clone(),
                    received,
                    total: Some(1024)
                },
                started + Duration::from_secs(seconds),
                1000 + seconds * 1000,
            ));
            assert!(model.active.is_none());
            assert_eq!(model.speed_bytes_per_second, None);
        }
        let event = ReceiverEvent::Received {
            transfer: offer,
            files: vec![],
        };
        let finished = started + Duration::from_secs(3);
        let record = model
            .history_record_for_event(&event, finished, 4000)
            .unwrap();
        assert_eq!(record.started_at_unix_ms, 1000);
        assert_eq!(record.duration_ms, 3000);
        assert_eq!(record.transferred_bytes, 1024);
        assert_eq!(
            record.finished_at_unix_ms - record.started_at_unix_ms,
            record.duration_ms
        );
        assert!(model.apply_at(&event, finished, 4000));
        assert_eq!(
            model.receive_elapsed(finished + Duration::from_secs(10)),
            Some(Duration::from_secs(3))
        );
    }

    #[test]
    fn notice_expiry_uses_the_current_notice_deadline_and_only_changes_once() {
        let mut model = Model::default();
        let start = Instant::now();
        model.show_notice_at("first".into(), NoticeKind::Error, start);
        assert!(!model.expire_notice(start + NOTICE_DURATION - Duration::from_millis(1)));
        model.show_notice_at(
            "second".into(),
            NoticeKind::Info,
            start + Duration::from_secs(4),
        );
        assert!(!model.expire_notice(start + NOTICE_DURATION));
        assert_eq!(model.notice.as_ref().unwrap().message, "second");
        assert!(model.expire_notice(start + Duration::from_secs(4) + NOTICE_DURATION));
        assert!(model.notice.is_none());
        assert!(!model.expire_notice(start + Duration::from_secs(60)));
    }

    #[test]
    fn dismissing_or_expiring_a_notice_keeps_transfer_state_and_late_success() {
        let mut model = Model::default();
        model.begin(&transfer("current"), "saved".into());
        let now = Instant::now();
        model.show_notice_at("info".into(), NoticeKind::Info, now);
        assert!(model.expire_notice(now + NOTICE_DURATION));
        assert_eq!(model.active.as_ref().unwrap().id, "current");
        model.interrupt("connection interrupted".into());
        assert!(model.dismiss_notice());
        assert!(!model.dismiss_notice());
        assert!(model.apply(&ReceiverEvent::Received {
            transfer: transfer("current"),
            files: vec![],
        }));
        assert_eq!(model.notice.as_ref().unwrap().kind, NoticeKind::Success);
        model.begin(&transfer("new"), "saved".into());
        assert!(model.notice.is_none());
    }

    #[test]
    fn hovering_a_notice_pauses_expiry_and_moving_into_details_keeps_it_open() {
        let mut model = Model::default();
        let start = Instant::now();
        model.show_notice_at("failure".into(), NoticeKind::Error, start);
        let id = model.notice.as_ref().unwrap().id;
        assert!(model.set_notice_hovered_at(
            id,
            NoticeArea::Banner,
            true,
            start + Duration::from_secs(7)
        ));
        assert!(!model.expire_notice(start + Duration::from_secs(30)));
        assert!(model.notice.as_ref().unwrap().details_visible);
        let leave = start + Duration::from_secs(31);
        assert!(model.set_notice_hovered_at(id, NoticeArea::Banner, false, leave));
        assert!(!model.expire_notice(leave + Duration::from_millis(200)));
        assert!(model.set_notice_hovered_at(
            id,
            NoticeArea::Details,
            true,
            leave + Duration::from_millis(200)
        ));
        assert!(!model.expire_notice(start + Duration::from_secs(60)));
        assert!(model.notice.as_ref().unwrap().details_visible);
        assert!(!model.set_notice_hovered_at(
            id,
            NoticeArea::Details,
            true,
            start + Duration::from_secs(60)
        ));
    }

    #[test]
    fn leaving_both_notice_areas_closes_details_after_grace_and_restarts_expiry() {
        let mut model = Model::default();
        let start = Instant::now();
        model.show_notice_at("failure".into(), NoticeKind::Error, start);
        let id = model.notice.as_ref().unwrap().id;
        model.set_notice_hovered_at(id, NoticeArea::Banner, true, start);
        model.set_notice_hovered_at(id, NoticeArea::Details, true, start);
        model.set_notice_hovered_at(id, NoticeArea::Banner, false, start);
        assert!(!model.expire_notice(start + Duration::from_secs(20)));
        let leave = start + Duration::from_secs(21);
        model.set_notice_hovered_at(id, NoticeArea::Details, false, leave);
        assert!(!model.expire_notice(leave + NOTICE_HOVER_GRACE - Duration::from_millis(1)));
        let closed = leave + NOTICE_HOVER_GRACE;
        assert!(model.expire_notice(closed));
        assert!(!model.notice.as_ref().unwrap().details_visible);
        assert!(!model.expire_notice(closed + NOTICE_DURATION - Duration::from_millis(1)));
        assert!(model.expire_notice(closed + NOTICE_DURATION));
        assert!(model.notice.is_none());
    }

    #[test]
    fn old_notice_hover_and_copy_events_cannot_change_a_replacement() {
        let mut model = Model::default();
        let start = Instant::now();
        model.show_notice_at("old".into(), NoticeKind::Error, start);
        let old = model.notice.as_ref().unwrap().id;
        model.set_notice_hovered_at(old, NoticeArea::Banner, true, start);
        model.copy_notice_at(old, start);
        model.show_notice_at("new".into(), NoticeKind::Info, start);
        let current = model.notice.as_ref().unwrap().id;
        assert_ne!(current, old);
        assert!(!model.set_notice_hovered_at(old, NoticeArea::Banner, false, start));
        assert!(!model.set_notice_hovered_at(old, NoticeArea::Details, true, start));
        assert!(model.copy_notice_at(old, start).is_none());
        let notice = model.notice.as_ref().unwrap();
        assert_eq!(notice.full_text, "new");
        assert!(!notice.details_visible && !notice.copied);
        assert!(model.expire_notice(start + NOTICE_DURATION));
        assert!(!model.set_notice_hovered_at(current, NoticeArea::Banner, true, start));
        assert!(model.copy_notice_at(current, start).is_none());
    }

    #[test]
    fn copying_notice_returns_full_diagnostics_and_feedback_has_its_own_deadline() {
        let mut model = Model::default();
        model.begin(&transfer("current"), "saved".into());
        let diagnostic =
            "upload stage=archive decoding; received=123; HTTP=ChunkHeader: socket timeout";
        model.apply(&ReceiverEvent::TransferFailed {
            transfer_id: "current".into(),
            kind: wayline_core::TransferFailureKind::Connection,
            error: diagnostic.into(),
        });
        let notice = model.notice.as_ref().unwrap();
        let id = notice.id;
        let expected = format!("{}\n{diagnostic}", notice.message);
        assert!(!notice.message.contains(diagnostic));
        let now = Instant::now();
        assert_eq!(model.copy_notice_at(id, now), Some(expected.clone()));
        assert!(model.notice.as_ref().unwrap().copied);
        assert_eq!(
            model.copy_notice_at(id, now + Duration::from_secs(1)),
            Some(expected)
        );
        assert!(!model.expire_notice(now + NOTICE_COPIED_DURATION));
        assert!(model.notice.as_ref().unwrap().copied);
        assert!(model.expire_notice(now + Duration::from_secs(1) + NOTICE_COPIED_DURATION));
        assert!(!model.notice.as_ref().unwrap().copied);
        assert!(!model.expire_notice(now + Duration::from_secs(4)));
    }

    #[test]
    fn hiding_notice_details_resumes_expiry_without_resetting_unpaused_notices() {
        let mut model = Model::default();
        model.begin(&transfer("current"), "saved".into());
        let start = Instant::now();
        model.show_notice_at("info".into(), NoticeKind::Info, start);
        let id = model.notice.as_ref().unwrap().id;
        model.set_notice_hovered_at(id, NoticeArea::Banner, true, start);
        model.set_notice_hovered_at(
            id,
            NoticeArea::Banner,
            false,
            start + Duration::from_secs(1),
        );
        let hidden = start + Duration::from_secs(1) + Duration::from_millis(100);
        assert!(model.reset_notice_hover_at(hidden));
        assert!(!model.notice.as_ref().unwrap().details_visible);
        assert!(!model.reset_notice_hover_at(hidden + Duration::from_secs(1)));
        assert!(!model.expire_notice(hidden + NOTICE_DURATION - Duration::from_millis(1)));
        assert!(model.expire_notice(hidden + NOTICE_DURATION));
        assert_eq!(model.active.as_ref().unwrap().id, "current");
        assert!(!model.reset_notice_hover_at(hidden + NOTICE_DURATION));
    }

    #[test]
    fn receiving_requires_a_live_positive_decision_and_fails_closed() {
        for answer in [false, true] {
            let (tx, rx) = mpsc::channel();
            tx.send(answer).unwrap();
            assert_eq!(
                wait_for_decision(
                    rx,
                    &AtomicBool::new(false),
                    Instant::now() + Duration::from_secs(1)
                ),
                answer
            );
        }
        let (tx, rx) = mpsc::channel();
        drop(tx); // Closed window/host: never accept silently.
        assert!(!wait_for_decision(
            rx,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(1)
        ));
        let (tx, rx) = mpsc::channel();
        tx.send(true).unwrap();
        assert!(!wait_for_decision(
            rx,
            &AtomicBool::new(true),
            Instant::now() + Duration::from_secs(1)
        ));
        let (tx, rx) = mpsc::channel();
        tx.send(true).unwrap();
        assert!(!wait_for_decision(
            rx,
            &AtomicBool::new(false),
            Instant::now() - Duration::from_secs(1)
        ));
        let (_tx, rx) = mpsc::channel();
        assert!(!wait_for_decision(
            rx,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_millis(5)
        ));
    }
}
