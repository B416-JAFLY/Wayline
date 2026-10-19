//! Lenient HTTP/1.1 framing used by AirDrop.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Invalid(&'static str),
    InvalidMessage(String),
    BodyTooLarge,
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Invalid(message) => f.write_str(message),
            Self::InvalidMessage(message) => f.write_str(message),
            Self::BodyTooLarge => f.write_str("HTTP body is too large"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHead {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: BodyKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    Empty,
    Length(u64),
    Chunked,
}

impl RequestHead {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn read<R: Read>(input: &mut Input<R>) -> Result<Option<Self>, Error> {
        let line = loop {
            match input.read_line()? {
                None => return Ok(None),
                Some(line) if !line.is_empty() => break line,
                Some(_) => {}
            }
        };
        let parts: Vec<_> = line.splitn(3, ' ').collect();
        if parts.len() < 3 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(Error::Invalid("missing HTTP version"));
        }
        let method = parts[0].to_owned();
        let path = parts[1].to_owned();
        let mut headers = BTreeMap::new();
        loop {
            let line = input
                .read_line()?
                .ok_or(Error::Invalid("truncated HTTP headers"))?;
            if line.is_empty() {
                break;
            }
            let colon = line
                .find(':')
                .ok_or(Error::Invalid("malformed HTTP header"))?;
            let key = line[..colon].trim().to_ascii_lowercase();
            let value = line[colon + 1..].trim().to_owned();
            headers.insert(key, value);
        }
        let body = if headers.get("transfer-encoding").is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
        }) {
            BodyKind::Chunked
        } else if let Some(length) = headers.get("content-length") {
            BodyKind::Length(
                length
                    .parse()
                    .map_err(|_| Error::Invalid("invalid Content-Length"))?,
            )
        } else {
            BodyKind::Empty
        };
        Ok(Some(Self {
            method,
            path,
            headers,
            body,
        }))
    }
}

pub struct Input<R> {
    inner: R,
    buffer: Vec<u8>,
    position: usize,
    filled: usize,
    read_probe: Option<fn(&mut R) -> io::Result<bool>>,
}

impl<R> Input<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: vec![0; 64 * 1024],
            position: 0,
            filled: 0,
            read_probe: None,
        }
    }

    /// Install a non-consuming transport probe. Without a probe, absence of
    /// buffered bytes never authorizes an application-specific early response.
    pub fn with_read_probe(mut self, probe: fn(&mut R) -> io::Result<bool>) -> Self {
        self.read_probe = Some(probe);
        self
    }

    pub fn has_pending_data(&mut self) -> io::Result<bool> {
        if self.position < self.filled {
            return Ok(true);
        }
        match self.read_probe {
            Some(probe) => probe(&mut self.inner),
            None => Ok(true),
        }
    }

    pub fn get_ref(&self) -> &R {
        &self.inner
    }

    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }
}

impl<R: Read> Input<R> {
    /// Buffer available plaintext without consuming any part of an HTTP line.
    pub fn ensure_data(&mut self) -> Result<bool, Error> {
        self.fill()
    }

