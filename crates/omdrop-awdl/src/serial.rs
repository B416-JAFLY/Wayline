//! Bounded, full-duplex radio I/O. Blocking drivers run only on owned workers.

use crate::{
    driver::{Air, Received},
    invalid,
    wire::{self, ReceiveClock},
};
use omdrop_proto::{encode_frame, Decoder, HostMessage, RadioMessage, COBS_MAX, PROTO_VERSION};
use omdrop_radio::Hello;
use owly_awdl::Frame;
use std::{
    io::{self, Read, Write},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc as async_mpsc, oneshot};

const QUEUE: usize = 128;
const POLL: Duration = Duration::from_millis(100);
const BATCH_BYTES: usize = 4096;
const BATCH_FRAMES: usize = 8;

struct Pending {
    request: u8,
    expected: u8,
    response: oneshot::Sender<io::Result<Vec<u8>>>,
}

struct Shared {
    stopped: AtomicBool,
    error: Mutex<Option<String>>,
    pending: Mutex<Option<Pending>>,
    rx_dropped: AtomicU64,
    tx_errors: AtomicU64,
    tx_acked: AtomicU64,
    tx_unacked: AtomicU64,
}

impl Shared {
    fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            error: Mutex::new(None),
            pending: Mutex::new(None),
            rx_dropped: AtomicU64::new(0),
            tx_errors: AtomicU64::new(0),
            tx_acked: AtomicU64::new(0),
            tx_unacked: AtomicU64::new(0),
        }
    }
    fn fail(&self, error: io::Error) {
        let mut stored = self.error.lock().unwrap();
        if stored.is_none() {
            *stored = Some(error.to_string());
        }
        self.stopped.store(true, Ordering::Release);
        if let Some(p) = self.pending.lock().unwrap().take() {
            let _ = p.response.send(Err(io::Error::other(error.to_string())));
        }
    }

    fn error(&self) -> io::Error {
        io::Error::other(
            self.error
                .lock()
                .unwrap()
                .as_deref()
                .unwrap_or("radio stopped"),
        )
    }
}

/// Restricted control channel used by startup and optional board feedback.
#[derive(Clone)]
pub struct Control {
    shared: Arc<Shared>,
    tx: mpsc::SyncSender<Vec<u8>>,
    requests: Arc<tokio::sync::Mutex<()>>,
    control_failed: Arc<AtomicBool>,
}

impl Control {
    fn enqueue(&self, message: &[u8]) -> io::Result<()> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err(self.shared.error());
        }
        let mut buffer = [0; COBS_MAX];
        let len =
            encode_frame(message, &mut buffer).map_err(|_| invalid("radio message too large"))?;
        self.tx
            .try_send(buffer[..len].to_vec())
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => io::ErrorKind::WouldBlock.into(),
                mpsc::TrySendError::Disconnected(_) => self.shared.error(),
            })
    }

    async fn request(
        &self,
        request: HostMessage,
        body: &[u8],
        expected: RadioMessage,
        wait: Duration,
    ) -> io::Result<Vec<u8>> {
        let _guard = self.requests.lock().await;
        if self.control_failed.load(Ordering::Acquire) {
            return Err(io::Error::other(
                "radio control channel timed out; restart the bridge to resynchronize",
            ));
        }
        let (tx, rx) = oneshot::channel();
        let mut message = Vec::with_capacity(body.len() + 1);
        message.push(request as u8);
        message.extend_from_slice(body);
        *self.shared.pending.lock().unwrap() = Some(Pending {
            request: request as u8,
            expected: expected as u8,
            response: tx,
        });
        if let Err(e) = self.enqueue(&message) {
            self.shared.pending.lock().unwrap().take();
            return Err(e);
        }
        // Dropping/cancelling a request poisons only the control channel. A late
        // reply must never be mistaken for the next query; radio traffic continues.
        struct Guard<'a>(&'a Control, bool);
        impl Drop for Guard<'_> {
            fn drop(&mut self) {
                self.0.shared.pending.lock().unwrap().take();
                if self.1 {
                    self.0.control_failed.store(true, Ordering::Release);
                }
            }
        }
        let mut pending = Guard(self, true);
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(result)) => {
                pending.1 = false;
                result
            }
            Ok(Err(_)) => Err(self.shared.error()),
            Err(_) => {
                // HELLO retries are identical, so an earlier HELLO is still valid.
                pending.1 = !matches!(request, HostMessage::Hello);
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "radio command timed out",
                ))
            }
        }
    }

    async fn command(&self, request: HostMessage, body: &[u8]) -> io::Result<()> {
        self.request(request, body, RadioMessage::Ack, Duration::from_secs(3))
            .await
            .map(|_| ())
    }

    pub fn publish(&self, frame: &[u8]) -> io::Result<()> {
        use omdrop_proto::transfer::{Transfer, FRAME_LEN, HEADER_LEN};
        if frame.len() != FRAME_LEN || Transfer::decode(&frame[..HEADER_LEN]).is_none() {
            return Err(invalid("invalid board feedback"));
        }
        let mut msg = Vec::with_capacity(frame.len() + 1);
        msg.push(HostMessage::Transfer as u8);
        msg.extend_from_slice(frame);
        self.enqueue(&msg)
    }

    pub async fn query(&self, direction: u8) -> io::Result<Vec<u8>> {
        if direction > 1 {
            return Err(invalid("invalid transfer direction"));
        }
        let body = self
            .request(
                HostMessage::TransferStatus,
                &[direction],
                RadioMessage::TransferResponse,
                Duration::from_millis(500),
            )
            .await?;
        let state = omdrop_proto::transfer::Transfer::decode(&body)
            .ok_or_else(|| invalid("invalid board status"))?;
        if state.direction != direction {
            return Err(invalid("board status direction mismatch"));
        }
        Ok(body)
    }
}

