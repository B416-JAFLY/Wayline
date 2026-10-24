//! Allocation-free USB batching. Each message keeps its own CRC/COBS delimiter.
//! A batch contains only already queued messages; idle traffic is never delayed.

use wayline_proto::{encode_frame, CodecError, RAW_MAX};

pub const BATCH_BYTES: usize = 4096;
pub const BATCH_FRAMES: usize = 8;

pub struct Batch {
    bytes: [u8; BATCH_BYTES],
    len: usize,
    frames: usize,
}

impl Default for Batch {
    fn default() -> Self {
        Self::new()
    }
}

impl Batch {
    pub const fn new() -> Self {
        Self {
            bytes: [0; BATCH_BYTES],
            len: 0,
            frames: 0,
        }
    }

    /// False means flush the existing batch and retry the same message.
    /// No input pointer is retained, so the caller may release its RX ring item.
    pub fn push(&mut self, message: &[u8]) -> Result<bool, CodecError> {
        if message.is_empty() {
            return Err(CodecError::EmptyFrame);
        }
        if message.len() > RAW_MAX - 2 {
            return Err(CodecError::FrameTooLarge);
        }
        let raw = message.len() + 2;
        let bound = raw + raw / 254 + 2;
        if self.frames == BATCH_FRAMES || bound > BATCH_BYTES - self.len {
            return Ok(false);
        }
        self.len += encode_frame(message, &mut self.bytes[self.len..])?;
        self.frames += 1;
        Ok(true)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
    pub fn frames(&self) -> usize {
        self.frames
    }
    pub fn clear(&mut self) {
        self.len = 0;
        self.frames = 0;
    }
}

#[derive(Clone, Copy, Default)]
pub struct Stats {
    pub frames: u32,
    pub batches: u32,
    pub bytes: u32,
    pub encode_us: u32,
    pub write_us: u32,
    pub max_write_us: u32,
    pub lock_wait_us: u32,
    pub errors: u32,
    pub max_batch_frames: u32,
    pub max_batch_bytes: u32,
}

impl Stats {
    pub const fn new() -> Self {
        Self {
            frames: 0,
            batches: 0,
            bytes: 0,
            encode_us: 0,
            write_us: 0,
            max_write_us: 0,
            lock_wait_us: 0,
            errors: 0,
            max_batch_frames: 0,
            max_batch_bytes: 0,
        }
    }

    pub fn record(
        &mut self,
        batch: &Batch,
        written: usize,
        encode_us: u32,
        wait_us: u32,
        write_us: u32,
    ) {
        self.frames = self.frames.wrapping_add(batch.frames() as u32);
        self.batches = self.batches.wrapping_add(1);
        self.bytes = self.bytes.wrapping_add(written as u32);
        self.encode_us = self.encode_us.wrapping_add(encode_us);
        self.write_us = self.write_us.wrapping_add(write_us);
        self.max_write_us = self.max_write_us.max(write_us);
        self.lock_wait_us = self.lock_wait_us.wrapping_add(wait_us);
        self.errors = self
            .errors
            .wrapping_add(u32::from(written != batch.bytes().len()));
        self.max_batch_frames = self.max_batch_frames.max(batch.frames() as u32);
        self.max_batch_bytes = self.max_batch_bytes.max(batch.bytes().len() as u32);
    }

