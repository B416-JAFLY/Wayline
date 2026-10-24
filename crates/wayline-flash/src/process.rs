//! Bounded subprocess output and cancellation. The Windows child is suspended until its
//! kill-on-close Job is assigned, preventing a running unowned flashing process.
use crate::Cancellation;
use anyhow::{Context, Result, bail};
use std::{
    collections::VecDeque,
    ffi::OsString,
    io::Read,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 64 * 1024;
const LINE_LIMIT: usize = 4096;

#[derive(Debug)]
pub(crate) struct ToolOutput {
    pub text: String,
}
pub(crate) fn run(
    executable: &Path,
    args: &[OsString],
    cancel: &Cancellation,
    timeout: Duration,
    mut output: impl FnMut(&str),
) -> Result<ToolOutput> {
    if cancel.is_cancelled() {
        bail!("操作已取消");
    }
    let mut command = Command::new(executable);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        command.creation_flags(CREATE_NO_WINDOW.0 | CREATE_SUSPENDED.0);
    }
    let child = command
        .spawn()
        .with_context(|| format!("无法启动烧录工具 {}", executable.display()))?;
    let mut child = ReapedChild(child);
    #[cfg(windows)]
    let _job = job::Job::assign_and_resume(&child.0)?;
    let stdout = child.0.stdout.take().context("缺少工具输出")?;
    let stderr = child.0.stderr.take().context("缺少工具错误输出")?;
    // A bounded channel applies backpressure to flooders; each read is at most 1024 bytes.
    let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(16);
    let readers = [pump(stdout, sender.clone()), pump(stderr, sender)];
    let started = Instant::now();
    let mut last_output = started;
    let mut tail = VecDeque::with_capacity(OUTPUT_LIMIT);
    let mut line = Vec::new();
    let result = loop {
        if cancel.is_cancelled() {
            break Err(anyhow::anyhow!("操作已取消；写入开始后可能需要完整重刷"));
        }
        if started.elapsed() > timeout || last_output.elapsed() > Duration::from_secs(45) {
            break Err(anyhow::anyhow!("烧录工具超时或长时间无输出"));
        }
        match receiver.recv_timeout(Duration::from_millis(30)) {
            Ok(chunk) => {
                last_output = Instant::now();
                for byte in chunk {
                    if tail.len() == OUTPUT_LIMIT {
                        tail.pop_front();
                    }
                    tail.push_back(byte);
                    if matches!(byte, b'\r' | b'\n') || line.len() == LINE_LIMIT {
                        if !line.is_empty() {
                            output(&String::from_utf8_lossy(&line));
                            line.clear();
                        }
                    } else {
                        line.push(byte);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => match child.0.try_wait()? {
                Some(status) => {
                    if !line.is_empty() {
                        output(&String::from_utf8_lossy(&line));
                    }
                    let text =
                        String::from_utf8_lossy(&tail.into_iter().collect::<Vec<_>>()).into_owned();
                    break if status.success() {
                        Ok(ToolOutput { text })
                    } else {
                        Err(anyhow::anyhow!("烧录工具退出 {status}：{text}"))
                    };
                }
                None => thread::sleep(Duration::from_millis(30)),
            },
        }
    };
    // Close the Job before joining readers, including descendants that inherited stdout.
    #[cfg(windows)]
    drop(_job);
    let _ = child.0.kill();
    let _ = child.0.wait();
    drop(receiver);
    for reader in readers {
        let _ = reader.join();
    }
    result
}
fn pump(
    mut stream: impl Read + Send + 'static,
    sender: mpsc::SyncSender<Vec<u8>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0u8; 1024];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if sender.send(buffer[..count].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    })
}
struct ReapedChild(Child);
impl Drop for ReapedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(windows)]
mod job {
    use anyhow::{Context, Result, bail};
    use std::{os::windows::io::AsRawHandle, process::Child};
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{Diagnostics::ToolHelp::*, JobObjects::*, Threading::*},
    };
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    pub(super) struct Job(Handle);
    impl Job {
        pub(super) fn assign_and_resume(child: &Child) -> Result<Self> {
            // Handles are owned by RAII guards. Child remains suspended until Job assignment;
            // any error drops the caller's ReapedChild and kills/reaps that suspended process.
            let handle = Handle(unsafe { CreateJobObjectW(None, None)? });
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            unsafe {
                SetInformationJobObject(
                    handle.0,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    size_of_val(&info) as u32,
                )?;
                AssignProcessToJobObject(handle.0, HANDLE(child.as_raw_handle()))?;
            }
            let job = Self(handle);
            let snapshot = Handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)? });
            let mut entry = THREADENTRY32 {
                dwSize: size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            unsafe { Thread32First(snapshot.0, &mut entry)? };
            loop {
                if entry.th32OwnerProcessID == child.id() {
                    let thread = Handle(unsafe {
                        OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)?
                    });
                    let result = unsafe { ResumeThread(thread.0) };
                    if result == u32::MAX {
                        return Err(std::io::Error::last_os_error())
                            .context("无法启动烧录进程线程");
                    }
                    return Ok(job);
                }
                if unsafe { Thread32Next(snapshot.0, &mut entry) }.is_err() {
                    break;
                }
            }
            bail!("无法找到烧录进程的挂起线程")
        }
    }
    impl Drop for Job {
        fn drop(&mut self) {
            // Terminate descendants before waiting on pipe readers.
            unsafe {
                let _ = TerminateJobObject(self.0.0, 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_executable_and_pre_cancelled_do_not_spawn() {
        assert!(
            run(
                Path::new("missing-wayline-test.exe"),
                &[],
                &Cancellation::default(),
                Duration::from_secs(1),
                |_| {}
            )
            .is_err()
        );
        let cancel = Cancellation::default();
        cancel.cancel();
        assert!(
            run(
                Path::new("missing-wayline-test.exe"),
                &[],
                &cancel,
                Duration::from_secs(1),
                |_| {}
            )
            .unwrap_err()
            .to_string()
            .contains("取消")
        );
    }
    #[cfg(windows)]
    #[test]
    fn simulation_bounds_output_reports_exit_and_cancels() {
        let tool = Path::new("C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe");
        let args: Vec<OsString> = [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::Out.Write(('x' * 200000)); Write-Output 'done'",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let mut longest = 0;
        let out = run(
            tool,
            &args,
            &Cancellation::default(),
            Duration::from_secs(15),
            |line| longest = longest.max(line.len()),
        )
        .unwrap();
        assert!(out.text.len() <= OUTPUT_LIMIT);
        assert!(longest <= LINE_LIMIT);
        assert!(out.text.ends_with("done\r\n") || out.text.ends_with("done\n"));
        let fail: Vec<OsString> = ["-NoProfile", "-Command", "Write-Output 'bad chip'; exit 7"]
            .into_iter()
            .map(Into::into)
            .collect();
        assert!(
            run(
                tool,
                &fail,
                &Cancellation::default(),
                Duration::from_secs(15),
                |_| {}
            )
            .is_err()
        );
        let cancel = Cancellation::default();
        let trigger = cancel.clone();
        let thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            trigger.cancel();
        });
        let wait: Vec<OsString> = ["-NoProfile", "-Command", "Start-Sleep -Seconds 30"]
            .into_iter()
            .map(Into::into)
            .collect();
        let now = Instant::now();
        assert!(run(tool, &wait, &cancel, Duration::from_secs(15), |_| {}).is_err());
        assert!(now.elapsed() < Duration::from_secs(5));
        thread.join().unwrap();
    }
}
