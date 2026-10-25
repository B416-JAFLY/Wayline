use anyhow::{Context, Result};
use process_wrap::std::{ChildWrapper, CommandWrap};
use std::{
    io::{self, Read},
    process::{Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const LINE_BYTES: usize = 4096;
const QUEUE_LINES: usize = 256;

#[derive(Debug)]
pub struct Outcome {
    pub status: ExitStatus,
    pub cancelled: bool,
}

pub struct Job {
    logs: Option<Receiver<String>>,
    cancel: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<Outcome>>>,
}

impl Job {
    pub fn spawn(mut command: Command) -> Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("CARGO_TERM_COLOR", "never")
            .env("NO_COLOR", "1")
            .env("PYTHONUNBUFFERED", "1");
        let (sender, logs) = mpsc::sync_channel(QUEUE_LINES);
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let worker = thread::Builder::new()
            .name("tui-build".into())
            .spawn(move || supervise(command, sender, signal))
            .context("启动构建工作线程")?;
        Ok(Self {
            logs: Some(logs),
            cancel,
            worker: Some(worker),
        })
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn drain(&self) -> Vec<String> {
        // Bound both memory and work per UI tick, including a child flooding stdout.
        self.logs
            .as_ref()
            .unwrap()
            .try_iter()
            .take(QUEUE_LINES)
            .collect()
    }

    pub fn finished(&self) -> bool {
        self.worker.as_ref().is_some_and(JoinHandle::is_finished)
    }

    pub fn finish(&mut self) -> Result<Outcome> {
        self.worker
            .take()
            .context("构建已经结束")?
            .join()
            .map_err(|_| anyhow::anyhow!("构建工作线程异常退出"))?
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
        // Unblock log producers before joining, even when rendering/input fails.
        self.logs.take();
        if let Some(worker) = self.worker.take() {
            if let Err(error) = worker
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("构建工作线程异常退出")))
            {
                eprintln!("构建退出清理失败: {error:#}");
            }
        }
    }
}

struct ProcessTree {
    child: Box<dyn ChildWrapper>,
    reaped: bool,
}

impl ProcessTree {
    fn stop(&mut self) -> Result<ExitStatus> {
        // The command may have exited while compilers it spawned still exist. Kill the
        // owned group/job before reaping it, rather than killing only the top-level PID.
        if let Err(error) = self.child.start_kill() {
            #[cfg(unix)]
            let already_gone = error.raw_os_error() == Some(3); // ESRCH: no process in group
            #[cfg(not(unix))]
            let already_gone = false;
            if !already_gone {
                return Err(error).context("终止构建进程树");
            }
        }
        let status = self.child.wait().context("等待构建进程树退出")?;
        self.reaped = true;
        Ok(status)
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if !self.reaped {
            if let Err(error) = self.stop() {
                eprintln!("构建进程树清理失败: {error:#}");
            }
        }
    }
}

fn supervise(
    command: Command,
    sender: SyncSender<String>,
    cancel: Arc<AtomicBool>,
) -> Result<Outcome> {
    let mut command = CommandWrap::from(command);
    #[cfg(windows)]
    command.wrap(process_wrap::std::JobObject);
    #[cfg(unix)]
    command.wrap(process_wrap::std::ProcessGroup::leader());
    let mut tree = ProcessTree {
        child: command.spawn().context("启动构建命令")?,
        reaped: false,
    };
    let stdout = tree.child.stdout().take().context("未捕获 stdout")?;
    let stderr = tree.child.stderr().take().context("未捕获 stderr")?;
    let out_sender = sender.clone();
    let out = thread::Builder::new()
        .name("tui-stdout".into())
        .spawn(move || read_lines(stdout, out_sender))
        .context("启动 stdout 读取线程")?;
    let err = thread::Builder::new()
        .name("tui-stderr".into())
        .spawn(move || read_lines(stderr, sender))
        .context("启动 stderr 读取线程")?;

    let cancelled = loop {
        // SAFETY: only ProcessGroup/JobObject wrap this native child. Polling the
        // leader caches its exit status but leaves their group/job handles intact.
        // stop() still kills and waits through the wrapper to reap descendants.
        if unsafe { tree.child.try_inner_child_mut() }
            .context("缺少原生子进程")?
            .try_wait()?
            .is_some()
        {
            break false;
        }
        if cancel.load(Ordering::Relaxed) {
            break true;
        }
        thread::sleep(Duration::from_millis(40));
    };
    let status = tree.stop()?;
    for reader in [out, err] {
        reader
            .join()
            .map_err(|_| anyhow::anyhow!("日志读取线程异常退出"))??;
    }
    Ok(Outcome { status, cancelled })
}

