//! Device exclusion shared by OWL and the firmware writer.
//! A dedicated owner thread makes the Windows mutex safe to move across runtimes,
//! while abandoned mutex recovery releases ownership when a client crashes.
use std::{
    io::{self, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    ptr,
    sync::mpsc,
    thread,
};

/// Persisted before any flash write, including first installation without a service.
/// Call while holding this port's DeviceGuard; a readiness handshake clears it.
pub fn mark_failed(port: &str) -> io::Result<()> {
    let path = marker_path(port)?;
    let directory = path.parent().expect("marker has a parent");
    create_shared_directory(directory.parent().expect("device-state has a parent"))?;
    create_shared_directory(directory)?;
    write_marker(&path)
}
pub fn clear_failed(port: &str) -> io::Result<()> {
    remove_marker(&marker_path(port)?)
}
pub fn has_failed(port: &str) -> io::Result<bool> {
    marker_path(port)?.try_exists()
}
/// Auto-selection must not guess that a re-enumerated COM port is a different device.
/// A caller with an explicitly configured port may instead use `has_failed`.
pub fn first_failed_port() -> io::Result<Option<String>> {
    first_failed_port_in(&marker_directory()?)
}
fn first_failed_port_in(directory: &Path) -> io::Result<Option<String>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let name = name.to_ascii_lowercase();
        let Some(port) = name.strip_suffix(".failed") else {
            continue;
        };
        if let Ok(port) = crate::config::normalize_port(port) {
            if port != "auto" {
                return Ok(Some(port));
            }
        }
    }
    Ok(None)
}
fn write_marker(path: &Path) -> io::Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().expect("marker has a parent"))?;
    file.write_all(b"Firmware writing was started; a successful device handshake is required.\n")?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
fn remove_marker(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
fn marker_path(port: &str) -> io::Result<PathBuf> {
    let port = crate::config::normalize_port(port)?;
    if port == "auto" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "firmware state requires an explicit COM port",
        ));
    }
    Ok(marker_directory()?.join(format!("{port}.failed")))
}
fn marker_directory() -> io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_Public, SHGetKnownFolderPath},
    };
    let mut raw = ptr::null_mut();
    // SAFETY: the known-folder GUID and writable output pointer are valid; no token
    // is passed, so all users and SYSTEM resolve the same machine-wide Public folder.
    let result = unsafe { SHGetKnownFolderPath(&FOLDERID_Public, 0, ptr::null_mut(), &mut raw) };
    if result < 0 {
        return Err(io::Error::other(format!(
            "cannot locate shared device state: HRESULT {result:#x}"
        )));
    }
    // SAFETY: the successful API result is a nul-terminated CoTaskMem allocation.
    let path = unsafe {
        let mut len = 0;
        while *raw.add(len) != 0 {
            len += 1;
        }
        let value = std::ffi::OsString::from_wide(std::slice::from_raw_parts(raw, len));
        CoTaskMemFree(raw.cast());
        PathBuf::from(value)
    };
    Ok(path.join("Wayline").join("device-state"))
}
fn create_shared_directory(path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::ERROR_ALREADY_EXISTS, Storage::FileSystem::CreateDirectoryW,
    };
    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1301bf;;;AU)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: SDDL is nul-terminated; descriptor points to valid output storage.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: path, attributes and security descriptor are live for this call.
    let result = unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) };
    let error = io::Error::last_os_error();
    // SAFETY: descriptor was allocated by the conversion API above.
    unsafe {
        LocalFree(descriptor);
    }
    if result == 0 && error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
        return Err(error);
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct DeviceNotReady(pub String);
impl std::fmt::Display for DeviceNotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} firmware readiness is unconfirmed; retry firmware maintenance",
            self.0
        )
    }
}
impl std::error::Error for DeviceNotReady {}
use windows_sys::Win32::{
    Foundation::{LocalFree, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    },
    System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
};