pub struct Radio {
    control: Control,
    received: async_mpsc::Receiver<Received>,
    workers: Vec<JoinHandle<()>>,
}

impl Radio {
    pub async fn open(port: &str, channel: u8) -> io::Result<(Self, Hello)> {
        // All settings are applied before cloning the port; clones share OS state.
        let mut port = serialport::new(port, 3_000_000)
            .timeout(POLL)
            .dtr_on_open(false)
            .open()
            .map_err(io::Error::other)?;
        port.write_data_terminal_ready(false)
            .map_err(io::Error::other)?;
        port.write_request_to_send(false)
            .map_err(io::Error::other)?;
        let reader = port.try_clone().map_err(io::Error::other)?;
        let radio = Self::start(reader, port, channel)?;
        let hello = radio.initialize(channel).await?;
        Ok((radio, hello))
    }

    async fn initialize(&self, channel: u8) -> io::Result<Hello> {
        let mut hello = None;
        for _ in 0..10 {
            match self
                .control
                .request(
                    HostMessage::Hello,
                    &[],
                    RadioMessage::HelloResponse,
                    Duration::from_millis(300),
                )
                .await
            {
                Ok(body) => {
                    hello = Some(parse_hello(&body)?);
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
                Err(e) => return Err(e),
            }
        }
        let hello = hello
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "ESP32 did not answer HELLO"))?;
        crate::configuration(hello.mac, "Omdrop", &hello.target, channel)?;
        self.control
            .command(
                HostMessage::SetFilter,
                &[omdrop_proto::FilterMode::Awdl as u8],
            )
            .await?;
        self.control
            .command(HostMessage::SetChannel, &[channel])
            .await?;
        tracing::info!(target = %hello.target, protocol = hello.protocol, "radio ready");
        Ok(hello)
    }

    fn start<R: Read + Send + 'static, W: Write + Send + 'static>(
        reader: R,
        writer: W,
        channel: u8,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared::new());
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let (receive_tx, received) = async_mpsc::channel(256);
        let control = Control {
            shared: shared.clone(),
            tx,
            requests: Arc::new(tokio::sync::Mutex::new(())),
            control_failed: Arc::new(AtomicBool::new(false)),
        };
        let mut radio = Self {
            control,
            received,
            workers: Vec::new(),
        };
        let read_shared = shared.clone();
        radio.workers.push(
            thread::Builder::new()
                .name("omdrop-radio-read".into())
                .spawn(move || {
                    if let Err(e) = read_frames(reader, &read_shared, receive_tx, channel) {
                        read_shared.fail(e);
                    }
                })?,
        );
        radio.workers.push(
            thread::Builder::new()
                .name("omdrop-radio-write".into())
                .spawn(move || {
                    if let Err(e) = write_frames(writer, &shared, rx) {
                        shared.fail(e);
                    }
                })?,
        );
        Ok(radio)
    }

    pub fn control(&self) -> Control {
        self.control.clone()
    }
}

