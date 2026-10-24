//! Allocation-free protocol policy. No ESP-IDF types cross this boundary.

use wayline_proto::{
    decode_frame_in_place, encode_frame_parts, CodecError, FilterMode, HostMessage, COBS_MAX,
    PROTO_MAX_FRAME, PROTO_VERSION, RATE_MCS, RAW_MAX,
};

pub const INVALID_ARG: i32 = 0x102;
pub const INVALID_SIZE: i32 = 0x104;
pub const NOT_SUPPORTED: i32 = 0x106;

#[derive(Debug, PartialEq, Eq)]
pub enum Command<'a> {
    Hello,
    SetChannel(u8),
    SetMac(&'a [u8; 6]),
    SetFilter(FilterMode),
    Tx(TxRequest<'a>),
    Time,
    Stats,
    UiStatus,
    AggregationStatus,
    UplinkStatus,
    Transfer(&'a [u8]),
    TransferStatus(u8),
}

#[derive(Debug, PartialEq, Eq)]
pub struct TxRequest<'a> {
    pub rate: u8,
    pub count: u8,
    pub gap_ms: u16,
    pub frame: &'a [u8],
}

/// 2.4 GHz diagnostic channels and the fixed 5 GHz AWDL channels supported by the host.
pub fn valid_channel(channel: u8) -> bool {
    matches!(channel, 1..=14 | 44 | 149)
}

pub fn parse_command(message: &[u8]) -> Result<Command<'_>, i32> {
    let (&kind, body) = message.split_first().ok_or(INVALID_SIZE)?;
    Ok(match HostMessage::from_byte(kind).ok_or(NOT_SUPPORTED)? {
        HostMessage::Hello => Command::Hello,
        HostMessage::Time => Command::Time,
        HostMessage::Stats if body == b"UI" => Command::UiStatus,
        HostMessage::Stats if body == b"BA" => Command::AggregationStatus,
        HostMessage::Stats if body == b"UP" => Command::UplinkStatus,
        HostMessage::Stats => Command::Stats,
        HostMessage::Transfer => {
            use wayline_proto::transfer::{Transfer, FRAME_LEN, HEADER_LEN};
            if body.len() != FRAME_LEN {
                return Err(INVALID_SIZE);
            }
            if Transfer::decode(&body[..HEADER_LEN]).is_none() {
                return Err(INVALID_ARG);
            }
            Command::Transfer(body)
        }
        HostMessage::TransferStatus => {
            if body.len() != 1 {
                return Err(INVALID_SIZE);
            }
            if body[0] > 1 {
                return Err(INVALID_ARG);
            }
            Command::TransferStatus(body[0])
        }
        HostMessage::SetChannel => {
            let &channel = body.first().ok_or(INVALID_SIZE)?;
            if !valid_channel(channel) {
                return Err(INVALID_ARG);
            }
            Command::SetChannel(channel)
        }
        HostMessage::SetMac => {
            Command::SetMac(body.get(..6).ok_or(INVALID_SIZE)?.try_into().unwrap())
        }
        HostMessage::SetFilter => Command::SetFilter(
            FilterMode::from_byte(*body.first().ok_or(INVALID_SIZE)?).ok_or(INVALID_ARG)?,
        ),
        HostMessage::Tx => {
            if !(28..=4 + PROTO_MAX_FRAME).contains(&body.len()) {
                return Err(INVALID_SIZE);
            }
            let rate = body[0];
            let valid_rate = if rate & RATE_MCS != 0 {
                rate & 0x0f <= 7
            } else {
                matches!(
                    rate,
                    2 | 4 | 11 | 22 | 12 | 18 | 24 | 36 | 48 | 72 | 96 | 108
                )
            };
            if !valid_rate {
                return Err(INVALID_ARG);
            }
            Command::Tx(TxRequest {
                rate,
                count: body[1].max(1),
                gap_ms: u16::from_le_bytes([body[2], body[3]]),
                frame: &body[4..],
            })
        }
    })
}

pub fn ack_body(request: u8, error: i32) -> [u8; 5] {
    let mut bytes = [0; 5];
    bytes[0] = request;
    bytes[1..].copy_from_slice(&error.to_le_bytes());
    bytes
}

pub fn hello_body(mac: &[u8; 6], target: &[u8], idf: &[u8]) -> [u8; 55] {
    let mut bytes = [0; 55];
    bytes[0] = PROTO_VERSION;
    bytes[1..7].copy_from_slice(mac);
    let target_len = target.len().min(15);
    bytes[7..7 + target_len].copy_from_slice(&target[..target_len]);
    let idf_len = idf.len().min(31);
    bytes[23..23 + idf_len].copy_from_slice(&idf[..idf_len]);
    bytes
}

/// Buffers belong to a single caller, or are protected by the transport TX mutex.
pub struct Encoder {
    encoded: [u8; COBS_MAX],
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    pub const fn new() -> Self {
        Self {
            encoded: [0; COBS_MAX],
        }
    }

    pub fn encode(&mut self, kind: u8, a: &[u8], b: &[u8]) -> Result<&[u8], CodecError> {
        let n = encode_frame_parts(&[&[kind], a, b], &mut self.encoded)?;
        Ok(&self.encoded[..n])
    }
}

