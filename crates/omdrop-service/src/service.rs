use crate::{
    config::Config,
    platform::WindowsBackend,
    server::{self, Control, Handler},
    session::{Registry, Reply, RunMode, SessionKind, CAPABILITIES, VERSION},
    session_server::{self, Event as SessionEvent},
    supervisor::Supervisor,
};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::{Arc, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, watch, Semaphore};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult},
    service_dispatcher,
};

pub const SERVICE_NAME: &str = "OmdropOwl";

#[derive(Clone, Debug)]
pub struct Options {
    pub data_dir: PathBuf,
    pub pipe: String,
    pub scm: bool,
    pub mode: RunMode,
}
impl Options {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> io::Result<Option<Self>> {
        let mut args = args.into_iter();
        let mut data_dir =
            std::env::var_os("ProgramData").map(|p| PathBuf::from(p).join("omdrop-owl"));
        let mut pipe = "omdrop-owl".to_owned();
        let mut scm = false;
        let mut mode = RunMode::Resident;
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--service") => scm = true,
                Some("--console") => scm = false,
                Some("--on-demand") => mode = RunMode::OnDemand,
                Some("--resident") => mode = RunMode::Resident,
                Some("--data-dir") => {
                    data_dir = Some(PathBuf::from(
                        args.next()
                            .ok_or_else(|| invalid("--data-dir requires a path"))?,
                    ))
                }
                Some("--pipe") => {
                    pipe = args
                        .next()
                        .and_then(|s| s.into_string().ok())
                        .ok_or_else(|| invalid("--pipe requires a name"))?
                }
                Some("--help" | "-h") => return Ok(None),
                _ => {
                    return Err(invalid(format!(
                        "unknown argument: {}",
                        arg.to_string_lossy()
                    )))
                }
            }
        }
        server::pipe_path(&pipe)?;
        let data_dir =
            data_dir.ok_or_else(|| invalid("ProgramData is undefined; specify --data-dir"))?;
        let data_dir = std::path::absolute(data_dir)?;
        Ok(Some(Self {
            data_dir,
            pipe,
            scm,
            mode,
        }))
    }
}
fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.into())
}

fn save_maintenance_marker(path: &std::path::Path) -> io::Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or_else(|| invalid("maintenance path has no parent"))?,
    )?;
    file.write_all(b"Device readiness must be confirmed by a successful maintenance session.\n")?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn log(options: &Options, message: impl AsRef<str>) {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(options.data_dir.join("service.log"))
    {
        let _ = writeln!(file, "{time} {}", message.as_ref());
    }
    if !options.scm {
        eprintln!("{}", message.as_ref());
    }
}

pub fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