impl Air for Radio {
    async fn recv(&mut self) -> io::Result<Received> {
        if self.control.shared.stopped.load(Ordering::Acquire) {
            return Err(self.control.shared.error());
        }
        self.received
            .recv()
            .await
            .ok_or_else(|| self.control.shared.error())
    }

    fn send(&mut self, frame: Frame) -> Result<(), Frame> {
        let message = match wire::transmit(&frame) {
            Ok(message) => message,
            Err(e) => {
                self.control.shared.fail(e);
                return Err(frame);
            }
        };
        match self.control.enqueue(&message) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(frame),
            Err(e) => {
                self.control.shared.fail(e);
                Err(frame)
            }
        }
    }
}

impl Drop for Radio {
    fn drop(&mut self) {
        self.control.shared.stopped.store(true, Ordering::Release);
        self.control.shared.pending.lock().unwrap().take();
        for worker in self.workers.drain(..) {
            if worker.join().is_err() {
                tracing::error!("radio worker panicked");
            }
        }
        let s = &self.control.shared;
        tracing::info!(
            rx_dropped = s.rx_dropped.load(Ordering::Relaxed),
            tx_errors = s.tx_errors.load(Ordering::Relaxed),
            tx_acked = s.tx_acked.load(Ordering::Relaxed),
            tx_unacked = s.tx_unacked.load(Ordering::Relaxed),
            "radio workers stopped"
        );
    }
}

fn parse_hello(body: &[u8]) -> io::Result<Hello> {
    if body.len() < 55 || body[0] != PROTO_VERSION {
        return Err(invalid("unsupported or truncated radio HELLO"));
    }
    let string = |b: &[u8]| {
        String::from_utf8_lossy(&b[..b.iter().position(|&v| v == 0).unwrap_or(b.len())])
            .into_owned()
    };
    Ok(Hello {
        protocol: body[0],
        mac: body[1..7].try_into().unwrap(),
        target: string(&body[7..23]),
        idf: string(&body[23..55]),
    })
}

