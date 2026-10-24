//! Presentation state and background coordination; flashing belongs to wayline-flash.
use std::{sync::mpsc::Sender, thread::JoinHandle};
use wayline_flash::{
    Cancellation, Device, FlashEvent, FlashOptions, FlashReport, FlashStage, InstallMode,
};

pub enum Event {
    Inventory(Result<(Vec<Device>, Vec<wayline_flash::Manifest>), String>),
    Progress(FlashEvent),
    Done(Result<FlashReport, wayline_flash::FlashError>),
}

pub struct Model {
    pub open: bool,
    pub loading: bool,
    pub devices: Vec<Device>,
    pub packages: Vec<wayline_flash::Manifest>,
    pub selected: Option<String>,
    pub board_confirmed: bool,
    pub mode: InstallMode,
    pub version: Option<String>,
    pub package: Option<wayline_flash::Manifest>,
    pub busy: bool,
    pub cancelling: bool,
    pub confirm_exit: bool,
    pub stage: Option<FlashStage>,
    pub progress: Option<u8>,
    pub message: String,
    pub error: Option<String>,
    pub ready: bool,
}
impl Default for Model {
    fn default() -> Self {
        Self {
            open: false,
            loading: false,
            devices: Vec::new(),
            packages: Vec::new(),
            selected: None,
            board_confirmed: false,
            mode: InstallMode::Reinstall,
            version: None,
            package: None,
            busy: false,
            cancelling: false,
            confirm_exit: false,
            stage: None,
            progress: None,
            message: crate::i18n::current().strings().firmware_idle.into(),
            error: None,
            ready: false,
        }
    }
}
impl Model {
    pub fn select_profile(&mut self, board: &str) -> bool {
        if self.busy || self.loading {
            return false;
        }
        let Some(package) = self
            .packages
            .iter()
            .find(|package| package.board == board)
            .cloned()
        else {
            return false;
        };
        if self.package.as_ref().is_some_and(|current| {
            current.board == package.board && current.firmware_version == package.firmware_version
        }) {
            return false;
        }
        self.version = Some(package.firmware_version.clone());
        self.package = Some(package);
        self.board_confirmed = false;
        self.ready = false;
        self.error = None;
        self.mode = InstallMode::Reinstall;
        true
    }

    pub fn request_exit(&mut self, confirmed: bool) -> bool {
        if self.busy && !confirmed {
            self.confirm_exit = true;
            return false;
        }
        true
    }

    pub fn stage_label(&self) -> &'static str {
        let text = crate::i18n::current().strings();
        match self.stage {
            Some(FlashStage::ValidatePackage) => text.firmware_stage_validate,
            Some(FlashStage::AcquireMaintenance) => text.firmware_stage_maintenance,
            Some(FlashStage::Connect) => text.firmware_stage_connect,
            Some(FlashStage::Write) => text.firmware_stage_write,
            Some(FlashStage::Verify) => text.firmware_stage_verify,
            Some(FlashStage::CheckBoot) => text.firmware_stage_boot,
            Some(FlashStage::Restore) => text.firmware_stage_restore,
            None => text.firmware_stage_prepare,
        }
    }

    pub fn can_start(&self, transfer_active: bool) -> bool {
        !self.busy
            && !self.loading
            && !transfer_active
            && self.board_confirmed
            && self.version.is_some()
            && self.package.is_some()
            && self.profile_matches_selected_port(
                self.package.as_ref().map(|package| package.board.as_str()),
            )
    }
    pub fn profile_matches_selected_port(&self, board: Option<&str>) -> bool {
        let (Some(board), Some(selected)) = (board, self.selected.as_ref()) else {
            return false;
        };
        self.devices
            .iter()
            .find(|device| &device.port == selected)
            .is_some_and(|device| {
                wayline_flash::profile_accepts_usb(board, device.usb_vid, device.usb_pid)
            })
    }
    pub fn apply(&mut self, event: Event) -> bool {
        match event {
            Event::Inventory(result) => {
                self.loading = false;
                match result {
                    Ok((devices, packages)) => {
                        let previous_board =
                            self.package.as_ref().map(|package| package.board.as_str());
                        let package = packages
                            .iter()
                            .find(|package| Some(package.board.as_str()) == previous_board)
                            .or_else(|| {
                                packages
                                    .iter()
                                    .find(|package| package.board == wayline_flash::SUPPORTED_BOARD)
                            })
                            .or_else(|| packages.first())
                            .cloned();
                        if self.package.as_ref().is_none_or(|previous| {
                            package.as_ref().is_none_or(|package| {
                                previous.board != package.board
                                    || previous.chip != package.chip
                                    || previous.flash_size != package.flash_size
                                    || previous.layout_id != package.layout_id
                                    || previous.firmware_version != package.firmware_version
                            })
                        }) {
                            self.board_confirmed = false;
                            self.ready = false;
                        }
                        if !devices
                            .iter()
                            .any(|d| Some(&d.port) == self.selected.as_ref())
                        {
                            self.selected = None;
                            self.board_confirmed = false;
                            self.ready = false;
                        }
                        self.devices = devices;
                        self.packages = packages;
                        self.version = package
                            .as_ref()
                            .map(|package| package.firmware_version.clone());
                        self.package = package;
                        self.error = None;
                    }
                    Err(error) => {
                        self.version = None;
                        self.package = None;
                        self.packages.clear();
                        self.board_confirmed = false;
                        self.ready = false;
                        self.error = Some(format!(
                            "{}{error}",
                            crate::i18n::current().strings().firmware_inventory_error
                        ));
                    }
                }
                false
            }
            Event::Progress(event) => {
                self.stage = Some(event.stage);
                self.progress = event.progress;
                // Flash-stage wording is part of the desktop UI. Detailed flash
                // diagnostics remain in error messages and logs.
                self.message = flash_stage_hint(event.stage).into();
                false
            }
            Event::Done(result) => {
                self.busy = false;
                self.cancelling = false;
                self.confirm_exit = false;
                self.progress = None;
                match result {
                    Ok(report) => {
                        let text = crate::i18n::current().strings();
                        self.ready = report.images_written && report.device_ready;
                        self.message = if self.ready {
                            text.firmware_done_ready
                        } else {
                            text.firmware_done_not_ready
                        }
                        .into();
                        self.error = None;
                    }
                    Err(error) => {
                        let text = crate::i18n::current().strings();
                        self.stage = Some(error.stage);
                        self.ready = false;
                        let hint = if error.stage == FlashStage::CheckBoot {
                            text.firmware_boot_hint
                        } else if error.stage == FlashStage::Restore {
                            text.firmware_restore_hint
                        } else if error.possibly_written {
                            text.firmware_partial_hint
                        } else {
                            ""
                        };
                        self.error = Some(format!(
                            "{}{}{hint}",
                            text.firmware_error_prefix, error.message
                        ));
                        self.message = if error.cancelled {
                            text.firmware_cancelled
                        } else {
                            text.firmware_incomplete
                        }
                        .into();
                    }
                }
                true
            }
        }
    }
}

