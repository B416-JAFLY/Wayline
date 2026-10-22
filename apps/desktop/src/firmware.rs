//! Presentation state and background coordination; flashing belongs to omdrop-flash.
use omdrop_flash::{
    Cancellation, Device, FlashEvent, FlashOptions, FlashReport, FlashStage, InstallMode,
};
use std::{sync::mpsc::Sender, thread::JoinHandle};

pub enum Event {
    Inventory(Result<(Vec<Device>, omdrop_flash::Manifest), String>),
    Progress(FlashEvent),
    Done(Result<FlashReport, omdrop_flash::FlashError>),
}

pub struct Model {
    pub open: bool,
    pub loading: bool,
    pub devices: Vec<Device>,
    pub selected: Option<String>,
    pub board_confirmed: bool,
    pub mode: InstallMode,
    pub version: Option<String>,
    pub package: Option<omdrop_flash::Manifest>,
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
            message: "连接 Cardputer Adv，选择端口后安装固件。".into(),
            error: None,
            ready: false,
        }
    }
}
impl Model {
    pub fn request_exit(&mut self, confirmed: bool) -> bool {
        if self.busy && !confirmed {
            self.confirm_exit = true;
            return false;
        }
        true
    }

    pub fn stage_label(&self) -> &'static str {
        match self.stage {
            Some(FlashStage::ValidatePackage) => "校验固件包",
            Some(FlashStage::AcquireMaintenance) => "取得维护许可",
            Some(FlashStage::Connect) => "连接设备",
            Some(FlashStage::Write) => "写入固件",
            Some(FlashStage::Verify) => "校验写入",
            Some(FlashStage::CheckBoot) => "检查启动",
            Some(FlashStage::Restore) => "恢复无线",
            None => "准备安装",
        }
    }

    pub fn can_start(&self, transfer_active: bool) -> bool {
        !self.busy
            && !self.loading
            && !transfer_active
            && self.board_confirmed
            && self.version.is_some()
            && self.package.is_some()
            && self
                .selected
                .as_ref()
                .is_some_and(|port| self.devices.iter().any(|d| &d.port == port))
    }
    pub fn apply(&mut self, event: Event) -> bool {
        match event {
            Event::Inventory(result) => {
                self.loading = false;
                match result {
                    Ok((devices, package)) => {
                        if self.package.as_ref().is_none_or(|previous| {
                            previous.board != package.board
                                || previous.chip != package.chip
                                || previous.flash_size != package.flash_size
                                || previous.layout_id != package.layout_id
                                || previous.firmware_version != package.firmware_version
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
                        self.version = Some(package.firmware_version.clone());
                        self.package = Some(package);
                        self.error = None;
                    }
                    Err(error) => {
                        self.version = None;
                        self.package = None;
                        self.board_confirmed = false;
                        self.ready = false;
                        self.error = Some(error);
                    }
                }
                false
            }
            Event::Progress(event) => {
                self.stage = Some(event.stage);
                self.progress = event.progress;
                self.message = event.message;
                false
            }
            Event::Done(result) => {
                self.busy = false;
                self.cancelling = false;
                self.confirm_exit = false;
                self.progress = None;
                match result {
                    Ok(report) => {
                        self.ready = report.images_written && report.device_ready;
                        self.message = if self.ready {
                            "固件安装完成，设备已就绪，可以收发文件。"
                        } else {
                            "镜像已写入，但设备尚未就绪，请重试启动检查。"
                        }
                        .into();
                        self.error = None;
                    }
                    Err(error) => {
                        self.stage = Some(error.stage);
                        self.ready = false;
                        self.error = Some(format!(
                            "{}{}",
                            error.message,
                            if error.stage == FlashStage::CheckBoot {
                                "。镜像已写入并通过校验，但设备尚未就绪。松开 G0 后点按 RST，再重试。"
                            } else if error.stage == FlashStage::Restore {
                                "。固件已完成启动检查，但无线服务恢复失败，请重试。"
                            } else if error.possibly_written {
                                "。设备可能已写入部分内容，请保持连接并完整重试。"
                            } else {
                                ""
                            }
                        ));
                        self.message = if error.cancelled {
                            "固件操作已取消"
                        } else {
                            "固件操作未完成"
                        }
                        .into();
                    }
                }
                true
            }
        }
    }
}

pub fn refresh(events: Sender<crate::backend::Event>) {
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<_> {
            let package = omdrop_flash::validate_package(&omdrop_flash::installed_manifest()?)?;
            Ok((omdrop_flash::enumerate_devices()?, package.manifest))
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
        let result = omdrop_flash::flash(&options, &cancel, |progress| {
            let _ = events.send(crate::backend::Event::Firmware(Event::Progress(progress)));
        });
        let _ = events.send(crate::backend::Event::Firmware(Event::Done(result)));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package() -> omdrop_flash::Manifest {
        omdrop_flash::Manifest {
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
        });
        assert!(model.can_start(false));
        assert!(!model.can_start(true));
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
        assert!(model.apply(Event::Done(Err(omdrop_flash::FlashError {
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
        model.apply(Event::Inventory(Ok((vec![], package()))));
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
            }],
            updated,
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