fn read_frames<R: Read>(
    mut reader: R,
    shared: &Shared,
    tx: async_mpsc::Sender<Received>,
    channel: u8,
) -> io::Result<()> {
    let mut decoder = Decoder::<COBS_MAX>::new();
    let mut buffer = [0; 8192];
    let mut clock = ReceiveClock::default();
    while !shared.stopped.load(Ordering::Acquire) {
        let len = match reader.read(&mut buffer) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(e) => return Err(e),
        };
        let arrived = Instant::now();
        for &byte in &buffer[..len] {
            let Some(message) = decoder.push(byte) else {
                continue;
            };
            let Some((&kind, body)) = message.split_first() else {
                continue;
            };
            if kind == RadioMessage::Rx as u8 {
                if let Some(rx) = wire::received(body, channel, arrived, &mut clock)? {
                    // A bounded queue drops newest frames; never blocks command replies.
                    if tx.try_send(rx).is_err() {
                        shared.rx_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            } else if kind == RadioMessage::Log as u8 {
                tracing::debug!(message = %String::from_utf8_lossy(body), "radio log");
            } else {
                dispatch_reply(shared, kind, body);
            }
        }
    }
    Ok(())
}

fn dispatch_reply(shared: &Shared, kind: u8, body: &[u8]) {
    if kind == RadioMessage::TxDone as u8 {
        if let Some(&acked) = body.first() {
            let count = if acked != 0 {
                &shared.tx_acked
            } else {
                &shared.tx_unacked
            };
            count.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }
    if kind == RadioMessage::Ack as u8 && body.len() == 5 && body[0] == HostMessage::Tx as u8 {
        let code = i32::from_le_bytes(body[1..5].try_into().unwrap());
        if code != 0 {
            let count = shared.tx_errors.fetch_add(1, Ordering::Relaxed) + 1;
            if count.is_power_of_two() {
                tracing::warn!(code, count, "ESP32 rejected transmitted frames");
            }
        }
        return;
    }
    let mut pending = shared.pending.lock().unwrap();
    let Some(p) = pending.as_ref() else {
        return;
    };
    if kind != p.expected {
        return;
    }
    if kind == RadioMessage::Ack as u8 && (body.len() != 5 || body[0] != p.request) {
        return;
    }
    let p = pending.take().unwrap();
    let result = if kind == RadioMessage::Ack as u8 {
        let code = i32::from_le_bytes(body[1..5].try_into().unwrap());
        if code != 0 {
            Err(io::Error::other(format!(
                "radio command {} failed: 0x{code:x}",
                p.request
            )))
        } else {
            Ok(body.to_vec())
        }
    } else {
        Ok(body.to_vec())
    };
    let _ = p.response.send(result);
}

fn write_frames<W: Write>(
    mut writer: W,
    shared: &Shared,
    rx: mpsc::Receiver<Vec<u8>>,
) -> io::Result<()> {
    let mut deferred = None;
    while !shared.stopped.load(Ordering::Acquire) {
        let first = match deferred.take() {
            Some(message) => message,
            None => match rx.recv_timeout(POLL) {
                Ok(message) => message,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            },
        };
        let mut batch = Vec::with_capacity(BATCH_BYTES);
        batch.extend(first);
        for _ in 1..BATCH_FRAMES {
            let Ok(message) = rx.try_recv() else {
                break;
            };
            if batch.len() + message.len() > BATCH_BYTES {
                deferred = Some(message);
                break;
            }
            batch.extend(message);
        }
        write_complete(&mut writer, &batch, &shared.stopped)?;
    }
    Ok(())
}

fn write_complete<W: Write>(
    writer: &mut W,
    mut bytes: &[u8],
    stopped: &AtomicBool,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !bytes.is_empty() {
        if stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match writer.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    struct Input(mpsc::Receiver<Vec<u8>>);
    impl Read for Input {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.0.recv_timeout(Duration::from_millis(10)) {
                Ok(bytes) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                Err(mpsc::RecvTimeoutError::Timeout) => Err(io::ErrorKind::TimedOut.into()),
                Err(_) => Ok(0),
            }
        }
    }
    struct Output(Arc<Mutex<Vec<u8>>>);
    impl Write for Output {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            let n = b.len().min(3);
            self.0.lock().unwrap().extend_from_slice(&b[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("streaming writer must not flush each frame")
        }
    }
    type OutputBytes = Arc<Mutex<Vec<u8>>>;
    pub(crate) fn fixture() -> (Radio, mpsc::Sender<Vec<u8>>, OutputBytes) {
        let (tx, rx) = mpsc::channel();
        let out = Arc::new(Mutex::new(Vec::new()));
        (
            Radio::start(Input(rx), Output(out.clone()), 6).unwrap(),
            tx,
            out,
        )
    }
    pub(crate) fn encoded(message: &[u8]) -> Vec<u8> {
        let mut b = [0; COBS_MAX];
        let n = encode_frame(message, &mut b).unwrap();
        b[..n].to_vec()
    }

    #[tokio::test]
    async fn partial_writes_preserve_fifo_and_control_reply_ignores_tx_ack() {
        let (radio, tx, out) = fixture();
        let control = radio.control();
        let request =
            tokio::spawn(async move { control.command(HostMessage::SetChannel, &[6]).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while out.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tx.send(encoded(&[0x80, 5, 0, 0, 0, 0])).unwrap();
        tx.send(encoded(&[0x80, 2, 0, 0, 0, 0])).unwrap();
        request.await.unwrap().unwrap();
        assert_eq!(*out.lock().unwrap(), encoded(&[2, 6]));
        drop(radio);
    }

    #[tokio::test]
    async fn control_timeout_isolated_from_radio_and_rejects_late_reply() {
        let (mut radio, tx, _) = fixture();
        let control = radio.control();
        assert_eq!(
            control
                .request(
                    HostMessage::TransferStatus,
                    &[0],
                    RadioMessage::TransferResponse,
                    Duration::from_millis(10)
                )
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(!control.shared.stopped.load(Ordering::Acquire));
        assert!(control.query(0).await.is_err());
        drop(tx);
        assert!(tokio::time::timeout(Duration::from_secs(1), radio.recv())
            .await
            .unwrap()
            .is_err());
    }

    #[test]
    fn cancellation_and_zero_writes_end_without_spinning() {
        let stop = AtomicBool::new(true);
        write_complete(&mut io::sink(), b"cancelled", &stop).unwrap();
        stop.store(false, Ordering::Relaxed);
        assert_eq!(
            write_complete(&mut &mut [0u8; 0][..], b"x", &stop)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WriteZero
        );
    }

    #[test]
    fn batches_keep_fifo_across_partial_writes_and_byte_limit() {
        struct RecordingWriter {
            bytes: Vec<u8>,
            largest: usize,
        }
        impl Write for RecordingWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.largest = self.largest.max(bytes.len());
                let n = bytes.len().min(17);
                self.bytes.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                panic!("no flush per batch")
            }
        }
        let shared = Shared::new();
        let (tx, rx) = mpsc::sync_channel(QUEUE);
        let mut expected = Vec::new();
        for i in 0..20 {
            let packet = encoded(&vec![i + 1; 1605]);
            expected.extend_from_slice(&packet);
            tx.send(packet).unwrap();
        }
        drop(tx);
        let mut writer = RecordingWriter {
            bytes: Vec::new(),
            largest: 0,
        };
        write_frames(&mut writer, &shared, rx).unwrap();
        assert_eq!(writer.bytes, expected);
        assert!(writer.largest <= BATCH_BYTES);
    }

    #[tokio::test]
    async fn cancelled_query_cannot_consume_a_late_response() {
        let (radio, _input, out) = fixture();
        let control = radio.control();
        let worker = control.clone();
        let query = tokio::spawn(async move { worker.query(0).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while out.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        query.abort();
        assert!(query.await.unwrap_err().is_cancelled());
        assert!(control.query(1).await.is_err());
        assert!(!control.shared.stopped.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn split_radio_frames_are_decoded_and_rx_queue_is_bounded() {
        let (mut radio, input, _) = fixture();
        let mut msg = vec![0; 1 + 10 + 24];
        msg[0] = RadioMessage::Rx as u8;
        msg[6] = 6;
        msg[27..33].copy_from_slice(&owly_awdl::BSSID.0);
        let bytes = encoded(&msg);
        for part in bytes.chunks(2) {
            input.send(part.to_vec()).unwrap();
        }
        let first = tokio::time::timeout(Duration::from_secs(1), radio.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.bytes.len(), 24);
        assert!(first.received_at.is_some());
        for _ in 0..300 {
            input.send(bytes.clone()).unwrap();
        }
        // A control response behind a full RX queue still reaches its waiter.
        let control = radio.control();
        let request =
            tokio::spawn(async move { control.command(HostMessage::SetFilter, &[0]).await });
        tokio::task::yield_now().await;
        input
            .send(encoded(&[
                RadioMessage::Ack as u8,
                HostMessage::SetFilter as u8,
                0,
                0,
                0,
                0,
            ]))
            .unwrap();
        request.await.unwrap().unwrap();
        assert_eq!(radio.received.len(), 256);
    }

    #[tokio::test]
    async fn write_failure_wakes_receive_and_joins_workers() {
        struct Broken;
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "unplugged"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (_tx, rx) = mpsc::channel();
        let mut radio = Radio::start(Input(rx), Broken, 6).unwrap();
        radio.control.enqueue(&[HostMessage::Hello as u8]).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(1), radio.recv())
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("unplugged"));
        drop(radio);
    }

    #[tokio::test]
    async fn startup_requires_compatible_hello_and_configuration_acks() {
        for valid in [true, false] {
            let (radio, input, out) = fixture();
            let device = tokio::spawn(async move {
                let mut expected = encoded(&[HostMessage::Hello as u8]);
                async fn wait_for(out: &OutputBytes, expected: &[u8]) {
                    tokio::time::timeout(Duration::from_secs(1), async {
                        while out.lock().unwrap().len() < expected.len() {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                    })
                    .await
                    .unwrap();
                    assert_eq!(*out.lock().unwrap(), expected);
                }
                wait_for(&out, &expected).await;
                let mut hello = vec![0; 56];
                hello[0] = RadioMessage::HelloResponse as u8;
                hello[1] = if valid { PROTO_VERSION } else { 99 };
                hello[2..8].copy_from_slice(&[2, 0, 0, 0, 0, 1]);
                hello[8..15].copy_from_slice(b"esp32s3");
                input.send(encoded(&hello)).unwrap();
                if valid {
                    for (command, body) in
                        [(HostMessage::SetFilter, 0), (HostMessage::SetChannel, 6)]
                    {
                        expected.extend(encoded(&[command as u8, body]));
                        wait_for(&out, &expected).await;
                        input
                            .send(encoded(&[
                                RadioMessage::Ack as u8,
                                command as u8,
                                0,
                                0,
                                0,
                                0,
                            ]))
                            .unwrap();
                    }
                }
                input
            });
            assert_eq!(radio.initialize(6).await.is_ok(), valid);
            let _input = device.await.unwrap();
            drop(radio);
        }
    }
}
