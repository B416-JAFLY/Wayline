//! Bounded streaming adapters for independent, length-prefixed zlib/stored blocks.
use std::io::{self, BufRead, BufReader, Read, Write};

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use crate::ArchiveError;

const STORED: u32 = 0x8000_0000;
const MAX_BLOCK: usize = 16 * 1024 * 1024;
const READ_BUFFER: usize = 16 * 1024;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DvZipStats {
    pub blocks: u32,
    pub stored_blocks: u32,
}

enum Block {
    Header,
    Stored,
    Compressed,
    Padding,
    End,
}

/// Decodes directly into the caller's buffer, with a fixed input buffer and a
/// reusable zlib window. Memory does not grow with block or archive size.
/// Read to EOF to validate the final checksum and all trailing block headers.
pub struct DvZipReader<R> {
    input: BufReader<R>,
    decoder: Decompress,
    block: Block,
    remaining: usize,
    stats: DvZipStats,
}

impl<R: Read> DvZipReader<R> {
    pub fn new(input: R) -> Self {
        Self {
            input: BufReader::with_capacity(READ_BUFFER, input),
            decoder: Decompress::new(true),
            block: Block::Header,
            remaining: 0,
            stats: DvZipStats::default(),
        }
    }

    pub fn stats(&self) -> DvZipStats {
        self.stats
    }

    /// Finish validating the blocks after the caller has parsed its archive trailer.
    /// The policy is consulted only between fully validated blocks with no buffered
    /// bytes left. `true` means that policy permitted ending without reading EOF;
    /// `false` means the underlying stream ended normally. A partially read block
    /// or header always remains an error, regardless of the policy.
    pub fn finish_at_archive_boundary(
        &mut self,
        mut can_finish: impl FnMut(&mut R) -> io::Result<bool>,
    ) -> io::Result<bool> {
        let mut buffer = [0; 8192];
        loop {
            while !matches!(self.block, Block::Header | Block::End) {
                self.read_mode(&mut buffer, true)?;
            }
            if matches!(self.block, Block::End) {
                return Ok(false);
            }
            if self.input.buffer().is_empty() && can_finish(self.input.get_mut())? {
                return Ok(true);
            }
            self.header()?;
        }
    }

    fn header(&mut self) -> io::Result<()> {
        let mut header = [0; 4];
        loop {
            match self.input.read(&mut header[..1]) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Ok(0) => {
                    self.block = Block::End;
                    return Ok(());
                }
                result => {
                    result?;
                    break;
                }
            }
        }
        self.input.read_exact(&mut header[1..])?;
        let word = u32::from_be_bytes(header);
        self.remaining = (word & !STORED) as usize;
        if self.remaining > MAX_BLOCK {
            return Err(invalid("dvzip: block is too large"));
        }
        self.stats.blocks += 1;
        self.block = if word & STORED != 0 {
            self.stats.stored_blocks += 1;
            Block::Stored
        } else {
            self.decoder.reset(true);
            Block::Compressed
        };
        Ok(())
    }
    fn read_mode(&mut self, output: &mut [u8], stop_at_header: bool) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            match self.block {
                Block::End => return Ok(0),
                Block::Header if stop_at_header => return Ok(0),
                Block::Header => self.header()?,
                Block::Stored => {
                    if self.remaining == 0 {
                        self.block = Block::Header;
                        continue;
                    }
                    let count = output.len().min(self.remaining);
                    let n = self.input.read(&mut output[..count])?;
                    if n == 0 {
                        return Err(truncated());
                    }
                    self.remaining -= n;
                    return Ok(n);
                }
                Block::Compressed => {
                    // Once the encoded block is exhausted, let zlib drain its
                    // window without reading (or blocking on) the next header.
                    let bytes = if self.remaining == 0 {
                        &[][..]
                    } else {
                        let bytes = self.input.fill_buf()?;
                        if bytes.is_empty() {
                            return Err(truncated());
                        }
                        &bytes[..bytes.len().min(self.remaining)]
                    };
                    let before_in = self.decoder.total_in();
                    let before_out = self.decoder.total_out();
                    let status = self
                        .decoder
                        .decompress(bytes, output, FlushDecompress::None)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    let consumed = (self.decoder.total_in() - before_in) as usize;
                    let produced = (self.decoder.total_out() - before_out) as usize;
                    self.input.consume(consumed);
                    self.remaining -= consumed;
                    if status == Status::StreamEnd {
                        // The old decoder ignored bytes after zlib's end within
                        // the declared block. Preserve that behavior, but check
                        // that those bytes actually exist before the next block.
                        self.block = Block::Padding;
                    } else if consumed == 0 && produced == 0 {
                        return Err(truncated());
                    }
                    if produced != 0 {
                        return Ok(produced);
                    }
                }
                Block::Padding => {
                    if self.remaining == 0 {
                        self.block = Block::Header;
                        continue;
                    }
                    let bytes = self.input.fill_buf()?;
                    if bytes.is_empty() {
                        return Err(truncated());
                    }
                    let count = bytes.len().min(self.remaining);
                    self.input.consume(count);
                    self.remaining -= count;
                }
            }
        }
    }
}