pub async fn run(
    options: Options,
    shutdown: watch::Sender<bool>,
    ready: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    fs::create_dir_all(&options.data_dir)?;
    let config_path = options.data_dir.join("config.json");
    let config = Config::load(&config_path)?;
    let maintenance_path = options.data_dir.join("maintenance-failed");
    let maintenance_failed = maintenance_path.try_exists()?;
    // Bind before starting wireless bridge. A duplicate service must not launch another radio.
    let listener = server::listen(&options.pipe, true)?;
    let session_name = format!("{}.sessions", options.pipe);
    let session_listener = server::listen(&session_name, true)?;
    let default_bridge = std::env::current_exe()?.with_file_name("omdrop-bridge.exe");
    let target_file = options.data_dir.join("radio-target.txt");
    let mut radio = Supervisor::new(
        WindowsBackend {
            data_dir: options.data_dir.clone(),
            default_bridge,
            target_file,
        },
        config,
        config_path,
    );
    let status = Arc::new(RwLock::new(radio.status()));
    let (commands, mut requests) = mpsc::channel::<Control>(32);
    let handler = Handler {
        commands,
        status: status.clone(),
        firewall_gate: Arc::new(Semaphore::new(1)),
    };
    let mut server = tokio::spawn(server::serve(
        options.pipe.clone(),
        listener,
        handler,
        shutdown.subscribe(),
    ));
    let (session_events, mut session_requests) = mpsc::unbounded_channel();
    let mut sessions = tokio::spawn(session_server::serve(
        session_name,
        session_listener,
        session_events,
        shutdown.subscribe(),
    ));
    let instance = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let mut registry = Registry {
        maintenance_failed,
        ..Registry::default()
    };
    if registry.maintenance_failed {
        radio.maintenance(true, Duration::ZERO);
    }
    if let Err(e) = ready() {
        shutdown.send_replace(true);
        let _ = server.await;
        let _ = sessions.await;
        return Err(e);
    }
    log(
        &options,
        format!(
            "service ready; pipe {}; config {}",
            options.pipe,
            options.data_dir.join("config.json").display()
        ),
    );
    let start = Instant::now();
    let mut timer = tokio::time::interval(Duration::from_millis(200));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut cancel = shutdown.subscribe();
    let mut old_state = String::new();
    let mut server_finished = false;
    let mut sessions_finished = false;
    let mut maintenance_pending: Option<(u64, tokio::sync::oneshot::Sender<Reply>)> = None;
    let mut release_pending: Option<tokio::sync::oneshot::Sender<Reply>> = None;
    let mut drain_reply: Option<Reply> = None;
    let mut drain_wait: Option<Instant> = None;
    let mut drain_confirmed = false;
    let session_reply = |token| Reply {
        version: VERSION,
        instance: instance.clone(),
        token,
        capabilities: CAPABILITIES.iter().map(|s| (*s).into()).collect(),
        ..Reply::default()
    };
    let outcome = loop {
        if *cancel.borrow() {
            break Ok(());
        }
        tokio::select! {
            _ = cancel.changed() => break Ok(()),
            result = &mut server => {
                server_finished = true;
                break result.map_err(io::Error::other).and_then(|r| r);
            }
            result = &mut sessions => {
                sessions_finished = true;
                break result.map_err(io::Error::other).and_then(|r| r);
            }
            Some(event) = session_requests.recv() => {
                match event {
                    SessionEvent::Acquire { kind, response } => {
                        if response.is_closed() { continue; }
                        let previous_maintenance_failed = registry.maintenance_failed;
                        match registry.acquire(kind) {
                            Err(code) => { let _ = response.send(Reply::error(code, if code == "stopping" { "service is stopping; wait for restart" } else { "requested radio role is busy or device recovery is required" })); }
                            Ok(token) if kind == SessionKind::Maintenance => {
                                // Persist before granting access, so a service/client crash never
                                // silently restarts wireless bridge against an unfinished flash operation.
                                match save_maintenance_marker(&maintenance_path) {
                                    Ok(()) => { radio.maintenance(true, start.elapsed()); maintenance_pending = Some((token,response)); }
                                    Err(e) => {
                                        // Admission failed before the client touched the radio;
                                        // retain any existing recovery requirement exactly.
                                        registry.release(token,!previous_maintenance_failed,options.mode);
                                        if registry.closing { drain_confirmed = true; radio.shutdown(start.elapsed()); }
                                        let _ = response.send(Reply::error("service",e));
                                    }
                                }
                            }
                            Ok(token) => {
                                if response.send(session_reply(token)).is_err() {
                                    registry.release(token,false,options.mode);
                                    if registry.closing { drain_confirmed = true; radio.shutdown(start.elapsed()); }
                                }
                            }
                        }
                    }
                    SessionEvent::Release { token, mut ready, response } => {
                        let maintenance = registry.entries.get(&token) == Some(&SessionKind::Maintenance);
                        let mut restore_error = None;
                        if maintenance && ready {
                            if let Err(e) = fs::remove_file(&maintenance_path) { if e.kind() != io::ErrorKind::NotFound { ready = false; restore_error = Some(e.to_string()); } }
                        }
                        let outcome = registry.release(token,ready,options.mode);
                        if maintenance && !registry.closing { radio.maintenance(registry.maintenance(),start.elapsed()); }
                        let mut reply = session_reply(token); reply.outcome = outcome;
                        reply.error = restore_error;
                        if registry.closing {
                            radio.shutdown(start.elapsed());
                            drain_confirmed = response.is_none();
                            release_pending = response;
                            drain_reply = Some(reply);
                        } else if let Some(response) = response { let _ = response.send(reply); }
                    }
                    SessionEvent::ConfirmDrain => drain_confirmed = true,
                }
            }
            Some(control) = requests.recv() => {
                // Clients that gave up before dispatch must not leave surprise
                // start/stop commands waiting behind a busy queue.
                if !control.response.is_closed() {
                    let reconfigures_radio = control.request.cmd == "set"
                        && (control.request.port.is_some() || control.request.channel.is_some());
                    let in_use = registry.radio_reconfiguration_busy();
                    let before = radio.status();
                    let was_ready = before.state == "running" && before.awdl0 && before.error.is_none();
                    let reply = if reconfigures_radio && in_use {
                        before.with_error("radio is busy; finish transfers and close receivers before changing its configuration")
                    } else {
                        radio.command(control.request, start.elapsed())
                    };
                    if reconfigures_radio
                        && !in_use
                        && was_ready
                        && matches!(reply.state.as_str(), "stopping" | "starting")
                    {
                        registry.radio_reconfiguring = true;
                    }
                    let _ = control.response.send(reply);
                }
            }
            _ = timer.tick() => {
                if options.mode == RunMode::Resident || registry.used || registry.closing { radio.tick(start.elapsed()); }
                if options.mode == RunMode::OnDemand && !registry.used && start.elapsed() >= Duration::from_secs(30) {
                    registry.closing = true; drain_confirmed = true; radio.shutdown(start.elapsed());
                }
            },
        }
        if radio.stopped() {
            if let Some((token, response)) = maintenance_pending.take() {
                if response.send(session_reply(token)).is_err() {
                    registry.release(token, false, options.mode);
                    if registry.closing {
                        drain_confirmed = true;
                        radio.shutdown(start.elapsed());
                    }
                }
            }
            if registry.closing {
                if let Some(response) = release_pending.take() {
                    if response
                        .send(drain_reply.take().unwrap_or_else(|| session_reply(0)))
                        .is_err()
                    {
                        drain_confirmed = true;
                    }
                    drain_wait = Some(Instant::now());
                }
                if drain_confirmed
                    || drain_wait.is_some_and(|t| t.elapsed() > Duration::from_secs(6))
                {
                    break Ok(());
                }
            }
        }
        let mut snapshot = radio.status();
        if registry.radio_reconfiguring
            && !matches!(snapshot.state.as_str(), "stopping" | "starting")
        {
            registry.radio_reconfiguring = false;
        }
        if registry.maintenance_failed && !registry.closing {
            snapshot.state = "maintenance-failed".into();
            snapshot.error = Some(
                "device readiness is unconfirmed; retry firmware maintenance to recover".into(),
            );
        }
        let state = format!(
            "{}: {}",
            snapshot.state,
            snapshot.error.as_deref().unwrap_or("")
        );
        if state != old_state {
            log(&options, format!("radio {state}"));
            old_state = state;
        }
        *status.write().unwrap_or_else(|e| e.into_inner()) = snapshot;
    };
    shutdown.send_replace(true);
    if !server_finished {
        let _ = server.await;
    }
    if !sessions_finished {
        let _ = sessions.await;
    }
    radio.shutdown(start.elapsed());
    let stop_deadline = Instant::now() + Duration::from_secs(10);
    while !radio.stopped() {
        radio.tick(start.elapsed());
        if Instant::now() >= stop_deadline {
            // Dropping the Windows job provides a final kernel-level cleanup;
            // return a failure to SCM rather than remaining StopPending forever.
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "wireless bridge did not stop within 10 seconds",
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    log(&options, "service stopped; wireless bridge reaped");
    outcome
}

define_windows_service!(ffi_service_main, scm_main);

pub fn dispatch() -> Result<(), windows_service::Error> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

fn status(state: ServiceState, exit_code: ServiceExitCode) -> ServiceStatus {
    let pending = matches!(
        state,
        ServiceState::StartPending | ServiceState::StopPending
    );
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code,
        checkpoint: u32::from(pending),
        wait_hint: if pending {
            Duration::from_secs(15)
        } else {
            Duration::ZERO
        },
        process_id: None,
    }
}

fn scm_main(_args: Vec<OsString>) {
    let (shutdown, _) = watch::channel(false);
    let stop = shutdown.clone();
    // Registration must precede configuration I/O so SCM sees initialization
    // failures as Stopped/nonzero instead of a misleading dispatch timeout.
    let handle = match service_control_handler::register(SERVICE_NAME, move |control| match control
    {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            stop.send_replace(true);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    }) {
        Ok(handle) => handle,
        Err(_) => return,
    };
    let _ = handle.set_service_status(status(
        ServiceState::StartPending,
        ServiceExitCode::Win32(0),
    ));
    let result = (|| -> io::Result<()> {
        let options = Options::parse(std::env::args_os().skip(1))?
            .ok_or_else(|| invalid("--help cannot run under SCM"))?;
        let rt = runtime()?;
        let log_options = options.clone();
        rt.block_on(async {
            let mut stopping = shutdown.subscribe();
            let monitor = tokio::spawn(async move {
                if !*stopping.borrow() {
                    let _ = stopping.changed().await;
                }
                let _ = handle.set_service_status(status(
                    ServiceState::StopPending,
                    ServiceExitCode::Win32(0),
                ));
            });
            let result = run(options, shutdown, || {
                handle
                    .set_service_status(status(ServiceState::Running, ServiceExitCode::Win32(0)))
                    .map_err(io::Error::other)
            })
            .await;
            monitor.abort();
            let _ = monitor.await;
            if let Err(e) = &result {
                log(&log_options, format!("service failed: {e}"));
            }
            result
        })
    })();
    let code = if result.is_ok() {
        ServiceExitCode::Win32(0)
    } else {
        ServiceExitCode::ServiceSpecific(1)
    };
    let _ = handle.set_service_status(status(ServiceState::Stopped, code));
}
