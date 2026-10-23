//! Shared host preferences and identity. UI and command-line overrides never persist implicitly.
mod advertisement;
pub use advertisement::{ReceiverAdvertisement, receiver_advertisement};
mod transfer_history;
use anyhow::{Context, Result, bail, ensure};
use omdrop_core::{Identity, NetworkInterface};
use serde_json::{Map, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::Ipv6Addr,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
pub use transfer_history::{
    TransferDirection, TransferFailureReason, TransferHistoryRecord, TransferOutcome,
    append_transfer_history, clear_transfer_history, load_transfer_history,
};

pub const MAX_NAME_CHARS: usize = 64;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const MAX_LANGUAGE_CODE_CHARS: usize = 16;

/// Structured name validation failures let localized front ends present their own text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    ControlCharacter,
    Empty,
    TooLong,
}
impl std::fmt::Display for NameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ControlCharacter => "名称不能包含换行或控制字符",
            Self::Empty => "请输入设备名称",
            Self::TooLong => "名称最多可输入 64 个字符",
        })
    }
}
impl std::error::Error for NameError {}

pub fn identity_dir() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("找不到用户数据目录 LOCALAPPDATA")?)
            .join("omdrop"),
    )
}
pub fn settings_path() -> Result<PathBuf> {
    Ok(identity_dir()?.join("desktop.json"))
}
pub fn default_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .and_then(|name| normalize_name(&name).ok())
        .unwrap_or_else(|| "Windows PC".into())
}
pub fn normalize_name(name: &str) -> Result<String> {
    if name.chars().any(char::is_control) {
        return Err(NameError::ControlCharacter.into());
    }
    let name = name.trim();
    if name.is_empty() {
        return Err(NameError::Empty.into());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(NameError::TooLong.into());
    }
    Ok(name.into())
}
pub fn load_name() -> Result<String> {
    load_name_from(&settings_path()?, &default_name())
}
/// Read-only diagnostic access; does not create a directory, lock, settings or identity.
pub fn inspect_name() -> Result<String> {
    load_name()
}
pub fn save_name(name: &str) -> Result<String> {
    save_name_to(&settings_path()?, name)
}
pub fn load_name_from(path: &Path, fallback: &str) -> Result<String> {
    // Writers replace the complete file atomically. Readers need no lock or filesystem
    // mutation and may observe either complete version during a concurrent update.
    let settings = read_settings(path)?;
    match settings.get("device_name") {
        Some(Value::String(name)) => normalize_name(name),
        None | Some(Value::Null) => normalize_name(fallback),
        _ => bail!("名称设置格式不正确"),
    }
}
pub fn save_name_to(path: &Path, name: &str) -> Result<String> {
    let name = normalize_name(name)?;
    let _guard = FileGuard::acquire(&path.with_extension("lock"))?;
    let mut settings = read_settings(path)?;
    settings.insert("device_name".into(), Value::String(name.clone()));
    write_settings(path, &settings)?;
    Ok(name)
}

/// The desktop UI language is stored beside the other shared preferences.
/// `None` means the user has not chosen a language and the system default applies.
pub fn load_language() -> Result<Option<String>> {
    load_language_from(&settings_path()?)
}

pub fn load_language_from(path: &Path) -> Result<Option<String>> {
    let settings = read_settings(path)?;
    match settings.get("language") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(language)) => normalize_language(language).map(Some),
        _ => bail!("界面语言设置格式不正确"),
    }
}

pub fn save_language(language: &str) -> Result<String> {
    save_language_to(&settings_path()?, language)
}

pub fn save_language_to(path: &Path, language: &str) -> Result<String> {
    let language = normalize_language(language)?;
    let _guard = FileGuard::acquire(&path.with_extension("lock"))?;
    let mut settings = read_settings(path)?;
    settings.insert("language".into(), Value::String(language.clone()));
    write_settings(path, &settings)?;
    Ok(language)
}

fn normalize_language(language: &str) -> Result<String> {
    let language = language.trim();
    ensure!(!language.is_empty(), "界面语言代码不能为空");
    ensure!(
        language.chars().count() <= MAX_LANGUAGE_CODE_CHARS,
        "界面语言代码过长"
    );
    ensure!(
        language
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_')),
        "界面语言代码格式不正确"
    );
    Ok(language.into())
}

