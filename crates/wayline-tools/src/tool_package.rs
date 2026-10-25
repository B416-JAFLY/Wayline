use crate::artifacts;
use anyhow::{bail, ensure, Context, Result};
use flate2::read::GzDecoder;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const ARCHIVE_LIMIT: u64 = 128 * 1024 * 1024;
const EXTRACT_LIMIT: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Linux,
}

impl Platform {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "windows-x64" => Ok(Self::Windows),
            "linux-x64" => Ok(Self::Linux),
            _ => bail!("unsupported package platform: {name}"),
        }
    }
    pub fn native() -> Result<Self> {
        ensure!(
            std::env::consts::ARCH == "x86_64",
            "packaging supports x86_64 hosts; specify --platform for cross-packaging"
        );
        match std::env::consts::OS {
            "windows" => Ok(Self::Windows),
            "linux" => Ok(Self::Linux),
            _ => bail!("specify --platform windows-x64 or linux-x64"),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Windows => "windows-x64",
            Self::Linux => "linux-x64",
        }
    }
    pub fn executable(self, name: &str) -> String {
        if self == Self::Windows {
            format!("{name}.exe")
        } else {
            name.into()
        }
    }
    fn asset(self) -> Asset {
        match self {
            Self::Windows => Asset { name: "esptool-v5.0.2-windows-amd64.zip", url: "https://github.com/espressif/esptool/releases/download/v5.0.2/esptool-v5.0.2-windows-amd64.zip", sha256: "1caef993a16c5915714a0da772d93f2e3239f316c06223981b262b838287268c", prefix: "esptool-windows-amd64", zip: true },
            Self::Linux => Asset { name: "esptool-v5.0.2-linux-amd64.tar.gz", url: "https://github.com/espressif/esptool/releases/download/v5.0.2/esptool-v5.0.2-linux-amd64.tar.gz", sha256: "519e0015872d527bdca850b18575d4f635fa88ccdffe47a14c99a80a90b780c5", prefix: "esptool-linux-amd64", zip: false },
        }
    }
}

struct Asset {
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
    prefix: &'static str,
    zip: bool,
}

fn verify_archive(path: &Path, asset: &Asset) -> Result<()> {
    artifacts::regular(path)?;
    ensure!(
        fs::metadata(path)?.len() <= ARCHIVE_LIMIT,
        "archive is too large: {}",
        path.display()
    );
    ensure!(
        artifacts::sha256(path)? == asset.sha256,
        "SHA-256 mismatch for {}; expected {}",
        path.display(),
        asset.sha256
    );
    Ok(())
}

fn archive(cache: &Path, supplied: Option<&Path>, asset: &Asset) -> Result<PathBuf> {
    if let Some(path) = supplied {
        verify_archive(path, asset)?;
        return Ok(path.to_owned());
    }
    fs::create_dir_all(cache)?;
    let target = cache.join(asset.name);
    if target.exists() {
        verify_archive(&target, asset)?;
        return Ok(target);
    }
    eprintln!("Downloading {}", asset.url);
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(600))
        .build()?;
    let response = client
        .get(asset.url)
        .send()
        .map_err(|e| anyhow::anyhow!("download {}: {}", asset.url, e.without_url()))?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("download {}: {}", asset.url, e.without_url()))?;
    if let Some(length) = response.content_length() {
        ensure!(length <= ARCHIVE_LIMIT, "download exceeds archive limit");
    }
    let mut temporary = tempfile::NamedTempFile::new_in(cache)?;
    let count = io::copy(&mut response.take(ARCHIVE_LIMIT + 1), &mut temporary)?;
    ensure!(count <= ARCHIVE_LIMIT, "download exceeds archive limit");
    temporary.flush()?;
    verify_archive(temporary.path(), asset)?;
    temporary
        .persist_noclobber(&target)
        .with_context(|| format!("publish download {}", target.display()))?;
    Ok(target)
}

fn archive_path(name: &str, prefix: &str) -> Result<Option<PathBuf>> {
    let name = name.trim_end_matches('/');
    if name == prefix {
        return Ok(None);
    }
    let relative = name
        .strip_prefix(&format!("{prefix}/"))
        .context("unexpected archive root directory")?;
    Ok(Some(artifacts::relative_path(relative)?))
}

