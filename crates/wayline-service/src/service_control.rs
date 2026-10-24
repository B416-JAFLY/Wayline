//! Windows service lifecycle, separate from the persistent radio start/stop commands.
use std::{
    io,
    time::{Duration, Instant},
};
pub use windows_service::service::ServiceState;
use windows_service::{
    service::{Service, ServiceAccess},
    service_manager::{ServiceManager, ServiceManagerAccess},
};
const TIMEOUT: Duration = Duration::from_secs(15);
fn error(error: windows_service::Error) -> io::Error {
    match error {
        windows_service::Error::Winapi(error) => error,
        other => io::Error::other(other),
    }
}
fn open(access: ServiceAccess) -> io::Result<Service> {
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(error)?
        .open_service(crate::service::SERVICE_NAME, access)
        .map_err(error)
}
pub fn status() -> io::Result<ServiceState> {
    open(ServiceAccess::QUERY_STATUS)?
        .query_status()
        .map(|s| s.current_state)
        .map_err(error)
}
trait Control {
    fn state(&mut self) -> io::Result<ServiceState>;
    fn start(&mut self) -> io::Result<()>;
    fn stop(&mut self) -> io::Result<()>;
}
struct Installed(Service);
impl Control for Installed {
    fn state(&mut self) -> io::Result<ServiceState> {
        self.0
            .query_status()
            .map(|s| s.current_state)
            .map_err(error)
    }
    fn start(&mut self) -> io::Result<()> {
        match open(ServiceAccess::START)?
            .start::<&str>(&[])
            .map_err(error)
        {
            Err(e) if e.raw_os_error() == Some(1056) => Ok(()),
            other => other,
        }
    }
    fn stop(&mut self) -> io::Result<()> {
        match open(ServiceAccess::STOP)?.stop().map(|_| ()).map_err(error) {
            Err(e) if matches!(e.raw_os_error(), Some(1062 | 1061)) => Ok(()),
            other => other,
        }
    }
}
fn transition(
    service: &mut impl Control,
    running: bool,
    mut elapsed: impl FnMut() -> Duration,
    mut wait: impl FnMut(),
) -> io::Result<()> {
    let mut requested = false;
    loop {
        let state = service.state()?;
        if (running && state == ServiceState::Running)
            || (!running && state == ServiceState::Stopped)
        {
            return Ok(());
        }
        if elapsed() >= TIMEOUT {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "waiting for Wayline service transition timed out",
            ));
        }
        match state {
            ServiceState::Stopped if running => {
                if requested {
                    return Err(io::Error::other(
                        "Wayline service exited during startup; inspect service.log",
                    ));
                }
                service.start()?;
                requested = true;
            }
            ServiceState::StartPending | ServiceState::StopPending => {}
            ServiceState::Running | ServiceState::Paused if !running => service.stop()?,
            ServiceState::PausePending | ServiceState::ContinuePending if !running => {}
            _ => {
                return Err(io::Error::other(format!(
                    "unexpected Wayline service state: {state:?}"
                )))
            }
        }
        wait();
    }
}
fn set_running(running: bool) -> io::Result<()> {
    let service = match open(ServiceAccess::QUERY_STATUS) {
        Err(e) if !running && e.raw_os_error() == Some(1060) => return Ok(()),
        result => result?,
    };
    let start = Instant::now();
    transition(
        &mut Installed(service),
        running,
        || start.elapsed(),
        || std::thread::sleep(Duration::from_millis(100)),
    )
}
pub fn start() -> io::Result<()> {
    set_running(true)
}
/// Explicit administrative stop. Applications release their own leases on exit.
pub fn stop() -> io::Result<()> {
    set_running(false)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, collections::VecDeque};
    struct Fake {
        states: VecDeque<ServiceState>,
        starts: usize,
        stops: usize,
        deny_stop: bool,
    }
    impl Fake {
        fn new(states: &[ServiceState]) -> Self {
            Self {
                states: states.iter().copied().collect(),
                starts: 0,
                stops: 0,
                deny_stop: false,
            }
        }
        fn run(&mut self, running: bool) -> io::Result<()> {
            let t = Cell::new(0);
            transition(
                self,
                running,
                || Duration::from_secs(t.get()),
                || t.set(t.get() + 1),
            )
        }
    }
    impl Control for Fake {
        fn state(&mut self) -> io::Result<ServiceState> {
            Ok(if self.states.len() > 1 {
                self.states.pop_front().unwrap()
            } else {
                self.states[0]
            })
        }
        fn start(&mut self) -> io::Result<()> {
            self.starts += 1;
            Ok(())
        }
        fn stop(&mut self) -> io::Result<()> {
            self.stops += 1;
            if self.deny_stop {
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"))
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn quit_during_startup_waits_for_reaping() {
        use ServiceState::*;
        let mut s = Fake::new(&[StartPending, Running, StopPending, Stopped]);
        s.run(false).unwrap();
        assert_eq!((s.starts, s.stops), (0, 1));
    }
    #[test]
    fn relaunch_waits_for_prior_stop() {
        use ServiceState::*;
        let mut s = Fake::new(&[StopPending, Stopped, StartPending, Running]);
        s.run(true).unwrap();
        assert_eq!((s.starts, s.stops), (1, 0));
    }
    #[test]
    fn completed_states_need_no_mutation() {
        let mut s = Fake::new(&[ServiceState::Running]);
        s.run(true).unwrap();
        assert_eq!(s.starts, 0);
        let mut s = Fake::new(&[ServiceState::Stopped]);
        s.run(false).unwrap();
        assert_eq!(s.stops, 0);
    }
    #[test]
    fn transition_errors_do_not_report_success() {
        use ServiceState::*;
        let mut s = Fake::new(&[Running]);
        s.deny_stop = true;
        assert_eq!(
            s.run(false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            Fake::new(&[StopPending]).run(false).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(Fake::new(&[Stopped, StartPending, Stopped])
            .run(true)
            .is_err());
    }
    #[test]
    #[ignore = "starts/stops installed Wayline; run separately with no active transfers"]
    fn installed_service_lifecycle_without_administrator_rights() {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, HANDLE},
            Security::{
                CreateRestrictedToken, CreateWellKnownSid, ImpersonateLoggedOnUser, RevertToSelf,
                WinBuiltinAdministratorsSid, DISABLE_MAX_PRIVILEGE, SID_AND_ATTRIBUTES,
                TOKEN_DUPLICATE, TOKEN_QUERY,
            },
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        };
        struct Token(HANDLE);
        impl Drop for Token {
            fn drop(&mut self) {
                // SAFETY: each token owns its handle.
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
        struct Impersonation;
        impl Drop for Impersonation {
            fn drop(&mut self) {
                // SAFETY: this thread established impersonation below.
                unsafe {
                    RevertToSelf();
                }
            }
        }
        let config = std::path::PathBuf::from(std::env::var_os("ProgramData").unwrap())
            .join("wayline/config.json");
        let before = std::fs::read(&config).unwrap();
        let mut original = Token(std::ptr::null_mut());
        let mut restricted = Token(std::ptr::null_mut());
        let mut sid = [0u32; 17];
        let mut len = std::mem::size_of_val(&sid) as u32;
        // SAFETY: all output pointers have sufficient live storage; tokens are owned
        // and the impersonation is limited to this dedicated ignored test thread.
        unsafe {
            assert_ne!(
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_QUERY | TOKEN_DUPLICATE,
                    &mut original.0
                ),
                0
            );
            assert_ne!(
                CreateWellKnownSid(
                    WinBuiltinAdministratorsSid,
                    std::ptr::null_mut(),
                    sid.as_mut_ptr().cast(),
                    &mut len
                ),
                0
            );
            let disabled = SID_AND_ATTRIBUTES {
                Sid: sid.as_mut_ptr().cast(),
                Attributes: 0,
            };
            assert_ne!(
                CreateRestrictedToken(
                    original.0,
                    DISABLE_MAX_PRIVILEGE,
                    1,
                    &disabled,
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                    &mut restricted.0
                ),
                0
            );
            assert_ne!(ImpersonateLoggedOnUser(restricted.0), 0);
        }
        let _impersonation = Impersonation;
        assert!(open(ServiceAccess::CHANGE_CONFIG).is_err());
        stop().unwrap();
        for _ in 0..2 {
            start().unwrap();
            assert_eq!(status().unwrap(), ServiceState::Running);
            stop().unwrap();
            assert_eq!(status().unwrap(), ServiceState::Stopped);
        }
        assert_eq!(std::fs::read(config).unwrap(), before);
    }
}
