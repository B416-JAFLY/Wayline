//! AirDrop message and upload orchestration above the archive and plist codecs.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;
use omdrop_airdrop::plist::{self, Value};
use omdrop_airdrop::{extract_cpio, write_cpio, DvZipReader, DvZipWriter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingFile {
    pub name: String,
    pub kind: String,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingTransfer {
    pub id: String,
    pub sender_name: String,
    pub sender_model: Option<String>,
    pub files: Vec<IncomingFile>,
    pub urls: Vec<String>,
    pub icon: Option<Vec<u8>>,
    pub sender_record_data: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadFormat {
    Cpio,
    GzipCpio,
    DvZip,
}

#[derive(Debug)]
pub enum Error {
    Plist(plist::PlistError),
    Archive(omdrop_airdrop::ArchiveError),
    Io(io::Error),
    Invalid(&'static str),
    Context {
        stage: &'static str,
        source: Box<Self>,
    },
}

impl Error {
    fn context(self, stage: &'static str) -> Self {
        Self::Context {
            stage,
            source: Box::new(self),
        }
    }

    pub(crate) fn kind(&self) -> crate::TransferFailureKind {
        match self {
            Self::Io(error) | Self::Archive(omdrop_airdrop::ArchiveError::Io(error)) => {
                crate::TransferFailureKind::from_io(error)
            }
            Self::Context { source, .. } => source.kind(),
            _ => crate::TransferFailureKind::Validation,
        }
    }
}

impl From<plist::PlistError> for Error {
    fn from(error: plist::PlistError) -> Self {
        Self::Plist(error)
    }
}

impl From<omdrop_airdrop::ArchiveError> for Error {
    fn from(error: omdrop_airdrop::ArchiveError) -> Self {
        Self::Archive(error)
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plist(error) => write!(f, "plist: {error}"),
            Self::Archive(error) => write!(f, "archive: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Invalid(message) => f.write_str(message),
            Self::Context { stage, source } => write!(f, "{stage}: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Context { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

pub fn parse_ask(body: &[u8]) -> Result<IncomingTransfer, Error> {
    let Value::Dictionary(entries) = plist::read(body)? else {
        return Err(Error::Invalid("/Ask root is not a dictionary"));
    };
    let map = Dictionary::new(&entries);
    let id = map
        .dictionary("TransferID")
        .and_then(|value| value.string("id"))
        .unwrap_or_else(|| "UNSPECIFIED".into());
    let files = map
        .array("Files")
        .unwrap_or_default()
        .iter()
        .filter_map(|value| {
            let Value::Dictionary(entries) = value else {
                return None;
            };
            let map = Dictionary::new(entries);
            Some(IncomingFile {
                name: map.string("FileName").unwrap_or_else(|| "unnamed".into()),
                kind: map
                    .string("FileType")
                    .unwrap_or_else(|| "public.data".into()),
                size: map.integer("FileSize"),
            })
        })
        .collect();
    Ok(IncomingTransfer {
        id,
        sender_name: map
            .string("SenderComputerName")
            .unwrap_or_else(|| "Someone".into()),
        sender_model: map.string("SenderModelName"),
        files,
        urls: map
            .array("Items")
            .unwrap_or_default()
            .iter()
            .filter_map(|value| match value {
                Value::String(value) => Some(value.clone()),
                _ => None,
            })
            .collect(),
        icon: map.data("FileIcon"),
        sender_record_data: map.data("SenderRecordData"),
    })
}

pub fn build_discover_reply(computer_name: &str, model_name: &str) -> Result<Vec<u8>, Error> {
    plist::write(&Value::Dictionary(vec![
        (
            "ReceiverComputerName".into(),
            Value::String(computer_name.into()),
        ),
        ("ReceiverModelName".into(), Value::String(model_name.into())),
        (
            "ReceiverMediaCapabilities".into(),
            Value::Data(br#"{"Version":1}"#.to_vec()),
        ),
    ]))
    .map_err(Error::from)
}

pub fn build_ask(
    sender_name: &str,
    sender_model: &str,
    transfer_id: &str,
    files: &[impl AsRef<Path>],
    icon: Option<&[u8]>,
) -> Result<Vec<u8>, Error> {
    let file_values = files
        .iter()
        .map(|path| {
            let path = path.as_ref();
            let metadata = fs::metadata(path)?;
            Ok(Value::Dictionary(vec![
                (
                    "FileName".into(),
                    Value::String(
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("unnamed")
                            .into(),
                    ),
                ),
                ("FileType".into(), Value::String(uti(path).into())),
                ("FileSize".into(), Value::Integer(metadata.len() as i64)),
                (
                    "FileBomPath".into(),
                    Value::String(format!(
                        "./{}",
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("unnamed")
                    )),
                ),
                ("FileIsDirectory".into(), Value::Bool(false)),
                ("ShouldConvertMediaFormats".into(), Value::Bool(false)),
            ]))
        })
        .collect::<Result<Vec<_>, io::Error>>()?;
    let mut entries = vec![
        (
            "TransferID".into(),
            Value::Dictionary(vec![("id".into(), Value::String(transfer_id.into()))]),
        ),
        (
            "TransferType".into(),
            Value::Dictionary(vec![("files".into(), Value::Dictionary(Vec::new()))]),
        ),
        (
            "SenderComputerName".into(),
            Value::String(sender_name.into()),
        ),
        ("SenderModelName".into(), Value::String(sender_model.into())),
        ("BundleID".into(), Value::String("com.apple.finder".into())),
        ("Items".into(), Value::Array(Vec::new())),
        ("Files".into(), Value::Array(file_values)),
        ("ConvertMediaFormats".into(), Value::Bool(false)),
    ];
    if let Some(icon) = icon {
        entries.push(("FileIcon".into(), Value::Data(icon.to_vec())));
    }
    Ok(plist::write(&Value::Dictionary(entries))?)
}

pub fn encode_upload(files: &[impl AsRef<Path>], format: UploadFormat) -> Result<Vec<u8>, Error> {
    match format {
        UploadFormat::Cpio => {
            let mut cpio = Vec::new();
            write_cpio(&mut cpio, files.iter())?;
            Ok(cpio)
        }
        UploadFormat::GzipCpio => {
            let mut output = Vec::new();
            let mut encoder = GzEncoder::new(&mut output, Compression::fast());
            write_cpio(&mut encoder, files.iter())?;
            encoder.finish()?;
            Ok(output)
        }
        UploadFormat::DvZip => {
            let mut encoder = DvZipWriter::new(Vec::new(), 64 * 1024)?;
            write_cpio(&mut encoder, files.iter())?;
            Ok(encoder.finish()?.0)
        }
    }
}

pub fn extract_upload<R: Read>(
    content_type: &str,
    input: R,
    destination: impl AsRef<Path>,
) -> Result<Vec<PathBuf>, Error> {
    PreparedUpload::read(content_type, input, destination.as_ref())?.commit()
}

/// Keep files private until archive checksums and the caller's transfer boundary validate.
/// Staging shares the destination volume so no-clobber moves need no whole-file buffer.
pub(crate) struct PreparedUpload {
    staging: tempfile::TempDir,
    destination: PathBuf,
    files: Vec<PathBuf>,
    pub(crate) archive_boundary: bool,
}

impl PreparedUpload {
    pub(crate) fn read<R: Read>(
        content_type: &str,
        input: R,
        destination: &Path,
    ) -> Result<Self, Error> {
        Self::read_with_dvzip_boundary(content_type, input, destination, |_| Ok(false))
    }

    /// The caller may end a self-delimited DVZip upload only after CPIO's trailer
    /// and every consumed DVZip block have validated. Other formats remain strict.
    pub(crate) fn read_with_dvzip_boundary<R: Read>(
        content_type: &str,
        input: R,
        destination: &Path,
        can_finish: impl FnMut(&mut R) -> io::Result<bool>,
    ) -> Result<Self, Error> {
        use std::io::BufRead;
        fs::create_dir_all(destination)?;
        let destination = fs::canonicalize(destination)?;
        let staging = tempfile::Builder::new()
            .prefix(".omdrop-incoming-")
            .tempdir_in(&destination)?;
        let mut archive_boundary = false;
        let files = match content_type.to_ascii_lowercase().as_str() {
            "application/x-dvzip" => {
                let mut decoded = DvZipReader::new(input);
                let files = extract_cpio(&mut decoded, staging.path())
                    .map_err(Error::from)
                    .map_err(|error| error.context("cpio entries through dvzip"))?;
                archive_boundary = decoded
                    .finish_at_archive_boundary(can_finish)
                    .map_err(Error::from)
                    .map_err(|error| error.context("dvzip final blocks/checksum"))?;
                files
            }
            "application/x-cpio" => {
                let mut input = io::BufReader::new(input);
                if input
                    .fill_buf()
                    .map_err(Error::from)
                    .map_err(|error| error.context("archive format detection"))?
                    .starts_with(&[0x1f, 0x8b])
                {
                    // Decode here so CPIO's trailer cannot skip gzip's CRC/length check.
                    let mut decoded = flate2::read::GzDecoder::new(input);
                    let files = extract_cpio(&mut decoded, staging.path())
                        .map_err(Error::from)
                        .map_err(|error| error.context("cpio entries through gzip"))?;
                    io::copy(&mut decoded, &mut io::sink())
                        .map_err(Error::from)
                        .map_err(|error| error.context("gzip final checksum/trailer"))?;
                    files
                } else {
                    extract_cpio(input, staging.path())
                        .map_err(Error::from)
                        .map_err(|error| error.context("cpio entries"))?
                }
            }
            _ => return Err(Error::Invalid("unsupported upload Content-Type")),
        };
        Ok(Self {
            staging,
            destination,
            files,
            archive_boundary,
        })
    }

    pub(crate) fn commit(self) -> Result<Vec<PathBuf>, Error> {
        let mut committed = Vec::new();
        let result = (|| {
            let staging_root = fs::canonicalize(self.staging.path())?;
            for source in &self.files {
                let relative = source
                    .strip_prefix(&staging_root)
                    .map_err(|_| Error::Invalid("extracted path escaped staging directory"))?;
                let destination = self.destination.join(relative);
                let parent = destination
                    .parent()
                    .ok_or(Error::Invalid("missing destination parent"))?;
                // Existing ancestor links must never redirect incoming files outside the save directory.
                let mut ancestor = parent;
                while !ancestor.exists() {
                    ancestor = ancestor
                        .parent()
                        .ok_or(Error::Invalid("missing destination ancestor"))?;
                }
                if !fs::canonicalize(ancestor)?.starts_with(&self.destination) {
                    return Err(Error::Invalid(
                        "destination directory escaped save location",
                    ));
                }
                fs::create_dir_all(parent)?;
                let mut staged = tempfile::TempPath::try_from_path(source)?;
                for suffix in 0..10000 {
                    let target = if suffix == 0 {
                        destination.clone()
                    } else {
                        let stem = destination
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy();
                        let extension = destination
                            .extension()
                            .map(|ext| format!(".{}", ext.to_string_lossy()))
                            .unwrap_or_default();
                        destination.with_file_name(format!("{stem} ({suffix}){extension}"))
                    };
                    match staged.persist_noclobber(&target) {
                        Ok(()) => {
                            committed.push(target);
                            break;
                        }
                        Err(error)
                            if error.error.kind() == io::ErrorKind::AlreadyExists
                                && suffix < 9999 =>
                        {
                            staged = error.path;
                            continue;
                        }
                        Err(error) => return Err(Error::Io(error.error)),
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            for path in &committed {
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
        Ok(committed)
    }
}

fn uti(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "public.jpeg",
        "png" => "public.png",
        "heic" => "public.heic",
        "gif" => "com.compuserve.gif",
        "mov" => "com.apple.quicktime-movie",
        "mp4" => "public.mpeg-4",
        "m4a" => "com.apple.m4a-audio",
        "mp3" => "public.mp3",
        "pdf" => "com.adobe.pdf",
        "txt" => "public.plain-text",
        "zip" => "public.zip-archive",
        "vcf" => "public.vcard",
        _ => "public.data",
    }
}

struct Dictionary<'a>(&'a [(String, Value)]);

impl<'a> Dictionary<'a> {
    fn new(entries: &'a [(String, Value)]) -> Self {
        Self(entries)
    }
    fn value(&self, key: &str) -> Option<&'a Value> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }
    fn string(&self, key: &str) -> Option<String> {
        match self.value(key) {
            Some(Value::String(value)) => Some(value.clone()),
            _ => None,
        }
    }
    fn integer(&self, key: &str) -> Option<u64> {
        match self.value(key) {
            Some(Value::Integer(value)) if *value >= 0 => Some(*value as u64),
            _ => None,
        }
    }
    fn data(&self, key: &str) -> Option<Vec<u8>> {
        match self.value(key) {
            Some(Value::Data(value)) => Some(value.clone()),
            _ => None,
        }
    }
    fn dictionary(&self, key: &str) -> Option<Dictionary<'a>> {
        match self.value(key) {
            Some(Value::Dictionary(value)) => Some(Dictionary::new(value)),
            _ => None,
        }
    }
    fn array(&self, key: &str) -> Option<&'a [Value]> {
        match self.value(key) {
            Some(Value::Array(value)) => Some(value),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor;

    #[test]
    fn dvzip_validates_data_after_the_cpio_trailer() {
        use flate2::write::ZlibEncoder;
        use std::io::Write;
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("file.txt");
        fs::write(&path, b"must validate the whole upload").unwrap();
        let cpio = encode_upload(&[&path], UploadFormat::Cpio).unwrap();
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&cpio).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut upload = (compressed.len() as u32).to_be_bytes().to_vec();
        upload.extend_from_slice(&compressed);
        for bad_tail in [false, true] {
            let mut bytes = upload.clone();
            if bad_tail {
                bytes.push(0x80);
            } else {
                *bytes.last_mut().unwrap() ^= 1;
            }
            let output = tempfile::tempdir().unwrap();
            assert!(
                extract_upload("application/x-dvzip", Cursor::new(bytes), output.path()).is_err()
            );
            assert!(fs::read_dir(output.path()).unwrap().next().is_none());
        }
    }

    #[test]
    fn dvzip_writes_files_before_reading_the_whole_upload() {
        use std::io::Write;
        let source = tempfile::tempdir().unwrap();
        let first = source.path().join("first.txt");
        fs::write(&first, b"stream me first").unwrap();
        let output = tempfile::tempdir().unwrap();
        let first_upload = encode_upload(&[&first], UploadFormat::DvZip).unwrap();
        // A valid extra block after CPIO's trailer is still read and checked.
        let mut extra = DvZipWriter::new(Vec::new(), 1024).unwrap();
        extra.write_all(b"padding after the archive").unwrap();
        struct Gated {
            first: Cursor<Vec<u8>>,
            tail: Cursor<Vec<u8>>,
            written: PathBuf,
        }
        impl Read for Gated {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                let n = self.first.read(out)?;
                if n != 0 {
                    return Ok(n);
                }
                assert!(
                    !self.written.exists(),
                    "unvalidated data must remain private"
                );
                let root = self.written.parent().unwrap();
                let staged = fs::read_dir(root)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| path.is_dir())
                    .expect("streaming staging directory");
                assert_eq!(
                    fs::read(staged.join("first.txt")).unwrap(),
                    b"stream me first"
                );
                self.tail.read(out)
            }
        }
        let reader = Gated {
            first: Cursor::new(first_upload),
            tail: Cursor::new(extra.finish().unwrap().0),
            written: output.path().join("first.txt"),
        };
        assert_eq!(
            extract_upload("application/x-dvzip", reader, output.path())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn dvzip_boundary_policy_runs_only_after_a_valid_archive_and_complete_blocks() {
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
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("file.txt");
        fs::write(&path, b"complete file").unwrap();
        let upload = encode_upload(&[&path], UploadFormat::DvZip).unwrap();
        let output = tempfile::tempdir().unwrap();
        let pending = PreparedUpload::read_with_dvzip_boundary(
            "application/x-dvzip",
            OpenPeer(Cursor::new(upload.clone())),
            output.path(),
            |_| Ok(true),
        )
        .unwrap();
        assert!(pending.archive_boundary);
        assert!(!output.path().join("file.txt").exists());
        assert_eq!(pending.commit().unwrap().len(), 1);

        let mut missing_trailer = encode_upload(&[&path], UploadFormat::Cpio).unwrap();
        missing_trailer.truncate(missing_trailer.len() - (76 + "TRAILER!!!".len() + 1));
        let mut encoded = Vec::new();
        omdrop_airdrop::encode_dvzip(Cursor::new(missing_trailer), &mut encoded, 1024).unwrap();
        for bytes in [
            upload[..upload.len() - 1].to_vec(),
            [upload.clone(), vec![0x80]].concat(),
            encoded,
        ] {
            let output = tempfile::tempdir().unwrap();
            let mut calls = 0;
            assert!(PreparedUpload::read_with_dvzip_boundary(
                "application/x-dvzip",
                Cursor::new(bytes),
                output.path(),
                |_| {
                    calls += 1;
                    Ok(true)
                },
            )
            .is_err());
            assert_eq!(calls, 0);
            assert!(fs::read_dir(output.path()).unwrap().next().is_none());
        }
        let output = tempfile::tempdir().unwrap();
        assert!(
            !PreparedUpload::read("application/x-dvzip", Cursor::new(upload), output.path())
                .unwrap()
                .archive_boundary
        );
        let upload = encode_upload(&[&path], UploadFormat::GzipCpio).unwrap();
        assert!(
            !PreparedUpload::read_with_dvzip_boundary(
                "application/x-cpio",
                Cursor::new(upload),
                output.path(),
                |_| panic!("gzip does not use the DVZip ending policy"),
            )
            .unwrap()
            .archive_boundary
        );
    }
    #[test]
    fn incomplete_or_corrupt_uploads_leave_no_partial_files_and_preserve_existing_data() {
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("payload.txt");
        fs::write(&file, b"new payload").unwrap();
        for format in [UploadFormat::Cpio, UploadFormat::GzipCpio] {
            let bytes = encode_upload(&[&file], format).unwrap();
            let output = tempfile::tempdir().unwrap();
            fs::write(output.path().join("payload.txt"), b"existing data").unwrap();
            for end in [bytes.len() / 2, bytes.len() - 1] {
                assert!(
                    extract_upload("application/x-cpio", &bytes[..end], output.path()).is_err()
                );
                assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
                assert_eq!(
                    fs::read(output.path().join("payload.txt")).unwrap(),
                    b"existing data"
                );
            }
            if format == UploadFormat::GzipCpio {
                let mut corrupted = bytes.clone();
                let crc = corrupted.len() - 8;
                corrupted[crc] ^= 1;
                assert!(
                    extract_upload("application/x-cpio", corrupted.as_slice(), output.path())
                        .is_err()
                );
                assert_eq!(fs::read_dir(output.path()).unwrap().count(), 1);
            }
            let paths =
                extract_upload("application/x-cpio", bytes.as_slice(), output.path()).unwrap();
            assert_ne!(paths[0], output.path().join("payload.txt"));
            assert_eq!(fs::read(&paths[0]).unwrap(), b"new payload");
            assert_eq!(
                fs::read(output.path().join("payload.txt")).unwrap(),
                b"existing data"
            );
        }
    }

    #[test]
    fn ask_round_trip_and_discover_reply() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("photo.jpg");
        fs::write(&file, b"photo").unwrap();
        let ask = build_ask("sender", "iPhone", "ABC", &[&file], None).unwrap();
        let transfer = parse_ask(&ask).unwrap();
        assert_eq!(transfer.id, "ABC");
        assert_eq!(transfer.files[0].kind, "public.jpeg");
        let reply = build_discover_reply("receiver", "omdrop").unwrap();
        assert!(matches!(plist::read(&reply).unwrap(), Value::Dictionary(_)));
    }

    #[test]
    fn upload_formats_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("one.txt");
        fs::write(&file, b"hello").unwrap();
        for format in [
            UploadFormat::Cpio,
            UploadFormat::GzipCpio,
            UploadFormat::DvZip,
        ] {
            let upload = encode_upload(&[&file], format).unwrap();
            let output = tempfile::tempdir().unwrap();
            let content_type = if format == UploadFormat::DvZip {
                "application/x-dvzip"
            } else {
                "application/x-cpio"
            };
            let paths = extract_upload(content_type, Cursor::new(upload), output.path()).unwrap();
            assert_eq!(fs::read(&paths[0]).unwrap(), b"hello");
        }
    }
}
