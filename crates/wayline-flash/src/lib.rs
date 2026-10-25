//! Firmware installation shared by desktop and CLI. No shell, automatic download or erase-all.
mod package;
mod process;
use anyhow::{Context, Result, bail, ensure};
pub use package::{
    BoardProfile, C5_FLASH_SIZE, C5_UART_BOARD, C5_USB_BOARD, FLASH_SIZE, FirmwareLink, Image,
    ImageKind, Manifest, S3_UART_BOARD, S3_USB_BOARD, SUPPORTED_BOARD, TOOL_VERSION,
    ValidatedPackage, installed_manifest, installed_manifest_for_board, installed_manifests,
    installed_tool, profile_accepts_usb, profile_for_board, supported_profiles, validate_package,
};
use serde::Serialize;
use std::{
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FlashStage {
    ValidatePackage,
    AcquireMaintenance,
    Connect,
    Write,
    Verify,
    CheckBoot,
    Restore,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlashEvent {
    pub stage: FlashStage,
    pub progress: Option<u8>,
    pub message: String,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlashError {
    pub stage: FlashStage,
    pub possibly_written: bool,
    pub retryable: bool,
    pub cancelled: bool,
    pub busy: bool,
    pub message: String,
}
impl std::fmt::Display for FlashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for FlashError {}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlashReport {
    pub port: String,
    pub firmware_version: String,
    pub images_written: bool,
    pub device_ready: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMode {
    /// Requires the caller's explicit replacement confirmation.
    Reinstall,
    /// Requires recognizable project firmware and exactly the same partition table.
    Upgrade,
}
#[derive(Debug, Clone)]
pub struct FlashOptions {
    pub manifest_path: PathBuf,
    pub tool_path: PathBuf,
    pub port: String,
    pub board: String,
    pub mode: InstallMode,
}
#[derive(Debug, Clone, Serialize)]
pub struct Device {
    pub port: String,
    pub label: String,
    pub usb_vid: u16,
    pub usb_pid: u16,
}
pub fn enumerate_devices() -> Result<Vec<Device>> {
    let mut ports: Vec<_> = serialport::available_ports()?
        .into_iter()
        .filter_map(|port| {
            if let serialport::SerialPortType::UsbPort(info) = port.port_type
                && supported_usb_adapter(info.vid, info.pid)
            {
                return Some(Device {
                    label: format!(
                        "{} ({})",
                        port.port_name,
                        info.product.as_deref().unwrap_or("USB serial adapter")
                    ),
                    port: port.port_name,
                    usb_vid: info.vid,
                    usb_pid: info.pid,
                });
            }
            None
        })
        .collect();
    ports.sort_by(|a, b| a.port.cmp(&b.port));
    Ok(ports)
}

fn supported_usb_adapter(vid: u16, pid: u16) -> bool {
    (vid == 0x303a && pid == 0x1001) || [0x1a86, 0x10c4, 0x0403].contains(&vid)
}

pub fn flash(
    options: &FlashOptions,
    cancel: &Cancellation,
    event: impl FnMut(FlashEvent),
) -> std::result::Result<FlashReport, FlashError> {
    flash_with(options, cancel, event, &mut Production::default())
}

fn validate_profile_port(board: &str, port: &str) -> Result<()> {
    let _ = profile_for_board(board).context("未支持的固件 profile")?;
    let device = serialport::available_ports()?
        .into_iter()
        .find(|device| device.port_name.eq_ignore_ascii_case(port))
        .with_context(|| format!("未找到串口 {port}"))?;
    let serialport::SerialPortType::UsbPort(info) = device.port_type else {
        bail!("串口 {port} 不是受支持的 USB 连接");
    };
    ensure!(
        profile_accepts_usb(board, info.vid, info.pid),
        "串口 USB 接口类型与固件 profile {} 不匹配",
        profile_for_board(board).unwrap().label
    );
    Ok(())
}

fn flash_with(
    options: &FlashOptions,
    cancel: &Cancellation,
    mut event: impl FnMut(FlashEvent),
    runtime: &mut impl Runtime,
) -> std::result::Result<FlashReport, FlashError> {
    let mut stage = FlashStage::ValidatePackage;
    let mut written = false;
    let mut emit = |stage, progress, message: &str| {
        event(FlashEvent {
            stage,
            progress,
            message: message.into(),
        })
    };
    let result = (|| -> Result<FlashReport> {
        emit(stage, None, "正在校验固件包");
        ensure!(!cancel.is_cancelled(), "操作已取消");
        validate_port(&options.port)?;
        let package = validate_package(&options.manifest_path)?;
        ensure!(
            options.board == package.manifest.board,
            "所选固件 profile 与清单不匹配"
        );
        runtime.validate_link(&package.manifest.board, &options.port)?;
        ensure!(
            options.tool_path.is_absolute() && options.tool_path.is_file(),
            "烧录工具必须是已安装的绝对文件路径"
        );
        // Work from a private snapshot, preventing package edits during installation.
        let staging = tempfile::tempdir()?;
        for (image, path) in package.manifest.images.iter().zip(&package.paths) {
            let target = staging.path().join(&image.path);
            std::fs::create_dir_all(target.parent().unwrap())?;
            use std::io::Read;
            let mut source = std::fs::File::open(path)?.take(image.length + 1);
            let mut destination = std::fs::File::create(target)?;
            ensure!(
                std::io::copy(&mut source, &mut destination)? == image.length,
                "镜像在校验后发生变化"
            );
        }
        let manifest_path = staging.path().join("manifest.json");
        std::fs::write(&manifest_path, serde_json::to_vec(&package.manifest)?)?;
        let package = validate_package(&manifest_path)?;
        let version = runtime.tool(
            &options.tool_path,
            &["version".into()],
            cancel,
            Duration::from_secs(20),
            |_| {},
        )?;
        ensure!(
            version.text.lines().any(|line| line.trim() == TOOL_VERSION
                || line.trim() == format!("esptool v{TOOL_VERSION}")),
            "烧录工具版本必须是 {TOOL_VERSION}"
        );
        stage = FlashStage::AcquireMaintenance;
        emit(stage, None, "正在取得设备维护许可");
        runtime.acquire(&options.port, cancel)?;
        let outcome = (|| -> Result<FlashReport> {
            stage = FlashStage::Connect;
            emit(stage, None, "正在连接设备；请确认数据线和所选端口");
            if options.mode == InstallMode::Upgrade {
                runtime
                    .boot(
                        &options.port,
                        package.manifest.protocol_version,
                        &package.manifest.chip,
                        cancel,
                        Duration::from_secs(3),
                    )
                    .context("无法识别现有项目固件；请明确选择重新安装")?;
            }
            let mut args = base_args(&package.manifest.chip, &options.port, "no-reset");
            args.push("flash-id".into());
            let info = runtime
                .tool(
                    &options.tool_path,
                    &args,
                    cancel,
                    Duration::from_secs(30),
                    |line| emit(stage, None, line),
                )
                .map_err(|error| explain_wrong_chip(error, &package.manifest.chip))?;
            check_chip(&info.text, &package.manifest)?;
            if options.mode == InstallMode::Upgrade {
                let index = package
                    .manifest
                    .images
                    .iter()
                    .position(|i| i.kind == ImageKind::PartitionTable)
                    .unwrap();
                let table = &package.manifest.images[index];
                let existing = staging.path().join("existing-partition-table.bin");
                let mut args = base_args(&package.manifest.chip, &options.port, "no-reset");
                args.extend([
                    "read-flash".into(),
                    format!("0x{:x}", table.offset).into(),
                    table.length.to_string().into(),
                    existing.as_os_str().to_owned(),
                ]);
                runtime.tool(
                    &options.tool_path,
                    &args,
                    cancel,
                    Duration::from_secs(45),
                    |line| emit(stage, None, line),
                )?;
                ensure!(
                    std::fs::metadata(&existing)?.len() == table.length
                        && package::hash_file(&existing)? == table.sha256.to_ascii_lowercase(),
                    "分区布局不兼容；请明确选择重新安装，不能保证保留设置"
                );
            }
            ensure!(!cancel.is_cancelled(), "操作已取消");
            stage = FlashStage::Write;
            emit(stage, Some(0), "正在写入固件；中断后可能需要完整重刷");
            // Persist before the first erase/write, including standalone first installs.
            // A later service startup must not seize an incompletely written device.
            runtime.prepare_write()?;
            written = true;
            let mut args = base_args(&package.manifest.chip, &options.port, "no-reset");
            args.extend([
                "write-flash".into(),
                "--flash-mode".into(),
                "keep".into(),
                "--flash-freq".into(),
                "keep".into(),
                "--flash-size".into(),
                "keep".into(),
            ]);
            append_images(&mut args, &package);
            runtime.tool(
                &options.tool_path,
                &args,
                cancel,
                Duration::from_secs(300),
                |line| emit(stage, progress(line), line),
            )?;
            stage = FlashStage::Verify;
            emit(stage, None, "正在校验 Flash 中的镜像");
            let mut args = base_args(&package.manifest.chip, &options.port, "hard-reset");
            args.push("verify-flash".into());
            append_images(&mut args, &package);
            runtime.tool(
                &options.tool_path,
                &args,
                cancel,
                Duration::from_secs(180),
                |line| emit(stage, progress(line), line),
            )?;
            stage = FlashStage::CheckBoot;
            emit(stage, None, "镜像写入校验通过，正在等待设备启动");
            runtime
                .boot(
                    &options.port,
                    package.manifest.protocol_version,
                    &package.manifest.chip,
                    cancel,
                    Duration::from_secs(20),
                )
                .context("镜像已写入并校验，但设备尚未就绪；松开 G0 后点按 RST，再重试")?;
            Ok(FlashReport {
                port: options.port.clone(),
                firmware_version: package.manifest.firmware_version.clone(),
                images_written: true,
                device_ready: true,
            })
        })();
        let ready = outcome.is_ok();
        if ready {
            stage = FlashStage::Restore;
            emit(stage, None, "设备已就绪，正在恢复无线");
        }
        let release = runtime.finish(ready);
        match (outcome, release) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error.context("设备已就绪，但无线恢复失败")),
            (Err(error), Err(release)) => {
                Err(anyhow::anyhow!("{error:#}；维护状态提交失败：{release:#}"))
            }
        }
    })();
    result.map_err(|error| FlashError {
        stage,
        possibly_written: written,
        retryable: stage != FlashStage::ValidatePackage,
        cancelled: cancel.is_cancelled(),
        busy: error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::WouldBlock)
                || cause
                    .downcast_ref::<wayline_service::client::ClientError>()
                    .is_some_and(|e| e.kind() == wayline_service::client::ClientErrorKind::Busy)
        }),
        message: format!("{error:#}"),
    })
}

