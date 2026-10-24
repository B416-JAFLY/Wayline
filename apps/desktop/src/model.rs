use crate::{
    backend::{Event, LinkStatus, SendFailure},
    i18n::{self, Language},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use wayline_core::{DiscoveryEvent, NearbyReceiver, SendEvent, SendFormat};
use wayline_host::{
    TransferDirection, TransferFailureReason, TransferFormat, TransferHistoryRecord,
    TransferOutcome,
};

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    #[default]
    Hidden,
    DropTarget,
    Share,
}
impl Visibility {
    pub fn file_drag(&mut self, active: bool) {
        match (*self, active) {
            (Self::Hidden, true) => *self = Self::DropTarget,
            (Self::DropTarget, false) => *self = Self::Hidden,
            _ => {}
        }
    }
}

#[derive(Default, Debug, Clone, PartialEq)]
pub enum Phase {
    #[default]
    Ready,
    Preparing,
    Waiting,
    Uploading {
        sent: u64,
        total: u64,
    },
    Confirming,
    Complete,
    Cancelling,
    Cancelled,
    Declined,
    Failed,
}
impl Phase {
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Self::Preparing
                | Self::Waiting
                | Self::Uploading { .. }
                | Self::Confirming
                | Self::Cancelling
        )
    }
    pub fn title(&self, language: Language) -> &'static str {
        let text = language.strings();
        match self {
            Self::Ready => text.phase_ready,
            Self::Preparing => text.phase_preparing,
            Self::Waiting => text.phase_waiting,
            Self::Uploading { .. } => text.phase_uploading,
            Self::Confirming => text.phase_confirming,
            Self::Complete => text.phase_complete,
            Self::Cancelling => text.phase_cancelling,
            Self::Cancelled => text.phase_cancelled,
            Self::Declined => text.phase_declined,
            Self::Failed => text.phase_failed,
        }
    }
}

#[derive(Default)]
pub struct Model {
    pub selection_id: u64,
    pub validating: bool,
    pub files: Vec<PathBuf>,
    pub previews: Vec<Option<std::sync::Arc<gpui::RenderImage>>>,
    pub bytes: u64,
    pub peers: Vec<NearbyReceiver>,
    pub selected: Option<String>,
    pub scan_id: u64,
    pub scanning: bool,
    pub rescan_requested: bool,
    pub send_id: u64,
    pub phase: Phase,
    pub speed_bytes_per_second: Option<u64>,
    pub send_started_at: Option<Instant>,
    pub send_started_at_unix_ms: u64,
    send_finished_at: Option<(Instant, u64)>,
    send_history_pending: bool,
    send_file_paths: Vec<PathBuf>,
    send_channel: Option<u16>,
    send_format: Option<TransferFormat>,
    pub send_file_count: usize,
    pub send_file_bytes: u64,
    pub send_transferred_bytes: u64,
    pub send_confirmed: bool,
    pub error: Option<String>,
    pub status: Option<Result<LinkStatus, String>>,
    pub target: String,
    speed: crate::transfer_speed::TransferSpeed,
}
impl Model {
    #[cfg(test)]
    pub fn apply(&mut self, event: Event) -> bool {
        self.apply_at(
            event,
            Instant::now(),
            crate::history::unix_time_ms(std::time::SystemTime::now()),
        )
    }