/// Resolve the shared receive destination without creating directories or probe files.
/// A configured but unavailable directory is an error, never an implicit fallback.
pub fn download_dir() -> Result<PathBuf> {
    download_dir_from(&settings_path()?, &default_download_dir()?)
}

pub fn download_dir_from(path: &Path, fallback: &Path) -> Result<PathBuf> {
    let settings = read_settings(path)?;
    match settings.get("download_directory") {
        Some(Value::String(directory)) => normalize_download_dir(Path::new(directory)),
        None => {
            ensure!(fallback.is_absolute(), "默认接收目录必须是绝对路径");
            Ok(fallback.to_path_buf())
        }
        _ => bail!("接收目录设置格式不正确"),
    }
}

pub fn save_download_dir(directory: &Path) -> Result<PathBuf> {
    save_download_dir_to(&settings_path()?, directory)
}

/// Store an existing, writable directory while preserving every other preference.
pub fn save_download_dir_to(path: &Path, directory: &Path) -> Result<PathBuf> {
    let directory = normalize_download_dir(directory)?;
    let text = directory.to_str().context("接收目录路径无法保存为文本")?;
    let mut probe = tempfile::Builder::new()
        .prefix(".omdrop-write-check-")
        .tempfile_in(&directory)
        .context("无法写入接收目录，请选择有写入权限的文件夹")?;
    probe
        .write_all(b"Omdrop")
        .context("无法写入接收目录，请选择有写入权限的文件夹")?;
    probe
        .as_file()
        .sync_all()
        .context("无法保存接收目录中的文件")?;
    probe.close().context("无法清理接收目录中的临时文件")?;

    let _guard = FileGuard::acquire(&path.with_extension("lock"))?;
    let mut settings = read_settings(path)?;
    settings.insert("download_directory".into(), Value::String(text.into()));
    write_settings(path, &settings)?;
    Ok(directory)
}

fn normalize_download_dir(directory: &Path) -> Result<PathBuf> {
    ensure!(directory.is_absolute(), "接收目录必须是绝对路径");
    ensure!(
        fs::metadata(directory)
            .context("无法访问接收目录")?
            .is_dir(),
        "接收位置必须是文件夹"
    );
    let directory = fs::canonicalize(directory).context("无法解析接收目录")?;
    Ok(readable_directory(directory))
}

#[cfg(windows)]
fn readable_directory(directory: PathBuf) -> PathBuf {
    // Canonical Windows paths normally carry a verbatim prefix. Prefer the familiar
    // spelling for UI/configuration, but preserve it when stripping changes semantics
    // (for example, a directory whose name ends in a dot or space).
    let Some(text) = directory.to_str() else {
        return directory;
    };
    let readable = if let Some(path) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{path}"))
    } else if let Some(path) = text.strip_prefix(r"\\?\") {
        PathBuf::from(path)
    } else {
        return directory;
    };
    if fs::canonicalize(&readable).is_ok_and(|resolved| resolved == directory) {
        readable
    } else {
        directory
    }
}

#[cfg(not(windows))]
fn readable_directory(directory: PathBuf) -> PathBuf {
    directory
}

fn write_settings(path: &Path, settings: &Map<String, Value>) -> Result<()> {
    let parent = path.parent().context("无效的设置目录")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).context("无法创建临时配置")?;
    let bytes = serde_json::to_vec_pretty(settings)?;
    ensure!((bytes.len() as u64) < MAX_SETTINGS_BYTES, "设置文件过大");
    temporary.write_all(&bytes)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).context("无法原子保存设置")?;
    Ok(())
}
fn read_settings(path: &Path) -> Result<Map<String, Value>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(error).context("无法读取设置"),
    };
    ensure!(file.metadata()?.len() <= MAX_SETTINGS_BYTES, "设置文件过大");
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_SETTINGS_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_SETTINGS_BYTES, "设置文件过大");
    serde_json::from_slice(&bytes).context("设置格式不正确")
}
pub fn load_identity(name: &str) -> Result<Identity> {
    load_identity_from(&identity_dir()?, name)
}
pub fn load_identity_from(directory: &Path, name: &str) -> Result<Identity> {
    let name = normalize_name(name)?;
    let _guard = FileGuard::acquire(&directory.join("identity.lock"))?;
    let (certificate, key) = Identity::paths(directory);
    // A half-persisted pair must never replace a previously published identity silently.
    ensure!(
        certificate.exists() == key.exists(),
        "身份文件不完整，请恢复证书与私钥后重试"
    );
    let identity = if certificate.exists() {
        let read = |path: &Path| -> Result<Vec<u8>> {
            let mut bytes = Vec::new();
            File::open(path)?
                .take(64 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                !bytes.is_empty() && bytes.len() <= 64 * 1024,
                "身份证书或私钥长度无效"
            );
            Ok(bytes)
        };
        Identity {
            certificate_der: read(&certificate)?,
            private_key_der: read(&key)?,
        }
    } else {
        Identity::load_or_create(directory, &name)?
    };
    omdrop_core::server_config(&identity).context("用户身份证书或私钥无效")?;
    Ok(identity)
}

