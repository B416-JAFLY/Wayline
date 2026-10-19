use crate::{
    config::Config,
    supervisor::{Backend, ManagedProcess},
};
use serialport::{SerialPortInfo, SerialPortType};
use std::{
    fs::{self, OpenOptions},
    io, mem,
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::PathBuf,
    process::{Child, Command, Stdio},
    ptr,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, ERROR_SUCCESS, INVALID_HANDLE_VALUE},
    NetworkManagement::{
        IpHelper::{GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH},
        Ndis::IfOperStatusUp,
    },
    Networking::WinSock::{AF_INET6, SOCKADDR_IN6},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{
            CreateEventW, OpenThread, ResumeThread, SetEvent, CREATE_NO_WINDOW, CREATE_SUSPENDED,
            THREAD_SUSPEND_RESUME,
        },
    },
};

pub struct WindowsBackend {
    pub data_dir: PathBuf,
    pub default_owl: PathBuf,
}

impl Backend for WindowsBackend {
    type Process = OwlProcess;
    fn find_port(&mut self) -> io::Result<Option<String>> {
        let ports = serialport::available_ports().map_err(io::Error::other)?;
        Ok(select_esp_port(ports))
    }
    fn awdl_ready(&mut self) -> io::Result<bool> {
        awdl_ready()
    }
    fn spawn(&mut self, config: &Config, port: &str) -> io::Result<OwlProcess> {
        let device = crate::device_lock::DeviceGuard::try_acquire(port)?;
        let failed_port = if config.port == "auto" {
            crate::device_lock::first_failed_port()?
        } else {
            crate::device_lock::has_failed(port)?.then(|| port.to_owned())
        };
        if let Some(failed_port) = failed_port {
            return Err(io::Error::other(crate::device_lock::DeviceNotReady(
                failed_port,
            )));
        }
        let exe = config
            .owl_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.default_owl.clone());
        if !exe.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} not found", exe.display()),
            ));
        }
        fs::create_dir_all(&self.data_dir)?;
        let event = StopEvent::new()?;
        let console = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.data_dir.join("owl-console.log"))?;
        let mut child = Command::new(exe)
            .args([
                "-i",
                port,
                "-c",
                &config.channel.to_string(),
                "-N",
                "-F",
                "-E",
                "-S",
                "pin",
                "-T",
                "-v",
            ])
            .current_dir(&self.data_dir)
            .env("OWL_LOG_FILE", self.data_dir.join("owl.log"))
            .env("OWL_STOP_EVENT", &event.name)
            .stdin(Stdio::null())
            .stdout(console.try_clone()?)
            .stderr(console)
            .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
            .spawn()?;
        // Windows closes this job on a service crash too, preventing an orphan
        // radio from competing for the serial port when SCM restarts the service.
        let job = match make_job(&child) {
            Ok(job) => job,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        // The child cannot access the serial port until crash cleanup is attached.
        if let Err(e) = resume_child(&child) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        Ok(OwlProcess {
            child,
            event,
            _job: job,
            _device: device,
        })
    }
}

pub fn select_esp_port(ports: Vec<SerialPortInfo>) -> Option<String> {
    let mut ports: Vec<_> = ports
        .into_iter()
        .filter_map(|port| match port.port_type {
            SerialPortType::UsbPort(usb) if [0x1a86, 0x10c4, 0x0403, 0x303a].contains(&usb.vid) => {
                crate::config::normalize_port(&port.port_name)
                    .ok()
                    .filter(|p| p != "auto")
            }
            _ => None,
        })
        .collect();
    ports.sort_by_key(|port| port[3..].parse::<u16>().unwrap_or(u16::MAX));
    ports.into_iter().next()
}