fn write_entry(
    reader: &mut impl Read,
    target: &Path,
    size: u64,
    mode: u32,
    total: &mut u64,
) -> Result<()> {
    *total = total.checked_add(size).context("archive size overflow")?;
    ensure!(
        *total <= EXTRACT_LIMIT,
        "unpacked archive exceeds size limit"
    );
    fs::create_dir_all(target.parent().context("archive entry has no parent")?)?;
    let mut output = File::options().write(true).create_new(true).open(target)?;
    ensure!(
        io::copy(&mut reader.take(size + 1), &mut output)? == size,
        "archive entry length mismatch"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            target,
            fs::Permissions::from_mode(if mode & 0o111 != 0 { 0o755 } else { 0o644 }),
        )?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

fn extract(path: &Path, asset: &Asset, output: &Path) -> Result<()> {
    fs::create_dir_all(output)?;
    let mut total = 0;
    let mut seen = HashSet::new();
    if asset.zip {
        let mut zip = zip::ZipArchive::new(File::open(path)?)?;
        ensure!(zip.len() <= 4096, "too many ZIP entries");
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index)?;
            let mode = entry.unix_mode().unwrap_or(0);
            ensure!(
                mode & 0o170000 != 0o120000,
                "ZIP symlinks are not supported"
            );
            let Some(relative) = archive_path(&entry.name()?, asset.prefix)? else {
                ensure!(entry.is_dir(), "archive root must be a directory");
                continue;
            };
            if entry.is_dir() {
                fs::create_dir_all(output.join(relative))?;
                continue;
            }
            ensure!(
                seen.insert(relative.to_string_lossy().to_ascii_lowercase()),
                "duplicate archive entry"
            );
            let size = entry.size();
            write_entry(&mut entry, &output.join(relative), size, mode, &mut total)?;
        }
    } else {
        let mut tar = tar::Archive::new(GzDecoder::new(File::open(path)?));
        for (index, entry) in tar.entries()?.enumerate() {
            ensure!(index < 4096, "too many tar entries");
            let mut entry = entry?;
            let name = entry
                .path()?
                .to_str()
                .context("archive path is not UTF-8")?
                .to_owned();
            let kind = entry.header().entry_type();
            ensure!(
                kind.is_file() || kind.is_dir(),
                "tar links and special files are not supported"
            );
            let Some(relative) = archive_path(&name, asset.prefix)? else {
                ensure!(kind.is_dir(), "archive root must be a directory");
                continue;
            };
            if kind.is_dir() {
                fs::create_dir_all(output.join(relative))?;
                continue;
            }
            ensure!(
                seen.insert(relative.to_string_lossy().to_ascii_lowercase()),
                "duplicate archive entry"
            );
            let size = entry.size();
            let mode = entry.header().mode()?;
            write_entry(&mut entry, &output.join(relative), size, mode, &mut total)?;
        }
    }
    ensure!(!seen.is_empty(), "archive contains no files");
    Ok(())
}

pub fn validate_executable(path: &Path, platform: Platform) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    artifacts::regular(path)?;
    let mut file = File::open(path)?;
    let mut header = [0u8; 64];
    file.read_exact(&mut header)?;
    match platform {
        Platform::Windows => {
            ensure!(
                &header[..2] == b"MZ",
                "not a Windows executable: {}",
                path.display()
            );
            let offset = u32::from_le_bytes(header[60..64].try_into().unwrap());
            ensure!(
                u64::from(offset) + 6 <= file.metadata()?.len(),
                "invalid PE header offset"
            );
            file.seek(SeekFrom::Start(u64::from(offset)))?;
            let mut pe = [0; 6];
            file.read_exact(&mut pe)?;
            ensure!(
                &pe[..4] == b"PE\0\0" && pe[4..] == [0x64, 0x86],
                "not a Windows x64 executable"
            );
        }
        Platform::Linux => ensure!(
            &header[..4] == b"\x7fELF"
                && header[4] == 2
                && header[5] == 1
                && header[18..20] == [62, 0],
            "not a Linux x64 ELF executable: {}",
            path.display()
        ),
    }
    Ok(())
}