    pub(crate) fn apply_at(&mut self, event: Event, now: Instant, now_unix_ms: u64) -> bool {
        match event {
            Event::FilesValidated(id, paths, result) if id == self.selection_id => {
                self.validating = false;
                match result {
                    Ok(bytes) => {
                        self.previews = vec![None; paths.len().min(crate::preview::MAX_PREVIEWS)];
                        self.files = paths;
                        self.bytes = bytes;
                        self.phase = Phase::Ready;
                        self.error = None;
                    }
                    Err(error) => {
                        self.error = Some(format!(
                            "{}{error}",
                            i18n::current().strings().err_select_files
                        ))
                    }
                }
            }
            Event::FilePreview(id, index, image) if id == self.selection_id => {
                let Some(slot) = self.previews.get_mut(index) else {
                    return false;
                };
                *slot = Some(image);
            }
            Event::Status(status) => {
                if self.status.as_ref() == Some(&status) {
                    return false;
                }
                let ready = status.as_ref().is_ok_and(LinkStatus::ready);
                if self.status.is_some()
                    && self.connection_ready() != ready
                    && self.phase == Phase::Ready
                {
                    // A recreated AWDL adapter invalidates both the old scan's
                    // socket and its receiver addresses. Ignore late callbacks.
                    self.scan_id += 1;
                    self.scanning = false;
                    self.peers.clear();
                    self.selected = None;
                    self.rescan_requested = ready && !self.files.is_empty() && !self.validating;
                }
                self.status = Some(status);
            }
            Event::Discovery(id, event) if id == self.scan_id && self.scanning => match event {
                DiscoveryEvent::Found(peer) | DiscoveryEvent::Updated(peer) => {
                    // Capability/name updates must not move a row under the cursor.
                    if let Some(existing) = self
                        .peers
                        .iter_mut()
                        .find(|p| p.service.instance == peer.service.instance)
                    {
                        *existing = peer;
                    } else {
                        self.peers.push(peer);
                    }
                }
                DiscoveryEvent::Removed { instance } => {
                    self.peers.retain(|p| p.service.instance != instance);
                    if self.selected.as_ref() == Some(&instance) {
                        self.selected = None;
                    }
                }
                DiscoveryEvent::ProbeFailed { .. } => return false,
            },
            Event::ScanDone(id, result) if id == self.scan_id => {
                self.scanning = false;
                if let Err(error) = result {
                    self.error = Some(error);
                }
            }
            Event::Transfer(id, event)
                if id == self.send_id && self.phase.busy() && self.phase != Phase::Cancelling =>
            {
                self.phase = match event {
                    SendEvent::Preparing => {
                        self.reset_speed();
                        Phase::Preparing
                    }
                    SendEvent::ModeSelected(format) => {
                        self.send_format = Some(match format {
                            SendFormat::Gzip => TransferFormat::Gzip,
                            SendFormat::DvZip => TransferFormat::DvZip,
                            SendFormat::Auto => return false,
                        });
                        self.phase.clone()
                    }
                    SendEvent::WaitingForAcceptance { .. } => {
                        self.reset_speed();
                        Phase::Waiting
                    }
                    SendEvent::Uploading { sent, total } => {
                        if self.send_started_at.is_none() {
                            self.send_started_at = Some(now);
                            self.send_started_at_unix_ms = now_unix_ms;
                        }
                        self.send_transferred_bytes = sent;
                        self.speed.update(now, sent);
                        self.speed_bytes_per_second = self
                            .speed
                            .available()
                            .then(|| self.speed.bytes_per_second());
                        Phase::Uploading { sent, total }
                    }
                    SendEvent::WaitingForConfirmation => {
                        self.reset_speed();
                        Phase::Confirming
                    }
                    SendEvent::Completed(report) => {
                        self.send_file_bytes = report.source_bytes;
                        self.send_transferred_bytes = report.archive_bytes;
                        self.send_confirmed = true;
                        self.send_finished_at.get_or_insert((now, now_unix_ms));
                        self.reset_speed();
                        Phase::Complete
                    }
                };
            }
            Event::Transfer(id, SendEvent::Uploading { sent, .. })
                if id == self.send_id && self.phase == Phase::Cancelling =>
            {
                // The worker may have queued progress before the cancel click reached it.
                // Keep the accounting while leaving the cancellation UI in place.
                if self.send_started_at.is_none() {
                    self.send_started_at = Some(now);
                    self.send_started_at_unix_ms = now_unix_ms;
                }
                self.send_transferred_bytes = sent;
            }
            Event::Transfer(id, SendEvent::Completed(report))
                if id == self.send_id && self.phase == Phase::Cancelling =>
            {
                // Preserve final file and wire byte counts while the matching
                // SendDone event resolves the cancellation race.
                self.send_file_bytes = report.source_bytes;
                self.send_transferred_bytes = report.archive_bytes;
                self.send_confirmed = true;
                self.send_finished_at.get_or_insert((now, now_unix_ms));
            }
            Event::SendDone(id, result) if id == self.send_id => {
                self.send_finished_at.get_or_insert((now, now_unix_ms));
                self.reset_speed();
                if self.phase == Phase::Cancelling {
                    // A final 200 can race a user's cancel click. Preserve delivery evidence.
                    self.phase = if result.is_ok() {
                        self.send_confirmed = true;
                        Phase::Complete
                    } else {
                        Phase::Cancelled
                    };
                    self.error = None;
                } else {
                    match result {
                        Ok(()) => {
                            self.send_confirmed = true;
                            self.phase = Phase::Complete;
                            self.error = None;
                        }
                        Err(failure) => match failure {
                            SendFailure::Declined => {
                                self.phase = Phase::Declined;
                                self.error = None;
                            }
                            SendFailure::Cancelled => {
                                self.phase = Phase::Cancelled;
                                self.error = None;
                            }
                            SendFailure::Connection(error)
                            | SendFailure::Validation(error)
                            | SendFailure::Failed(error) => {
                                self.phase = Phase::Failed;
                                self.error = Some(error);
                            }
                        },
                    }
                }
            }
            _ => return false,
        }
        true
    }
    pub fn toggle_peer(&mut self, instance: &str) {
        if self.phase.busy() {
            return;
        }
        self.selected = if self.selected.as_deref() == Some(instance) {
            None
        } else {
            Some(instance.to_owned())
        };
    }
    pub fn reset_selection(&mut self) {
        if self.phase.busy() {
            return;
        }
        // Invalidate in-flight filesystem/thumbnail/discovery results so Back
        // cannot be undone by a background completion arriving later.
        self.selection_id += 1;
        self.scan_id += 1;
        self.send_id += 1;
        self.validating = false;
        self.scanning = false;
        self.rescan_requested = false;
        self.phase = Phase::Ready;
        self.files.clear();
        self.previews.clear();
        self.bytes = 0;
        self.peers.clear();
        self.selected = None;
        self.target.clear();
        self.error = None;
        self.send_started_at = None;
        self.send_finished_at = None;
        self.send_history_pending = false;
        self.send_file_paths.clear();
    }
    pub fn selected_peer(&self) -> Option<&NearbyReceiver> {
        self.peers
            .iter()
            .find(|peer| Some(&peer.service.instance) == self.selected.as_ref())
    }
    pub fn connection_ready(&self) -> bool {
        matches!(self.status, Some(Ok(ref status)) if status.ready())
    }
    pub fn can_send(&self) -> bool {
        !self.phase.busy()
            && !self.validating
            && !self.files.is_empty()
            && self.selected_peer().is_some()
            && self.connection_ready()
    }
    pub fn begin_send(&mut self) -> Option<NearbyReceiver> {
        if !self.can_send() {
            return None;
        }
        let peer = self.selected_peer()?.clone();
        let paths = match self
            .files
            .iter()
            .map(std::path::absolute)
            .collect::<std::io::Result<Vec<_>>>()
        {
            Ok(paths) => paths,
            Err(error) => {
                self.error = Some(error.to_string());
                return None;
            }
        };
        // Sending owns a snapshot of the selected receiver. The remaining
        // discovery callbacks must not clear it or replace a transfer error.
        self.scan_id += 1;
        self.scanning = false;
        self.rescan_requested = false;
        self.send_id += 1;
        self.target = peer.name.clone();
        self.phase = Phase::Preparing;
        self.error = None;
        self.send_started_at = None;
        self.send_started_at_unix_ms = crate::history::unix_time_ms(std::time::SystemTime::now());
        self.send_finished_at = None;
        self.send_history_pending = true;
        self.send_file_paths = paths;
        self.send_channel = self.current_channel();
        self.send_format = None;
        self.send_file_count = self.files.len();
        self.send_file_bytes = self.bytes;
        self.send_transferred_bytes = 0;
        self.send_confirmed = false;
        self.reset_speed();
        Some(peer)
    }