    pub fn read_line(&mut self) -> Result<Option<String>, Error> {
        let mut line = Vec::new();
        loop {
            if !self.fill()? {
                return if line.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(String::from_utf8_lossy(&line).into_owned()))
                };
            }
            let byte = self.buffer[self.position];
            self.position += 1;
            if byte == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
            }
            if line.len() >= 16 * 1024 {
                return Err(Error::Invalid("HTTP header line is too long"));
            }
            line.push(byte);
        }
    }

    fn fill(&mut self) -> Result<bool, Error> {
        if self.position < self.filled {
            return Ok(true);
        }
        self.filled = loop {
            match self.inner.read(&mut self.buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        self.position = 0;
        Ok(self.filled > 0)
    }

    fn read_some(&mut self, output: &mut [u8]) -> Result<usize, Error> {
        if output.is_empty() {
            return Ok(0);
        }
        if !self.fill()? {
            return Ok(0);
        }
        let count = output.len().min(self.filled - self.position);
        output[..count].copy_from_slice(&self.buffer[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

impl<R: Read + Write> Write for Input<R> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyReadStage {
    Data,
    ChunkHeader,
    ChunkDelimiter,
    ChunkTrailers,
    Complete,
}

/// The next framing operation, retained on errors to diagnose stalled uploads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyReadState {
    pub kind: BodyKind,
    pub stage: BodyReadStage,
    pub remaining: u64,
    pub chunk_remaining: u64,
}

pub struct BodyReader<'a, R> {
    input: &'a mut Input<R>,
    kind: BodyKind,
    remaining: u64,
    chunk_remaining: u64,
    done: bool,
    stage: BodyReadStage,
    reading_chunk_header: bool,
}

impl<'a, R: Read> BodyReader<'a, R> {
    pub fn new(input: &'a mut Input<R>, kind: BodyKind) -> Self {
        let remaining = match kind {
            BodyKind::Length(length) => length,
            _ => 0,
        };
        Self {
            input,
            kind,
            remaining,
            chunk_remaining: 0,
            done: false,
            stage: match kind {
                BodyKind::Empty | BodyKind::Length(0) => BodyReadStage::Complete,
                BodyKind::Length(_) => BodyReadStage::Data,
                BodyKind::Chunked => BodyReadStage::ChunkHeader,
            },
            reading_chunk_header: false,
        }
    }

    /// Whether a separately validated application archive may finish at this
    /// chunk boundary. This does not mark the HTTP body complete or synthesize EOF.
    pub fn can_finish_archive_boundary(&mut self) -> io::Result<bool> {
        if self.kind != BodyKind::Chunked
            || self.stage != BodyReadStage::ChunkHeader
            || self.chunk_remaining != 0
            || self.done
            || self.reading_chunk_header
        {
            return Ok(false);
        }
        Ok(!self.input.has_pending_data()?)
    }

    pub fn state(&self) -> BodyReadState {
        BodyReadState {
            kind: self.kind,
            stage: self.stage,
            remaining: self.remaining,
            chunk_remaining: self.chunk_remaining,
        }
    }

    pub fn read_to_end_limited(mut self, max: usize) -> Result<Vec<u8>, Error> {
        let mut output = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let count = self.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            if output.len() + count > max {
                return Err(Error::BodyTooLarge);
            }
            output.extend_from_slice(&buffer[..count]);
        }
        Ok(output)
    }

    fn read_chunk_header(&mut self) -> Result<(), Error> {
        self.stage = BodyReadStage::ChunkHeader;
        self.reading_chunk_header = true;
        let line = self
            .input
            .read_line()?
            .ok_or(Error::Invalid("truncated chunk header"))?;
        let value = line.split(';').next().unwrap_or("").trim();
        self.chunk_remaining =
            u64::from_str_radix(value, 16).map_err(|_| Error::Invalid("invalid chunk size"))?;
        if self.chunk_remaining == 0 {
            self.stage = BodyReadStage::ChunkTrailers;
            while !self
                .input
                .read_line()?
                .ok_or(Error::Invalid("truncated chunk trailers"))?
                .is_empty()
            {}
            self.done = true;
            self.stage = BodyReadStage::Complete;
        } else {
            self.stage = BodyReadStage::Data;
        }
        self.reading_chunk_header = false;
        Ok(())
    }

    fn read_chunk_delimiter(&mut self) -> Result<(), Error> {
        self.stage = BodyReadStage::ChunkDelimiter;
        for expected in b"\r\n" {
            let mut byte = [0];
            if self.input.read_some(&mut byte)? == 0 {
                return Err(Error::Invalid("truncated HTTP chunk delimiter"));
            }
            if byte[0] != *expected {
                return Err(Error::Invalid(
                    "invalid HTTP chunk delimiter: expected CRLF",
                ));
            }
        }
        self.stage = BodyReadStage::ChunkHeader;
        Ok(())
    }
}

impl<R: Read> Read for BodyReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.done || output.is_empty() {
            return Ok(0);
        }
        let result: io::Result<usize> = match self.kind {
            BodyKind::Empty => {
                self.done = true;
                self.stage = BodyReadStage::Complete;
                Ok(0)
            }
            BodyKind::Length(_) => {
                if self.remaining == 0 {
                    self.done = true;
                    self.stage = BodyReadStage::Complete;
                    return Ok(0);
                }
                let count = output
                    .len()
                    .min(self.remaining.min(usize::MAX as u64) as usize);
                let n = self.input.read_some(&mut output[..count]).map_err(to_io)?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated HTTP body",
                    ));
                }
                self.remaining -= n as u64;
                if self.remaining == 0 {
                    self.stage = BodyReadStage::Complete;
                }
                Ok(n)
            }
            BodyKind::Chunked => {
                if self.chunk_remaining == 0 {
                    self.read_chunk_header().map_err(to_io)?;
                    if self.done {
                        return Ok(0);
                    }
                }
                let count = output
                    .len()
                    .min(self.chunk_remaining.min(usize::MAX as u64) as usize);
                let n = self.input.read_some(&mut output[..count]).map_err(to_io)?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated HTTP chunk",
                    ));
                }
                self.chunk_remaining -= n as u64;
                if self.chunk_remaining == 0 {
                    self.read_chunk_delimiter().map_err(to_io)?;
                }
                Ok(n)
            }
        };
        result
    }
}