pub struct FrameDecoder {
    encoded: [u8; COBS_MAX],
    len: usize,
    overflow: bool,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    pub const fn new() -> Self {
        Self {
            encoded: [0; COBS_MAX],
            len: 0,
            overflow: false,
        }
    }

    /// An invalid delimited frame is reported once, then the stream resynchronizes.
    pub fn push(&mut self, byte: u8) -> Option<Result<&[u8], CodecError>> {
        if byte != 0 {
            if self.len < self.encoded.len() {
                self.encoded[self.len] = byte;
                self.len += 1;
            } else {
                self.overflow = true;
            }
            return None;
        }
        if self.len == 0 {
            return None;
        }
        let len = self.len;
        let overflow = self.overflow;
        self.len = 0;
        self.overflow = false;
        Some(if overflow {
            Err(CodecError::FrameTooLarge)
        } else {
            decode_frame_in_place(&mut self.encoded[..len]).and_then(|n| {
                if n > RAW_MAX - 2 {
                    Err(CodecError::FrameTooLarge)
                } else {
                    Ok(&self.encoded[..n])
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lengths_errors_and_tx_limits_match_wire_contract() {
        assert_eq!(parse_command(&[]), Err(INVALID_SIZE));
        assert_eq!(parse_command(&[7]), Ok(Command::Stats));
        assert_eq!(parse_command(&[7, b'U', b'I']), Ok(Command::UiStatus));
        assert_eq!(parse_command(&[7, b'U', b'P']), Ok(Command::UplinkStatus));
        assert_eq!(
            parse_command(&[7, b'B', b'A']),
            Ok(Command::AggregationStatus)
        );
        for kind in [2, 3, 4, 5] {
            assert_eq!(parse_command(&[kind]), Err(INVALID_SIZE));
        }
        assert_eq!(parse_command(&[0xfe]), Err(NOT_SUPPORTED));
        for channel in [0, 15, 35, 36, 165, 255] {
            assert_eq!(parse_command(&[2, channel]), Err(INVALID_ARG));
        }
        for channel in [1, 6, 14, 44, 149] {
            assert!(valid_channel(channel));
            assert_eq!(
                parse_command(&[2, channel]),
                Ok(Command::SetChannel(channel))
            );
        }
        assert_eq!(parse_command(&[4, 3]), Err(INVALID_ARG));
        let mut message = [0; 5 + PROTO_MAX_FRAME + 1];
        message[0] = 5;
        message[1] = 12;
        message[3] = 0x34;
        message[4] = 0x12;
        let Command::Tx(tx) = parse_command(&message[..29]).unwrap() else {
            panic!()
        };
        assert_eq!((tx.count, tx.gap_ms, tx.frame.len()), (1, 0x1234, 24));
        assert!(parse_command(&message[..5 + PROTO_MAX_FRAME]).is_ok());
        assert_eq!(parse_command(&message), Err(INVALID_SIZE));
        message[1] = 0x88;
        assert_eq!(parse_command(&message[..29]), Err(INVALID_ARG));
    }

    #[test]
    fn replies_keep_protocol_two_layout_and_little_endian_errors() {
        let hello = hello_body(&[1, 2, 3, 4, 5, 6], b"esp32s3", b"v6.0");
        assert_eq!(hello.len(), 55);
        assert_eq!(&hello[..7], &[2, 1, 2, 3, 4, 5, 6]);
        assert_eq!(&hello[7..15], b"esp32s3\0");
        assert_eq!(&hello[23..28], b"v6.0\0");
        assert_eq!(ack_body(5, INVALID_SIZE), [5, 4, 1, 0, 0]);
        assert_eq!(ack_body(3, -1), [3, 255, 255, 255, 255]);
    }

    #[test]
    fn stream_recovers_after_corruption_overflow_and_empty_delimiters() {
        let mut receiver = FrameDecoder::new();
        assert!(receiver.push(0).is_none());
        for _ in 0..COBS_MAX + 100 {
            assert!(receiver.push(1).is_none());
        }
        assert_eq!(receiver.push(0), Some(Err(CodecError::FrameTooLarge)));
        for &b in b"boot log" {
            receiver.push(b);
        }
        assert!(receiver.push(0).unwrap().is_err());
        let mut encoder = Encoder::new();
        for _ in 0..3 {
            for &byte in encoder.encode(2, &[6], &[]).unwrap() {
                if let Some(frame) = receiver.push(byte) {
                    assert_eq!(frame.unwrap(), &[2, 6]);
                }
            }
        }
    }

    #[test]
    fn maximum_nonzero_tx_survives_cobs_expansion_and_split_input() {
        let frame = [0xff; PROTO_MAX_FRAME];
        let mut encoder = Encoder::new();
        let wire = encoder.encode(5, &[12, 1, 0, 0], &frame).unwrap();
        let mut receiver = FrameDecoder::new();
        let mut messages = 0;
        for chunk in wire.chunks(17) {
            for &byte in chunk {
                if let Some(decoded) = receiver.push(byte) {
                    let Command::Tx(tx) = parse_command(decoded.unwrap()).unwrap() else {
                        panic!()
                    };
                    assert_eq!(tx.frame, frame);
                    messages += 1;
                }
            }
        }
        assert_eq!(messages, 1);
    }
}
