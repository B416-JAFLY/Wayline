use crate::{
    config::Config,
    protocol::{Request, Status},
};
use std::{io, path::PathBuf, time::Duration};

const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(60);
const STOP_GRACE: Duration = Duration::from_secs(5);

pub trait ManagedProcess {
    /// Polls and reaps an exited child. None means it is still alive.
    fn exit_code(&mut self) -> io::Result<Option<String>>;
    fn request_stop(&mut self) -> io::Result<()>;
    fn kill_and_wait(&mut self) -> io::Result<()>;
}

pub trait Backend {
    type Process: ManagedProcess;
    fn find_port(&mut self) -> io::Result<Option<String>>;
    fn awdl_ready(&mut self) -> io::Result<bool>;
    fn spawn(&mut self, config: &Config, port: &str) -> io::Result<Self::Process>;
    fn radio_target(&mut self) -> Option<String> {
        None
    }
}

/// The only owner of the child process. All transitions run on one task; stopping
/// is polled so a slow child never prevents status requests or SCM cancellation.
pub struct Supervisor<B: Backend> {
    backend: B,
    config: Config,
    config_path: PathBuf,
    child: Option<B::Process>,
    status: Status,
    stopping: Option<Duration>,
    shutdown: bool,
    maintenance: bool,
    retry_at: Duration,
    retry_delay: Duration,
    started_at: Duration,
}

impl<B: Backend> Supervisor<B> {
    pub fn new(backend: B, config: Config, config_path: PathBuf) -> Self {
        let status = Status {
            state: "stopped".into(),
            port: None,
            channel: config.channel,
            target: None,
            awdl0: false,
            error: None,
            enabled: config.enabled,
        };
        Self {
            backend,
            config,
            config_path,
            child: None,
            status,
            stopping: None,
            shutdown: false,
            maintenance: false,
            retry_at: Duration::ZERO,
            retry_delay: RETRY_MIN,
            started_at: Duration::ZERO,
        }
    }

    pub fn status(&self) -> Status {
        self.status.clone()
    }

    pub fn command(&mut self, req: Request, now: Duration) -> Status {
        match self.configure(req, now) {
            Ok(()) => self.status(),
            Err(e) => self.status().with_error(e),
        }
    }