pub struct DeviceGuard {
    release: Option<mpsc::Sender<()>>,
    owner: Option<thread::JoinHandle<()>>,
}
impl DeviceGuard {
    pub fn try_acquire(port: &str) -> io::Result<Self> {
        let port = crate::config::normalize_port(port)?;
        if port == "auto" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "device exclusion requires an explicit COM port",
            ));
        }
        let (ready, result) = mpsc::sync_channel(1);
        let (release, done) = mpsc::channel();
        let owner = thread::spawn(move || {
            let acquired = acquire(&port);
            match acquired {
                Ok(handle) => {
                    if ready.send(Ok(())).is_ok() {
                        let _ = done.recv();
                    }
                    // SAFETY: this same thread acquired and still owns this live mutex.
                    unsafe {
                        ReleaseMutex(handle.as_raw_handle());
                    }
                }
                Err(e) => {
                    let _ = ready.send(Err(e));
                }
            }
        });
        match result.recv().map_err(io::Error::other)? {
            Ok(()) => Ok(Self {
                release: Some(release),
                owner: Some(owner),
            }),
            Err(e) => {
                let _ = owner.join();
                Err(e)
            }
        }
    }
}
impl Drop for DeviceGuard {
    fn drop(&mut self) {
        self.release.take();
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}
fn acquire(port: &str) -> io::Result<OwnedHandle> {
    let name: Vec<u16> = format!("Global\\WaylineRadio-{port}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // Authenticated local processes must share this object with the SYSTEM service.
    let sddl: Vec<u16> = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;AU)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: nul-terminated SDDL and valid output storage; LocalFree below releases it.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // SAFETY: attributes, descriptor and name remain live for the synchronous call.
    // CreateMutexW requests MUTEX_ALL_ACCESS on an existing object, so authenticated
    // users need that right as well; ACL below is intentionally local-user shared.
    let raw = unsafe { CreateMutexW(&attributes, 0, name.as_ptr()) };
    // SAFETY: descriptor is the allocation returned by the conversion API.
    unsafe {
        LocalFree(descriptor);
    }
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful CreateMutexW returns a uniquely owned handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: handle remains valid and the zero timeout cannot block the caller.
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(handle),
        WAIT_TIMEOUT => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("{port} is in use by OWL or a firmware operation"),
        )),
        _ => Err(io::Error::last_os_error()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mutual_exclusion_is_shared_and_movable() {
        let port = format!("COM{}", 50000 + (std::process::id() % 10000));
        let guard = DeviceGuard::try_acquire(&port).unwrap();
        assert_eq!(
            DeviceGuard::try_acquire(&port).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        thread::spawn(move || drop(guard)).join().unwrap();
        assert!(DeviceGuard::try_acquire(&port).is_ok());
    }
    #[test]
    fn readiness_marker_is_atomic_and_recovery_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("COM125.failed");
        write_marker(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        write_marker(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        remove_marker(&path).unwrap();
        remove_marker(&path).unwrap();
        assert!(!path.exists());
        assert!(marker_path("auto").is_err());
        assert!(marker_path("../COM1").is_err());
    }
    #[test]
    fn automatic_port_selection_preserves_failure_across_usb_reenumeration() {
        let dir = tempfile::tempdir().unwrap();
        assert!(first_failed_port_in(dir.path()).unwrap().is_none());
        std::fs::write(dir.path().join("unrelated.failed"), b"").unwrap();
        std::fs::write(dir.path().join("COM12.pending"), b"").unwrap();
        assert!(first_failed_port_in(dir.path()).unwrap().is_none());
        write_marker(&dir.path().join("COM125.failed")).unwrap();
        assert_eq!(
            first_failed_port_in(dir.path()).unwrap().as_deref(),
            Some("COM125")
        );
        // A successful handshake on a different port cannot prove which old device recovered.
        remove_marker(&dir.path().join("COM126.failed")).unwrap();
        assert_eq!(
            first_failed_port_in(dir.path()).unwrap().as_deref(),
            Some("COM125")
        );
        remove_marker(&dir.path().join("COM125.failed")).unwrap();
        assert!(first_failed_port_in(dir.path()).unwrap().is_none());
        write_marker(&dir.path().join("com127.FAILED")).unwrap();
        assert_eq!(
            first_failed_port_in(dir.path()).unwrap().as_deref(),
            Some("COM127")
        );
    }
}
