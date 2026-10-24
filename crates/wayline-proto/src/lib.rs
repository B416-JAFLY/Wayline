#![cfg_attr(not(feature = "std"), no_std)]

//! The binary protocol shared by the ESP32 radio and its host.
//!
//! The on-wire format is deliberately small and allocation-free:
//!
//! `type:u8 || body || crc16:u16le`, COBS encoded, followed by `0x00`.
//!
//! This crate is the canonical implementation for new Rust code. The ESP-IDF firmware can use
//! the same functions through the small C ABI crate in `firmware` while older C callers
//! continue to use the protocol constants in `crates/wayline-proto/include/proto.h`.

pub const PROTO_VERSION: u8 = 2;
pub mod transfer;
pub const PROTO_MAX_FRAME: usize = 1600;
pub const MESSAGE_METADATA_MAX: usize = 16;

/// A decoded frame can contain the type, the maximum body, and its two-byte CRC.
pub const RAW_MAX: usize = 1 + MESSAGE_METADATA_MAX + PROTO_MAX_FRAME + 2;
pub const COBS_MAX: usize = RAW_MAX + RAW_MAX / 254 + 2;

pub const RATE_MCS: u8 = 0x80;
pub const RATE_MCS_SGI: u8 = 0x10;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMessage {
    Hello = 0x01,
    SetChannel = 0x02,
    SetMac = 0x03,
    SetFilter = 0x04,
    Tx = 0x05,
    Time = 0x06,
    Stats = 0x07,
    Transfer = 0x08,
    TransferStatus = 0x09,
}

