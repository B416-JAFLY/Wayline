use std::{
    ffi::OsString,
    fs, io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
    time::Duration,
};
use tokio::sync::watch;
use wayline_awdl::{
    driver,
    serial::Radio,
    windows::{feedback, wintun::Wintun},
    MacAddr,
};
use windows_sys::Win32::{
    Foundation::{WAIT_FAILED, WAIT_OBJECT_0},
    System::Threading::{OpenEventW, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE},
};

struct Options {
    port: String,
    channel: u8,
    managed: bool,
}
impl Options {
    fn parse(args: impl IntoIterator<Item = OsString>) -> io::Result<Option<Self>> {
        let mut args = args.into_iter();
        let mut port = None;
        let mut channel = 6;
        let mut managed = false;
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--help" | "-h") => {
                    println!("wayline-bridge --port COM<n> [--channel 6|44|149] [--managed]\nRequires the verified wintun.dll beside the executable. --managed is for service launches.");
                    return Ok(None);
                }
                Some("--port") => {
                    let value = args
                        .next()
                        .ok_or_else(|| invalid("--port requires a value"))?;
                    port = Some(wayline_service::config::normalize_port(
                        value.to_str().ok_or_else(|| invalid("invalid port"))?,
                    )?);
                }
                Some("--channel") => {
                    channel = args
                        .next()
                        .and_then(|v| v.to_str().and_then(|s| s.parse().ok()))
                        .ok_or_else(|| invalid("--channel requires 6, 44 or 149"))?;
                }
                Some("--managed") => managed = true,
                _ => {
                    return Err(invalid(format!(
                        "unknown argument: {}",
                        arg.to_string_lossy()
                    )))
                }
            }
        }
        let port = port
            .filter(|p| p != "auto")
            .ok_or_else(|| invalid("--port requires an explicit COM port"))?;
        if ![6, 44, 149].contains(&channel) {
            return Err(invalid("unsupported channel"));
        }
        Ok(Some(Self {
            port,
            channel,
            managed,
        }))
    }
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

struct StopEvent(OwnedHandle);
impl StopEvent {
    fn open() -> io::Result<Option<Self>> {
        let Some(name) = std::env::var_os("WAYLINE_BRIDGE_STOP_EVENT") else {
            return Ok(None);
        };
        use std::os::windows::ffi::OsStrExt;
        let name: Vec<u16> = name.encode_wide().chain(Some(0)).collect();
        // SAFETY: name is terminated; ownership of the newly opened handle moves
        // to OwnedHandle exactly once and lasts through all polling.
        let handle = unsafe { OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(Self(unsafe { OwnedHandle::from_raw_handle(handle) })))
    }
    fn signalled(&self) -> io::Result<bool> {
        // SAFETY: the owned event is live; a zero timeout never blocks.
        match unsafe { WaitForSingleObject(self.0.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_FAILED => Err(io::Error::last_os_error()),
            _ => Ok(false),
        }
    }
}

pub fn main() -> io::Result<()> {
    let Some(options) = Options::parse(std::env::args_os().skip(1))? else {
        return Ok(());
    };
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
    let stop = StopEvent::open()?;
    if options.managed && stop.is_none() {
        return Err(invalid("--managed requires the service stop event"));
    }
    if let Some(event) = &stop {
        if event.signalled()? {
            return Ok(());
        }
    }
    // The service owns the device guard for managed children. Foreground use
    // acquires the same guard before any serial/driver access.
    let _guard = if options.managed {
        None
    } else {
        Some(wayline_service::device_lock::DeviceGuard::try_acquire(
            &options.port,
        )?)
    };
    if wayline_service::device_lock::has_failed(&options.port)? {
        return Err(io::Error::other(
            "device readiness is unconfirmed; complete firmware maintenance before starting AWDL",
        ));
    }
    let dll = std::env::current_exe()?.with_file_name("wintun.dll");
    if !dll.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "verified wintun.dll is required beside wayline-bridge.exe",
        ));
    }
    tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async move {
        let stopping = wait_for_stop(&stop);
        tokio::pin!(stopping);
        let target_file = std::env::var_os("WAYLINE_BRIDGE_TARGET_FILE").map(std::path::PathBuf::from);
        let publish_target = move |hello: &wayline_awdl::serial::Hello| {
            if let Some(path) = target_file.as_deref() {
                publish_radio_target(path, &hello.target)?;
            }
            Ok(())
        };
        let (radio,hello)=tokio::select! {
            result=Radio::open_with_target(&options.port,options.channel,publish_target)=>result?,
            result=&mut stopping=>return result,
        };
        let config=wayline_awdl::configuration(hello.mac,"Wayline",&hello.target,options.channel)?;
        let control=radio.control();
        let (peers,_)=watch::channel(Vec::new());
        let running=driver::spawn(config,move || {
            let net=Wintun::open(&dll,MacAddr(hello.mac))?;
            Ok((radio,net))
        },peers)?;
        let mut failed=running.failed();
        let (shutdown,feedback_stop)=watch::channel(false);
        let feedback=tokio::spawn(async move {
            if let Err(e)=feedback::serve(feedback::PIPE,control,feedback_stop).await {tracing::warn!("board feedback unavailable: {e}");}
        });
        tracing::info!("Rust AWDL ready on {} channel {}",options.port,options.channel);
        let result=loop {
            if let Some(e)=failed.borrow().as_ref() {break Err(io::Error::other(e.clone()));}
            tokio::select! {
                result=&mut stopping=>break result,
                result=failed.changed()=>{if result.is_err() {break Err(io::Error::other("AWDL worker ended unexpectedly"));}},
            }
        };
        shutdown.send_replace(true);
        let _=feedback.await;
        // Joins AWDL, serial and Wintun workers before returning to the supervisor.
        running.stop();
        tracing::info!("Rust AWDL stopped; radio and awdl0 released");
        result
    })
}

fn publish_radio_target(path: &Path, target: &str) -> io::Result<()> {
    if !matches!(target, "esp32s3" | "esp32c5") {
        return Err(invalid("unsupported radio target"));
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, target.as_bytes())?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(temporary);
        return Err(error);
    }
    Ok(())
}

async fn wait_for_stop(event: &Option<StopEvent>) -> io::Result<()> {
    let mut poll = tokio::time::interval(Duration::from_millis(50));
    let ctrl = tokio::signal::ctrl_c();
    tokio::pin!(ctrl);
    loop {
        tokio::select! {
            result=&mut ctrl=>return result,
            _=poll.tick()=>if let Some(event)=event {if event.signalled()? {return Ok(());}},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(s: &[&str]) -> Vec<OsString> {
        s.iter().map(OsString::from).collect()
    }
    #[test]
    fn rejects_ambiguous_or_unsupported_arguments_before_opening_devices() {
        for a in [
            vec![],
            vec!["--port", "auto"],
            vec!["--port", "COM0"],
            vec!["--port", "COM1", "--channel", "5"],
            vec!["--port", "COM1", "--unknown"],
        ] {
            assert!(Options::parse(args(&a)).is_err());
        }
        let o = Options::parse(args(&["--port", "com004", "--channel", "149", "--managed"]))
            .unwrap()
            .unwrap();
        assert_eq!(o.port, "COM4");
        assert_eq!(o.channel, 149);
        assert!(o.managed);
    }
}