    pub fn expire_speed(&mut self, now: Instant) -> bool {
        if !matches!(self.phase, Phase::Uploading { .. }) {
            return false;
        }
        if self.speed.expire(now) {
            self.speed_bytes_per_second = Some(0);
            true
        } else {
            false
        }
    }

    pub fn take_history_record(
        &mut self,
        id: u64,
        result: &Result<(), SendFailure>,
        finished_at: Instant,
        finished_at_unix_ms: u64,
    ) -> Option<TransferHistoryRecord> {
        if id != self.send_id || !self.send_history_pending {
            return None;
        }
        self.send_history_pending = false;
        let (_, terminal_unix_ms) = *self
            .send_finished_at
            .get_or_insert((finished_at, finished_at_unix_ms));
        let (outcome, failure_reason, transferred_bytes) = if self.send_confirmed || result.is_ok()
        {
            (
                TransferOutcome::Completed,
                None,
                self.send_transferred_bytes,
            )
        } else {
            match result {
                Err(SendFailure::Declined) => {
                    (TransferOutcome::Declined, None, self.send_transferred_bytes)
                }
                Err(SendFailure::Cancelled) => (
                    TransferOutcome::Cancelled,
                    None,
                    self.send_transferred_bytes,
                ),
                Err(SendFailure::Connection(_)) => (
                    TransferOutcome::Failed,
                    Some(TransferFailureReason::Connection),
                    self.send_transferred_bytes,
                ),
                Err(SendFailure::Validation(_)) => (
                    TransferOutcome::Failed,
                    Some(TransferFailureReason::Validation),
                    self.send_transferred_bytes,
                ),
                Err(SendFailure::Failed(_)) => (
                    TransferOutcome::Failed,
                    Some(TransferFailureReason::Other),
                    self.send_transferred_bytes,
                ),
                Ok(()) => unreachable!("successful send is handled above"),
            }
        };
        Some(TransferHistoryRecord {
            started_at_unix_ms: self.send_started_at_unix_ms,
            finished_at_unix_ms: terminal_unix_ms.max(self.send_started_at_unix_ms),
            direction: TransferDirection::Send,
            peer: self.target.clone(),
            file_count: self.send_file_count,
            file_names: self
                .send_file_paths
                .iter()
                .map(|path| {
                    path.file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                })
                .collect(),
            file_paths: self.send_file_paths.clone(),
            channel: self.send_channel,
            format: self.send_format,
            file_bytes: Some(self.send_file_bytes),
            transferred_bytes,
            duration_ms: self
                .send_elapsed(finished_at)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64,
            outcome,
            failure_reason,
            destination: None,
        })
    }

