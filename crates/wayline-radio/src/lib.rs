//! Rust host-side client for the ESP32 `owl-radio` protocol.

use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use wayline_proto::{Decoder, FilterMode, HostMessage, RadioMessage, COBS_MAX};

pub const MSG_HELLO: u8 = HostMessage::Hello as u8;
pub const MSG_STATS: u8 = HostMessage::Stats as u8;

#[derive(Debug)]
pub enum RadioError {
    Io(io::Error),
    Serial(serialport::Error),
    Timeout,
    Protocol(&'static str),
    Remote(i32),
}

impl std::fmt::Display for RadioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Serial(error) => write!(f, "serial error: {error}"),
            Self::Timeout => f.write_str("radio request timed out"),
            Self::Protocol(message) => f.write_str(message),
            Self::Remote(error) => write!(f, "radio returned error {error}"),
        }
    }
}

impl std::error::Error for RadioError {}

impl From<io::Error> for RadioError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serialport::Error> for RadioError {
    fn from(error: serialport::Error) -> Self {
        Self::Serial(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub protocol: u8,
    pub mac: [u8; 6],
    pub target: String,
    pub idf: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub rx: u32,
    pub rx_drop: u32,
    pub tx_ok: u32,
    pub tx_err: u32,
    pub tx_acked: u32,
    pub tx_noack: u32,
    pub bad_msg: u32,
}

pub struct RadioClient<P> {
    port: P,
    decoder: Decoder<COBS_MAX>,
    encoded: [u8; COBS_MAX],
}

impl<P: Read + Write> RadioClient<P> {
    pub fn new(port: P) -> Self {
        Self {
            port,
            decoder: Decoder::new(),
            encoded: [0; COBS_MAX],
        }
    }

    pub fn into_inner(self) -> P {
        self.port
    }

    pub fn hello(&mut self, timeout: Duration) -> Result<Hello, RadioError> {
        let body = self.request(MSG_HELLO, &[], RadioMessage::HelloResponse as u8, timeout)?;
        if body.len() < 55 {
            return Err(RadioError::Protocol("short HELLO response"));
        }
        Ok(Hello {
            protocol: body[0],
            mac: body[1..7].try_into().unwrap(),
            target: fixed_string(&body[7..23]),
            idf: fixed_string(&body[23..55]),
        })
    }

    pub fn stats(&mut self, timeout: Duration) -> Result<Stats, RadioError> {
        let body = self.request(MSG_STATS, &[], RadioMessage::StatsResponse as u8, timeout)?;
        if body.len() < 28 {
            return Err(RadioError::Protocol("short stats response"));
        }
        let mut values = [0u32; 7];
        for (index, value) in values.iter_mut().enumerate() {
            let at = index * 4;
            *value = u32::from_le_bytes(body[at..at + 4].try_into().unwrap());
        }
        Ok(Stats {
            rx: values[0],
            rx_drop: values[1],
            tx_ok: values[2],
            tx_err: values[3],
            tx_acked: values[4],
            tx_noack: values[5],
            bad_msg: values[6],
        })
    }

    pub fn set_channel(&mut self, channel: u8, timeout: Duration) -> Result<(), RadioError> {
        self.ack_request(HostMessage::SetChannel as u8, &[channel], timeout)
    }

    pub fn set_filter(&mut self, filter: FilterMode, timeout: Duration) -> Result<(), RadioError> {
        self.ack_request(HostMessage::SetFilter as u8, &[filter as u8], timeout)
    }

    pub fn set_mac(&mut self, mac: [u8; 6], timeout: Duration) -> Result<(), RadioError> {
        self.ack_request(HostMessage::SetMac as u8, &mac, timeout)
    }

    pub fn transmit(
        &mut self,
        rate: u8,
        count: u8,
        gap_ms: u16,
        frame: &[u8],
        timeout: Duration,
    ) -> Result<(), RadioError> {
        let mut body = Vec::with_capacity(frame.len() + 4);
        body.extend_from_slice(&[rate, count]);
        body.extend_from_slice(&gap_ms.to_le_bytes());
        body.extend_from_slice(frame);
        self.ack_request(HostMessage::Tx as u8, &body, timeout)
    }

    pub fn time_us(&mut self, timeout: Duration) -> Result<u64, RadioError> {
        let body = self.request(
            HostMessage::Time as u8,
            &[],
            RadioMessage::TimeResponse as u8,
            timeout,
        )?;
        if body.len() != 8 {
            return Err(RadioError::Protocol("short time response"));
        }
        Ok(u64::from_le_bytes(body.try_into().unwrap()))
    }

    fn ack_request(
        &mut self,
        message: u8,
        body: &[u8],
        timeout: Duration,
    ) -> Result<(), RadioError> {
        self.request(message, body, RadioMessage::Ack as u8, timeout)
            .map(|_| ())
    }

    pub fn request(
        &mut self,
        message: u8,
        body: &[u8],
        expected: u8,
        timeout: Duration,
    ) -> Result<Vec<u8>, RadioError> {
        let mut payload = Vec::with_capacity(body.len() + 1);
        payload.push(message);
        payload.extend_from_slice(body);
        let encoded_len = wayline_proto::encode_frame(&payload, &mut self.encoded)
            .map_err(|_| RadioError::Protocol("message is too large"))?;
        self.port.write_all(&self.encoded[..encoded_len])?;
        self.port.flush()?;

        let deadline = Instant::now() + timeout;
        let mut byte = [0u8; 1];
        loop {
            if Instant::now() >= deadline {
                return Err(RadioError::Timeout);
            }
            match self.port.read_exact(&mut byte) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue
                }
                Err(error) => return Err(error.into()),
            }
            if let Some(frame) = self.decoder.push(byte[0]) {
                if frame.is_empty() {
                    continue;
                }
                let kind = frame[0];
                if kind == RadioMessage::Log as u8 || kind == RadioMessage::Rx as u8 {
                    continue;
                }
                if kind == RadioMessage::Ack as u8 {
                    if frame.len() < 6 || frame[1] != message {
                        continue;
                    }
                    let error = i32::from_le_bytes(frame[2..6].try_into().unwrap());
                    if error != 0 {
                        return Err(RadioError::Remote(error));
                    }
                    if expected == RadioMessage::Ack as u8 {
                        return Ok(frame[1..].to_vec());
                    }
                    continue;
                }
                if kind == expected {
                    return Ok(frame[1..].to_vec());
                }
            }
        }
    }
}