/// OS file locks are released after a process exits, including crashes. Keep the lock file:
/// unlinking it would let another process lock a different inode under the same name.
struct FileGuard(File);
impl FileGuard {
    fn acquire(path: &Path) -> Result<Self> {
        fs::create_dir_all(path.parent().context("无效的锁目录")?)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20))
                }
                Err(error) => return Err(error).context("配置或身份正被另一进程使用"),
            }
        }
    }
}
impl Drop for FileGuard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub fn select_interface(
    link_local: Option<Ipv6Addr>,
    index: Option<u32>,
) -> Result<NetworkInterface> {
    match (link_local, index) {
        (Some(address), Some(index)) => {
            ensure!(
                address.is_unicast_link_local() && index != 0,
                "网卡必须指定有效链路本地 IPv6 和非零索引"
            );
            let interfaces = interfaces()?;
            interfaces
                .into_iter()
                .find(|i| i.index == index && i.link_local == address)
                .context("指定的 IPv6 地址不属于该活动网卡")
        }
        (None, None) => awdl_interface(),
        _ => bail!("--link-local 与 --iface-index 必须成对指定"),
    }
}
pub fn awdl_interface() -> Result<NetworkInterface> {
    let mut candidates = interfaces()?.into_iter().filter(|i| i.name == "awdl0");
    let first = candidates
        .next()
        .context("无线桥接尚未就绪，请检查 ESP32 连接")?;
    ensure!(
        !candidates.any(|i| i.index != first.index),
        "存在多个 awdl0 网卡，请明确指定地址与索引"
    );
    Ok(first)
}

#[cfg(windows)]
pub fn default_download_dir() -> Result<PathBuf> {
    use windows::Win32::{System::Com::CoTaskMemFree, UI::Shell::*};
    // SHGetKnownFolderPath allocates a terminated string owned by the caller.
    let path = unsafe { SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None)? };
    let text = unsafe { path.to_string() };
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    Ok(PathBuf::from(text?).join("Omdrop"))
}
#[cfg(not(windows))]
pub fn default_download_dir() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("HOME").context("HOME is unavailable")?)
            .join("Downloads/Omdrop"),
    )
}