/// Dependency boundary for hardware-free testing of the entire installation workflow.
trait Runtime {
    fn acquire(&mut self, port: &str, cancel: &Cancellation) -> Result<()>;
    fn prepare_write(&mut self) -> Result<()>;
    fn finish(&mut self, ready: bool) -> Result<()>;
    fn tool(
        &mut self,
        executable: &std::path::Path,
        args: &[OsString],
        cancel: &Cancellation,
        timeout: Duration,
        output: impl FnMut(&str),
    ) -> Result<process::ToolOutput>;
    fn validate_link(&mut self, _board: &str, _port: &str) -> Result<()> {
        Ok(())
    }
    fn boot(
        &mut self,
        port: &str,
        protocol: u8,
        target: &str,
        cancel: &Cancellation,
        timeout: Duration,
    ) -> Result<()>;
}
#[derive(Default)]
struct Production {
    maintenance: Option<Maintenance>,
    port: Option<String>,
    #[cfg(windows)]
    device: Option<wayline_service::device_lock::DeviceGuard>,
}
impl Runtime for Production {
    fn validate_link(&mut self, board: &str, port: &str) -> Result<()> {
        validate_profile_port(board, port)
    }
    fn acquire(&mut self, port: &str, cancel: &Cancellation) -> Result<()> {
        self.maintenance = Some(Maintenance::acquire(cancel)?);
        #[cfg(windows)]
        {
            match wayline_service::device_lock::DeviceGuard::try_acquire(port) {
                Ok(device) => self.device = Some(device),
                Err(error) => {
                    // No handshake was possible. Preserve any pre-existing recovery
                    // requirement instead of treating lock contention as proof of readiness.
                    self.finish(false)?;
                    return Err(error).context("设备正在被无线服务或另一烧录任务使用");
                }
            }
        }
        #[cfg(not(windows))]
        let _ = port;
        self.port = Some(port.to_owned());
        Ok(())
    }
    fn prepare_write(&mut self) -> Result<()> {
        #[cfg(windows)]
        wayline_service::device_lock::mark_failed(
            self.port.as_deref().context("尚未取得设备维护许可")?,
        )
        .context("无法保存设备写入恢复标记，已阻止写入")?;
        Ok(())
    }
    fn finish(&mut self, ready: bool) -> Result<()> {
        #[cfg(windows)]
        if ready && let Some(port) = self.port.as_deref() {
            // Only a verified boot removes the durable failure marker, while the same
            // device guard still excludes both service startup and another flasher.
            wayline_service::device_lock::clear_failed(port)
                .context("设备已就绪，但无法清除维护失败标记")?;
        }
        // Tool and serial calls have returned with their child/handle fully closed. Drop
        // device exclusion before service resume, while maintenance still blocks new work.
        #[cfg(windows)]
        self.device.take();
        if let Some(mut maintenance) = self.maintenance.take() {
            maintenance.finish(ready)?;
        }
        Ok(())
    }
    fn tool(
        &mut self,
        executable: &std::path::Path,
        args: &[OsString],
        cancel: &Cancellation,
        timeout: Duration,
        output: impl FnMut(&str),
    ) -> Result<process::ToolOutput> {
        process::run(executable, args, cancel, timeout, output)
    }
    fn boot(
        &mut self,
        port: &str,
        protocol: u8,
        target: &str,
        cancel: &Cancellation,
        timeout: Duration,
    ) -> Result<()> {
        check_boot(port, protocol, target, cancel, timeout)
    }
}
fn validate_port(port: &str) -> Result<()> {
    let suffix = port
        .strip_prefix("COM")
        .or_else(|| port.strip_prefix("com"))
        .context("必须明确指定 COM 端口")?;
    ensure!(
        !suffix.is_empty()
            && suffix.bytes().all(|b| b.is_ascii_digit())
            && suffix.parse::<u32>().is_ok_and(|n| n > 0 && n <= 65535),
        "无效的 COM 端口"
    );
    Ok(())
}
fn base_args(chip: &str, port: &str, after: &str) -> Vec<OsString> {
    // Use the target chip's ROM for all profiles and explicitly reset before
    // each operation so S3 and C5 use the same verified workflow.
    [
        "--chip",
        chip,
        "--port",
        port,
        "--baud",
        "460800",
        "--no-stub",
        "--before",
        "default-reset",
        "--after",
        after,
    ]
    .into_iter()
    .map(Into::into)
    .collect()
}
fn append_images(args: &mut Vec<OsString>, package: &ValidatedPackage) {
    for (image, path) in package.manifest.images.iter().zip(&package.paths) {
        args.push(format!("0x{:x}", image.offset).into());
        args.push(path.as_os_str().to_owned());
    }
}
fn chip_label(chip: &str) -> Result<&'static str> {
    match chip {
        "esp32s3" => Ok("ESP32-S3"),
        "esp32c5" => Ok("ESP32-C5"),
        _ => bail!("不支持的芯片目标：{chip}"),
    }
}

