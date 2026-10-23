// This test executable doubles as a fake bridge child. It exercises real Windows
// process/event/pipe APIs without opening a radio or modifying installed services.
#[cfg(windows)]
fn main() {
    if let Some(port) = std::env::var_os("OMDROP_TEST_DEVICE_LOCK") {
        let _guard =
            omdrop_service::device_lock::DeviceGuard::try_acquire(port.to_str().unwrap()).unwrap();
        std::fs::write(
            std::env::var_os("OMDROP_TEST_SESSION_READY").unwrap(),
            b"ready",
        )
        .unwrap();
        loop {
            std::thread::park();
        }
    }
    if let Some(pipe) = std::env::var_os("OMDROP_TEST_SESSION_PIPE") {
        let client = omdrop_service::client::Client::new(omdrop_service::client::ClientConfig {
            pipe_name: pipe.into_string().unwrap(),
            ..Default::default()
        })
        .unwrap();
        let _lease = client
            .acquire(omdrop_service::session::SessionKind::Receive)
            .unwrap();
        std::fs::write(
            std::env::var_os("OMDROP_TEST_SESSION_READY").unwrap(),
            b"ready",
        )
        .unwrap();
        loop {
            std::thread::park();
        }
    }
    if std::env::var_os("OMDROP_BRIDGE_STOP_EVENT").is_some() {
        fixture();
        return;
    }
    integration();
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn fixture() {
    use std::{fs, io::Write, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{OpenEventW, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE},
    };
    let dir = std::env::current_dir().unwrap();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let event = std::env::var("OMDROP_BRIDGE_STOP_EVENT").unwrap();
    let mut starts = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("starts.jsonl"))
        .unwrap();
    writeln!(
        starts,
        "{}",
        serde_json::json!({"pid":std::process::id(), "args":args, "event":event})
    )
    .unwrap();
    drop(starts);
    if dir.join("crash").exists() {
        std::process::exit(42);
    }
    let wide: Vec<u16> = event.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, wide.as_ptr()) };
    assert_ne!(handle, ptr::null_mut());
    let ignore = dir.join("ignore-stop").exists();
    loop {
        if unsafe { WaitForSingleObject(handle, 100) } == WAIT_OBJECT_0 && !ignore {
            fs::write(
                dir.join(format!("stopped-{}", std::process::id())),
                b"event received",
            )
            .unwrap();
            unsafe {
                CloseHandle(handle);
            }
            break;
        }
        if ignore {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

#[cfg(windows)]
fn integration() {
    omdrop_service::service::runtime().unwrap().block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(90), checks::run())
            .await
            .expect("integration suite timed out");
    });
    println!("Windows integration passed: real bridge child/events/reaping, config persistence, crash backoff, forced stop, pipe isolation and shutdown");
}

#[cfg(windows)]
mod checks {
    use omdrop_service::{
        config::Config,
        protocol::Status,
        server,
        service::{self, Options},
    };
    use serde_json::{json, Value};
    use std::{
        fs, io,
        path::Path,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::windows::named_pipe::{ClientOptions, NamedPipeClient},
        sync::{oneshot, watch},
    };

