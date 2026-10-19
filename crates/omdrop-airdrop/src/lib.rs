//! Allocation-conscious AirDrop archive codecs.
//!
//! Apple sends uploads as an odc/newc cpio stream, optionally gzip-wrapped, or as DVZip blocks
//! containing the same cpio bytes. This crate is independent of the Windows service and can be
//! reused by the eventual Rust receiver and tray application.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

mod dvzip;
pub use dvzip::{decode_dvzip, encode_dvzip, DvZipReader, DvZipStats, DvZipWriter};

pub mod plist;

#[derive(Debug)]
pub enum ArchiveError {
    Io(io::Error),
    Invalid(&'static str),
    InvalidMessage(String),
}

impl From<io::Error> for ArchiveError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Invalid(message) => f.write_str(message),
            Self::InvalidMessage(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ArchiveError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpioFormat {
    Odc,
    Newc,
}

const TRAILER: &str = "TRAILER!!!";
const REGULAR: u32 = 0o100000;
const DIRECTORY: u32 = 0o040000;

/// Extract cpio from a stream. Gzip is detected from the first two bytes. Unsafe paths and
/// unsupported file types are consumed but skipped.
pub fn extract_cpio<R: Read>(
    input: R,
    destination: impl AsRef<Path>,
) -> Result<Vec<PathBuf>, ArchiveError> {
    let mut input = BufReaderWithPrefix::new(input);
    let mut header = [0u8; 2];
    input.read_exact(&mut header)?;
    let mut stream: Box<dyn Read> = if header == [0x1f, 0x8b] {
        Box::new(GzDecoder::new(input.with_prefix(header)))
    } else {
        Box::new(input.with_prefix(header))
    };

    let root = fs::canonicalize(destination.as_ref()).or_else(|_| {
        fs::create_dir_all(destination.as_ref())?;
        fs::canonicalize(destination.as_ref())
    })?;
    let mut written = Vec::new();

    loop {
        let entry = read_entry(&mut stream)?;
        if entry.name == TRAILER {
            break;
        }

        let safe = safe_path(&root, &entry.name);
        let mut data_consumed = false;
        match (safe, entry.mode & 0o170000) {
            (Some(path), DIRECTORY) => {
                ensure_confined(&root, &path)?;
                fs::create_dir_all(path)?;
            }
            (Some(path), REGULAR) => {
                if let Some(parent) = path.parent() {
                    ensure_confined(&root, parent)?;
                    fs::create_dir_all(parent)?;
                }
                let (path, mut file) = create_unused_file(path)?;
                copy_exact(&mut stream, &mut file, entry.size)?;
                data_consumed = true;
                written.push(path);
            }
            _ => {}
        }
        if !data_consumed {
            skip_exact(&mut stream, entry.size)?;
        }

        if entry.format == CpioFormat::Newc {
            skip_exact(&mut stream, entry.data_pad)?;
        }
    }
    Ok(written)
}

/// Write an odc cpio archive containing regular files as `./basename` entries.
pub fn write_cpio<W: Write>(
    mut output: W,
    files: impl IntoIterator<Item = impl AsRef<Path>>,
) -> Result<(), ArchiveError> {
    for (inode, file) in (1u64..).zip(files) {
        let path = file.as_ref();
        let metadata = fs::metadata(path)?;
        let name = format!(
            "./{}",
            path.file_name()
                .and_then(|n| n.to_str())
                .ok_or(ArchiveError::Invalid("invalid filename"))?
        );
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        write_odc_header(
            &mut output,
            &name,
            0o100644,
            metadata.len(),
            inode,
            modified,
        )?;
        let mut input = File::open(path)?;
        io::copy(&mut input, &mut output)?;
    }
    write_odc_header(&mut output, TRAILER, 0, 0, 0, 0)
}

#[derive(Debug)]
struct ParsedEntry {
    name: String,
    mode: u32,
    size: u64,
    format: CpioFormat,
    data_pad: u64,
}

fn read_entry(input: &mut dyn Read) -> Result<ParsedEntry, ArchiveError> {
    let mut magic = [0u8; 6];
    input.read_exact(&mut magic)?;
    match &magic {
        b"070707" => {
            let mut fields = [0u8; 70];
            input.read_exact(&mut fields)?;
            let mode = parse_octal(&fields[12..18])? as u32;
            let name_size = parse_octal(&fields[53..59])? as usize;
            let size = parse_octal(&fields[59..70])?;
            let name = read_name(input, name_size)?;
            Ok(ParsedEntry {
                name,
                mode,
                size,
                format: CpioFormat::Odc,
                data_pad: 0,
            })
        }
        b"070701" | b"070702" => {
            let mut fields = [0u8; 104];
            input.read_exact(&mut fields)?;
            let mode = parse_hex(&fields[8..16])? as u32;
            let size = parse_hex(&fields[48..56])?;
            let name_size = parse_hex(&fields[88..96])? as usize;
            let name = read_name(input, name_size)?;
            let name_pad = (4 - (110 + name_size) % 4) % 4;
            skip_exact(input, name_pad as u64)?;
            Ok(ParsedEntry {
                name,
                mode,
                size,
                format: CpioFormat::Newc,
                data_pad: (4 - size % 4) % 4,
            })
        }
        _ => Err(ArchiveError::Invalid("cpio: unknown format")),
    }
}

fn read_name(input: &mut dyn Read, size: usize) -> Result<String, ArchiveError> {
    if size == 0 || size > 4096 {
        return Err(ArchiveError::Invalid("cpio: invalid name size"));
    }
    let mut name = vec![0u8; size];
    input.read_exact(&mut name)?;
    if name.last() == Some(&0) {
        name.pop();
    }
    String::from_utf8(name).map_err(|_| ArchiveError::Invalid("cpio: invalid UTF-8 name"))
}

fn parse_octal(bytes: &[u8]) -> Result<u64, ArchiveError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| ArchiveError::Invalid("cpio: invalid octal"))?;
    u64::from_str_radix(text.trim(), 8).map_err(|_| ArchiveError::Invalid("cpio: invalid octal"))
}