pub fn write_response<W: Write>(
    output: &mut W,
    status: u16,
    body: &[u8],
    content_type: Option<&str>,
    close: bool,
) -> Result<(), Error> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        406 => "Not Acceptable",
        _ => "Status",
    };
    write!(
        output,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n",
        body.len()
    )?;
    if let Some(content_type) = content_type {
        write!(output, "Content-Type: {content_type}\r\n")?;
    }
    if close {
        output.write_all(b"Connection: close\r\n")?;
    }
    output.write_all(b"\r\n")?;
    output.write_all(body)?;
    output.flush()?;
    Ok(())
}

pub fn write_continue<W: Write>(output: &mut W) -> Result<(), Error> {
    output.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    output.flush()?;
    Ok(())
}

fn to_io(error: Error) -> io::Error {
    match error {
        Error::Io(error) => error,
        error => io::Error::new(io::ErrorKind::InvalidData, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn readiness_preserves_partial_http_line_and_recovers_from_timeout() {
        struct Fragmented {
            stage: usize,
        }
        impl Read for Fragmented {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                let stage = self.stage;
                self.stage += 1;
                let bytes: &[u8] = match stage {
                    0 => return Err(io::ErrorKind::TimedOut.into()),
                    1 => b"HT",
                    2 => b"TP/1.1 100 Continue\r\n\r\n",
                    _ => b"",
                };
                out[..bytes.len()].copy_from_slice(bytes);
                Ok(bytes.len())
            }
        }
        let mut input = Input::new(Fragmented { stage: 0 });
        assert!(
            matches!(input.ensure_data(), Err(Error::Io(error)) if error.kind() == io::ErrorKind::TimedOut)
        );
        assert!(input.ensure_data().unwrap());
        assert!(input.ensure_data().unwrap());
        assert_eq!(
            input.read_line().unwrap().as_deref(),
            Some("HTTP/1.1 100 Continue")
        );
        assert_eq!(input.read_line().unwrap().as_deref(), Some(""));
        assert!(!input.ensure_data().unwrap());
    }

    #[test]
    fn parses_hostless_content_length_request() {
        let request = b"POST /Discover HTTP/1.1\r\nContent-Length: 4\r\n\r\nbody";
        let mut input = Input::new(Cursor::new(request));
        let head = RequestHead::read(&mut input).unwrap().unwrap();
        assert_eq!(head.path, "/Discover");
        assert_eq!(head.header("host"), None);
        assert_eq!(
            BodyReader::new(&mut input, head.body)
                .read_to_end_limited(16)
                .unwrap(),
            b"body"
        );
    }

    #[test]
    fn parses_chunked_request_and_writes_response() {
        let request = b"POST /Upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nRust\r\n3;foo=bar\r\nNet\r\n0\r\nX-Test: yes\r\n\r\n";
        let mut input = Input::new(Cursor::new(request));
        let head = RequestHead::read(&mut input).unwrap().unwrap();
        assert_eq!(
            BodyReader::new(&mut input, head.body)
                .read_to_end_limited(16)
                .unwrap(),
            b"RustNet"
        );
        let mut response = Vec::new();
        write_response(&mut response, 200, b"ok", Some("text/plain"), true).unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with(b"\r\nok"));
    }

    #[test]
    fn archive_boundary_probe_requires_an_empty_complete_chunk_boundary() {
        fn pending(input: &mut Cursor<Vec<u8>>) -> io::Result<bool> {
            Ok(input.position() < input.get_ref().len() as u64)
        }
        let mut input = Input::new(Cursor::new(b"3\r\nabc\r\n".to_vec()));
        let mut body = BodyReader::new(&mut input, BodyKind::Chunked);
        body.read_exact(&mut [0; 3]).unwrap();
        assert!(
            !body.can_finish_archive_boundary().unwrap(),
            "no probe stays strict"
        );

        let mut input = Input::new(Cursor::new(b"3\r\nabc\r\n".to_vec())).with_read_probe(pending);
        let mut body = BodyReader::new(&mut input, BodyKind::Chunked);
        body.read_exact(&mut [0; 2]).unwrap();
        assert!(
            !body.can_finish_archive_boundary().unwrap(),
            "payload remains"
        );
        body.read_exact(&mut [0; 1]).unwrap();
        assert!(body.can_finish_archive_boundary().unwrap());
        assert_eq!(body.state().stage, BodyReadStage::ChunkHeader);
        assert!(!body.done, "an archive boundary is not HTTP EOF");

        for tail in [b"0\r\n\r\n".as_slice(), b"1", b"1\r\nx\r\n"] {
            let mut input = Input::new(Cursor::new([b"3\r\nabc\r\n".as_slice(), tail].concat()))
                .with_read_probe(pending);
            let mut body = BodyReader::new(&mut input, BodyKind::Chunked);
            body.read_exact(&mut [0; 3]).unwrap();
            assert!(
                !body.can_finish_archive_boundary().unwrap(),
                "buffered tail {tail:?}"
            );
        }
        let mut input = Input::new(Cursor::new(b"3\r\nabc\r\n1".to_vec())).with_read_probe(pending);
        let mut body = BodyReader::new(&mut input, BodyKind::Chunked);
        body.read_exact(&mut [0; 3]).unwrap();
        assert!(body.read(&mut [0]).is_err());
        assert!(
            !body.can_finish_archive_boundary().unwrap(),
            "failed partial header stays strict"
        );

        let mut input = Input::new(Cursor::new(b"abc".to_vec())).with_read_probe(pending);
        let mut body = BodyReader::new(&mut input, BodyKind::Length(3));
        body.read_exact(&mut [0; 3]).unwrap();
        assert!(
            !body.can_finish_archive_boundary().unwrap(),
            "Content-Length stays strict"
        );
    }

    #[test]
    fn body_timeout_preserves_io_kind_and_records_the_pending_framing_operation() {
        struct Stalled(Cursor<Vec<u8>>);
        impl Read for Stalled {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                match self.0.read(out)? {
                    0 => Err(io::ErrorKind::TimedOut.into()),
                    count => Ok(count),
                }
            }
        }
        for (kind, bytes, stage, remaining, chunk_remaining) in [
            (BodyKind::Length(3), &b"ab"[..], BodyReadStage::Data, 1, 0),
            (
                BodyKind::Chunked,
                &b"3\r\nab"[..],
                BodyReadStage::Data,
                0,
                1,
            ),
            (
                BodyKind::Chunked,
                &b"3\r\nabc"[..],
                BodyReadStage::ChunkDelimiter,
                0,
                0,
            ),
            (
                BodyKind::Chunked,
                &b"3\r\nabc\r\n"[..],
                BodyReadStage::ChunkHeader,
                0,
                0,
            ),
            (
                BodyKind::Chunked,
                &b"3\r\nabc\r\n0\r\n"[..],
                BodyReadStage::ChunkTrailers,
                0,
                0,
            ),
        ] {
            let mut input = Input::new(Stalled(Cursor::new(bytes.to_vec())));
            let mut body = BodyReader::new(&mut input, kind);
            let error = body.read_to_end(&mut Vec::new()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert_eq!(
                body.state(),
                BodyReadState {
                    kind,
                    stage,
                    remaining,
                    chunk_remaining
                }
            );
        }
        for kind in [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::UnexpectedEof,
        ] {
            assert_eq!(to_io(Error::Io(kind.into())).kind(), kind);
        }
    }

    #[test]
    fn chunk_payload_requires_an_exact_crlf_delimiter() {
        for tail in [
            &b""[..],
            &b"\n0\r\n\r\n"[..],
            &b"\rX0\r\n\r\n"[..],
            &b"ignored\r\n0\r\n\r\n"[..],
        ] {
            let mut input = Input::new(Cursor::new([&b"3\r\nabc"[..], tail].concat()));
            let mut body = BodyReader::new(&mut input, BodyKind::Chunked);
            let error = body.read_to_end(&mut Vec::new()).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(body.state().stage, BodyReadStage::ChunkDelimiter);
        }
        struct Fragmented {
            input: Cursor<Vec<u8>>,
            interrupt: bool,
        }
        impl Read for Fragmented {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                self.interrupt = !self.interrupt;
                if self.interrupt {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let count = out.len().min(1);
                self.input.read(&mut out[..count])
            }
        }
        let mut input = Input::new(Fragmented {
            input: Cursor::new(b"3\r\nabc\r\n0\r\n\r\n".to_vec()),
            interrupt: false,
        });
        let mut body = BodyReader::new(&mut input, BodyKind::Chunked);
        let mut bytes = Vec::new();
        body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"abc");
        assert_eq!(body.state().stage, BodyReadStage::Complete);
    }
}
