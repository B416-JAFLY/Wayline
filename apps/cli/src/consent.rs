//! One terminal reader, a bounded offer queue, and fail-closed decisions.
use omdrop_core::{IncomingTransfer, SendCancellation};
use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

struct Prompt {
    transfer: IncomingTransfer,
    reply: SyncSender<bool>,
    deadline: Instant,
    expired: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct Consent {
    requests: SyncSender<Prompt>,
    cancel: SendCancellation,
    timeout: Duration,
}

impl Consent {
    pub fn terminal(save: PathBuf, cancel: SendCancellation, timeout: Duration) -> Self {
        let (requests, pending) = mpsc::sync_channel(4);
        let worker_cancel = cancel.clone();
        // This is the only thread allowed to read terminal input. If the user never
        // submits a line, expired requests remain denied and cannot authorize a later offer.
        thread::spawn(move || terminal_worker(pending, save, worker_cancel));
        Self {
            requests,
            cancel,
            timeout,
        }
    }

    pub fn decide(&self, transfer: &IncomingTransfer) -> bool {
        if self.cancel.is_cancelled() {
            return false;
        }
        let (reply, answer) = mpsc::sync_channel(1);
        let expired = Arc::new(AtomicBool::new(false));
        let deadline = Instant::now() + self.timeout;
        if self
            .requests
            .try_send(Prompt {
                transfer: transfer.clone(),
                reply,
                deadline,
                expired: expired.clone(),
            })
            .is_err()
        {
            return false;
        }
        let accepted = wait_answer(&answer, deadline, &self.cancel);
        expired.store(true, Ordering::Release);
        accepted
    }
}

fn wait_answer(answer: &Receiver<bool>, deadline: Instant, cancel: &SendCancellation) -> bool {
    while !cancel.is_cancelled() && Instant::now() < deadline {
        match answer.recv_timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50)),
        ) {
            Ok(value) => return value && !cancel.is_cancelled() && Instant::now() < deadline,
            Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    false
}

fn read_answer(input: &mut impl BufRead) -> io::Result<bool> {
    let mut line = String::new();
    let count = std::io::Read::take(&mut *input, 256).read_line(&mut line)?;
    if count == 256 && !line.ends_with('\n') {
        // Drain a rejected oversized answer without storing it. Its tail must
        // never become authorization for the next offer.
        loop {
            let buffer = input.fill_buf()?;
            if buffer.is_empty() {
                break;
            }
            let newline = buffer.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(buffer.len(), |offset| offset + 1);
            input.consume(consumed);
            if newline.is_some() {
                break;
            }
        }
    }
    Ok(count > 0
        && count < 256
        && line.ends_with('\n')
        && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

fn terminal_worker(pending: Receiver<Prompt>, save: PathBuf, cancel: SendCancellation) {
    let mut input = io::stdin().lock();
    while let Ok(prompt) = pending.recv() {
        if cancel.is_cancelled()
            || prompt.expired.load(Ordering::Acquire)
            || Instant::now() >= prompt.deadline
        {
            let _ = prompt.reply.send(false);
            continue;
        }
        let mut output = io::stderr().lock();
        let shown = (|| -> io::Result<()> {
            writeln!(
                output,
                "\n发送方：{:?}\n保存到：{}",
                prompt.transfer.sender_name,
                save.display()
            )?;
            for file in &prompt.transfer.files {
                writeln!(output, "  {:?} ({:?} 字节)", file.name, file.size)?;
            }
            write!(output, "接受这次传输？[y/N] ")?;
            output.flush()
        })()
        .is_ok();
        drop(output);
        let accept = if shown {
            match read_answer(&mut input) {
                Ok(answer) => answer,
                Err(_) => {
                    let _ = prompt.reply.send(false);
                    break;
                }
            }
        } else {
            false
        };
        let accept = accept
            && !cancel.is_cancelled()
            && !prompt.expired.load(Ordering::Acquire)
            && Instant::now() < prompt.deadline;
        let _ = prompt.reply.send(accept);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn offer() -> IncomingTransfer {
        IncomingTransfer {
            id: "queued".into(),
            sender_name: "sender".into(),
            sender_model: None,
            files: Vec::new(),
            urls: Vec::new(),
            icon: None,
            sender_record_data: None,
        }
    }
    #[test]
    fn full_queue_denies_immediately_and_timeout_expires_the_pending_prompt() {
        let (requests, pending) = mpsc::sync_channel(1);
        let consent = Consent {
            requests,
            cancel: SendCancellation::default(),
            timeout: Duration::from_millis(10),
        };
        assert!(!consent.decide(&offer()));
        let start = Instant::now();
        assert!(
            !consent.decide(&offer()),
            "full queue must not block or implicitly accept"
        );
        assert!(start.elapsed() < Duration::from_millis(100));
        let prompt = pending.recv().unwrap();
        assert!(prompt.expired.load(Ordering::Acquire));
        assert!(
            prompt.reply.send(true).is_err(),
            "late yes cannot authorize an expired request"
        );
    }
    #[test]
    fn only_explicit_complete_yes_accepts_and_eof_denies() {
        for text in ["y\n", "YES\r\n"] {
            assert!(read_answer(&mut text.as_bytes()).unwrap());
        }
        for text in ["", "yes", "\n", "no\n", "maybe\n"] {
            assert!(!read_answer(&mut text.as_bytes()).unwrap());
        }
        let oversized = format!("{}yes\nno\n", "x".repeat(256));
        let mut input = oversized.as_bytes();
        assert!(!read_answer(&mut input).unwrap());
        assert!(!read_answer(&mut input).unwrap());
    }
    #[test]
    fn timeout_closed_ui_and_cancellation_deny() {
        let cancel = SendCancellation::default();
        let (tx, rx) = mpsc::sync_channel(1);
        assert!(!wait_answer(&rx, Instant::now(), &cancel));
        tx.send(true).unwrap();
        cancel.cancel();
        assert!(!wait_answer(
            &rx,
            Instant::now() + Duration::from_secs(1),
            &cancel
        ));
        drop(tx);
        let (_, closed) = mpsc::sync_channel(1);
        assert!(!wait_answer(
            &closed,
            Instant::now() + Duration::from_secs(1),
            &SendCancellation::default()
        ));
    }
}