    pub fn wire_bytes(&self, heap_free: u32, heap_min: u32, link_core: u32) -> [u8; 56] {
        let values = [
            1,
            self.frames,
            self.batches,
            self.bytes,
            self.encode_us,
            self.write_us,
            self.max_write_us,
            self.lock_wait_us,
            self.errors,
            self.max_batch_frames,
            self.max_batch_bytes,
            heap_free,
            heap_min,
            link_core,
        ];
        let mut bytes = [0; 56];
        for (slot, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(values) {
            slot.copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::FrameDecoder;
    use wayline_proto::{encode_frame, COBS_MAX};

    #[test]
    fn mixed_frames_remain_wire_identical_and_decode_across_usb_chunks() {
        let mut rx = vec![0x90; 1611];
        for i in (1..rx.len()).step_by(13) {
            rx[i] = 0;
        }
        let messages = [rx, vec![0x86, 1, 2, 3, 0, 5, 6, 7], vec![0x90; 1500]];
        let mut batch = Batch::new();
        let mut expected = Vec::new();
        for message in &messages {
            let mut encoded = [0; COBS_MAX];
            let len = encode_frame(message, &mut encoded).unwrap();
            expected.extend_from_slice(&encoded[..len]);
            assert!(batch.push(message).unwrap());
        }
        assert_eq!(batch.bytes(), expected);
        // Simulate arbitrary short USB/COM reads, including inside COBS runs.
        for chunk_size in [1, 7, 63, 64, 65, 511] {
            let mut decoder = FrameDecoder::new();
            let mut decoded = Vec::new();
            for chunk in batch.bytes().chunks(chunk_size) {
                for &byte in chunk {
                    if let Some(frame) = decoder.push(byte) {
                        decoded.push(frame.unwrap().to_vec());
                    }
                }
            }
            assert_eq!(decoded, messages);
        }
    }

    #[test]
    fn full_byte_batch_preserves_pending_frame_for_next_write() {
        let message = [0xff; RAW_MAX - 2];
        let mut batch = Batch::new();
        assert!(batch.push(&message).unwrap());
        assert!(batch.push(&message).unwrap());
        let snapshot = batch.bytes().to_vec();
        assert!(!batch.push(&message).unwrap());
        assert_eq!(batch.bytes(), snapshot);
        assert_eq!(batch.frames(), 2);
        batch.clear();
        assert!(batch.bytes().is_empty());
        assert!(batch.push(&message).unwrap());
        assert_eq!(batch.frames(), 1);
    }

    #[test]
    fn frame_limit_bounds_control_bursts_and_rejects_invalid_input_atomically() {
        let mut batch = Batch::new();
        for _ in 0..BATCH_FRAMES {
            assert!(batch.push(&[0x86, 0]).unwrap());
        }
        let before = batch.bytes().to_vec();
        assert!(!batch.push(&[0x86, 1]).unwrap());
        assert_eq!(batch.push(&[]), Err(CodecError::EmptyFrame));
        assert_eq!(batch.push(&[1; RAW_MAX]), Err(CodecError::FrameTooLarge));
        assert_eq!(batch.bytes(), before);
    }

    #[test]
    fn single_message_bounds_include_crc_cobs_and_zero_delimiter() {
        for len in 1..=RAW_MAX - 2 {
            for fill in [0, 0xff] {
                let mut batch = Batch::new();
                let message = vec![fill; len];
                assert!(batch.push(&message).unwrap());
                assert!(batch.bytes().len() <= BATCH_BYTES);
                assert_eq!(batch.bytes().last(), Some(&0));
                let mut decoder = FrameDecoder::new();
                for &byte in batch.bytes() {
                    if let Some(result) = decoder.push(byte) {
                        assert_eq!(result.unwrap(), message);
                    }
                }
            }
        }
    }

    #[test]
    fn counters_record_short_writes_and_timing_without_reporting_full_delivery() {
        let mut batch = Batch::new();
        batch.push(&[0x86, 1, 2]).unwrap();
        let mut stats = Stats::new();
        stats.record(&batch, 2, 3, 4, 5);
        stats.record(&batch, batch.bytes().len(), 6, 7, 8);
        assert_eq!((stats.frames, stats.batches, stats.errors), (2, 2, 1));
        assert_eq!(
            (
                stats.encode_us,
                stats.lock_wait_us,
                stats.write_us,
                stats.max_write_us
            ),
            (9, 11, 13, 8)
        );
        let wire = stats.wire_bytes(100, 80, 1);
        assert_eq!(wire.len(), 56);
        assert_eq!(&wire[44..48], &100u32.to_le_bytes());
        assert_eq!(&wire[52..56], &1u32.to_le_bytes());
    }
}