impl<R: Read> Read for DvZipReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.read_mode(output, false)
    }
}

/// Produces independent DVZip blocks from a push-based source such as CPIO.
/// Both buffers and the compressor are reused. Call `finish` to emit the tail;
/// dropping the writer does not perform I/O or silently ignore a final error.
pub struct DvZipWriter<W> {
    output: W,
    raw: Vec<u8>,
    compressed: Vec<u8>,
    encoder: Compress,
    block_size: usize,
    stats: DvZipStats,
    failed: bool,
}

impl<W: Write> DvZipWriter<W> {
    pub fn new(output: W, block_size: usize) -> Result<Self, ArchiveError> {
        if block_size == 0 || block_size > MAX_BLOCK {
            return Err(ArchiveError::Invalid("dvzip: invalid block size"));
        }
        Ok(Self {
            output,
            raw: Vec::with_capacity(block_size),
            compressed: vec![0; block_size],
            encoder: Compress::new(Compression::fast(), true),
            block_size,
            stats: DvZipStats::default(),
            failed: false,
        })
    }

    fn check(&self) -> io::Result<()> {
        if self.failed {
            Err(io::Error::other("dvzip: writer failed"))
        } else {
            Ok(())
        }
    }

    fn emit_block(&mut self) -> io::Result<()> {
        self.check()?;
        if self.raw.is_empty() {
            return Ok(());
        }
        // A partial output failure poisons the stream: retrying must not emit a
        // second header inside an already partially written block.
        self.failed = true;
        self.encoder.reset();
        let mut complete = false;
        loop {
            let used = self.encoder.total_in() as usize;
            let made = self.encoder.total_out() as usize;
            // Once compression fills the original size, stored is no worse.
            if made == self.raw.len() {
                break;
            }
            let status = self
                .encoder
                .compress(
                    &self.raw[used..],
                    &mut self.compressed[made..self.raw.len()],
                    FlushCompress::Finish,
                )
                .map_err(io::Error::other)?;
            if status == Status::StreamEnd {
                complete = true;
                break;
            }
            if self.encoder.total_in() as usize == used && self.encoder.total_out() as usize == made
            {
                return Err(io::Error::other("dvzip: compressor made no progress"));
            }
        }
        let compressed_len = self.encoder.total_out() as usize;
        let stored = !complete || compressed_len >= self.raw.len();
        let payload = if stored {
            &self.raw[..]
        } else {
            &self.compressed[..compressed_len]
        };
        let word = payload.len() as u32 | if stored { STORED } else { 0 };
        self.output.write_all(&word.to_be_bytes())?;
        self.output.write_all(payload)?;
        self.stats.blocks += 1;
        self.stats.stored_blocks += u32::from(stored);
        self.raw.clear();
        self.failed = false;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(W, DvZipStats), ArchiveError> {
        self.flush()?;
        Ok((self.output, self.stats))
    }
}

impl<W: Write> Write for DvZipWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.check()?;
        let count = bytes.len().min(self.block_size - self.raw.len());
        self.raw.extend_from_slice(&bytes[..count]);
        if self.raw.len() == self.block_size {
            self.emit_block()?;
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.emit_block()?;
        if let Err(error) = self.output.flush() {
            self.failed = true;
            return Err(error);
        }
        Ok(())
    }
}