impl RadioClient<Box<dyn serialport::SerialPort>> {
    pub fn open_serial(port: &str, baud: u32, timeout: Duration) -> Result<Self, RadioError> {
        let mut serial = serialport::new(port, baud).timeout(timeout).open()?;
        // Keep opening the port from toggling EN/IO0 and dropping the ESP32 into its bootloader.
        let _ = serial.write_data_terminal_ready(false);
        let _ = serial.write_request_to_send(false);
        Ok(Self::new(serial))
    }
}

fn fixed_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

pub fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct FakePort {
        input: VecDeque<u8>,
        output: Vec<u8>,
    }

    impl Read for FakePort {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.input.is_empty() {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "empty"));
            }
            bytes[0] = self.input.pop_front().unwrap();
            Ok(1)
        }
    }

    impl Write for FakePort {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn parses_hello_and_formats_mac() {
        let mut body = vec![2];
        body.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        let mut target = [0u8; 16];
        target[..7].copy_from_slice(b"esp32s3");
        body.extend_from_slice(&target);
        let mut idf = [0u8; 32];
        idf[..9].copy_from_slice(b"ESP-IDF 6");
        body.extend_from_slice(&idf);
        let mut response = vec![RadioMessage::HelloResponse as u8];
        response.extend_from_slice(&body);
        let mut encoded = [0u8; COBS_MAX];
        let encoded_len = wayline_proto::encode_frame(&response, &mut encoded).unwrap();
        let port = FakePort {
            input: encoded[..encoded_len].iter().copied().collect(),
            output: Vec::new(),
        };
        let mut client = RadioClient::new(port);
        let hello = client.hello(Duration::from_secs(1)).unwrap();
        assert_eq!(hello.protocol, 2);
        assert_eq!(format_mac(hello.mac), "01:02:03:04:05:06");
    }
}