fn flash_stage_hint(stage: FlashStage) -> &'static str {
    let text = crate::i18n::current().strings();
    match stage {
        FlashStage::ValidatePackage => text.firmware_hint_validate,
        FlashStage::AcquireMaintenance => text.firmware_hint_maintenance,
        FlashStage::Connect => text.firmware_hint_connect,
        FlashStage::Write => text.firmware_hint_write,
        FlashStage::Verify => text.firmware_hint_verify,
        FlashStage::CheckBoot => text.firmware_hint_boot,
        FlashStage::Restore => text.firmware_hint_restore,
    }
}

pub fn refresh(events: Sender<crate::backend::Event>) {
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<_> {
            let packages = wayline_flash::installed_manifests()?;
            Ok((wayline_flash::enumerate_devices()?, packages))
        })()
        .map_err(|error| format!("{error:#}"));
        let _ = events.send(crate::backend::Event::Firmware(Event::Inventory(result)));
    });
}

pub fn start(
    options: FlashOptions,
    cancel: Cancellation,
    receiver: Option<JoinHandle<()>>,
    scans: Vec<JoinHandle<()>>,
    events: Sender<crate::backend::Event>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        if let Some(receiver) = receiver {
            let _ = receiver.join();
        }
        for scan in scans {
            let _ = scan.join();
        }
        let result = wayline_flash::flash(&options, &cancel, |progress| {
            let _ = events.send(crate::backend::Event::Firmware(Event::Progress(progress)));
        });
        let _ = events.send(crate::backend::Event::Firmware(Event::Done(result)));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package() -> wayline_flash::Manifest {
        wayline_flash::Manifest {
            format_version: 1,
            board: "cardputer-adv".into(),
            chip: "esp32s3".into(),
            flash_size: 8 * 1024 * 1024,
            firmware_version: "test".into(),
            source_revision: "test".into(),
            protocol_version: 2,
            idf_version: "6.0".into(),
            layout_id: "test".into(),
            tool_version: "5.0.2".into(),
            images: vec![],
        }
    }
    #[test]
    fn no_install_without_explicit_device_board_and_idle_transfers() {
        let mut model = Model {
            version: Some("test".into()),
            package: Some(package()),
            ..Default::default()
        };
        assert!(!model.can_start(false));
        model.board_confirmed = true;
        assert!(!model.can_start(false));
        // A selected port removed from the device inventory must not be flashed.
        model.selected = Some("COM9".into());
        assert!(!model.can_start(false));
        model.devices.push(Device {
            port: "COM9".into(),
            label: "Cardputer candidate".into(),
            usb_vid: 0x303a,
            usb_pid: 0x1001,
        });
        assert!(model.can_start(false));
        assert!(!model.can_start(true));
    }
    #[test]
    fn selecting_a_c5_profile_clears_confirmation_and_checks_usb_link_mode() {
        let mut c5_uart = package();
        c5_uart.board = wayline_flash::C5_UART_BOARD.into();
        c5_uart.chip = "esp32c5".into();
        c5_uart.flash_size = wayline_flash::C5_FLASH_SIZE;
        let mut c5_usb = c5_uart.clone();
        c5_usb.board = wayline_flash::C5_USB_BOARD.into();
        let mut model = Model::default();
        model.apply(Event::Inventory(Ok((
            vec![Device {
                port: "COM8".into(),
                label: "ESP USB Serial/JTAG".into(),
                usb_vid: 0x303a,
                usb_pid: 0x1001,
            }],
            vec![package(), c5_uart, c5_usb],
        ))));
        model.selected = Some("COM8".into());
        model.board_confirmed = true;
        assert!(model.select_profile(wayline_flash::C5_USB_BOARD));
        assert!(!model.board_confirmed);
        assert!(!model.can_start(false)); // Confirmation is required again.
        model.board_confirmed = true;
        assert!(model.can_start(false));
        assert!(model.select_profile(wayline_flash::C5_UART_BOARD));
        model.board_confirmed = true;
        assert!(!model.can_start(false)); // Native USB port cannot use the UART0 profile.
    }
    #[test]
    fn headless_s3_profiles_are_available_for_uart_and_native_usb() {
        assert!(wayline_flash::profile_for_board(wayline_flash::S3_UART_BOARD).is_some());
        assert!(wayline_flash::profile_for_board(wayline_flash::S3_USB_BOARD).is_some());
        assert!(wayline_flash::profile_accepts_usb(
            wayline_flash::S3_UART_BOARD,
            0x1a86,
            0x7523
        ));
        assert!(wayline_flash::profile_accepts_usb(
            wayline_flash::S3_USB_BOARD,
            0x303a,
            0x1001
        ));
    }
    #[test]
    fn repeated_quit_requests_require_explicit_firmware_confirmation() {
        let mut model = Model {
            busy: true,
            ..Default::default()
        };
        assert!(!model.request_exit(false));
        assert!(model.confirm_exit);
        assert!(!model.request_exit(false));
        assert!(model.request_exit(true));
    }

    #[test]
    fn progress_keeps_hidden_operation_busy_until_verified_completion() {
        let mut model = Model {
            busy: true,
            ..Default::default()
        };
        for stage in [
            FlashStage::ValidatePackage,
            FlashStage::AcquireMaintenance,
            FlashStage::Connect,
            FlashStage::Write,
            FlashStage::Verify,
            FlashStage::CheckBoot,
            FlashStage::Restore,
        ] {
            assert!(!model.apply(Event::Progress(FlashEvent {
                stage,
                progress: Some(100),
                message: "progress".into(),
            })));
            assert!(!model.ready);
            assert!(!model.open);
            assert_eq!(model.stage, Some(stage));
        }
        assert!(model.busy);
        assert!(!model.can_start(false));
        assert!(model.apply(Event::Done(Ok(FlashReport {
            port: "COM9".into(),
            firmware_version: "test".into(),
            images_written: true,
            device_ready: true,
        }))));
        assert!(model.ready);
        assert!(!model.busy);
        assert!(model.progress.is_none());
    }

    #[test]
    fn boot_failure_is_distinct_from_partial_write_and_allows_retry() {
        let mut model = Model {
            busy: true,
            cancelling: true,
            confirm_exit: true,
            ..Default::default()
        };
        assert!(model.apply(Event::Done(Err(wayline_flash::FlashError {
            stage: FlashStage::CheckBoot,
            possibly_written: true,
            retryable: true,
            cancelled: false,
            busy: false,
            message: "握手超时".into(),
        }))));
        assert!(!model.ready && !model.busy && !model.cancelling && !model.confirm_exit);
        assert!(model.error.as_ref().unwrap().contains("通过校验"));
        assert!(!model.error.as_ref().unwrap().contains("部分内容"));
    }

    #[test]
    fn removed_device_requires_board_confirmation_again() {
        let mut model = Model {
            selected: Some("COM9".into()),
            board_confirmed: true,
            ..Default::default()
        };
        model.apply(Event::Inventory(Ok((vec![], vec![package()]))));
        assert!(model.selected.is_none());
        assert!(!model.board_confirmed);
    }

    #[test]
    fn changed_package_and_inventory_failure_clear_confirmation() {
        let mut model = Model {
            selected: Some("COM9".into()),
            board_confirmed: true,
            ready: true,
            package: Some(package()),
            ..Default::default()
        };
        let mut updated = package();
        updated.board = "esp32c5".into();
        updated.chip = "esp32c5".into();
        model.apply(Event::Inventory(Ok((
            vec![Device {
                port: "COM9".into(),
                label: "ESP32".into(),
                usb_vid: 0x303a,
                usb_pid: 0x1001,
            }],
            vec![updated],
        ))));
        assert!(!model.board_confirmed);
        assert!(!model.ready);
        assert_eq!(model.package.as_ref().unwrap().chip, "esp32c5");
        model.board_confirmed = true;
        model.apply(Event::Inventory(Err("missing package".into())));
        assert!(model.package.is_none());
        assert!(!model.board_confirmed);
        assert!(!model.can_start(false));
    }
}