    fn configure(&mut self, req: Request, now: Duration) -> io::Result<()> {
        let mut next = self.config.clone();
        match req.cmd.as_str() {
            "status" => return Ok(()),
            "start" => next.enabled = true,
            "stop" => next.enabled = false,
            "set" => {
                if let Some(port) = req.port {
                    next.port = port;
                }
                if let Some(channel) = req.channel {
                    validate_target_channel(&self.status, channel)?;
                    next.channel = channel;
                }
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unknown command",
                ))
            }
        }
        next.validate()?;
        let port_changed = next.port != self.config.port;
        if port_changed && next.channel != 6 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "switch to channel 6 before changing the radio port, then confirm the new board capabilities",
            ));
        }
        if next != self.config {
            // Commit before changing the live process. Invalid/unwritable settings
            // leave both the saved config and the running child untouched.
            next.save(&self.config_path)?;
            let restart = next.port != self.config.port || next.channel != self.config.channel;
            self.config = next;
            self.status.enabled = self.config.enabled;
            self.status.channel = self.config.channel;
            if restart || !self.config.enabled {
                self.begin_stop(now);
            }
            self.retry_at = now;
            self.retry_delay = RETRY_MIN;
        } else if req.cmd == "start" && self.child.is_none() {
            self.retry_at = now;
        }
        self.tick(now);
        Ok(())
    }

    pub fn shutdown(&mut self, now: Duration) {
        // Shutdown is transient. It must not persist Enabled=false, otherwise a
        // service restarted by Windows would leave the radio disabled forever.
        self.shutdown = true;
        self.begin_stop(now);
    }

    pub fn stopped(&self) -> bool {
        self.child.is_none()
    }

    /// Temporarily release the radio without changing the saved enabled setting.
    pub fn maintenance(&mut self, paused: bool, now: Duration) {
        self.maintenance = paused;
        if paused {
            self.begin_stop(now);
        } else {
            self.retry_at = now;
        }
        self.tick(now);
    }

    fn begin_stop(&mut self, now: Duration) {
        if self.stopping.is_none() {
            if let Some(child) = &mut self.child {
                self.stopping = Some(now + STOP_GRACE);
                self.status.state = "stopping".into();
                self.status.error = child
                    .request_stop()
                    .err()
                    .map(|e| format!("wireless bridge stop event: {e}"));
            }
        }
    }

    fn failed(&mut self, error: impl ToString, now: Duration) {
        self.status.state = "error".into();
        self.status.error = Some(error.to_string());
        self.retry_at = now + self.retry_delay;
        self.retry_delay = (self.retry_delay * 2).min(RETRY_MAX);
    }

    pub fn tick(&mut self, now: Duration) {
        let executable = "omdrop-bridge.exe";
        self.status.target = self.backend.radio_target();
        let adapter_error = match self.backend.awdl_ready() {
            Ok(up) => {
                self.status.awdl0 = up;
                None
            }
            Err(e) => {
                self.status.awdl0 = false;
                Some(format!("adapter discovery: {e}"))
            }
        };
        if let Some(child) = &mut self.child {
            match child.exit_code() {
                Ok(Some(code)) => {
                    self.child = None;
                    if self.stopping.take().is_none() && self.config.enabled && !self.shutdown {
                        self.failed(
                            format!("{executable} exited ({code}); waiting to restart"),
                            now,
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    self.begin_stop(now);
                    self.status.error = Some(format!("cannot inspect {executable}: {e}"));
                }
            }
        }
        if let Some(deadline) = self.stopping {
            if now >= deadline {
                if let Some(child) = &mut self.child {
                    match child.kill_and_wait() {
                        Ok(()) => {
                            self.child = None;
                            self.stopping = None;
                        }
                        Err(e) => {
                            self.status.error = Some(format!("cannot terminate {executable}: {e}"));
                            self.stopping = Some(now + Duration::from_secs(1));
                        }
                    }
                }
            }
        }
        if self.child.is_some() {
            if self.stopping.is_none() {
                self.status.error = adapter_error;
                self.status.state = if self.status.awdl0 {
                    "running"
                } else {
                    "starting"
                }
                .into();
                // Short-lived processes must not reset an exponentially increasing
                // crash delay just because an adapter appeared briefly.
                if now.saturating_sub(self.started_at) >= RETRY_MAX {
                    self.retry_delay = RETRY_MIN;
                }
            }
            return;
        }
        if self.maintenance && !self.shutdown {
            self.status.state = "maintenance".into();
            self.status.awdl0 = false;
            self.status.error = None;
            return;
        }
        if self.shutdown || !self.config.enabled {
            self.status.state = "stopped".into();
            self.status.port = None;
            self.status.error = None;
            return;
        }
        if now < self.retry_at {
            return;
        }
        let port = if self.config.port == "auto" {
            match self.backend.find_port() {
                Ok(port) => port,
                Err(e) => {
                    self.failed(format!("serial discovery: {e}"), now);
                    return;
                }
            }
        } else {
            Some(self.config.port.clone())
        };
        self.status.port = port.clone();
        let Some(port) = port else {
            self.status.state = "no-radio".into();
            self.status.error =
                Some("no ESP32 USB serial port found; connect the radio or set a COM port".into());
            self.retry_at = now + RETRY_MIN;
            return;
        };
        match self.backend.spawn(&self.config, &port) {
            Ok(child) => {
                self.child = Some(child);
                self.started_at = now;
                self.status.state = "starting".into();
                self.status.error = None;
            }
            Err(e) => {
                self.failed(format!("{executable}: {e}"), now);
                #[cfg(windows)]
                if e.get_ref()
                    .is_some_and(|error| error.is::<crate::device_lock::DeviceNotReady>())
                {
                    self.status.state = "maintenance-failed".into();
                }
            }
        }
    }
}

fn validate_target_channel(status: &Status, channel: u16) -> io::Result<()> {
    if !matches!(channel, 6 | 44 | 149) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "channel must be 6, 44 or 149",
        ));
    }
    let target = status.target.as_deref();
    if channel == 6
        || (matches!((target, channel), (Some("esp32c5"), 44 | 149))
            && status.state == "running"
            && status.awdl0
            && status.error.is_none())
    {
        return Ok(());
    }
    let message = if target == Some("esp32s3") {
        "ESP32-S3 supports only channel 6"
    } else if target == Some("esp32c5") {
        "5 GHz requires a running ESP32-C5 radio"
    } else {
        "5 GHz requires a running ESP32-C5 radio; board capabilities are unknown"
    };
    Err(io::Error::new(io::ErrorKind::InvalidInput, message))
}