pub fn decode_dvzip<R: Read, W: Write>(
    input: R,
    mut output: W,
) -> Result<DvZipStats, ArchiveError> {
    let mut decoder = DvZipReader::new(input);
    io::copy(&mut decoder, &mut output)?;
    Ok(decoder.stats())
}

pub fn encode_dvzip<R: Read, W: Write>(
    mut input: R,
    output: W,
    block_size: usize,
) -> Result<DvZipStats, ArchiveError> {
    let mut encoder = DvZipWriter::new(output, block_size)?;
    io::copy(&mut input, &mut encoder)?;
    Ok(encoder.finish()?.1)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn truncated() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "dvzip: truncated block")
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{read::ZlibDecoder, write::ZlibEncoder};
    use std::{cell::Cell, io::Cursor};

    fn compressed(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(bytes).unwrap();
        let bytes = encoder.finish().unwrap();
        [(bytes.len() as u32).to_be_bytes().as_slice(), &bytes].concat()
    }

    #[test]
    fn reader_streams_maximum_stored_block_without_reading_it_all() {
        struct Counted<'a, R>(R, &'a Cell<usize>);
        impl<R: Read> Read for Counted<'_, R> {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                let n = self.0.read(out)?;
                self.1.set(self.1.get() + n);
                Ok(n)
            }
        }
        let count = Cell::new(0);
        let source = Cursor::new((STORED | MAX_BLOCK as u32).to_be_bytes())
            .chain(io::repeat(42).take(MAX_BLOCK as u64));
        let mut reader = DvZipReader::new(Counted(source, &count));
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        assert_eq!(count.get(), 0);
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [42]);
        assert!(count.get() <= READ_BUFFER + 4);
        assert_eq!(
            io::copy(&mut reader, &mut io::sink()).unwrap(),
            MAX_BLOCK as u64 - 1
        );
        assert_eq!(
            reader.stats(),
            DvZipStats {
                blocks: 1,
                stored_blocks: 1
            }
        );
    }

    #[test]
    fn reader_handles_fragmented_zlib_stored_empty_and_padded_blocks() {
        let mut wire = STORED.to_be_bytes().to_vec(); // empty stored block
        wire.extend(compressed(b"compressed one"));
        let mut padded = compressed(b"two");
        let length = u32::from_be_bytes(padded[..4].try_into().unwrap());
        padded[..4].copy_from_slice(&(length + 2).to_be_bytes());
        padded.extend_from_slice(&[0, 0]);
        wire.extend(padded);
        wire.extend_from_slice(&(STORED | 3).to_be_bytes());
        wire.extend_from_slice(b"raw");
        struct Fragmented(Cursor<Vec<u8>>);
        impl Read for Fragmented {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                let len = out.len().min(1);
                self.0.read(&mut out[..len])
            }
        }
        let mut decoder = DvZipReader::new(Fragmented(Cursor::new(wire)));
        let mut actual = Vec::new();
        let mut byte = [0];
        while decoder.read(&mut byte).unwrap() != 0 {
            actual.push(byte[0]);
        }
        assert_eq!(actual, b"compressed onetworaw");
        assert_eq!(
            decoder.stats(),
            DvZipStats {
                blocks: 4,
                stored_blocks: 2
            }
        );
        assert_eq!(decoder.read(&mut byte).unwrap(), 0);
    }

    #[test]
    fn reader_rejects_truncated_blocks_checksums_and_oversized_headers() {
        let wire = compressed(&vec![b'x'; 128 * 1024]);
        // EOF is only valid between blocks, never in a header or zlib checksum.
        for cut in 1..wire.len() {
            assert!(decode_dvzip(&wire[..cut], io::sink()).is_err(), "cut={cut}");
        }
        let mut bad = wire.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert!(decode_dvzip(&bad[..], io::sink()).is_err());
        // Even an accurately advertised shorter block needs zlib StreamEnd.
        bad = wire[..wire.len() - 1].to_vec();
        let short = (bad.len() - 4) as u32;
        bad[..4].copy_from_slice(&short.to_be_bytes());
        assert!(decode_dvzip(&bad[..], io::sink()).is_err());
        for header in [
            MAX_BLOCK as u32 + 1,
            STORED | (MAX_BLOCK as u32 + 1),
            STORED | 4,
            0,
        ] {
            assert!(decode_dvzip(&header.to_be_bytes()[..], io::sink()).is_err());
        }
    }

    #[test]
    fn archive_boundary_finishes_the_current_block_without_waiting_for_eof() {
        struct OpenPeer(Cursor<Vec<u8>>);
        impl Read for OpenPeer {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                match self.0.read(out)? {
                    0 => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "peer awaits response",
                    )),
                    n => Ok(n),
                }
            }
        }
        let data = b"archive contents and trailer";
        for stored in [false, true] {
            let wire = if stored {
                [(STORED | data.len() as u32).to_be_bytes().as_slice(), data].concat()
            } else {
                compressed(data)
            };
            let mut decoder = DvZipReader::new(OpenPeer(Cursor::new(wire)));
            decoder.read_exact(&mut [0; 1]).unwrap();
            let mut calls = 0;
            assert!(decoder
                .finish_at_archive_boundary(|_| {
                    calls += 1;
                    Ok(true)
                })
                .unwrap());
            assert_eq!(calls, 1);
            assert!(matches!(decoder.block, Block::Header));
            assert_eq!(decoder.remaining, 0);
            assert!(decoder.input.buffer().is_empty());
        }
        let mut decoder = DvZipReader::new(Cursor::new(compressed(data)));
        decoder.read_exact(&mut [0; 1]).unwrap();
        assert!(!decoder.finish_at_archive_boundary(|_| Ok(false)).unwrap());
        assert!(matches!(decoder.block, Block::End));
    }

    #[test]
    fn archive_boundary_rejects_bad_checksums_short_blocks_and_buffered_trailing_headers() {
        let wire = compressed(b"enough output to stop before the final checksum");
        let mut bad_checksum = wire.clone();
        *bad_checksum.last_mut().unwrap() ^= 1;
        let mut short_block = wire.clone();
        short_block[..4].copy_from_slice(&((wire.len() - 5) as u32).to_be_bytes());
        let mut short_padding = wire.clone();
        short_padding[..4].copy_from_slice(&((wire.len() - 2) as u32).to_be_bytes());
        let mut bad_tail = wire.clone();
        bad_tail.push(0x80);
        let mut zero_then_bad_tail = wire.clone();
        zero_then_bad_tail.extend_from_slice(&STORED.to_be_bytes());
        zero_then_bad_tail.push(0x80);
        let mut bad_second_block = wire.clone();
        let mut second_block = compressed(b"buffered trailing block");
        *second_block.last_mut().unwrap() ^= 1;
        bad_second_block.extend(second_block);
        let short_stored = [(STORED | 4).to_be_bytes().as_slice(), b"abc"].concat();
        for bytes in [
            bad_checksum,
            short_block,
            short_padding,
            bad_tail,
            zero_then_bad_tail,
            bad_second_block,
            short_stored,
        ] {
            let mut decoder = DvZipReader::new(Cursor::new(bytes));
            decoder.header().unwrap();
            let mut calls = 0;
            assert!(decoder
                .finish_at_archive_boundary(|_| {
                    calls += 1;
                    Ok(true)
                })
                .is_err());
            assert_eq!(
                calls, 0,
                "invalid buffered data cannot reach the ending policy"
            );
        }
    }

    #[test]
    fn archive_boundary_validates_buffered_extra_blocks_including_empty_stored_blocks() {
        let mut wire = compressed(b"archive and trailer");
        wire.extend(compressed(b"padding after archive"));
        wire.extend_from_slice(&STORED.to_be_bytes());
        let mut decoder = DvZipReader::new(Cursor::new(wire));
        decoder.read_exact(&mut [0; 1]).unwrap();
        assert!(decoder.finish_at_archive_boundary(|_| Ok(true)).unwrap());
        assert_eq!(
            decoder.stats(),
            DvZipStats {
                blocks: 3,
                stored_blocks: 1
            }
        );
        assert!(matches!(decoder.block, Block::Header));
    }

    #[test]
    fn writer_interoperates_with_independent_zlib_decoder_and_short_writes() {
        struct Short(Vec<u8>);
        impl Write for Short {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let n = bytes.len().min(3);
                self.0.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        for size in [1, 31, 1024, 64 * 1024] {
            let mut source = vec![b'a'; size];
            let mut seed = 42u32;
            source.extend((0..size + 19).map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            }));
            let mut writer = DvZipWriter::new(Short(Vec::new()), size).unwrap();
            for fragment in source.chunks(17) {
                writer.write_all(fragment).unwrap();
            }
            let (Short(wire), stats) = writer.finish().unwrap();
            assert!(stats.stored_blocks > 0);
            if size >= 31 {
                assert!(stats.blocks > stats.stored_blocks);
            }
            let mut wire = &wire[..];
            let mut result = Vec::new();
            while !wire.is_empty() {
                let word = u32::from_be_bytes(wire[..4].try_into().unwrap());
                let len = (word & !STORED) as usize;
                let payload = &wire[4..4 + len];
                if word & STORED != 0 {
                    result.extend_from_slice(payload);
                } else {
                    ZlibDecoder::new(payload).read_to_end(&mut result).unwrap();
                }
                wire = &wire[4 + len..];
            }
            assert_eq!(result, source);
        }
    }

    #[test]
    fn writer_flush_and_errors_do_not_lose_or_duplicate_tail_blocks() {
        let mut writer = DvZipWriter::new(Vec::new(), 1024).unwrap();
        writer.flush().unwrap();
        writer.write_all(b"one").unwrap();
        writer.flush().unwrap();
        writer.flush().unwrap();
        writer.write_all(b"two").unwrap();
        let (wire, stats) = writer.finish().unwrap();
        let mut decoded = Vec::new();
        decode_dvzip(&wire[..], &mut decoded).unwrap();
        assert_eq!(decoded, b"onetwo");
        assert_eq!(stats.blocks, 2);
        assert!(DvZipWriter::new(io::sink(), 0).is_err());
        assert!(DvZipWriter::new(io::sink(), MAX_BLOCK + 1).is_err());
        struct Broken {
            writes: usize,
        }
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                self.writes += 1;
                if self.writes == 1 {
                    Ok(1)
                } else {
                    Err(io::ErrorKind::BrokenPipe.into())
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
        }
        let mut writer = DvZipWriter::new(Broken { writes: 0 }, 1).unwrap();
        assert!(writer.write_all(b"a").is_err());
        let writes = writer.output.writes;
        assert!(writer.write_all(b"b").is_err());
        assert!(writer.flush().is_err());
        assert_eq!(writer.output.writes, writes);
        assert!(writer.finish().is_err());
    }
}
