use crate::{
    backend::{Event, LinkStatus, SendFailure},
    i18n::{self, Language},
};
use omdrop_core::{DiscoveryEvent, NearbyReceiver, SendEvent};
use std::path::PathBuf;

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

#[derive(Default, Debug, PartialEq)]
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
    pub error: Option<String>,
    pub status: Option<Result<LinkStatus, String>>,
    pub target: String,
}
impl Model {
    pub fn apply(&mut self, event: Event) -> bool {
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
                    SendEvent::Preparing => Phase::Preparing,
                    SendEvent::WaitingForAcceptance { .. } => Phase::Waiting,
                    SendEvent::Uploading { sent, total } => Phase::Uploading { sent, total },
                    SendEvent::WaitingForConfirmation => Phase::Confirming,
                    SendEvent::Completed(_) => Phase::Complete,
                };
            }
            Event::SendDone(id, result) if id == self.send_id => {
                if self.phase == Phase::Cancelling {
                    // A final 200 can race a user's cancel click. Preserve delivery evidence.
                    self.phase = if result.is_ok() {
                        Phase::Complete
                    } else {
                        Phase::Cancelled
                    };
                    self.error = None;
                } else if let Err(failure) = result {
                    match failure {
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
        // Sending owns a snapshot of the selected receiver. The remaining
        // discovery callbacks must not clear it or replace a transfer error.
        self.scan_id += 1;
        self.scanning = false;
        self.rescan_requested = false;
        self.send_id += 1;
        self.target = peer.name.clone();
        self.phase = Phase::Preparing;
        self.error = None;
        Some(peer)
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
            service: omdrop_core::AirDropService {
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