impl<B: Backend> Drop for Supervisor<B> {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill_and_wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Default)]
    struct Observed {
        starts: Vec<(String, u16)>,
        stops: usize,
        kills: usize,
        exited: bool,
        present: bool,
        adapter_error: bool,
        adapter_up: bool,
    }
    struct Fake(Rc<RefCell<Observed>>);
    impl ManagedProcess for Fake {
        fn exit_code(&mut self) -> io::Result<Option<String>> {
            Ok(self.0.borrow().exited.then(|| "42".into()))
        }
        fn request_stop(&mut self) -> io::Result<()> {
            self.0.borrow_mut().stops += 1;
            Ok(())
        }
        fn kill_and_wait(&mut self) -> io::Result<()> {
            self.0.borrow_mut().kills += 1;
            Ok(())
        }
    }
    impl Backend for Fake {
        type Process = Fake;
        fn radio_target(&mut self) -> Option<String> {
            Some("esp32c5".into())
        }
        fn find_port(&mut self) -> io::Result<Option<String>> {
            Ok(self.0.borrow().present.then(|| "COM4".into()))
        }
        fn awdl_ready(&mut self) -> io::Result<bool> {
            let observed = self.0.borrow();
            if observed.adapter_error {
                Err(io::Error::other("adapter unavailable"))
            } else {
                Ok(observed.adapter_up)
            }
        }
        fn spawn(&mut self, c: &Config, p: &str) -> io::Result<Fake> {
            let mut o = self.0.borrow_mut();
            o.starts.push((p.into(), c.channel));
            o.exited = false;
            Ok(Fake(self.0.clone()))
        }
    }
    fn request(json: &str) -> Request {
        serde_json::from_str(json).unwrap()
    }
    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn channel_validation_uses_the_identified_board_capabilities() {
        let status = |target: Option<&str>, state: &str, awdl0: bool| Status {
            state: state.into(),
            port: Some("COM4".into()),
            channel: 6,
            target: target.map(str::to_owned),
            awdl0,
            error: None,
            enabled: true,
        };
        assert!(validate_target_channel(&status(Some("esp32s3"), "running", true), 6).is_ok());
        assert!(validate_target_channel(&status(Some("esp32s3"), "running", true), 44).is_err());
        assert!(validate_target_channel(&status(Some("esp32c5"), "running", true), 44).is_ok());
        assert!(validate_target_channel(&status(Some("esp32c5"), "running", true), 149).is_ok());
        assert!(validate_target_channel(&status(Some("esp32c5"), "starting", false), 44).is_err());
        assert!(validate_target_channel(&status(None, "running", true), 44).is_err());
        assert!(validate_target_channel(&status(Some("esp32c5"), "running", true), 36).is_err());
    }

    #[test]
    fn discovers_later_backs_off_crashes_and_cancels_restart() {
        let dir = tempfile::tempdir().unwrap();
        let obs = Rc::new(RefCell::new(Observed::default()));
        let mut s = Supervisor::new(
            Fake(obs.clone()),
            Config::default(),
            dir.path().join("config.json"),
        );
        s.tick(secs(0));
        assert_eq!(s.status().state, "no-radio");
        obs.borrow_mut().present = true;
        s.tick(secs(2));
        assert_eq!(obs.borrow().starts.len(), 1);
        obs.borrow_mut().exited = true;
        s.tick(secs(3));
        assert_eq!(s.status().state, "error");
        s.tick(secs(4));
        assert_eq!(obs.borrow().starts.len(), 1);
        s.tick(secs(5));
        assert_eq!(obs.borrow().starts.len(), 2);
        obs.borrow_mut().exited = true;
        s.tick(secs(6));
        s.tick(secs(9));
        assert_eq!(obs.borrow().starts.len(), 2);
        s.tick(secs(10));
        assert_eq!(obs.borrow().starts.len(), 3);
        obs.borrow_mut().exited = true;
        s.tick(secs(11));
        s.command(request(r#"{"cmd":"stop"}"#), secs(12));
        s.tick(secs(100));
        assert_eq!(obs.borrow().starts.len(), 3);
        assert!(
            !Config::load(&dir.path().join("config.json"))
                .unwrap()
                .enabled
        );
    }

    #[test]
    fn reconfiguration_waits_for_old_child_and_invalid_changes_are_transactional() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let obs = Rc::new(RefCell::new(Observed {
            present: true,
            adapter_up: true,
            ..Default::default()
        }));
        let mut s = Supervisor::new(Fake(obs.clone()), Config::default(), path.clone());
        s.tick(secs(0));
        s.tick(secs(1));
        let bad = s.command(request(r#"{"cmd":"set","channel":7}"#), secs(1));
        assert!(bad.error.is_some());
        assert_eq!(obs.borrow().stops, 0);
        assert!(!path.exists());
        let unsafe_port_change = s.command(
            request(r#"{"cmd":"set","port":"COM9","channel":44}"#),
            secs(2),
        );
        assert!(unsafe_port_change.error.is_some());
        assert_eq!(obs.borrow().stops, 0);
        assert!(!path.exists());
        s.command(request(r#"{"cmd":"set","channel":44}"#), secs(2));
        assert_eq!(s.status().state, "stopping");
        assert_eq!(obs.borrow().starts.len(), 1);
        s.tick(secs(6));
        assert_eq!(obs.borrow().kills, 0);
        s.tick(secs(7));
        assert_eq!(obs.borrow().kills, 1);
        assert_eq!(obs.borrow().starts.last().unwrap(), &("COM4".into(), 44));
        s.tick(secs(7));
        let port_change = s.command(request(r#"{"cmd":"set","port":"COM9"}"#), secs(7));
        assert!(port_change.error.is_some());
        assert_eq!(Config::load(&path).unwrap().port, "auto");
        s.shutdown(secs(8));
        obs.borrow_mut().exited = true;
        s.tick(secs(9));
        assert!(s.stopped());
        assert!(Config::load(&path).unwrap().enabled);
        assert_eq!(obs.borrow().kills, 1);
    }

    #[test]
    fn cannot_save_does_not_stop_child() {
        let dir = tempfile::tempdir().unwrap();
        let obs = Rc::new(RefCell::new(Observed {
            present: true,
            ..Default::default()
        }));
        let mut s = Supervisor::new(Fake(obs.clone()), Config::default(), dir.path().into());
        s.tick(secs(0));
        let status = s.command(request(r#"{"cmd":"stop"}"#), secs(1));
        assert!(status.error.is_some());
        assert!(status.enabled);
        assert_eq!(obs.borrow().stops, 0);
    }

    #[test]
    fn adapter_read_failure_recovers_without_restarting_owl() {
        let dir = tempfile::tempdir().unwrap();
        let obs = Rc::new(RefCell::new(Observed {
            present: true,
            ..Default::default()
        }));
        let mut s = Supervisor::new(
            Fake(obs.clone()),
            Config::default(),
            dir.path().join("config.json"),
        );
        s.tick(secs(0));
        obs.borrow_mut().adapter_error = true;
        s.tick(secs(1));
        assert!(s.status().error.unwrap().contains("adapter discovery"));
        obs.borrow_mut().adapter_error = false;
        obs.borrow_mut().adapter_up = true;
        s.tick(secs(2));
        assert_eq!(s.status().state, "running");
        assert!(s.status().error.is_none());
        assert_eq!(obs.borrow().starts.len(), 1);
    }
}