fn parse_hex(bytes: &[u8]) -> Result<u64, ArchiveError> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| ArchiveError::Invalid("cpio: invalid hex"))?;
    u64::from_str_radix(text, 16).map_err(|_| ArchiveError::Invalid("cpio: invalid hex"))
}

fn write_odc_header<W: Write>(
    output: &mut W,
    name: &str,
    mode: u32,
    size: u64,
    inode: u64,
    mtime: u64,
) -> Result<(), ArchiveError> {
    let name = format!("{name}\0");
    let fields = [
        0u64,
        inode,
        u64::from(mode),
        0,
        0,
        1,
        0,
        mtime,
        name.len() as u64,
        size,
    ];
    output.write_all(b"070707")?;
    for (index, width) in [6usize, 6, 6, 6, 6, 6, 6, 11, 6, 11]
        .into_iter()
        .enumerate()
    {
        let field = format!("{:0width$o}", fields[index], width = width);
        if field.len() != width {
            return Err(ArchiveError::Invalid("cpio: field value is too large"));
        }
        output.write_all(field.as_bytes())?;
    }
    output.write_all(name.as_bytes())?;
    Ok(())
}

fn safe_path(root: &Path, name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    if name.starts_with('/') || name.contains(':') {
        return None;
    }
    let mut path = root.to_path_buf();
    let mut had_component = false;
    for component in name.split('/') {
        match component {
            "" | "." => {}
            ".." => return None,
            component => {
                let mut safe: String = component
                    .chars()
                    .map(|ch| {
                        if ch.is_control() || "<>\"/\\|?*".contains(ch) {
                            '_'
                        } else {
                            ch
                        }
                    })
                    .collect();
                while safe.ends_with(['.', ' ']) {
                    safe.pop();
                }
                if safe.is_empty() {
                    safe.push('_');
                }
                let device_name = safe.split('.').next().unwrap_or("").to_ascii_uppercase();
                if matches!(
                    device_name.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$"
                ) || (device_name.len() == 4
                    && (device_name.starts_with("COM") || device_name.starts_with("LPT"))
                    && device_name.as_bytes()[3].is_ascii_digit()
                    && device_name.as_bytes()[3] != b'0')
                {
                    safe.insert(0, '_');
                }
                path.push(safe);
                had_component = true;
            }
        }
    }
    if !had_component || !path.starts_with(root) {
        return None;
    }
    Some(path)
}

/// Reject a pre-existing symlink/junction anywhere between the archive destination and the
/// target. This is a best-effort check; the extraction directory should still be private to the
/// receiver process while a transfer is being written.
fn ensure_confined(root: &Path, target: &Path) -> Result<(), ArchiveError> {
    if !target.starts_with(root) {
        return Err(ArchiveError::Invalid("cpio: path escapes destination"));
    }
    let mut ancestor = target;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or(ArchiveError::Invalid("cpio: path escapes destination"))?;
    }
    let resolved = fs::canonicalize(ancestor)?;
    if !resolved.starts_with(root) {
        return Err(ArchiveError::Invalid("cpio: path escapes destination"));
    }
    Ok(())
}