fn check_chip(text: &str, manifest: &Manifest) -> Result<()> {
    let expected_chip = chip_label(&manifest.chip)?;
    let expected_flash_size = format!("{}MB", manifest.flash_size / (1024 * 1024));
    ensure!(
        text.contains(expected_chip)
            && text.lines().any(|line| {
                line.contains("Detected flash size:")
                    && (line.replace(' ', "").contains(&expected_flash_size) || line.contains("16MB"))
            }),
        "芯片或 Flash 容量与所选固件 profile 不匹配；期望 {expected_chip} / {expected_flash_size}"
    );
    Ok(())
}

fn explain_wrong_chip(error: anyhow::Error, expected: &str) -> anyhow::Error {
    let details = error.to_string();
    let expected_label = chip_label(expected).unwrap_or("所选芯片");
    let wrong_chip = match expected {
        "esp32s3" => details.contains("This chip is ESP32-C5"),
        "esp32c5" => details.contains("This chip is ESP32-S3"),
        _ => false,
    };
    if wrong_chip {
        anyhow::anyhow!(
            "检测到的芯片与所选 {expected_label} 固件 profile 不匹配；尚未写入 Flash。esptool 详情：{details}"
        )
    } else {
        error
    }
}
fn progress(line: &str) -> Option<u8> {
    let at = line.find('%')?;
    let value = line[..at]
        .trim_end()
        .rsplit(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
        .next()?
        .parse::<f64>()
        .ok()?;
    (value.is_finite() && (0.0..=100.0).contains(&value)).then_some(value as u8)
}
fn check_boot(
    port: &str,
    protocol: u8,
    target: &str,
    cancel: &Cancellation,
    timeout: Duration,
) -> Result<()> {
    let start = Instant::now();
    let mut last = "设备未响应".to_owned();
    while start.elapsed() < timeout {
        ensure!(!cancel.is_cancelled(), "操作已取消");
        match wayline_radio::RadioClient::open_serial(port, 3_000_000, Duration::from_millis(150)) {
            Ok(mut radio) => match radio.hello(Duration::from_millis(500)) {
                Ok(hello) => {
                    ensure!(
                        hello.target == target && hello.protocol == protocol,
                        "设备启动了不兼容的无线固件"
                    );
                    return Ok(());
                }
                Err(error) => last = error.to_string(),
            },
            Err(error) => last = error.to_string(),
        }
        thread::sleep(Duration::from_millis(150));
    }
    anyhow::bail!("{last}")
}

/// Cancels an in-flight process or serial handshake when its service lease disappears.
/// The owner joins this watcher before releasing a lease intentionally.
#[cfg(any(windows, test))]
struct ConnectionWatch {
    stop: Option<std::sync::mpsc::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}
#[cfg(any(windows, test))]
impl ConnectionWatch {
    fn new(cancel: Cancellation, is_alive: impl Fn() -> bool + Send + 'static) -> Self {
        let (stop, stopped) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                stopped.recv_timeout(Duration::from_millis(50))
            {
                if !is_alive() {
                    cancel.cancel();
                    break;
                }
            }
        });
        Self {
            stop: Some(stop),
            worker: Some(worker),
        }
    }
}
#[cfg(any(windows, test))]
impl Drop for ConnectionWatch {
    fn drop(&mut self) {
        self.stop.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(windows)]
struct Maintenance {
    // Drop the watcher first, including unwinding, so its Arc never keeps a lease alive.
    watch: Option<ConnectionWatch>,
    lease: Option<Arc<wayline_service::session::ClientLease>>,
}
#[cfg(windows)]
impl Maintenance {
    fn acquire(cancel: &Cancellation) -> Result<Self> {
        use wayline_service::{client::Client, service_control, session::SessionKind};
        match service_control::status() {
            // Only confirmed absence permits standalone mode. Access denied and broken
            // pipes cannot bypass coordination. DeviceGuard covers concurrent installation.
            Err(error) if error.raw_os_error() == Some(1060) => Ok(Self {
                watch: None,
                lease: None,
            }),
            Err(error) => Err(error).context("无法检查服务维护状态"),
            Ok(_) => {
                service_control::start().context("无法启动维护服务")?;
                let lease = Arc::new(Client::default().acquire(SessionKind::Maintenance)?);
                let watched = lease.clone();
                let watch = ConnectionWatch::new(cancel.clone(), move || watched.is_alive());
                Ok(Self {
                    watch: Some(watch),
                    lease: Some(lease),
                })
            }
        }
    }
    fn finish(&mut self, ready: bool) -> Result<()> {
        self.watch.take();
        if let Some(lease) = self.lease.take() {
            let lease =
                Arc::try_unwrap(lease).map_err(|_| anyhow::anyhow!("设备维护监视未结束"))?;
            lease.finish_maintenance(ready)?;
        }
        Ok(())
    }
}
#[cfg(not(windows))]
struct Maintenance;
#[cfg(not(windows))]
impl Maintenance {
    fn acquire(_: &Cancellation) -> Result<Self> {
        anyhow::bail!("烧录协调目前仅支持 Windows")
    }
    fn finish(&mut self, _: bool) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Simulated {
        commands: Vec<String>,
        arguments: Vec<Vec<OsString>>,
        acquired: bool,
        prepared: bool,
        restored: Option<bool>,
        fail_command: Option<&'static str>,
        chip_error: Option<&'static str>,
        wrong_chip: bool,
        fail_boot: bool,
        fail_restore: bool,
        cancel_write: bool,
        table: Vec<u8>,
        boot_target: Option<String>,
    }
    impl Runtime for Simulated {
        fn acquire(&mut self, _: &str, _: &Cancellation) -> Result<()> {
            self.acquired = true;
            Ok(())
        }
        fn finish(&mut self, ready: bool) -> Result<()> {
            self.restored = Some(ready);
            ensure!(!self.fail_restore, "simulated service recovery failure");
            Ok(())
        }
        fn prepare_write(&mut self) -> Result<()> {
            self.prepared = true;
            Ok(())
        }
        fn tool(
            &mut self,
            _: &std::path::Path,
            args: &[OsString],
            cancel: &Cancellation,
            _: Duration,
            mut output: impl FnMut(&str),
        ) -> Result<process::ToolOutput> {
            ensure!(!cancel.is_cancelled(), "cancelled");
            let command = args
                .iter()
                .find_map(|a| {
                    a.to_str().filter(|a| {
                        [
                            "version",
                            "flash-id",
                            "read-flash",
                            "write-flash",
                            "verify-flash",
                        ]
                        .contains(a)
                    })
                })
                .unwrap();
            self.commands.push(command.into());
            self.arguments.push(args.to_vec());
            if let ("flash-id", Some(error)) = (command, self.chip_error) {
                return Err(anyhow::anyhow!("烧录工具退出 exit code:2：{error}"));
            }
            ensure!(
                self.fail_command != Some(command),
                "simulated {command} failure"
            );
            if command == "write-flash" && self.cancel_write {
                cancel.cancel();
                anyhow::bail!("cancelled during write");
            }
            if command == "read-flash" {
                std::fs::write(std::path::Path::new(args.last().unwrap()), &self.table)?;
            }
            if command == "write-flash" {
                output("Writing 100%");
            }
            let chip = args
                .windows(2)
                .find(|pair| pair[0] == "--chip")
                .and_then(|pair| pair[1].to_str())
                .unwrap_or("esp32s3");
            Ok(process::ToolOutput {
                text: match command {
                    "version" => TOOL_VERSION.into(),
                    "flash-id" => format!(
                        "Chip is {}\nDetected flash size: {}",
                        if self.wrong_chip {
                            "ESP32-C3"
                        } else if chip == "esp32c5" {
                            "ESP32-C5"
                        } else {
                            "ESP32-S3"
                        },
                        if chip == "esp32c5" { "2MB" } else { "8MB" }
                    ),
                    _ => String::new(),
                },
            })
        }
        fn boot(
            &mut self,
            _: &str,
            _: u8,
            target: &str,
            _: &Cancellation,
            _: Duration,
        ) -> Result<()> {
            self.boot_target = Some(target.into());
            ensure!(!self.fail_boot, "simulated boot timeout");
            Ok(())
        }
    }
    fn operation_fixture() -> (tempfile::TempDir, FlashOptions, Simulated) {
        let (dir, path, manifest) = package::tests::fixture();
        operation_fixture_for(dir, path, manifest)
    }
    fn operation_fixture_for(
        dir: tempfile::TempDir,
        path: PathBuf,
        manifest: Manifest,
    ) -> (tempfile::TempDir, FlashOptions, Simulated) {
        let simulation = Simulated {
            table: std::fs::read(dir.path().join(&manifest.images[1].path)).unwrap(),
            ..Default::default()
        };
        let options = FlashOptions {
            manifest_path: path,
            tool_path: std::env::current_exe().unwrap(),
            port: "COM4".into(),
            board: manifest.board.clone(),
            mode: InstallMode::Reinstall,
        };
        (dir, options, simulation)
    }
    #[test]
    fn workflow_only_restores_after_verification_and_boot() {
        let (_dir, options, mut simulation) = operation_fixture();
        let mut stages = Vec::new();
        let report = flash_with(
            &options,
            &Cancellation::default(),
            |event| stages.push(event.stage),
            &mut simulation,
        )
        .unwrap();
        assert!(report.images_written && report.device_ready);
        assert!(simulation.prepared);
        assert_eq!(
            simulation.commands,
            ["version", "flash-id", "write-flash", "verify-flash"]
        );
        assert_eq!(simulation.restored, Some(true));
        assert_eq!(stages.last(), Some(&FlashStage::Restore));
        assert!(stages.contains(&FlashStage::CheckBoot));
        assert_eq!(simulation.boot_target.as_deref(), Some("esp32s3"));
    }
    #[test]
    fn c5_profile_flashes_with_c5_chip_and_waits_for_c5_hello() {
        let (dir, path, manifest) = package::tests::c5_fixture(C5_UART_BOARD);
        let (_dir, options, mut simulation) = operation_fixture_for(dir, path, manifest);
        let report =
            flash_with(&options, &Cancellation::default(), |_| {}, &mut simulation).unwrap();
        assert!(report.images_written && report.device_ready);
        assert_eq!(simulation.boot_target.as_deref(), Some("esp32c5"));
        assert_eq!(simulation.restored, Some(true));
        for args in simulation
            .arguments
            .iter()
            .filter(|args| !args.iter().any(|arg| arg == "version"))
        {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "--chip" && pair[1] == "esp32c5")
            );
        }
    }
    #[test]
    fn requested_profile_must_match_manifest_before_maintenance_or_write() {
        let (dir, path, manifest) = package::tests::c5_fixture(C5_UART_BOARD);
        let (_dir, mut options, mut simulation) = operation_fixture_for(dir, path, manifest);
        options.board = SUPPORTED_BOARD.into();
        let error =
            flash_with(&options, &Cancellation::default(), |_| {}, &mut simulation).unwrap_err();
        assert_eq!(error.stage, FlashStage::ValidatePackage);
        assert!(!simulation.acquired && !simulation.prepared);
    }
    #[test]
    fn tool_failure_preserves_phase_and_failed_maintenance() {
        for (command, stage, written) in [
            ("flash-id", FlashStage::Connect, false),
            ("write-flash", FlashStage::Write, true),
            ("verify-flash", FlashStage::Verify, true),
        ] {
            let (_dir, options, mut simulation) = operation_fixture();
            simulation.fail_command = Some(command);
            let error = flash_with(&options, &Cancellation::default(), |_| {}, &mut simulation)
                .unwrap_err();
            assert_eq!(error.stage, stage);
            assert_eq!(error.possibly_written, written);
            assert_eq!(simulation.restored, Some(false));
        }
    }
    #[test]
    fn wrong_chip_never_writes_and_boot_failure_is_not_success() {
        let (_dir, options, mut simulation) = operation_fixture();
        simulation.wrong_chip = true;
        let error =
            flash_with(&options, &Cancellation::default(), |_| {}, &mut simulation).unwrap_err();
        assert_eq!(error.stage, FlashStage::Connect);
        assert!(!error.possibly_written);
        assert!(!simulation.prepared);
        assert!(!simulation.commands.iter().any(|c| c == "write-flash"));
        simulation = Simulated {
            fail_boot: true,
            ..Default::default()
        };
        let error =
            flash_with(&options, &Cancellation::default(), |_| {}, &mut simulation).unwrap_err();
        assert_eq!(error.stage, FlashStage::CheckBoot);
        assert!(error.possibly_written);
        assert_eq!(simulation.restored, Some(false));
        assert!(error.message.contains("镜像已写入并校验"));
    }

    #[test]
    fn mismatched_chip_is_explained_without_writing() {
        let (_dir, options, mut simulation) = operation_fixture();
        simulation.chip_error = Some(
            "A fatal error occurred: This chip is ESP32-C5, not ESP32-S3. Wrong chip argument?",
        );
        let error =
            flash_with(&options, &Cancellation::default(), |_| {}, &mut simulation).unwrap_err();
        assert_eq!(error.stage, FlashStage::Connect);
        assert!(!error.possibly_written);
        assert!(!simulation.prepared);
        assert!(
            error
                .message
                .contains("与所选 ESP32-S3 固件 profile 不匹配")
        );
        assert!(error.message.contains("尚未写入 Flash"));
    }
    #[test]
    fn cancel_and_recovery_failure_never_report_completion() {
        let (_dir, options, mut simulation) = operation_fixture();
        simulation.cancel_write = true;
        let error =
            flash_with(&options, &Cancellation::default(), |_| {}, &mut simulation).unwrap_err();
        assert!(error.cancelled && error.possibly_written);
        assert_eq!(error.stage, FlashStage::Write);
        assert_eq!(simulation.restored, Some(false));
        simulation = Simulated {
            fail_restore: true,
            ..Default::default()
        };
        let error =
            flash_with(&options, &Cancellation::default(), |_| {}, &mut simulation).unwrap_err();
        assert_eq!(error.stage, FlashStage::Restore);
        assert!(error.message.contains("设备已就绪"));
    }
    #[test]
    fn upgrade_reads_partition_table_and_rejects_incompatible_layout() {
        let (_dir, mut options, mut simulation) = operation_fixture();
        options.mode = InstallMode::Upgrade;
        flash_with(&options, &Cancellation::default(), |_| {}, &mut simulation).unwrap();
        assert!(simulation.commands.iter().any(|c| c == "read-flash"));
        simulation.commands.clear();
        simulation.table[16] ^= 1;
        let error =
            flash_with(&options, &Cancellation::default(), |_| {}, &mut simulation).unwrap_err();
        assert!(!error.possibly_written);
        assert_eq!(error.stage, FlashStage::Connect);
        assert!(!simulation.commands.iter().any(|c| c == "write-flash"));
        assert!(error.message.contains("布局不兼容"));
    }
    #[test]
    fn native_usb_workflow_uses_rom_for_every_flash_operation() {
        let (_dir, mut options, mut simulation) = operation_fixture();
        options.mode = InstallMode::Upgrade;
        flash_with(&options, &Cancellation::default(), |_| {}, &mut simulation).unwrap();
        assert_eq!(
            simulation.commands,
            [
                "version",
                "flash-id",
                "read-flash",
                "write-flash",
                "verify-flash"
            ]
        );
        for (command, args) in simulation.commands.iter().zip(&simulation.arguments) {
            if command == "version" {
                continue;
            }
            assert!(args.iter().any(|arg| arg == "--no-stub"), "{command}");
            assert!(
                args.windows(2)
                    .any(|pair| { pair[0] == "--before" && pair[1] == "default-reset" })
            );
            let expected_reset = if command == "verify-flash" {
                "hard-reset"
            } else {
                "no-reset"
            };
            assert!(
                args.windows(2)
                    .any(|pair| { pair[0] == "--after" && pair[1] == expected_reset })
            );
        }
    }
    #[test]
    fn chip_capacity_and_port_are_explicit() {
        let (_, _, s3) = package::tests::fixture();
        let (_, _, c5) = package::tests::c5_fixture(C5_UART_BOARD);
        assert!(check_chip("Chip is ESP32-S3\nDetected flash size: 8MB", &s3).is_ok());
        assert!(check_chip("Chip is ESP32\nDetected flash size: 8MB", &s3).is_err());
        assert!(check_chip("Chip is ESP32-S3\nDetected flash size: 4MB", &s3).is_err());
        assert!(check_chip("Chip is ESP32-C5\nDetected flash size: 2MB", &c5).is_ok());
        assert!(check_chip("Chip is ESP32-C5\nDetected flash size: 4MB", &c5).is_err());
        for port in ["auto", "COM0", "COM4 --erase-all", "\\\\.\\COM4"] {
            assert!(validate_port(port).is_err());
        }
        assert!(validate_port("COM4").is_ok());
    }

    #[test]
    fn desktop_device_filter_includes_common_c5_usb_uart_adapters() {
        assert!(supported_usb_adapter(0x303a, 0x1001));
        assert!(supported_usb_adapter(0x1a86, 0x7523));
        assert!(supported_usb_adapter(0x10c4, 0xea60));
        assert!(supported_usb_adapter(0x0403, 0x6001));
        assert!(!supported_usb_adapter(0x303a, 0x1002));
        assert!(!supported_usb_adapter(0x1234, 0x5678));
    }
    #[test]
    fn recognizes_progress_without_promising_completion() {
        assert_eq!(progress("Writing at 0x00010000 (42 %)"), Some(42));
        assert_eq!(progress("Writing 100%"), Some(100));
        assert_eq!(
            progress("Reading from 0x00009000 [==> ] 12.5% 4096/32768 bytes..."),
            Some(12)
        );
        assert_eq!(
            progress("Writing at 0x00101850 [======] 100.0% 989264/989264 bytes..."),
            Some(100)
        );
        for line in ["-1%", "100.1%", "101%", "12..5%", "NaN%", "inf%"] {
            assert_eq!(progress(line), None, "{line}");
        }
        assert_eq!(progress("no progress"), None);
    }
    #[test]
    fn lost_maintenance_connection_cancels_and_stopped_watch_does_not() {
        let alive = Arc::new(AtomicBool::new(true));
        let cancel = Cancellation::default();
        let watched = alive.clone();
        let watch = ConnectionWatch::new(cancel.clone(), move || watched.load(Ordering::Acquire));
        alive.store(false, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !cancel.is_cancelled() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(cancel.is_cancelled());
        drop(watch);

        alive.store(true, Ordering::Release);
        let cancel = Cancellation::default();
        let watched = alive.clone();
        let watch = ConnectionWatch::new(cancel.clone(), move || watched.load(Ordering::Acquire));
        drop(watch);
        alive.store(false, Ordering::Release);
        assert!(!cancel.is_cancelled());
        assert_eq!(Arc::strong_count(&alive), 1);
    }
}
