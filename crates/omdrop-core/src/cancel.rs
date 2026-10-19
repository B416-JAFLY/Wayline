//! Cooperative archive cancellation and interruption of a blocked TLS socket.
use std::{
    io,
    net::{Shutdown, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

#[derive(Clone, Default)]
pub struct SendCancellation(Arc<State>);

#[derive(Default)]
struct State {
    cancelled: AtomicBool,
    socket: Mutex<Option<TcpStream>>,
}

impl SendCancellation {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        if let Some(socket) = self.0.socket.lock().unwrap().as_ref() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    pub(crate) fn check(&self) -> io::Result<()> {
        if self.is_cancelled() {
            Err(io::Error::other("transfer cancelled"))
        } else {
            Ok(())
        }
    }

    pub(crate) fn attach(&self, socket: &TcpStream) -> io::Result<()> {
        let mut active = self.0.socket.lock().unwrap();
        self.check()?;
        *active = Some(socket.try_clone()?);
        Ok(())
    }

    pub(crate) fn detach(&self) {
        *self.0.socket.lock().unwrap() = None;
    }
}

pub(crate) struct CheckedWriter<'a, W>(pub W, pub &'a SendCancellation);
impl<W: io::Write> io::Write for CheckedWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.1.check()?;
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.1.check()?;
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        time::{Duration, Instant},
    };

    #[test]
    fn cancellation_interrupts_blocked_socket_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_server, _) = listener.accept().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let cancel = SendCancellation::default();
        cancel.attach(&client).unwrap();
        let worker = std::thread::spawn(move || client.read(&mut [0; 1]));
        let start = Instant::now();
        cancel.cancel();
        assert!(!matches!(worker.join().unwrap(), Ok(1..)));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn cancelled_archive_writer_never_writes_or_reuses_a_socket() {
        let cancel = SendCancellation::default();
        cancel.cancel();
        let mut output = Vec::new();
        assert!(CheckedWriter(&mut output, &cancel)
            .write_all(b"private data")
            .is_err());
        assert!(output.is_empty());
        assert!(cancel.check().is_err());
    }
}