fn read_lines(mut input: impl Read, sender: SyncSender<String>) -> io::Result<()> {
    let mut bytes = [0_u8; 1024];
    let mut line = Vec::with_capacity(LINE_BYTES);
    loop {
        let count = match input.read(&mut bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        for byte in &bytes[..count] {
            if matches!(byte, b'\r' | b'\n') {
                if !line.is_empty() && !send_line(&sender, &mut line) {
                    return Ok(());
                }
            } else {
                line.push(*byte);
                // Allow up to three extra bytes so a valid UTF-8 character is not split.
                if line.len() >= LINE_BYTES
                    && (std::str::from_utf8(&line).is_ok() || line.len() >= LINE_BYTES + 3)
                    && !send_line(&sender, &mut line)
                {
                    return Ok(());
                }
            }
        }
    }
    if !line.is_empty() {
        send_line(&sender, &mut line);
    }
    Ok(())
}

fn send_line(sender: &SyncSender<String>, bytes: &mut Vec<u8>) -> bool {
    let plain = strip_ansi_escapes::strip(bytes.as_slice());
    let text: String = String::from_utf8_lossy(&plain)
        .chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .collect();
    bytes.clear();
    sender.send(text.replace('\t', "    ")).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, time::Instant};

    fn fixture(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "tui::runner::tests::process_fixture",
                "--nocapture",
            ])
            .env("WAYLINE_TUI_TEST_FIXTURE", mode);
        command
    }

    // A real subprocess fixture; only explicitly launched by the tests below.
    #[test]
    #[ignore = "subprocess fixture"]
    fn process_fixture() {
        let mode = std::env::var("WAYLINE_TUI_TEST_FIXTURE").expect("fixture mode is required");
        match mode.as_str() {
            "success" => {
                println!("stdout 中文");
                eprintln!("stderr diagnostic");
            }
            "failure" => {
                eprintln!("intentional exit 17");
                std::process::exit(17);
            }
            "parent" => {
                let _ = fixture("leaf").spawn().unwrap().wait();
            }
            "leaf" => {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                println!("LISTEN={}", listener.local_addr().unwrap());
                loop {
                    thread::sleep(Duration::from_millis(100));
                }
            }
            _ => panic!("unknown fixture"),
        }
    }

    fn complete(mut job: Job) -> (Outcome, Vec<String>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut lines = Vec::new();
        while !job.finished() {
            assert!(Instant::now() < deadline, "child did not exit");
            lines.extend(job.drain());
            thread::sleep(Duration::from_millis(10));
        }
        let outcome = job.finish().unwrap();
        lines.extend(job.drain());
        (outcome, lines)
    }

    fn descendant_address(job: &Job) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            for line in job.drain() {
                if let Some(address) = line.strip_prefix("LISTEN=") {
                    return address.into();
                }
            }
            assert!(
                !job.finished() && Instant::now() < deadline,
                "descendant never became ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn captures_both_streams_and_preserves_real_exit_codes() {
        let (result, lines) = complete(Job::spawn(fixture("success")).unwrap());
        assert!(result.status.success() && !result.cancelled);
        assert!(lines.iter().any(|s| s.contains("stdout 中文")));
        assert!(lines.iter().any(|s| s.contains("stderr diagnostic")));
        let (result, lines) = complete(Job::spawn(fixture("failure")).unwrap());
        assert_eq!(result.status.code(), Some(17));
        assert!(lines.iter().any(|s| s.contains("intentional exit 17")));
    }

    #[test]
    fn cancel_and_drop_stop_descendants() {
        for cancel_explicitly in [true, false] {
            let job = Job::spawn(fixture("parent")).unwrap();
            let address = descendant_address(&job);
            assert!(TcpListener::bind(&address).is_err());
            if cancel_explicitly {
                job.cancel();
                assert!(complete(job).0.cancelled);
            } else {
                drop(job);
            }
            // A surviving grandchild would still own this socket on either OS.
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if TcpListener::bind(&address).is_ok() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "descendant survived cancellation"
                );
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    #[test]
    fn missing_executable_is_an_error() {
        let mut job = Job::spawn(Command::new("wayline-nonexistent-tui-test-command")).unwrap();
        while !job.finished() {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(job
            .finish()
            .unwrap_err()
            .to_string()
            .contains("启动构建命令"));
    }

    #[test]
    fn log_decoder_bounds_lines_and_removes_terminal_controls() {
        let (sender, receiver) = mpsc::sync_channel(20);
        let text = format!("\x1b[31m错误\x1b[0m\rprogress\n{}tail", "中".repeat(5000));
        read_lines(text.as_bytes(), sender).unwrap();
        let lines: Vec<_> = receiver.try_iter().collect();
        assert_eq!(&lines[..2], &["错误", "progress"]);
        assert!(lines
            .iter()
            .all(|s| s.len() <= LINE_BYTES + 3 && !s.contains('\u{fffd}')));
        assert_eq!(lines[2..].concat(), format!("{}tail", "中".repeat(5000)));
    }
}