pub fn write(
    platform: Platform,
    cache: &Path,
    supplied: Option<&Path>,
    output: &Path,
) -> Result<()> {
    let asset = platform.asset();
    let archive = archive(cache, supplied, &asset)?;
    extract(&archive, &asset, output)?;
    let executable = output.join(platform.executable("esptool"));
    validate_executable(&executable, platform)?;
    ensure!(
        output.join("LICENSE").is_file(),
        "esptool distribution is missing LICENSE"
    );
    let native = Platform::native().ok() == Some(platform);
    if native {
        let result = Command::new(&executable)
            .arg("version")
            .output()
            .context("run packaged esptool version")?;
        ensure!(
            result.status.success(),
            "packaged esptool failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        ensure!(
            String::from_utf8_lossy(&result.stdout)
                .lines()
                .any(|line| line.trim() == wayline_flash::TOOL_VERSION),
            "packaged esptool version does not match {}",
            wayline_flash::TOOL_VERSION
        );
    }
    artifacts::write_json(
        &output.join("tool.json"),
        &serde_json::json!({"version":wayline_flash::TOOL_VERSION,"platform":platform.name(),"archiveUrl":asset.url,"archiveSha256":asset.sha256,"executableSha256":artifacts::sha256(&executable)?,"nativeVersionChecked":native}),
    )
}

pub fn write_wintun(cache: &Path, supplied: Option<&Path>, output: &Path) -> Result<()> {
    let asset = Asset {
        name: "wintun-0.14.1.zip",
        url: "https://www.wintun.net/builds/wintun-0.14.1.zip",
        sha256: "07c256185d6ee3652e09fa55c0b673e2624b565e02c4b9091c79ca7d2f24ef51",
        prefix: "wintun",
        zip: true,
    };
    let archive = archive(cache, supplied, &asset)?;
    let temp = tempfile::tempdir()?;
    extract(&archive, &asset, temp.path())?;
    let dll = temp.path().join("bin/amd64/wintun.dll");
    ensure!(
        artifacts::sha256(&dll)?
            == "e5da8447dc2c320edc0fc52fa01885c103de8c118481f683643cacc3220dafce",
        "Wintun DLL hash mismatch"
    );
    artifacts::copy(&dll, &output.join("wintun.dll"))?;
    artifacts::copy(
        &temp.path().join("LICENSE.txt"),
        &output.join("licenses/wintun/LICENSE.txt"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_paths_reject_escape_absolute_and_windows_prefixes() {
        for name in [
            "tool/../evil",
            "tool//absolute",
            "tool/C:/evil",
            "tool/a\\b",
            "other/file",
        ] {
            assert!(archive_path(name, "tool").is_err(), "{name}");
        }
        assert_eq!(
            archive_path("tool/bin/file", "tool").unwrap(),
            Some(PathBuf::from("bin/file"))
        );
    }

    #[test]
    fn zip_traversal_and_tar_symlinks_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("bad.zip");
        let mut writer = zip::ZipWriter::new(File::create(&zip_path).unwrap());
        writer
            .start_file("tool/../outside", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"bad").unwrap();
        writer.finish().unwrap();
        let mut asset = Asset {
            name: "bad.zip",
            url: "",
            sha256: "",
            prefix: "tool",
            zip: true,
        };
        assert!(extract(&zip_path, &asset, &dir.path().join("zip-out")).is_err());
        let tar_path = dir.path().join("bad.tar.gz");
        let encoder = flate2::write::GzEncoder::new(
            File::create(&tar_path).unwrap(),
            flate2::Compression::default(),
        );
        let mut writer = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        writer
            .append_link(&mut header, "tool/link", "../outside")
            .unwrap();
        writer.into_inner().unwrap().finish().unwrap();
        asset.zip = false;
        assert!(extract(&tar_path, &asset, &dir.path().join("tar-out")).is_err());
        assert!(!dir.path().join("outside").exists());
    }

    #[test]
    fn untrusted_archive_hash_is_rejected_before_unpacking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.zip");
        fs::write(&path, b"not the pinned release").unwrap();
        assert!(archive(dir.path(), Some(&path), &Platform::Windows.asset()).is_err());
    }

    #[test]
    fn size_limit_is_checked_before_creating_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oversized");
        assert!(write_entry(&mut &b""[..], &path, EXTRACT_LIMIT + 1, 0, &mut 0).is_err());
        assert!(!path.exists());
    }
}