fn create_unused_file(path: PathBuf) -> Result<(PathBuf, File), ArchiveError> {
    let mut candidate = path.clone();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let mut index = 0u64;
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                index += 1;
                let name = if extension.is_empty() {
                    format!("{stem} ({index})")
                } else {
                    format!("{stem} ({index}).{extension}")
                };
                candidate = parent.join(name);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn copy_exact(
    input: &mut dyn Read,
    output: &mut dyn Write,
    mut amount: u64,
) -> Result<(), ArchiveError> {
    let mut buffer = [0u8; 8192];
    while amount > 0 {
        let want = amount.min(buffer.len() as u64) as usize;
        let read = input.read(&mut buffer[..want])?;
        if read == 0 {
            return Err(ArchiveError::Invalid("archive: truncated payload"));
        }
        output.write_all(&buffer[..read])?;
        amount -= read as u64;
    }
    Ok(())
}

fn skip_exact(input: &mut dyn Read, mut amount: u64) -> Result<(), ArchiveError> {
    let mut buffer = [0u8; 8192];
    while amount > 0 {
        let want = amount.min(buffer.len() as u64) as usize;
        let read = input.read(&mut buffer[..want])?;
        if read == 0 {
            return Err(ArchiveError::Invalid("archive: truncated payload"));
        }
        amount -= read as u64;
    }
    Ok(())
}

struct BufReaderWithPrefix<R> {
    inner: R,
    prefix: Vec<u8>,
    position: usize,
}

impl<R: Read> BufReaderWithPrefix<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            prefix: Vec::new(),
            position: 0,
        }
    }

    fn with_prefix(mut self, prefix: [u8; 2]) -> Self {
        self.prefix = prefix.to_vec();
        self.position = 0;
        self
    }
}

impl<R: Read> Read for BufReaderWithPrefix<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.position < self.prefix.len() {
            let amount = (self.prefix.len() - self.position).min(output.len());
            output[..amount].copy_from_slice(&self.prefix[self.position..self.position + amount]);
            self.position += amount;
            return Ok(amount);
        }
        self.inner.read(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Cursor;

    #[test]
    fn cpio_write_and_extract_rejects_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let mut archive = Vec::new();
        write_odc_header(&mut archive, "./hello.txt", 0o100644, 5, 1, 0).unwrap();
        archive.extend_from_slice(b"hello");
        write_odc_header(&mut archive, "../escaped.txt", 0o100644, 7, 2, 0).unwrap();
        archive.extend_from_slice(b"escaped");
        write_odc_header(&mut archive, TRAILER, 0, 0, 0, 0).unwrap();
        let output = tempfile::tempdir().unwrap();
        let files = extract_cpio(Cursor::new(archive), output.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(fs::read(&files[0]).unwrap(), b"hello");
        assert!(!temp.path().join("escaped.txt").exists());
    }

    #[test]
    fn cpio_gzip_upload_and_duplicate_names_are_supported() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("one.txt");
        fs::write(&source, b"first").unwrap();
        let mut archive = Vec::new();
        write_cpio(&mut archive, [&source, &source]).unwrap();
        let mut gzip = GzEncoder::new(Vec::new(), Compression::fast());
        gzip.write_all(&archive).unwrap();
        let compressed = gzip.finish().unwrap();
        let output = tempfile::tempdir().unwrap();
        let files = extract_cpio(Cursor::new(compressed), output.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files
            .iter()
            .any(|path| path.file_name().unwrap() == "one.txt"));
        assert!(files
            .iter()
            .any(|path| path.file_name().unwrap() == "one (1).txt"));
    }

    #[test]
    fn unsafe_windows_names_are_confined_and_sanitized() {
        let root = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(root.path()).unwrap();
        let safe = safe_path(&root, r"outside\CON.txt").unwrap();
        assert!(safe.starts_with(&root));
        assert_eq!(safe.file_name().unwrap(), "_CON.txt");
        assert!(safe_path(&root, r"..\outside.txt").is_none());
        assert!(safe_path(&root, r"/absolute.txt").is_none());
    }

    #[test]
    fn dvzip_rejects_partial_trailing_header() {
        let mut archive = Vec::from(0x8000_0001u32.to_be_bytes());
        archive.push(b'x');
        archive.push(0x80);
        assert!(decode_dvzip(Cursor::new(archive), Vec::new()).is_err());
    }

    #[test]
    fn dvzip_round_trip_uses_stored_and_compressed_blocks() {
        let mut source = Vec::new();
        source.extend(std::iter::repeat_n(b'a', 1000));
        source.extend((0..1000).map(|n| (n % 251) as u8));
        let mut encoded = Vec::new();
        let stats = encode_dvzip(Cursor::new(&source), &mut encoded, 1000).unwrap();
        assert!(stats.blocks >= 2);
        let mut decoded = Vec::new();
        decode_dvzip(Cursor::new(encoded), &mut decoded).unwrap();
        assert_eq!(decoded, source);
    }
}