    async fn connect(name: &str) -> NamedPipeClient {
        let start = Instant::now();
        loop {
            match ClientOptions::new().open(server::pipe_path(name).unwrap()) {
                Ok(client) => return client,
                Err(e)
                    if start.elapsed() < Duration::from_secs(2)
                        && matches!(e.raw_os_error(), Some(2 | 231)) =>
                {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(e) => panic!("connect: {e}"),
            }
        }
    }
    async fn raw(name: &str, bytes: &[u8]) -> Status {
        let mut client = connect(name).await;
        client.write_all(bytes).await.unwrap();
        let mut line = String::new();
        tokio::time::timeout(
            Duration::from_secs(3),
            BufReader::new(client).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let value: Value = serde_json::from_str(&line).expect("service returned a JSON line");
        assert!(
            value.get("state").is_some(),
            "status responses must use camelCase fields"
        );
        assert!(value.get("State").is_none());
        serde_json::from_value(value).unwrap()
    }
    async fn request(name: &str, json: Value) -> Status {
        let mut bytes = serde_json::to_vec(&json).unwrap();
        bytes.push(b'\n');
        raw(name, &bytes).await
    }
    async fn state(name: &str, expected: &str) -> Status {
        let start = Instant::now();
        loop {
            let status = request(name, json!({"cmd":"status"})).await;
            if status.state == expected {
                return status;
            }
            assert!(
                start.elapsed() < Duration::from_secs(8),
                "expected {expected}, got {status:?}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    fn starts(dir: &Path) -> Vec<Value> {
        fs::read_to_string(dir.join("starts.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
    async fn started(dir: &Path, count: usize) -> Value {
        let start = Instant::now();
        loop {
            let records = starts(dir);
            if records.len() >= count {
                return records[count - 1].clone();
            }
            assert!(
                start.elapsed() < Duration::from_secs(8),
                "expected {count} child starts, got {}",
                records.len()
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }
    fn assert_exited(record: &Value) {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, WAIT_OBJECT_0},
            System::Threading::{OpenProcess, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE},
        };
        let pid = record["pid"].as_u64().unwrap() as u32;
        let handle = unsafe { OpenProcess(SYNCHRONIZATION_SYNCHRONIZE, 0, pid) };
        if !handle.is_null() {
            let result = unsafe { WaitForSingleObject(handle, 0) };
            unsafe {
                CloseHandle(handle);
            }
            assert_eq!(result, WAIT_OBJECT_0, "bridge child {pid} must be gone");
        } else {
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(87));
        }
    }

    pub async fn run() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("radio-target.txt"), b"esp32c5").unwrap();
        let config_path = dir.path().join("config.json");
        let config = Config {
            port: "COM123".into(),
            enabled: false,
            bridge_path: Some(std::env::current_exe().unwrap().to_str().unwrap().into()),
            ..Config::default()
        };
        config.save(&config_path).unwrap();
        let pipe = format!(
            "omdrop-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let options = Options {
            data_dir: dir.path().into(),
            pipe: pipe.clone(),
            scm: false,
            mode: omdrop_service::session::RunMode::Resident,
        };
        let (shutdown, _) = watch::channel(false);
        let (ready, initialized) = oneshot::channel();
        let running = tokio::spawn(service::run(options.clone(), shutdown.clone(), || {
            let _ = ready.send(());
            Ok(())
        }));
        initialized.await.unwrap();
        assert_eq!(
            request(&pipe, json!({"cmd":"status"})).await.state,
            "stopped"
        );
        assert!(
            server::listen(&pipe, true).is_err(),
            "second server must fail before spawning bridge"
        );

        let mut idle = connect(&pipe).await;
        idle.write_all(b"{\"cmd\":").await.unwrap();
        assert!(raw(&pipe, b"{broken\n").await.error.is_some());
        assert!(raw(&pipe, &[b'x'; server::MAX_REQUEST + 1])
            .await
            .error
            .is_some());
        assert!(request(&pipe, json!({"cmd":"set","channel":-1}))
            .await
            .error
            .is_some());
        assert!(request(&pipe, json!({"cmd":"firewall","exe":"bad.exe"}))
            .await
            .error
            .is_some());
        assert!(request(&pipe, json!({"cmd":"unknown"}))
            .await
            .error
            .is_some());
        assert!(request(&pipe, json!({"cmd":"status"}))
            .await
            .error
            .is_none());
        let a = request(&pipe, json!({"Cmd":"start"})).await;
        assert!(a.enabled);
        assert_eq!(a.state, "starting");
        let first = started(dir.path(), 1).await;
        assert_eq!(
            first["args"],
            json!(["--port", "COM123", "--channel", "6", "--managed"])
        );
        assert!(dir.path().join("bridge-console.log").is_file());
        assert!(Config::load(&config_path).unwrap().enabled);
        request(&pipe, json!({"cmd":"start"})).await;
        let before = fs::read(&config_path).unwrap();
        assert!(request(&pipe, json!({"cmd":"set","port":"COM0"}))
            .await
            .error
            .is_some());
        assert_eq!(fs::read(&config_path).unwrap(), before);
        assert_eq!(starts(dir.path()).len(), 1);

        fs::write(dir.path().join("radio-target.txt"), b"esp32c5").unwrap();
        let target_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if request(&pipe, json!({"cmd":"status"}))
                .await
                .target
                .as_deref()
                == Some("esp32c5")
            {
                break;
            }
            assert!(
                Instant::now() < target_deadline,
                "board target was not reported"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let receive_lease =
            omdrop_service::client::Client::new(omdrop_service::client::ClientConfig {
                pipe_name: pipe.clone(),
                ..Default::default()
            })
            .unwrap()
            .acquire_async(omdrop_service::session::SessionKind::Receive)
            .await
            .unwrap();
        let busy = request(&pipe, json!({"cmd":"set","channel":6})).await;
        assert!(busy
            .error
            .as_deref()
            .is_some_and(|error| error.contains("busy")));
        receive_lease.finish_maintenance_async(false).await.unwrap();
        request(&pipe, json!({"cmd":"set","port":"com124","channel":6})).await;
        let second = started(dir.path(), 2).await;
        assert_exited(&first);
        assert!(dir
            .path()
            .join(format!("stopped-{}", first["pid"]))
            .exists());
        assert_eq!(second["args"][1], "COM124");
        assert_eq!(second["args"][3], "6");
        assert_ne!(first["event"], second["event"]);

        // Controlled crash on the next start, then recover after the retry delay.
        fs::write(dir.path().join("crash"), b"42").unwrap();
        request(&pipe, json!({"cmd":"set","port":"com125","channel":6})).await;
        let crashed = started(dir.path(), 3).await;
        state(&pipe, "error").await;
        assert_exited(&crashed);
        fs::remove_file(dir.path().join("crash")).unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            starts(dir.path()).len(),
            3,
            "crash must not cause a tight spawn loop"
        );
        let fourth = started(dir.path(), 4).await;
        request(&pipe, json!({"cmd":"stop"})).await;
        state(&pipe, "stopped").await;
        assert_exited(&fourth);
        assert!(!Config::load(&config_path).unwrap().enabled);

        // A stuck child is killed and reaped after five seconds, while other pipe
        // connections continue answering during the grace interval.
        fs::write(dir.path().join("ignore-stop"), b"ignore").unwrap();
        request(&pipe, json!({"cmd":"start"})).await;
        let fifth = started(dir.path(), 5).await;
        let stopping = Instant::now();
        assert_eq!(
            request(&pipe, json!({"cmd":"stop"})).await.state,
            "stopping"
        );
        for _ in 0..4 {
            assert_eq!(
                request(&pipe, json!({"cmd":"status"})).await.state,
                "stopping"
            );
        }
        state(&pipe, "stopped").await;
        assert!(stopping.elapsed() >= Duration::from_secs(5));
        assert_exited(&fifth);
        assert!(!dir
            .path()
            .join(format!("stopped-{}", fifth["pid"]))
            .exists());
        fs::remove_file(dir.path().join("ignore-stop")).unwrap();
        let mut byte = [0];
        if let Ok(n) = tokio::time::timeout(Duration::from_secs(1), idle.read(&mut byte))
            .await
            .unwrap()
        {
            assert_eq!(n, 0, "idle request must expire");
        }

        request(&pipe, json!({"cmd":"start"})).await;
        let sixth = started(dir.path(), 6).await;
        let _idle_at_shutdown = connect(&pipe).await;
        shutdown.send_replace(true);
        tokio::time::timeout(Duration::from_secs(3), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_exited(&sixth);
        assert!(
            Config::load(&config_path).unwrap().enabled,
            "shutdown must preserve the desired state"
        );

        // A fresh service instance reuses the saved port/channel/enabled state.
        let (stop_again, _) = watch::channel(false);
        let (ready_again, initialized_again) = oneshot::channel();
        let restarted = tokio::spawn(service::run(options, stop_again.clone(), || {
            let _ = ready_again.send(());
            Ok(())
        }));
        if initialized_again.await.is_err() {
            panic!("restart initialization failed: {:?}", restarted.await);
        }
        let seventh = started(dir.path(), 7).await;
        assert_eq!(seventh["args"][1], "COM125");
        stop_again.send_replace(true);
        tokio::time::timeout(Duration::from_secs(3), restarted)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_exited(&seventh);
        rust_backend().await;
        sessions().await;
    }

    async fn rust_backend() {
        use omdrop_service::{
            platform::WindowsBackend,
            supervisor::{Backend, ManagedProcess},
        };
        let dir = tempfile::tempdir().unwrap();
        fs::copy(
            std::env::current_exe().unwrap(),
            dir.path().join("omdrop-bridge.exe"),
        )
        .unwrap();
        let config = Config {
            port: "COM126".into(),
            ..Config::default()
        };
        let mut backend = WindowsBackend {
            data_dir: dir.path().to_owned(),
            default_bridge: dir.path().join("omdrop-bridge.exe"),
            target_file: dir.path().join("radio-target.txt"),
        };
        let mut child = backend.spawn(&config, &config.port).unwrap();
        let start = started(dir.path(), 1).await;
        assert_eq!(
            start["args"],
            json!(["--port", "COM126", "--channel", "6", "--managed"])
        );
        child.request_stop().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while child.exit_code().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "Rust backend did not exit after stop event"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(dir
            .path()
            .join(format!("stopped-{}", start["pid"]))
            .is_file());
        assert_exited(&start);
        drop(child);
        let child = backend.spawn(&config, &config.port).unwrap();
        let start = started(dir.path(), 2).await;
        drop(child);
        assert_exited(&start);
    }

    async fn sessions() {
        use omdrop_service::{
            client::{Client, ClientConfig, ClientErrorKind},
            session::{RunMode, SessionKind},
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        Config {
            port: "COM125".into(),
            enabled: true,
            bridge_path: Some(std::env::current_exe().unwrap().to_str().unwrap().into()),
            ..Config::default()
        }
        .save(&path)
        .unwrap();
        let original = fs::read(&path).unwrap();
        let pipe = format!(
            "omdrop-session-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut options = Options {
            data_dir: dir.path().into(),
            pipe: pipe.clone(),
            scm: false,
            mode: RunMode::Resident,
        };
        let client = Client::new(ClientConfig {
            pipe_name: pipe.clone(),
            timeout: Duration::from_secs(12),
        })
        .unwrap();
        let (stop, _) = watch::channel(false);
        let (ready, initialized) = oneshot::channel();
        let running = tokio::spawn(service::run(options.clone(), stop.clone(), || {
            let _ = ready.send(());
            Ok(())
        }));
        initialized.await.unwrap();
        let first = started(dir.path(), 1).await;
        let session_pipe = format!("{pipe}.sessions");
        for (version, pid, code) in [(999, std::process::id(), "upgrade"), (1, 0, "identity")] {
            let mut connection = connect(&session_pipe).await;
            connection
                .write_all(
                    format!("{}\n", json!({"version":version,"pid":pid,"kind":"client"}))
                        .as_bytes(),
                )
                .await
                .unwrap();
            let mut line = String::new();
            BufReader::new(connection)
                .read_line(&mut line)
                .await
                .unwrap();
            let reply: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(reply["code"], code);
        }
        // A token copied from another service instance is rejected on its own pipe.
        let mut forged = connect(&session_pipe).await;
        forged
            .write_all(
                format!(
                    "{}\n",
                    json!({"version":1,"pid":std::process::id(),"kind":"client"})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(&mut forged)
            .read_line(&mut line)
            .await
            .unwrap();
        let granted: Value = serde_json::from_str(&line).unwrap();
        forged.write_all(format!("{}\n",json!({"instance":"another-instance","token":granted["token"],"deviceReady":true})).as_bytes()).await.unwrap();
        let mut byte = [0];
        let closed = tokio::time::timeout(Duration::from_secs(2), forged.read(&mut byte))
            .await
            .unwrap();
        assert!(closed.is_err() || closed.unwrap() == 0);
        let a = client.acquire_async(SessionKind::Client).await.unwrap();
        let b = client.acquire_async(SessionKind::Client).await.unwrap();
        let send = client.acquire_async(SessionKind::Send).await.unwrap();
        let receive = client.acquire_async(SessionKind::Receive).await.unwrap();
        for kind in [
            SessionKind::Send,
            SessionKind::Receive,
            SessionKind::Maintenance,
        ] {
            assert_eq!(
                client.acquire_async(kind).await.unwrap_err().kind(),
                ClientErrorKind::Busy
            );
        }
        assert!(!a.release_async().await.unwrap().service_stopping);
        assert!(!b.release_async().await.unwrap().service_stopping);
        send.release_async().await.unwrap();
        receive.release_async().await.unwrap();
        assert!(
            client.status_async().await.is_ok(),
            "resident must remain after its final client"
        );

        let maintenance = client
            .acquire_async(SessionKind::Maintenance)
            .await
            .unwrap();
        assert_exited(&first);
        assert_eq!(client.status_async().await.unwrap().state, "maintenance");
        assert_eq!(
            client
                .acquire_async(SessionKind::Send)
                .await
                .unwrap_err()
                .kind(),
            ClientErrorKind::Busy
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            original,
            "maintenance must preserve enabled config"
        );
        maintenance.finish_maintenance_async(true).await.unwrap();
        let second = started(dir.path(), 2).await;
        let maintenance = client
            .acquire_async(SessionKind::Maintenance)
            .await
            .unwrap();
        assert_exited(&second);
        drop(maintenance);
        state(&pipe, "maintenance-failed").await;
        stop.send_replace(true);
        running.await.unwrap().unwrap();
        // Failure survives a full service restart; recovery must explicitly confirm readiness.
        let (stop, _) = watch::channel(false);
        let (ready, initialized) = oneshot::channel();
        let running = tokio::spawn(service::run(options.clone(), stop.clone(), || {
            let _ = ready.send(());
            Ok(())
        }));
        initialized.await.unwrap();
        state(&pipe, "maintenance-failed").await;
        assert_eq!(starts(dir.path()).len(), 2);
        let recovery = client
            .acquire_async(SessionKind::Maintenance)
            .await
            .unwrap();
        recovery.finish_maintenance_async(true).await.unwrap();
        let third = started(dir.path(), 3).await;
        assert!(!dir.path().join("maintenance-failed").exists());

        // A real client process exits without a release message. The service reclaims
        // its receive lane while the resident supervisor and bridge remain available.
        let ready_file = dir.path().join("client-ready");
        let mut crashed = std::process::Command::new(std::env::current_exe().unwrap())
            .env("OMDROP_TEST_SESSION_PIPE", &pipe)
            .env("OMDROP_TEST_SESSION_READY", &ready_file)
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !ready_file.exists() {
            assert!(start.elapsed() < Duration::from_secs(4));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            client
                .acquire_async(SessionKind::Receive)
                .await
                .unwrap_err()
                .kind(),
            ClientErrorKind::Busy
        );
        crashed.kill().unwrap();
        crashed.wait().unwrap();
        let start = Instant::now();
        let receiver = loop {
            match client.acquire_async(SessionKind::Receive).await {
                Ok(lease) => break lease,
                Err(e)
                    if e.kind() == ClientErrorKind::Busy
                        && start.elapsed() < Duration::from_secs(4) =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(e) => panic!("receive lane not reclaimed: {e}"),
            }
        };
        receiver.release_async().await.unwrap();

        // Old leases cannot be reused after a service restart.
        let stale = client.acquire_async(SessionKind::Client).await.unwrap();
        stop.send_replace(true);
        running.await.unwrap().unwrap();
        assert_exited(&third);
        assert!(stale.release_async().await.is_err());
        options.mode = RunMode::OnDemand;
        for reverse in [false, true] {
            if reverse {
                fs::write(dir.path().join("ignore-stop"), b"ignore").unwrap();
            }
            let (stop, _) = watch::channel(false);
            let (ready, initialized) = oneshot::channel();
            let running = tokio::spawn(service::run(options.clone(), stop, || {
                let _ = ready.send(());
                Ok(())
            }));
            initialized.await.unwrap();
            let a = client.acquire_async(SessionKind::Client).await.unwrap();
            let b = client.acquire_async(SessionKind::Send).await.unwrap();
            let record = started(dir.path(), if reverse { 5 } else { 4 }).await;
            let (first, last) = if reverse { (b, a) } else { (a, b) };
            let first = first.release_async().await.unwrap();
            assert_eq!(first.remaining_clients, 1);
            assert!(!first.service_stopping);
            let last = if reverse {
                let release = tokio::spawn(last.release_async());
                state(&pipe, "stopping").await;
                assert_eq!(
                    client
                        .acquire_async(SessionKind::Client)
                        .await
                        .unwrap_err()
                        .kind(),
                    ClientErrorKind::Stopping
                );
                release.await.unwrap().unwrap()
            } else {
                last.release_async().await.unwrap()
            };
            assert!(last.service_stopping);
            assert_eq!(last.remaining_clients, 0);
            assert_exited(&record);
            tokio::time::timeout(Duration::from_secs(3), running)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }
        assert_eq!(fs::read(&path).unwrap(), original);
        // Kernel ownership recovers even when the mutex owner process is killed.
        fs::remove_file(&ready_file).unwrap();
        let port = format!("COM{}", 40000 + (std::process::id() % 10000));
        let mut owner = std::process::Command::new(std::env::current_exe().unwrap())
            .env("OMDROP_TEST_DEVICE_LOCK", &port)
            .env("OMDROP_TEST_SESSION_READY", &ready_file)
            .spawn()
            .unwrap();
        let start = Instant::now();
        while !ready_file.exists() {
            assert!(start.elapsed() < Duration::from_secs(4));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            omdrop_service::device_lock::DeviceGuard::try_acquire(&port)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        owner.kill().unwrap();
        owner.wait().unwrap();
        assert!(omdrop_service::device_lock::DeviceGuard::try_acquire(&port).is_ok());
    }
}