impl HostMessage {
    pub const fn from_byte(value: u8) -> Option<Self> {
        Some(match value {
            0x01 => Self::Hello,
            0x02 => Self::SetChannel,
            0x03 => Self::SetMac,
            0x04 => Self::SetFilter,
            0x05 => Self::Tx,
            0x06 => Self::Time,
            0x07 => Self::Stats,
            0x08 => Self::Transfer,
            0x09 => Self::TransferStatus,
            _ => return None,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioMessage {
    Ack = 0x80,
    HelloResponse = 0x81,
    TxDone = 0x86,
    TimeResponse = 0x87,
    StatsResponse = 0x88,
    TransferResponse = 0x89,
    Rx = 0x90,
    Log = 0x91,
}

impl RadioMessage {
    pub const fn from_byte(value: u8) -> Option<Self> {
        Some(match value {
            0x80 => Self::Ack,
            0x81 => Self::HelloResponse,
            0x86 => Self::TxDone,
            0x87 => Self::TimeResponse,
            0x88 => Self::StatsResponse,
            0x89 => Self::TransferResponse,
            0x90 => Self::Rx,
            0x91 => Self::Log,
            _ => return None,
        })
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    Awdl = 0,
    ManagementAndData = 1,
    All = 2,
}

impl FilterMode {
    pub const fn from_byte(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Awdl,
            1 => Self::ManagementAndData,
            2 => Self::All,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecError {
    OutputTooSmall,
    InvalidCode,
    InvalidCrc,
    EmptyFrame,
    FrameTooLarge,
}

// 512 bytes of read-only data replace eight polynomial steps per USB byte.
const CRC16_TABLE: [u16; 256] = {
    let mut table = [0; 256];
    let mut i = 0;
    while i < table.len() {
        let mut crc = (i as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

#[inline]
fn crc16_byte(crc: u16, byte: u8) -> u16 {
    (crc << 8) ^ CRC16_TABLE[usize::from((crc >> 8) as u8 ^ byte)]
}

/// CRC-16/CCITT-FALSE for the shared radio wire protocol.
pub fn crc16_ccitt_false(bytes: &[u8]) -> u16 {
    bytes
        .iter()
        .fold(0xffff, |crc, &byte| crc16_byte(crc, byte))
}

/// Encode bytes with COBS. The returned length includes the trailing zero delimiter.
pub fn cobs_encode(input: &[u8], output: &mut [u8]) -> Result<usize, CodecError> {
    if output.len() < input.len().saturating_add(input.len() / 254 + 2) {
        return Err(CodecError::OutputTooSmall);
    }

    let mut code_index = 0usize;
    let mut write_index = 1usize;
    let mut code = 1u8;

    for &byte in input {
        if byte != 0 {
            output[write_index] = byte;
            write_index += 1;
            code = code.wrapping_add(1);
        }

        if byte == 0 || code == 0xff {
            output[code_index] = code;
            code_index = write_index;
            write_index += 1;
            code = 1;
        }
    }

    output[code_index] = code;
    output[write_index] = 0;
    Ok(write_index + 1)
}

/// Decode COBS bytes into a separate output buffer. The input must not include the delimiter.
pub fn cobs_decode(input: &[u8], output: &mut [u8]) -> Result<usize, CodecError> {
    if input.is_empty() {
        return Err(CodecError::EmptyFrame);
    }

    let mut read_index = 0usize;
    let mut write_index = 0usize;
    while read_index < input.len() {
        let code = input[read_index];
        read_index += 1;
        if code == 0 || read_index + usize::from(code) - 1 > input.len() {
            return Err(CodecError::InvalidCode);
        }

        for _ in 1..code {
            if write_index >= output.len() {
                return Err(CodecError::OutputTooSmall);
            }
            output[write_index] = input[read_index];
            read_index += 1;
            write_index += 1;
        }

        if code != 0xff && read_index < input.len() {
            if write_index >= output.len() {
                return Err(CodecError::OutputTooSmall);
            }
            output[write_index] = 0;
            write_index += 1;
        }
    }

    Ok(write_index)
}

/// Decode COBS in place. This is the allocation-free path used by the firmware ABI.
pub fn cobs_decode_in_place(buffer: &mut [u8]) -> Result<usize, CodecError> {
    if buffer.is_empty() {
        return Err(CodecError::EmptyFrame);
    }

    let mut read_index = 0usize;
    let mut write_index = 0usize;
    while read_index < buffer.len() {
        let code = buffer[read_index];
        read_index += 1;
        if code == 0 || read_index + usize::from(code) - 1 > buffer.len() {
            return Err(CodecError::InvalidCode);
        }

        // Copying forwards is safe because write_index never exceeds read_index. Preserve the
        // source byte before writing in case a future refactor changes that invariant.
        for _ in 1..code {
            let byte = buffer[read_index];
            read_index += 1;
            buffer[write_index] = byte;
            write_index += 1;
        }

        if code != 0xff && read_index < buffer.len() {
            buffer[write_index] = 0;
            write_index += 1;
        }
    }

    Ok(write_index)
}

/// Encode a complete protocol frame from the type and body bytes.
pub fn encode_frame(message: &[u8], output: &mut [u8]) -> Result<usize, CodecError> {
    encode_frame_parts(&[message], output)
}

/// Encode discontiguous type/body slices without a raw-frame staging buffer.
/// CRC and COBS are produced in one pass; parts may be empty or split anywhere.
/// The output is unchanged if the message or output capacity is invalid.
pub fn encode_frame_parts(parts: &[&[u8]], output: &mut [u8]) -> Result<usize, CodecError> {
    let message_len = parts.iter().try_fold(0usize, |len, part| {
        len.checked_add(part.len()).ok_or(CodecError::FrameTooLarge)
    })?;
    if message_len == 0 {
        return Err(CodecError::EmptyFrame);
    }
    if message_len > RAW_MAX - 2 {
        return Err(CodecError::FrameTooLarge);
    }
    let raw_len = message_len + 2;
    if output.len() < raw_len + raw_len / 254 + 2 {
        return Err(CodecError::OutputTooSmall);
    }

    let mut code_index = 0;
    let mut write_index = 1;
    let mut code = 1u8;
    let mut push = |byte: u8| {
        if byte != 0 {
            output[write_index] = byte;
            write_index += 1;
            code += 1;
        }
        if byte == 0 || code == 0xff {
            output[code_index] = code;
            code_index = write_index;
            write_index += 1;
            code = 1;
        }
    };
    let mut crc = 0xffff;
    for part in parts {
        for &byte in *part {
            crc = crc16_byte(crc, byte);
            push(byte);
        }
    }
    for byte in crc.to_le_bytes() {
        push(byte);
    }
    output[code_index] = code;
    output[write_index] = 0;
    Ok(write_index + 1)
}

/// Decode a complete frame in place, returning the type and body length (CRC removed).
/// `buffer` must exclude the trailing COBS delimiter.
pub fn decode_frame_in_place(buffer: &mut [u8]) -> Result<usize, CodecError> {
    let decoded_len = cobs_decode_in_place(buffer)?;
    if decoded_len < 3 {
        return Err(CodecError::EmptyFrame);
    }
    let payload_len = decoded_len - 2;
    let expected = u16::from(buffer[payload_len]) | (u16::from(buffer[payload_len + 1]) << 8);
    if crc16_ccitt_false(&buffer[..payload_len]) != expected {
        return Err(CodecError::InvalidCrc);
    }
    Ok(payload_len)
}

/// Incremental decoder for a byte stream. It ignores boot logs and malformed frames and returns
/// only a frame whose CRC is valid.
pub struct Decoder<const N: usize = COBS_MAX> {
    buffer: [u8; N],
    len: usize,
    overflow: bool,
}

impl<const N: usize> Decoder<N> {
    pub const fn new() -> Self {
        Self {
            buffer: [0; N],
            len: 0,
            overflow: false,
        }
    }

    pub fn reset(&mut self) {
        self.len = 0;
        self.overflow = false;
    }

    /// Push one byte. A valid frame returns a borrowed type/body slice; malformed frames return
    /// `None` after resetting so the next delimiter can recover the stream.
    pub fn push(&mut self, byte: u8) -> Option<&[u8]> {
        if byte != 0 {
            if self.len < N {
                self.buffer[self.len] = byte;
                self.len += 1;
            } else {
                self.overflow = true;
            }
            return None;
        }

        if self.len == 0 {
            return None;
        }

        let payload_len = if self.overflow {
            None
        } else {
            decode_frame_in_place(&mut self.buffer[..self.len]).ok()
        };
        self.len = 0;
        self.overflow = false;
        payload_len.map(|len| &self.buffer[..len])
    }
}

impl<const N: usize> Default for Decoder<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_crc(bytes: &[u8]) -> u16 {
        let mut crc = 0xffff;
        for &byte in bytes {
            crc ^= u16::from(byte) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    (crc << 1) ^ 0x1021
                } else {
                    crc << 1
                };
            }
        }
        crc
    }

    #[test]
    fn table_crc_matches_bitwise_reference() {
        assert_eq!(crc16_ccitt_false(&[]), 0xffff);
        for value in 0..=u16::MAX {
            let bytes = value.to_le_bytes();
            assert_eq!(crc16_ccitt_false(&bytes), reference_crc(&bytes));
        }
    }

    #[test]
    fn segmented_encoding_is_wire_identical_at_every_frame_length() {
        let mut message = [0; RAW_MAX - 2];
        let mut raw = [0; RAW_MAX];
        let mut expected = [0; COBS_MAX];
        let mut actual = [0; COBS_MAX];
        for pattern in 0..3 {
            for (i, byte) in message.iter_mut().enumerate() {
                *byte = match pattern {
                    0 => 0,
                    1 => 0xff,
                    _ => (i * 131 + i / 7) as u8,
                };
            }
            for len in 1..=message.len() {
                raw[..len].copy_from_slice(&message[..len]);
                raw[len..len + 2].copy_from_slice(&reference_crc(&message[..len]).to_le_bytes());
                let expected_len = cobs_encode(&raw[..len + 2], &mut expected).unwrap();
                let split = (len / 2).max(1);
                let parts: &[&[u8]] = &[
                    &[],
                    &message[..1],
                    &message[1..split],
                    &[],
                    &message[split..len],
                ];
                let actual_len = encode_frame_parts(parts, &mut actual).unwrap();
                assert_eq!(
                    &actual[..actual_len],
                    &expected[..expected_len],
                    "pattern {pattern}, length {len}"
                );
                assert_eq!(
                    decode_frame_in_place(&mut actual[..actual_len - 1]),
                    Ok(len)
                );
                assert_eq!(&actual[..len], &message[..len]);
            }
        }
    }

    #[test]
    fn segmented_encoding_checks_capacity_before_writing() {
        let mut output = [0xa5; COBS_MAX];
        assert_eq!(
            encode_frame_parts(&[&[], &[]], &mut output),
            Err(CodecError::EmptyFrame)
        );
        let oversized = [1; RAW_MAX - 2];
        assert_eq!(
            encode_frame_parts(&[&oversized, &[2]], &mut output),
            Err(CodecError::FrameTooLarge)
        );
        assert_eq!(
            encode_frame_parts(&[&[1]], &mut output[..4]),
            Err(CodecError::OutputTooSmall)
        );
        assert!(output.iter().all(|&byte| byte == 0xa5));
        assert_eq!(encode_frame_parts(&[&[1]], &mut output[..5]), Ok(5));
    }

    #[test]
    fn crc_matches_original_algorithm() {
        assert_eq!(crc16_ccitt_false(b"123456789"), 0x29b1);
    }

    #[test]
    fn cobs_round_trips_zeroes_and_long_runs() {
        let mut input = [0u8; 700];
        for (i, byte) in input.iter_mut().enumerate() {
            *byte = if i % 37 == 0 { 0 } else { (i % 251) as u8 };
        }
        let mut encoded = [0u8; COBS_MAX];
        let encoded_len = cobs_encode(&input, &mut encoded).unwrap();
        assert_eq!(encoded[encoded_len - 1], 0);

        let mut decoded = [0u8; 700];
        let decoded_len = cobs_decode(&encoded[..encoded_len - 1], &mut decoded).unwrap();
        assert_eq!(&decoded[..decoded_len], &input);
    }

    #[test]
    fn frame_round_trip_and_crc_validation() {
        let message = [0x05, 0x80, 0x01, 0x00, 0x42, 0x00, 0xff];
        let mut encoded = [0u8; COBS_MAX];
        let encoded_len = encode_frame(&message, &mut encoded).unwrap();
        let mut frame = [0u8; COBS_MAX];
        frame[..encoded_len - 1].copy_from_slice(&encoded[..encoded_len - 1]);
        let payload_len = decode_frame_in_place(&mut frame[..encoded_len - 1]).unwrap();
        assert_eq!(&frame[..payload_len], &message);

        let mut corrupted = [0u8; COBS_MAX];
        corrupted[..encoded_len - 1].copy_from_slice(&encoded[..encoded_len - 1]);
        corrupted[1] ^= 0x01;
        assert_eq!(
            decode_frame_in_place(&mut corrupted[..encoded_len - 1]),
            Err(CodecError::InvalidCrc)
        );
    }

    #[test]
    fn incremental_decoder_skips_noise_and_empty_delimiters() {
        let message = [0x01, 0x99, 0x00, 0x17];
        let mut encoded = [0u8; COBS_MAX];
        let len = encode_frame(&message, &mut encoded).unwrap();
        let mut decoder = Decoder::<COBS_MAX>::new();
        for byte in [b'\n', b'o', b'w', b'l', 0, 0] {
            assert!(decoder.push(byte).is_none());
        }
        let mut found = None;
        for &byte in &encoded[..len] {
            if let Some(frame) = decoder.push(byte) {
                found = Some(frame.to_vec());
            }
        }
        assert_eq!(found.as_deref(), Some(&message[..]));
    }
}