#[cfg(windows)]
fn interfaces() -> Result<Vec<NetworkInterface>> {
    use windows::Win32::{
        Foundation::ERROR_BUFFER_OVERFLOW,
        NetworkManagement::{IpHelper::*, Ndis::IfOperStatusUp},
        Networking::WinSock::*,
    };
    let mut size = 16 * 1024u32;
    for _ in 0..4 {
        ensure!(size <= 1024 * 1024, "网卡列表过大");
        // usize storage satisfies IP_ADAPTER_ADDRESSES alignment. All pointer fields returned
        // by GetAdaptersAddresses refer to this live buffer; no reference escapes the loop.
        let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
        let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        let result = unsafe {
            GetAdaptersAddresses(
                AF_INET6.0 as u32,
                GET_ADAPTERS_ADDRESSES_FLAGS(0),
                None,
                Some(head),
                &mut size,
            )
        };
        if result == ERROR_BUFFER_OVERFLOW.0 {
            continue;
        }
        ensure!(result == 0, "读取无线网卡失败（{result}）");
        let mut found = Vec::new();
        let mut adapter = head;
        while !adapter.is_null() {
            let a = unsafe { &*adapter };
            if a.OperStatus == IfOperStatusUp && a.Ipv6IfIndex != 0 {
                let name = unsafe { a.FriendlyName.to_string() }.unwrap_or_default();
                let mut address = a.FirstUnicastAddress;
                while !address.is_null() {
                    let unicast = unsafe { &*address };
                    let socket = unicast.Address;
                    if unicast.DadState == IpDadStatePreferred
                        && !socket.lpSockaddr.is_null()
                        && socket.iSockaddrLength as usize >= size_of::<SOCKADDR_IN6>()
                    {
                        let addr = unsafe { &*socket.lpSockaddr.cast::<SOCKADDR_IN6>() };
                        if addr.sin6_family == AF_INET6 {
                            let link_local = Ipv6Addr::from(unsafe { addr.sin6_addr.u.Byte });
                            if link_local.is_unicast_link_local() {
                                found.push(NetworkInterface {
                                    name: name.clone(),
                                    index: a.Ipv6IfIndex,
                                    link_local,
                                });
                            }
                        }
                    }
                    address = unicast.Next;
                }
            }
            adapter = a.Next;
        }
        return Ok(found);
    }
    bail!("无线网卡正在变化，请稍后重试")
}
#[cfg(not(windows))]
fn interfaces() -> Result<Vec<NetworkInterface>> {
    bail!("自动网卡选择目前仅支持 Windows")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_preserve_unicode_and_reject_controls() {
        assert_eq!(normalize_name("  工作电脑 🦉 ").unwrap(), "工作电脑 🦉");
        assert!(normalize_name(&"中".repeat(64)).is_ok());
        for (name, kind) in [
            (String::new(), NameError::Empty),
            ("   ".into(), NameError::Empty),
            ("a\nb".into(), NameError::ControlCharacter),
            ("\ta".into(), NameError::ControlCharacter),
            ("中".repeat(65), NameError::TooLong),
        ] {
            let error = normalize_name(&name).unwrap_err();
            assert_eq!(error.downcast_ref::<NameError>(), Some(&kind));
        }
    }
    #[test]
    fn language_preference_preserves_other_settings_and_rejects_invalid_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop.json");
        assert_eq!(load_language_from(&path).unwrap(), None);
        fs::write(&path, br#"{"device_name":"PC","future":{"x":1}}"#).unwrap();
        save_language_to(&path, " zh-TW ").unwrap();
        assert_eq!(load_language_from(&path).unwrap().as_deref(), Some("zh-TW"));
        assert_eq!(load_name_from(&path, "fallback").unwrap(), "PC");
        assert_eq!(read_settings(&path).unwrap()["future"]["x"], 1);
        for invalid in [
            b"{\"language\":false}".as_slice(),
            b"{\"language\":\"\"}",
            b"{\"language\":\"zh CN\"}",
            b"{\"language\":\"zh-CN-very-long-code\"}",
        ] {
            fs::write(&path, invalid).unwrap();
            assert!(load_language_from(&path).is_err());
        }
        assert!(save_language_to(&path, "").is_err());
    }
    #[test]
    fn saves_preserve_unknown_settings_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desktop.json");
        fs::write(&path, br#"{"device_name":"old","future":{"x":1}}"#).unwrap();
        save_name_to(&path, " 新设备 ").unwrap();
        assert_eq!(load_name_from(&path, "fallback").unwrap(), "新设备");
        assert_eq!(read_settings(&path).unwrap()["future"]["x"], 1);
        fs::write(&path, b"{").unwrap();
        assert!(save_name_to(&path, "new").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"{");
    }
    #[test]
    fn download_directory_preserves_unicode_spaces_and_other_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("收到的文件 🦉 with spaces");
        fs::create_dir(&destination).unwrap();
        let settings = dir.path().join("desktop.json");
        fs::write(&settings, br#"{"device_name":"PC","future":{"x":1}}"#).unwrap();
        let saved = save_download_dir_to(&settings, &destination).unwrap();
        assert_eq!(
            fs::canonicalize(&saved).unwrap(),
            fs::canonicalize(&destination).unwrap()
        );
        assert_eq!(download_dir_from(&settings, dir.path()).unwrap(), saved);
        assert_eq!(load_name_from(&settings, "fallback").unwrap(), "PC");
        assert_eq!(read_settings(&settings).unwrap()["future"]["x"], 1);
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        #[cfg(windows)]
        assert!(!saved.to_string_lossy().starts_with(r"\\?\"));
    }
    #[test]
    fn missing_download_directory_setting_is_read_only_and_uses_default() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("not-created").join("desktop.json");
        let fallback = dir.path().join("Downloads").join("Omdrop");
        assert_eq!(download_dir_from(&settings, &fallback).unwrap(), fallback);
        assert!(!fallback.exists());
        assert!(!settings.parent().unwrap().exists());
    }
    #[test]
    fn download_directory_rejects_invalid_paths_and_preserves_corrupt_settings() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("desktop.json");
        let file = dir.path().join("file.txt");
        fs::write(&file, b"contents").unwrap();
        for invalid in [PathBuf::from("relative"), dir.path().join("missing"), file] {
            assert!(save_download_dir_to(&settings, &invalid).is_err());
            assert!(!settings.exists());
        }
        for invalid in [
            b"{".as_slice(),
            br#"{"download_directory":false}"#,
            br#"{"download_directory":null}"#,
            br#"{"download_directory":"relative"}"#,
        ] {
            fs::write(&settings, invalid).unwrap();
            assert!(download_dir_from(&settings, dir.path()).is_err());
        }
        fs::write(&settings, b"{").unwrap();
        assert!(save_download_dir_to(&settings, dir.path()).is_err());
        assert_eq!(fs::read(&settings).unwrap(), b"{");
        let destination = dir.path().join("removed-later");
        fs::create_dir(&destination).unwrap();
        fs::write(&settings, b"{}").unwrap();
        save_download_dir_to(&settings, &destination).unwrap();
        fs::remove_dir(&destination).unwrap();
        assert!(download_dir_from(&settings, dir.path()).is_err());
    }
    #[test]
    fn concurrent_name_and_download_directory_updates_preserve_both_fields() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("desktop.json");
        fs::write(&settings, br#"{"future":{"x":1}}"#).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let name_worker = {
            let settings = settings.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                for index in 0..12 {
                    save_name_to(&settings, &format!("电脑 {index}")).unwrap();
                }
            })
        };
        let directory_worker = {
            let settings = settings.clone();
            let destination = dir.path().to_path_buf();
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..12 {
                    save_download_dir_to(&settings, &destination).unwrap();
                }
            })
        };
        name_worker.join().unwrap();
        directory_worker.join().unwrap();
        assert_eq!(load_name_from(&settings, "fallback").unwrap(), "电脑 11");
        assert_eq!(
            fs::canonicalize(download_dir_from(&settings, dir.path()).unwrap()).unwrap(),
            fs::canonicalize(dir.path()).unwrap()
        );
        assert_eq!(read_settings(&settings).unwrap()["future"]["x"], 1);
    }
    #[test]
    fn simultaneous_first_identity_creation_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
        let handles: Vec<_> = (0..6)
            .map(|_| {
                let path = dir.path().to_path_buf();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    load_identity_from(&path, "电脑").unwrap()
                })
            })
            .collect();
        let identities: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(identities.windows(2).all(|v| v[0] == v[1]));
    }
    #[test]
    fn identity_subprocess_worker() {
        let Some(directory) = std::env::var_os("OMDROP_TEST_IDENTITY_ROOT") else {
            return;
        };
        let identity = load_identity_from(Path::new(&directory), "并发电脑").unwrap();
        println!("identity={}", identity.service_id());
    }
    #[test]
    fn separate_processes_share_one_first_identity() {
        let dir = tempfile::tempdir().unwrap();
        let children: Vec<_> = (0..4)
            .map(|_| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "tests::identity_subprocess_worker",
                        "--nocapture",
                    ])
                    .env("OMDROP_TEST_IDENTITY_ROOT", dir.path().join("identity"))
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        let ids: Vec<_> = children
            .into_iter()
            .map(|child| {
                let output = child.wait_with_output().unwrap();
                assert!(output.status.success());
                String::from_utf8(output.stdout)
                    .unwrap()
                    .lines()
                    .find_map(|line| line.strip_prefix("identity=").map(str::to_owned))
                    .unwrap()
            })
            .collect();
        assert!(ids.windows(2).all(|pair| pair[0] == pair[1]));
    }
    #[test]
    fn reading_missing_settings_is_non_mutating() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("absent");
        assert_eq!(
            load_name_from(&parent.join("desktop.json"), "电脑").unwrap(),
            "电脑"
        );
        assert!(!parent.exists());
    }
    #[test]
    fn rejects_partial_identity_and_partial_interface() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("receiver.cert.der"), b"old").unwrap();
        assert!(load_identity_from(dir.path(), "name").is_err());
        assert!(select_interface(Some("fe80::1".parse().unwrap()), None).is_err());
        assert!(select_interface(None, Some(3)).is_err());
        assert!(select_interface(Some("::1".parse().unwrap()), Some(3)).is_err());
    }
}