struct StopEvent {
    handle: OwnedHandle,
    name: String,
}
impl StopEvent {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!(
            "Global\\omdrop-owl-stop-{}-{time}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let raw = unsafe { CreateEventW(ptr::null(), 1, 0, wide.as_ptr()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle: unsafe { OwnedHandle::from_raw_handle(raw) },
            name,
        })
    }
    fn signal(&self) -> io::Result<()> {
        if unsafe { SetEvent(self.handle.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

pub struct OwlProcess {
    child: Child,
    event: StopEvent,
    _job: OwnedHandle,
    _device: crate::device_lock::DeviceGuard,
}
impl ManagedProcess for OwlProcess {
    fn exit_code(&mut self) -> io::Result<Option<String>> {
        self.child
            .try_wait()
            .map(|status| status.map(|s| s.to_string()))
    }
    fn request_stop(&mut self) -> io::Result<()> {
        self.event.signal()
    }
    fn kill_and_wait(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        self.child.wait()?;
        Ok(())
    }
}
impl Drop for OwlProcess {
    fn drop(&mut self) {
        let _ = self.kill_and_wait();
    }
}

fn make_job(child: &Child) -> io::Result<OwnedHandle> {
    let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            raw,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            mem::size_of_val(&limits) as u32,
        )
    } == 0
        || unsafe { AssignProcessToJobObject(raw, child.as_raw_handle()) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

fn resume_child(child: &Child) -> io::Result<()> {
    // SAFETY: snapshot returns an owned handle and requires no pointer arguments.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: raw is a successful snapshot handle, closed exactly once by OwnedHandle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = THREADENTRY32 {
        dwSize: mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    // SAFETY: entry is writable and advertises the exact structure size.
    let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
    while found {
        if entry.th32OwnerProcessID == child.id() {
            // SAFETY: only resume access is requested for a thread owned by our suspended child.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: thread is a new owned handle to the child's sole primary thread.
            let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
            // SAFETY: the child was created suspended and assigned to its cleanup job.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        // SAFETY: snapshot and entry remain live throughout enumeration.
        found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
    }
    Err(io::Error::other("cannot find suspended OWL primary thread"))
}

pub fn awdl_ready() -> io::Result<bool> {
    // ULONG_PTR storage gives the linked adapter/socket structures their required
    // alignment. Windows may change the required size while adapters appear.
    let mut size = 16 * 1024u32;
    for _ in 0..4 {
        let mut buffer = vec![0usize; (size as usize).div_ceil(mem::size_of::<usize>())];
        let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let result =
            unsafe { GetAdaptersAddresses(AF_INET6 as u32, 0, ptr::null(), head, &mut size) };
        if result == ERROR_BUFFER_OVERFLOW {
            continue;
        }
        if result == ERROR_NO_DATA {
            return Ok(false);
        }
        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let mut adapter = head;
        while !adapter.is_null() {
            let a = unsafe { &*adapter };
            if a.OperStatus == IfOperStatusUp && unsafe { wide_name_is(a.FriendlyName, "awdl0") } {
                let mut address = a.FirstUnicastAddress;
                while !address.is_null() {
                    let unicast = unsafe { &*address };
                    let socket = unicast.Address;
                    if !socket.lpSockaddr.is_null()
                        && socket.iSockaddrLength as usize >= mem::size_of::<SOCKADDR_IN6>()
                    {
                        let addr = unsafe { &*socket.lpSockaddr.cast::<SOCKADDR_IN6>() };
                        if addr.sin6_family == AF_INET6 {
                            let bytes = unsafe { addr.sin6_addr.u.Byte };
                            if bytes[0] == 0xfe && bytes[1] & 0xc0 == 0x80 {
                                return Ok(true);
                            }
                        }
                    }
                    address = unicast.Next;
                }
            }
            adapter = a.Next;
        }
        return Ok(false);
    }
    Err(io::Error::other(
        "adapter list kept changing during discovery",
    ))
}

unsafe fn wide_name_is(ptr: *const u16, expected: &str) -> bool {
    if ptr.is_null() {
        return false;
    }
    for (i, value) in expected.encode_utf16().chain(Some(0)).enumerate() {
        // GetAdaptersAddresses owns this valid, NUL-terminated string for the
        // lifetime of the backing buffer; stop at the first differing character.
        if *ptr.add(i) != value {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use serialport::UsbPortInfo;
    fn usb(name: &str, vid: u16) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.into(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid: 1,
                serial_number: None,
                manufacturer: None,
                product: None,
            }),
        }
    }
    #[test]
    fn only_supported_present_usb_bridges_are_candidates() {
        assert_eq!(
            select_esp_port(vec![
                usb("COM3", 0x0483),
                usb("COM12", 0x303a),
                usb("COM4", 0x10c4)
            ])
            .as_deref(),
            Some("COM4")
        );
        assert!(select_esp_port(vec![usb("COM3", 0x0483)]).is_none());
    }
}
