//! Stream adapters for the shared radio protocol.

use std::io::{self, Read, Write};

use omdrop_proto::{encode_frame, Decoder, COBS_MAX};

pub const MAX_ENCODED_FRAME: usize = COBS_MAX;

#[derive(Debug)]
pub enum TransportError {
    Io(io::Error),
    FrameTooLarge,
}

impl From<io::Error> for TransportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// A blocking reader that recovers from boot logs and malformed frames by waiting for the next
/// valid CRC-protected delimiter frame.
pub struct FramedReader<R> {
    inner: R,
    decoder: Decoder<COBS_MAX>,
}

impl<R: Read> FramedReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            decoder: Decoder::new(),
        }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Read the next valid type/body frame. The returned vector owns the bytes so the reader can
    /// continue immediately.
    pub fn read_frame(&mut self) -> Result<Vec<u8>, TransportError> {
        let mut byte = [0u8; 1];
        loop {
            self.inner.read_exact(&mut byte)?;
            if let Some(frame) = self.decoder.push(byte[0]) {
                return Ok(frame.to_vec());
            }
        }
    }
}

/// A blocking writer that adds the CRC, COBS encoding, and delimiter for every message.
pub struct FramedWriter<W> {
    inner: W,
    encoded: [u8; COBS_MAX],
}

impl<W: Write> FramedWriter<W> {
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            encoded: [0; COBS_MAX],
        }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }

    pub fn write_frame(&mut self, message: &[u8]) -> Result<(), TransportError> {
        let len =
            encode_frame(message, &mut self.encoded).map_err(|_| TransportError::FrameTooLarge)?;
        self.inner.write_all(&self.encoded[..len])?;
        self.inner.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn reader_ignores_boot_text_and_writer_round_trips() {
        let mut bytes = Vec::new();
        {
            let mut writer = FramedWriter::new(&mut bytes);
            writer.write_frame(&[0x01, 0x00, 0x02]).unwrap();
        }
        let mut stream = b"owl-radio up\n\0".to_vec();
        stream.extend_from_slice(&bytes);
        let mut reader = FramedReader::new(Cursor::new(stream));
        assert_eq!(reader.read_frame().unwrap(), vec![0x01, 0x00, 0x02]);
    }
}