    /// Preparation and waiting for acceptance do not count as data transfer time.
    pub fn send_elapsed(&self, now: Instant) -> Option<Duration> {
        self.send_started_at.map(|started| {
            self.send_finished_at
                .map(|(finished, _)| finished)
                .unwrap_or(now)
                .saturating_duration_since(started)
        })
    }

    pub fn current_channel(&self) -> Option<u16> {
        self.status
            .as_ref()
            .and_then(|status| status.as_ref().ok())
            .map(|status| status.channel)
            .filter(|channel| matches!(channel, 6 | 44 | 149))
    }

    pub fn transfer_metadata(&self) -> (Option<u16>, Option<TransferFormat>) {
        (self.send_channel, self.send_format)
    }

    fn reset_speed(&mut self) {
        self.speed = crate::transfer_speed::TransferSpeed::default();
        self.speed_bytes_per_second = None;
    }
}

pub fn file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn receiver(instance: &str) -> NearbyReceiver {
        NearbyReceiver {
            name: "iPhone".into(),
            service: wayline_core::AirDropService {
                instance: instance.into(),
                host: "phone.local".into(),
                port: 8771,
                address: "[::1]:8771".parse().unwrap(),
                flags: None,
            },
        }
    }
    fn searching_model() -> Model {
        Model {
            scan_id: 7,
            scanning: true,
            files: vec!["photo.png".into()],
            status: Some(Ok(crate::backend::LinkStatus {
                state: "running".into(),
                port: None,
                channel: 6,
                target: Some("esp32c5".into()),
                awdl0: true,
                error: None,
            })),
            ..Default::default()
        }
    }
    fn preview_image() -> std::sync::Arc<gpui::RenderImage> {
        std::sync::Arc::new(gpui::RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::new(1, 1),
        )]))
    }
    #[test]
    fn reconnect_restarts_discovery_and_rejects_results_from_the_old_adapter() {
        let mut model = Model {
            peers: vec![receiver("phone")],
            selected: Some("phone".into()),
            ..searching_model()
        };
        model.apply(Event::Status(Err("service stopped".into())));
        assert!(!model.scanning && !model.can_send());
        assert!(model.peers.is_empty() && model.selected.is_none());
        assert!(!model.rescan_requested);
        assert!(!model.apply(Event::Discovery(7, DiscoveryEvent::Found(receiver("old")))));
        assert!(!model.apply(Event::ScanDone(7, Err("old adapter closed".into()))));

        // A manual retry during startup must also be invalidated on recovery.
        model.scanning = true;
        let retry_id = model.scan_id;
        let ready = searching_model().status.unwrap();
        model.apply(Event::Status(ready.clone()));
        assert!(model.rescan_requested && !model.scanning);
        assert!(!model.apply(Event::ScanDone(retry_id, Err("not ready yet".into()))));
        assert!(std::mem::take(&mut model.rescan_requested));
        assert!(!model.apply(Event::Status(ready)));
        assert!(!model.rescan_requested);
        assert!(model.error.is_none());
        assert_eq!(model.files, vec![PathBuf::from("photo.png")]);
    }
    #[test]
    fn reconnect_does_not_interrupt_a_transfer_or_search_without_valid_files() {
        for phase in [
            Phase::Preparing,
            Phase::Uploading { sent: 1, total: 2 },
            Phase::Declined,
        ] {
            let mut model = Model {
                phase,
                ..searching_model()
            };
            model.apply(Event::Status(Err("offline".into())));
            model.apply(Event::Status(searching_model().status.unwrap()));
            assert!(!model.rescan_requested);
            assert_eq!(model.scan_id, 7);
        }
        for validating in [false, true] {
            let mut model = Model {
                validating,
                ..searching_model()
            };
            if !validating {
                model.reset_selection();
            }
            model.apply(Event::Status(Err("offline".into())));
            model.apply(Event::Status(searching_model().status.unwrap()));
            assert!(!model.rescan_requested);
        }
        // The first healthy status must not duplicate a scan started by file validation.
        let mut model = Model {
            status: None,
            ..searching_model()
        };
        model.apply(Event::Status(searching_model().status.unwrap()));
        assert!(model.scanning && !model.rescan_requested);
        assert_eq!(model.scan_id, 7);
    }
    #[test]
    fn clicking_selected_device_again_disables_send_and_another_device_switches_selection() {
        let mut model = Model {
            peers: vec![receiver("first"), receiver("second")],
            ..searching_model()
        };
        model.toggle_peer("first");
        assert!(model.can_send());
        model.toggle_peer("first");
        assert!(model.selected.is_none());
        assert!(!model.can_send());
        model.toggle_peer("first");
        model.toggle_peer("second");
        assert_eq!(model.selected_peer().unwrap().service.instance, "second");
        model.phase = Phase::Preparing;
        model.toggle_peer("second");
        assert_eq!(model.selected.as_deref(), Some("second"));
    }
    #[test]
    fn selected_receiver_sends_before_scan_finishes_and_late_discovery_cannot_affect_transfer() {
        let mut model = searching_model();
        assert!(!model.can_send());
        assert!(model.apply(Event::Discovery(
            7,
            DiscoveryEvent::Found(receiver("phone"))
        )));
        model.toggle_peer("phone");
        assert!(model.scanning && model.can_send());
        let peer = model.begin_send().unwrap();
        assert_eq!(peer.service.instance, "phone");
        assert_eq!(model.target, "iPhone");
        assert_eq!(model.phase, Phase::Preparing);
        assert!(!model.scanning);
        assert!(model.begin_send().is_none());
        assert_eq!(model.send_id, 1);
        assert!(!model.apply(Event::Discovery(
            7,
            DiscoveryEvent::Removed {
                instance: "phone".into()
            }
        )));
        assert!(!model.apply(Event::Discovery(
            7,
            DiscoveryEvent::Found(receiver("another"))
        )));
        assert!(!model.apply(Event::ScanDone(7, Err("late scan failure".into()))));
        assert_eq!(model.selected.as_deref(), Some("phone"));
        assert_eq!(model.peers.len(), 1);
        assert!(model.error.is_none());
        model.apply(Event::SendDone(
            1,
            Err(SendFailure::Failed("transfer error".into())),
        ));
        assert_eq!(model.phase, Phase::Failed);
        assert!(!model.apply(Event::ScanDone(7, Ok(()))));
        assert_eq!(model.error.as_deref(), Some("transfer error"));
    }

    #[test]
    fn history_ignores_stale_send_done_and_delivery_evidence_wins_cancel() {
        let started = Instant::now() - std::time::Duration::from_secs(2);
        let mut model = Model {
            send_id: 5,
            send_started_at: Some(started),
            send_history_pending: true,
            send_started_at_unix_ms: 100,
            send_file_count: 2,
            send_file_bytes: 4096,
            send_transferred_bytes: 2048,
            target: "iPhone".into(),
            ..Default::default()
        };
        let finished = Instant::now();
        assert!(
            model
                .take_history_record(4, &Err(SendFailure::Cancelled), finished, 200,)
                .is_none()
        );
        model.send_confirmed = true;
        let record = model
            .take_history_record(5, &Err(SendFailure::Cancelled), finished, 200)
            .unwrap();
        assert_eq!(record.outcome, TransferOutcome::Completed);
        assert_eq!(record.transferred_bytes, 2048);
        assert_eq!(record.file_bytes, Some(4096));
        assert!(record.duration_ms >= 1000);
    }

    #[test]
    fn send_elapsed_starts_with_upload_and_freezes_after_confirmation() {
        let mut model = Model {
            peers: vec![receiver("phone")],
            selected: Some("phone".into()),
            ..searching_model()
        };
        model.begin_send().unwrap();
        let now = Instant::now();
        assert_eq!(model.send_elapsed(now + Duration::from_secs(30)), None);
        assert!(!model.apply_at(
            Event::Transfer(
                0,
                SendEvent::Uploading {
                    sent: 0,
                    total: 100
                }
            ),
            now,
            1000,
        ));
        assert_eq!(model.send_elapsed(now), None);
        model.apply_at(
            Event::Transfer(
                1,
                SendEvent::WaitingForAcceptance {
                    receiver: "iPhone".into(),
                },
            ),
            now,
            1000,
        );
        model.apply_at(
            Event::Transfer(
                1,
                SendEvent::Uploading {
                    sent: 0,
                    total: 100,
                },
            ),
            now + Duration::from_secs(30),
            31_000,
        );
        model.apply_at(
            Event::Transfer(
                1,
                SendEvent::Uploading {
                    sent: 100,
                    total: 100,
                },
            ),
            now + Duration::from_secs(32),
            33_000,
        );
        model.apply_at(
            Event::Transfer(1, SendEvent::WaitingForConfirmation),
            now + Duration::from_secs(32),
            33_000,
        );
        assert_eq!(
            model.send_elapsed(now + Duration::from_secs(34)),
            Some(Duration::from_secs(4))
        );
        model.apply_at(
            Event::Transfer(
                1,
                SendEvent::Completed(wayline_core::SendReport {
                    receiver: "iPhone".into(),
                    files: 1,
                    source_bytes: 100,
                    archive_bytes: 100,
                    format: wayline_core::SendFormat::Auto,
                }),
            ),
            now + Duration::from_secs(35),
            36_000,
        );
        model.files.clear();
        let record = model
            .take_history_record(1, &Ok(()), now + Duration::from_secs(45), 46_000)
            .unwrap();
        assert_eq!(record.started_at_unix_ms, 31_000);
        assert_eq!(record.duration_ms, 5000);
        assert_eq!(record.file_names, vec!["photo.png"]);
        assert!(record.file_paths[0].is_absolute());
        assert_eq!(
            model.send_elapsed(now + Duration::from_secs(60)),
            Some(Duration::from_secs(5))
        );
        assert!(model.take_history_record(1, &Ok(()), now, 46_000).is_none());
    }

    #[test]
    fn send_declined_before_upload_has_no_transfer_duration() {
        let mut model = Model {
            peers: vec![receiver("phone")],
            selected: Some("phone".into()),
            ..searching_model()
        };
        model.begin_send().unwrap();
        let record = model
            .take_history_record(
                1,
                &Err(SendFailure::Declined),
                Instant::now() + Duration::from_secs(60),
                u64::MAX,
            )
            .unwrap();
        assert_eq!(record.duration_ms, 0);
        assert_eq!(model.send_elapsed(Instant::now()), None);
    }

    #[test]
    fn cancelling_send_accounts_for_queued_progress_without_restoring_upload_ui() {
        let mut model = Model {
            peers: vec![receiver("phone")],
            selected: Some("phone".into()),
            ..searching_model()
        };
        model.begin_send().unwrap();
        model.phase = Phase::Cancelling;
        let started = Instant::now();
        for (seconds, sent) in [(0, 0), (2, 512)] {
            assert!(model.apply_at(
                Event::Transfer(1, SendEvent::Uploading { sent, total: 1024 }),
                started + Duration::from_secs(seconds),
                1000 + seconds * 1000,
            ));
            assert_eq!(model.phase, Phase::Cancelling);
            assert_eq!(model.speed_bytes_per_second, None);
        }
        let record = model
            .take_history_record(
                1,
                &Err(SendFailure::Cancelled),
                started + Duration::from_secs(3),
                4000,
            )
            .unwrap();
        assert_eq!(record.started_at_unix_ms, 1000);
        assert_eq!(record.duration_ms, 3000);
        assert_eq!(record.transferred_bytes, 512);
        assert_eq!(record.outcome, TransferOutcome::Cancelled);
    }

    #[test]
    fn send_completion_freezes_both_clocks_before_worker_cleanup() {
        let started = Instant::now();
        let mut model = Model {
            send_id: 1,
            phase: Phase::Cancelling,
            send_started_at: Some(started),
            send_started_at_unix_ms: 1000,
            send_history_pending: true,
            ..Default::default()
        };
        model.apply_at(
            Event::Transfer(
                1,
                SendEvent::Completed(wayline_core::SendReport {
                    receiver: "iPhone".into(),
                    files: 1,
                    source_bytes: 100,
                    archive_bytes: 100,
                    format: wayline_core::SendFormat::Auto,
                }),
            ),
            started + Duration::from_secs(2),
            3000,
        );
        let record = model
            .take_history_record(1, &Ok(()), started + Duration::from_secs(10), 11_000)
            .unwrap();
        model.apply_at(
            Event::SendDone(1, Ok(())),
            started + Duration::from_secs(10),
            11_000,
        );
        assert_eq!(record.duration_ms, 2000);
        assert_eq!(record.finished_at_unix_ms, 3000);
        assert_eq!(
            record.finished_at_unix_ms - record.started_at_unix_ms,
            record.duration_ms
        );
        assert_eq!(
            model.send_elapsed(started + Duration::from_secs(20)),
            Some(Duration::from_secs(2))
        );
    }

    #[test]
    fn batched_events_keep_worker_duration_and_average_speed_for_short_transfers() {
        let mut model = Model {
            peers: vec![receiver("phone")],
            selected: Some("phone".into()),
            ..searching_model()
        };
        model.begin_send().unwrap();
        let started = Instant::now();
        let events = [
            Event::Timestamped(
                Box::new(Event::Transfer(
                    1,
                    SendEvent::Uploading {
                        sent: 0,
                        total: 1024,
                    },
                )),
                started,
                1000,
            ),
            Event::Timestamped(
                Box::new(Event::Transfer(
                    1,
                    SendEvent::Completed(wayline_core::SendReport {
                        receiver: "iPhone".into(),
                        files: 1,
                        source_bytes: 1024,
                        archive_bytes: 1024,
                        format: wayline_core::SendFormat::Auto,
                    }),
                )),
                started + Duration::from_millis(50),
                1050,
            ),
        ];
        // Consume both events in one UI poll, without sleeping between them.
        for event in events {
            let (event, at, unix_ms) = event.with_timestamp();
            model.apply_at(event, at, unix_ms);
        }
        let record = model
            .take_history_record(1, &Ok(()), started + Duration::from_secs(1), 2000)
            .unwrap();
        assert_eq!(record.duration_ms, 50);
        assert_eq!(record.finished_at_unix_ms, 1050);
        assert_eq!(
            crate::history::average_bytes_per_second(&record),
            Some(20_480)
        );
    }

    #[test]
    fn send_snapshot_resolves_relative_parent_components_without_filesystem_access() {
        let mut model = Model {
            files: vec![PathBuf::from(r"missing-folder\..\photo.png")],
            peers: vec![receiver("phone")],
            selected: Some("phone".into()),
            ..searching_model()
        };
        model.begin_send().unwrap();
        let record = model
            .take_history_record(1, &Err(SendFailure::Cancelled), Instant::now(), u64::MAX)
            .unwrap();
        assert_eq!(
            record.file_paths,
            vec![std::path::absolute("photo.png").unwrap()]
        );
        assert!(record.file_paths[0].components().all(|component| !matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )));
    }

    #[test]
    fn live_discovery_keeps_row_order_and_selection_until_selected_device_is_removed() {
        let mut model = searching_model();
        model.apply(Event::Discovery(
            7,
            DiscoveryEvent::Found(receiver("first")),
        ));
        model.apply(Event::Discovery(
            7,
            DiscoveryEvent::Found(receiver("second")),
        ));
        model.toggle_peer("first");
        let mut updated = receiver("first");
        updated.service.flags = Some(0x1b3fb);
        model.apply(Event::Discovery(
            7,
            DiscoveryEvent::Updated(updated.clone()),
        ));
        model.apply(Event::Discovery(7, DiscoveryEvent::Found(updated)));
        assert_eq!(
            model
                .peers
                .iter()
                .map(|p| p.service.instance.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(model.selected_peer().unwrap().service.flags, Some(0x1b3fb));
        assert!(model.can_send());
        model.apply(Event::Discovery(
            7,
            DiscoveryEvent::Removed {
                instance: "first".into(),
            },
        ));
        assert!(model.selected.is_none());
        assert!(!model.can_send());
    }
    #[test]
    fn sending_during_search_still_requires_valid_files_and_a_ready_connection() {
        let mut model = searching_model();
        model.apply(Event::Discovery(
            7,
            DiscoveryEvent::Found(receiver("phone")),
        ));
        model.toggle_peer("phone");
        model.validating = true;
        assert!(model.begin_send().is_none());
        model.validating = false;
        model.status = Some(Err("disconnected".into()));
        assert!(model.begin_send().is_none());
        model.status = searching_model().status;
        model.files.clear();
        assert!(model.begin_send().is_none());
        assert_eq!(model.phase, Phase::Ready);
        assert_eq!(model.scan_id, 7);
        assert!(model.scanning);
    }
    #[test]
    fn back_clears_selection_and_rejects_late_validation_preview_and_scan_results() {
        let mut model = Model {
            selection_id: 5,
            scan_id: 7,
            validating: true,
            scanning: true,
            files: vec!["old.png".into()],
            bytes: 42,
            selected: Some("phone".into()),
            previews: vec![Some(preview_image())],
            ..Default::default()
        };
        model.reset_selection();
        assert!(model.files.is_empty() && model.previews.is_empty());
        assert_eq!(model.bytes, 0);
        assert!(model.selected.is_none());
        assert!(!model.scanning && !model.validating);
        assert!(!model.apply(Event::FilesValidated(5, vec!["late.png".into()], Ok(10))));
        assert!(!model.apply(Event::FilePreview(5, 0, preview_image())));
        assert!(!model.apply(Event::ScanDone(7, Err("stale".into()))));
        assert!(model.error.is_none());
    }
    #[test]
    fn thumbnail_from_a_replaced_selection_cannot_overwrite_the_current_file() {
        let mut model = Model {
            selection_id: 2,
            ..Default::default()
        };
        model.apply(Event::FilesValidated(2, vec!["new.png".into()], Ok(42)));
        assert!(!model.apply(Event::FilePreview(1, 0, preview_image())));
        assert!(model.previews[0].is_none());
        assert!(!model.apply(Event::FilePreview(2, 8, preview_image())));
        assert!(model.apply(Event::FilePreview(2, 0, preview_image())));
        assert!(model.previews[0].is_some());
    }
    #[test]
    fn capsule_is_visible_only_during_drag_and_does_not_close_sharing() {
        let mut visibility = Visibility::default();
        assert_eq!(visibility, Visibility::Hidden);
        visibility.file_drag(true);
        assert_eq!(visibility, Visibility::DropTarget);
        visibility.file_drag(false);
        assert_eq!(visibility, Visibility::Hidden);
        visibility = Visibility::Share;
        visibility.file_drag(false);
        assert_eq!(visibility, Visibility::Share);
        visibility = Visibility::Hidden; // Close/Escape must not leave a pill.
        visibility.file_drag(false);
        assert_eq!(visibility, Visibility::Hidden);
    }
    #[test]
    fn latest_selection_wins_when_background_validation_finishes_out_of_order() {
        let mut model = Model {
            selection_id: 2,
            validating: true,
            ..Default::default()
        };
        assert!(!model.apply(Event::FilesValidated(1, vec!["old".into()], Ok(1))));
        assert!(model.validating);
        assert!(model.apply(Event::FilesValidated(2, vec!["new".into()], Ok(42))));
        assert_eq!(model.files, vec![PathBuf::from("new")]);
        assert_eq!(model.bytes, 42);
        assert!(!model.validating);
        assert!(!model.apply(Event::FilesValidated(1, vec![], Err("old error".into()))));
        assert!(model.error.is_none());
    }
    #[test]
    fn full_progress_still_needs_remote_confirmation() {
        let mut model = Model {
            send_id: 2,
            phase: Phase::Preparing,
            ..Default::default()
        };
        model.apply(Event::Transfer(
            2,
            SendEvent::Uploading {
                sent: 100,
                total: 100,
            },
        ));
        assert!(model.phase.busy());
        assert_ne!(model.phase, Phase::Complete);
        model.apply(Event::Transfer(2, SendEvent::WaitingForConfirmation));
        assert_eq!(model.phase, Phase::Confirming);
    }
    #[test]
    fn late_events_cannot_revive_an_old_transfer_or_cancelled_scan() {
        let mut model = Model {
            send_id: 2,
            scan_id: 3,
            phase: Phase::Cancelled,
            ..Default::default()
        };
        assert!(!model.apply(Event::Transfer(1, SendEvent::Preparing)));
        assert!(!model.apply(Event::ScanDone(2, Err("old failure".into()))));
        assert!(!model.apply(Event::Transfer(2, SendEvent::Preparing)));
        assert_eq!(model.phase, Phase::Cancelled);
        assert!(model.error.is_none());
        assert!(!model.can_send());
    }
    #[test]
    fn acknowledged_delivery_wins_a_cancel_race() {
        let mut model = Model {
            send_id: 1,
            phase: Phase::Cancelling,
            ..Default::default()
        };
        model.apply(Event::SendDone(1, Ok(())));
        assert_eq!(model.phase, Phase::Complete);
    }
    #[test]
    fn receiver_decline_has_a_clear_status_and_keeps_the_selection_for_retry() {
        let mut model = Model {
            peers: vec![receiver("phone")],
            selected: Some("phone".into()),
            ..searching_model()
        };
        model.begin_send().unwrap();
        model.apply(Event::Transfer(
            1,
            SendEvent::WaitingForAcceptance {
                receiver: "iPhone".into(),
            },
        ));
        model.error = Some("earlier diagnostic".into());
        assert!(model.apply(Event::SendDone(1, Err(SendFailure::Declined))));
        assert_eq!(model.phase, Phase::Declined);
        assert_eq!(model.phase.title(Language::ZhCn), "对方已拒绝");
        assert!(!model.phase.busy());
        assert!(model.error.is_none());
        assert_eq!(model.files, vec![PathBuf::from("photo.png")]);
        assert_eq!(model.selected.as_deref(), Some("phone"));
        assert_eq!(model.target, "iPhone");
        assert!(!model.apply(Event::Transfer(1, SendEvent::Preparing)));
        model.phase = Phase::Ready; // Return to the existing selection.
        assert!(model.begin_send().is_some());
        assert!(!model.apply(Event::SendDone(1, Err(SendFailure::Declined))));
        assert_eq!(model.phase, Phase::Preparing);
    }
    #[test]
    fn local_cancellation_remains_cancelled_when_a_remote_decline_arrives() {
        let mut model = Model {
            send_id: 1,
            phase: Phase::Cancelling,
            ..Default::default()
        };
        model.apply(Event::SendDone(1, Err(SendFailure::Declined)));
        assert_eq!(model.phase, Phase::Cancelled);
        assert!(model.error.is_none());
    }
}
