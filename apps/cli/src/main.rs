use omdrop_core::{
    discover_receivers_live_cancellable, send_to_receiver_cancellable, server_config,
    validate_files, DiscoveryEvent, Identity, NearbyReceiver, NetworkInterface, ReceiverCore,
    ReceiverEvent, ReceiverOptions, SendCancellation, SendEvent, SendOptions,
};
use omdrop_service::{
    client::{Client, ClientErrorKind},
    session::{ClientLease, SessionKind},
};
use serde_json::{json, Value};
use std::{
    env,
    io::IsTerminal,
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

mod args;
mod consent;
mod output;
mod selection;
use args::Args;
use output::{Failure, Output};

#[derive(Clone, Default)]
struct Signals {
    cancel: SendCancellation,
    shutdown: Arc<AtomicBool>,
    flash: omdrop_flash::Cancellation,
}
impl Signals {
    fn stop(&self) {
        self.cancel.cancel();
        self.shutdown.store(true, Ordering::Release);
        self.flash.cancel();
    }
    fn check(&self) -> Result<(), Failure> {
        if self.cancel.is_cancelled() {
            Err(Failure::cancelled())
        } else {
            Ok(())
        }
    }
}

fn main() {
    let values: Vec<_> = env::args().skip(1).collect();
    if values == ["--version"] || values == ["-V"] {
        println!("omdrop-cli {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let output = Output {
        json: args::json_requested(&values),
    };
    let command = values
        .iter()
        .find(|arg| arg.as_str() != "--json")
        .cloned()
        .unwrap_or_else(|| "help".into());
    let parsed = Args::parse(values, std::io::stdin().is_terminal());
    let signals = Signals::default();
    let stop = signals.clone();
    let result = parsed.and_then(|args| {
        if args.help {
            output.event("help", json!({"usage":args::HELP}));
            return Ok(());
        }
        ctrlc::set_handler(move || stop.stop()).map_err(Failure::operation)?;
        omdrop_core::enable_board_feedback();
        run(args, output, &signals)
    });
    output.terminal(&command, &result);
    std::process::exit(result.err().map_or(0, |error| error.code));
}

fn run(args: Args, output: Output, signals: &Signals) -> Result<(), Failure> {
    match args.command.as_str() {
        "status" => {
            let status = Client::default().status()?;
            output.event(
                "status",
                serde_json::to_value(status).map_err(Failure::operation)?,
            );
            Ok(())
        }
        "doctor" => doctor(output),
        "firmware info" => firmware_info(&args, output),
        "firmware flash" => firmware_flash(&args, output, signals),
        "discover" | "send" | "receive" => {
            if args.command == "send" {
                validate_files(&args.files).map_err(Failure::policy)?;
            }
            let name = match &args.name {
                Some(name) => omdrop_host::normalize_name(name).map_err(Failure::policy)?,
                None => omdrop_host::load_name().map_err(Failure::operation)?,
            };
            let identity = omdrop_host::load_identity(&name).map_err(Failure::operation)?;
            if args.command == "receive" && args.bind.is_some() {
                return receive(&args, &name, identity, None, output, signals);
            }
            let kind = match args.command.as_str() {
                "send" => SessionKind::Send,
                "receive" => SessionKind::Receive,
                _ => SessionKind::Client,
            };
            let lease = acquire(kind, signals)?;
            with_lease(lease, output, signals, || {
                let interface = wait_interface(&args, signals)?;
                Client::default().firewall(&env::current_exe().map_err(Failure::operation)?)?;
                match args.command.as_str() {
                    "receive" => receive(&args, &name, identity, Some(interface), output, signals),
                    "discover" => {
                        let receivers = discover(&args, &identity, interface, output, signals)?;
                        output.event("discoveryCompleted", json!({"receivers":receivers.iter().map(receiver_json).collect::<Vec<_>>(),"count":receivers.len()}));
                        Ok(())
                    }
                    _ => send(&args, &name, &identity, interface, output, signals),
                }
            })
        }
        _ => Err(Failure::policy("未知命令")),
    }
}

fn acquire(kind: SessionKind, signals: &Signals) -> Result<ClientLease, Failure> {
    signals.check()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        omdrop_service::service_control::start().map_err(Failure::unavailable)?;
        signals.check()?;
        match Client::default().acquire(kind) {
            Ok(lease) => return Ok(lease),
            Err(error)
                if matches!(
                    error.kind(),
                    ClientErrorKind::Stopping | ClientErrorKind::Connect
                ) && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(200))
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn with_lease(
    lease: ClientLease,
    output: Output,
    signals: &Signals,
    operation: impl FnOnce() -> Result<(), Failure>,
) -> Result<(), Failure> {
    let finished = AtomicBool::new(false);
    let disconnected = AtomicBool::new(false);
    let result = thread::scope(|scope| {
        scope.spawn(|| {
            while !finished.load(Ordering::Acquire) {
                if !lease.is_alive() {
                    disconnected.store(true, Ordering::Release);
                    signals.stop();
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        });
        let result = operation();
        finished.store(true, Ordering::Release);
        result
    });
    let cleanup = lease.release();
    if disconnected.load(Ordering::Acquire) {
        return Err(Failure::unavailable("服务会话已断开，请重新运行命令"));
    }
    match cleanup {
        Ok(outcome) => output.event("sessionReleased", json!({"remainingClients":outcome.remaining_clients,"serviceStopping":outcome.service_stopping})),
        Err(error) if result.is_ok() => return Err(error.into()),
        Err(error) => output.event("cleanupFailed", json!({"message":error.to_string()})),
    }
    result
}

fn wait_interface(args: &Args, signals: &Signals) -> Result<NetworkInterface, Failure> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        signals.check()?;
        match omdrop_host::select_interface(args.address, args.index) {
            Ok(interface) => return Ok(interface),
            Err(error) if args.address.is_some() || Instant::now() >= deadline => {
                return Err(Failure::unavailable(error))
            }
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn discover(
    args: &Args,
    identity: &Identity,
    interface: NetworkInterface,
    output: Output,
    signals: &Signals,
) -> Result<Vec<NearbyReceiver>, Failure> {
    signals.check()?;
    discover_receivers_live_cancellable(
        interface,
        identity,
        args.timeout,
        args.ble,
        args.legacy_ble,
        &signals.cancel,
        |event| match event {
            DiscoveryEvent::Found(receiver) => {
                output.event("receiverFound", receiver_json(&receiver))
            }
            DiscoveryEvent::Updated(receiver) => {
                output.event("receiverUpdated", receiver_json(&receiver))
            }
            DiscoveryEvent::Removed { instance } => {
                output.event("receiverRemoved", json!({"instance":instance}))
            }
            DiscoveryEvent::ProbeFailed {
                instance,
                attempt,
                error,
            } => output.event(
                "probeFailed",
                json!({"instance":instance,"attempt":attempt,"message":error}),
            ),
        },
    )
    .map_err(Failure::from)
}

fn receiver_json(receiver: &NearbyReceiver) -> Value {
    json!({"name":receiver.name,"instance":receiver.service.instance,"address":receiver.service.address.to_string(),"flags":receiver.service.flags})
}

fn send(
    args: &Args,
    name: &str,
    identity: &Identity,
    interface: NetworkInterface,
    output: Output,
    signals: &Signals,
) -> Result<(), Failure> {
    let receivers = discover(args, identity, interface, output, signals)?;
    signals.check()?;
    if receivers.is_empty() {
        return Err(Failure::unavailable("没有发现可用的 AirDrop 接收设备"));
    }
    let selected = if let Some(target) = &args.target {
        let matches: Vec<_> = receivers
            .iter()
            .filter(|receiver| receiver.name.eq_ignore_ascii_case(target))
            .collect();
        match matches.as_slice() {
            [selected] => (*selected).clone(),
            [] => {
                return Err(Failure::unavailable(format!(
                    "没有名为 {target:?} 的接收设备"
                )))
            }
            _ => {
                return Err(Failure::policy(format!(
                    "多个设备名为 {target:?}，请更改设备名称后重试，或在终端省略 --to 按编号选择"
                )))
            }
        }
    } else {
        // Selection has one reader and remains cancellable while stdin waits for a line.
        let (tx, rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            let result = selection::choose(
                &receivers,
                &mut std::io::stdin().lock(),
                &mut std::io::stderr().lock(),
            )
            .map(|choice| choice.cloned());
            let _ = tx.send(result);
        });
        loop {
            signals.check()?;
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(Some(selected))) => break selected,
                Ok(Ok(None)) => return Err(Failure::cancelled()),
                Ok(Err(error)) => return Err(Failure::policy(error)),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Failure::cancelled()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    };
    let options = SendOptions {
        sender_name: name.into(),
        sender_model: args.model.clone(),
        target_name: selected.name.clone(),
        timeout: args.timeout,
        format: args.format,
        ble_wake: args.ble,
        legacy_ble: args.legacy_ble,
    };
    let mut last_progress = None;
    send_to_receiver_cancellable(
        &selected,
        identity,
        &args.files,
        &options,
        &signals.cancel,
        |event| {
            match event {
            SendEvent::Preparing => output.event("preparing", json!({})),
            SendEvent::WaitingForAcceptance { receiver } => output.event("waitingForAcceptance", json!({"receiver":receiver})),
            SendEvent::Uploading { sent, total } => {
                let now = Instant::now();
                if sent != 0 && sent != total && last_progress.is_some_and(|last| now.duration_since(last) < Duration::from_millis(250)) { return; }
                last_progress = Some(now);
                output.event("uploading", json!({"sent":sent,"total":total}));
            }
            SendEvent::WaitingForConfirmation => output.event("waitingForConfirmation", json!({})),
            SendEvent::Completed(report) => output.event("transferCompleted", json!({"direction":"send","receiver":report.receiver,"files":report.files,"sourceBytes":report.source_bytes,"archiveBytes":report.archive_bytes,"format":format!("{:?}", report.format)})),
        }
        },
    )?;
    // The core's final protocol confirmation decides success, including cancellation races.
    Ok(())
}

fn receive(
    args: &Args,
    name: &str,
    identity: Identity,
    interface: Option<NetworkInterface>,
    output: Output,
    signals: &Signals,
) -> Result<(), Failure> {
    signals.check()?;
    let save = match &args.save {
        Some(save) => save.clone(),
        None => omdrop_host::download_dir().map_err(Failure::operation)?,
    };
    let mut options = ReceiverOptions::new(name, &args.model, &save);
    options.accept_all = args.accept_all;
    options.decision_timeout = args.decision_timeout;
    options.allow_dvzip_archive_boundary = interface.is_some();
    let service_id = if interface.is_some() {
        let advertisement =
            omdrop_host::receiver_advertisement(&identity, name).map_err(Failure::operation)?;
        options.retired_service_ids = advertisement.retired_service_ids;
        advertisement.service_id
    } else {
        identity.named_service_id(name)
    };
    let mut core = ReceiverCore::new(options);
    if !args.accept_all {
        let consent =
            consent::Consent::terminal(save.clone(), signals.cancel.clone(), args.decision_timeout);
        core = core.with_decision(move |transfer| consent.decide(transfer));
    }
    let core = Arc::new(core);
    let tls = server_config(&identity).map_err(Failure::operation)?;
    let (tx, events) = mpsc::channel();
    let shutdown = signals.shutdown.clone();
    let bind = args.bind;
    output.event("receiverStarting", json!({"name":name,"save":save,"acceptAll":args.accept_all,"localOnly":interface.is_none()}));
    let worker = thread::spawn(move || {
        if let Some(interface) = interface {
            ReceiverCore::run_listener(interface, 8771, &service_id, tls, core, shutdown, tx)
        } else {
            let listener = TcpListener::bind(bind.expect("validated explicit TCP binding"))?;
            ReceiverCore::run_tcp_listener(listener, tls, core, shutdown, tx)
        }
    });
    while let Ok(event) = events.recv() {
        receiver_event(event, output);
    }
    worker
        .join()
        .map_err(|_| Failure::operation("接收线程意外退出"))?
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AddrInUse {
                Failure::new(6, "busy", error)
            } else {
                Failure::operation(error)
            }
        })?;
    signals.check()
}

fn receiver_event(event: ReceiverEvent, output: Output) {
    match event {
        ReceiverEvent::Listening => output.event("listening", json!({})),
        ReceiverEvent::Discovered => output.event("discovered", json!({})),
        ReceiverEvent::ConnectionFailed { peer, error, kind } => output.event("transferFailed", json!({"direction":"receive","peer":peer.to_string(),"category":kind.as_str(),"message":error})),
        ReceiverEvent::Asked(transfer) => output.event("transferAccepted", json!({"sender":transfer.sender_name,"transferId":transfer.id,"files":transfer.files.iter().map(|f| json!({"name":f.name,"size":f.size})).collect::<Vec<_>>()})),
        ReceiverEvent::Receiving { transfer_id, received, total } => output.event("receiving", json!({"transferId":transfer_id,"received":received,"total":total})),
        ReceiverEvent::ArchiveBoundary { transfer_id } => output.event("uploadArchiveBoundary", json!({"direction":"receive","transferId":transfer_id})),
        ReceiverEvent::Confirming { transfer_id } => output.event("confirming", json!({"direction":"receive","transferId":transfer_id})),
        ReceiverEvent::TransferFailed { transfer_id, error, kind } => output.event("transferFailed", json!({"direction":"receive","transferId":transfer_id,"category":kind.as_str(),"message":error})),
        ReceiverEvent::Received { transfer, files } => output.event("transferCompleted", json!({"direction":"receive","sender":transfer.sender_name,"transferId":transfer.id,"files":files})),
        ReceiverEvent::Rejected { path, status } => output.event("transferRejected", json!({"direction":"receive","path":path,"status":status})),
    }
}

fn doctor(output: Output) -> Result<(), Failure> {
    // Diagnostics never start a service, open a radio, create an identity or set firewall rules.
    let mut failed = false;
    match Client::default().status() {
        Ok(status) => {
            failed |= status.error.is_some() || !status.awdl0;
            output.event("diagnostic", json!({"check":"service","ok":status.error.is_none() && status.awdl0,"status":status}));
        }
        Err(error) => {
            failed = true;
            output.event(
                "diagnostic",
                json!({"check":"service","ok":false,"message":error.to_string()}),
            );
        }
    }
    match omdrop_host::awdl_interface() {
        Ok(interface) => output.event("diagnostic", json!({"check":"interface","ok":true,"name":interface.name,"index":interface.index,"address":interface.link_local.to_string()})),
        Err(error) => { failed = true; output.event("diagnostic", json!({"check":"interface","ok":false,"message":error.to_string()})); }
    }
    match omdrop_host::inspect_name() {
        Ok(name) => output.event(
            "diagnostic",
            json!({"check":"configuration","ok":true,"name":name}),
        ),
        Err(error) => {
            failed = true;
            output.event(
                "diagnostic",
                json!({"check":"configuration","ok":false,"message":error.to_string()}),
            );
        }
    }
    if failed {
        Err(Failure::unavailable(
            "诊断发现未就绪项目，请查看 diagnostic 事件",
        ))
    } else {
        Ok(())
    }
}

fn firmware_info(args: &Args, output: Output) -> Result<(), Failure> {
    let manifest = match &args.manifest {
        Some(path) => path.clone(),
        None => omdrop_flash::installed_manifest().map_err(Failure::unavailable)?,
    };
    let package = omdrop_flash::validate_package(&manifest).map_err(Failure::operation)?;
    output.event(
        "firmwarePackage",
        serde_json::to_value(&package.manifest).map_err(Failure::operation)?,
    );
    let devices = omdrop_flash::enumerate_devices().map_err(Failure::unavailable)?;
    output.event("firmwareDevices", json!({"devices":devices.iter().map(|device| json!({"port":device.port,"label":device.label,"firmwareVersion":null})).collect::<Vec<_>>() }));
    Ok(())
}

fn firmware_flash(args: &Args, output: Output, signals: &Signals) -> Result<(), Failure> {
    signals.check()?;
    let options = omdrop_flash::FlashOptions {
        manifest_path: match &args.manifest {
            Some(path) => path.clone(),
            None => omdrop_flash::installed_manifest().map_err(Failure::unavailable)?,
        },
        tool_path: match &args.tool {
            Some(path) => path.clone(),
            None => omdrop_flash::installed_tool().map_err(Failure::unavailable)?,
        },
        port: args.port.clone().expect("validated port"),
        board: args.board.clone().expect("validated board"),
        mode: if args.reinstall {
            omdrop_flash::InstallMode::Reinstall
        } else {
            omdrop_flash::InstallMode::Upgrade
        },
    };
    if args.reinstall {
        output.event("firmwareReplacement", json!({"message":"本次将替换现有程序和分区布局，不保证保留其他固件数据；写入中取消可能需要完整重刷。"}));
    }
    let report = omdrop_flash::flash(&options, &signals.flash, |event| {
        output.event("firmwareProgress", serde_json::to_value(event).unwrap_or_else(|error| json!({"message":error.to_string()})));
    }).map_err(|error| {
        output.event("firmwareFailed", json!({"stage":error.stage,"possiblyWritten":error.possibly_written,"retryable":error.retryable,"cancelled":error.cancelled,"message":error.message}));
        let code = if error.cancelled { 130 } else if error.busy { 6 }
            else if matches!(error.stage, omdrop_flash::FlashStage::Connect | omdrop_flash::FlashStage::AcquireMaintenance) && !error.possibly_written { 3 } else { 5 };
        Failure::new(code, "firmware", error)
    })?;
    output.event("firmwareCompleted", json!({"port":report.port,"firmwareVersion":report.firmware_version,"imagesWritten":report.images_written,"deviceReady":report.device_ready}));
    Ok(())
}
