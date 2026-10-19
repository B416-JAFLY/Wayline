//! Per-user Explorer verbs, Send To, and lossless single-instance file activation.
use anyhow::{Result, bail, ensure};
use std::{
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::*,
        System::{Com::*, Registry::*},
        UI::Shell::*,
    },
    core::{HSTRING, Interface, w},
};

const VERB: &str = r"Software\Classes\*\shell\Omdrop.Send";
const LINK: &str = "Omdrop AirDrop.lnk";
pub const COPY_DATA_ID: usize = 0x4f4d4452;
pub const MAX_ACTIVATION_BYTES: usize = 1024 * 1024;

pub enum Mode {
    Desktop(Launch),
    Service,
}
impl Mode {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self> {
        let args: Vec<_> = args.into_iter().collect();
        // Only an explicit leading switch selects the SCM entry point. A file
        // named --service after Explorer's "--send --" remains a file.
        if args.first().is_some_and(|arg| arg == "--service") {
            let options = omdrop_service::service::Options::parse(args)?;
            ensure!(
                options.is_some_and(|options| options.scm),
                "--service 只能用于 Windows 服务模式"
            );
            return Ok(Self::Service);
        }
        Ok(Self::Desktop(Launch::parse(args)?))
    }
}

#[derive(Default)]
pub struct Launch {
    pub files: Vec<PathBuf>,
    pub unregister: bool,
}
impl Launch {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self> {
        let mut result = Self::default();
        let mut paths_only = false;
        for arg in args {
            if !paths_only {
                if arg == "--" {
                    paths_only = true;
                    continue;
                }
                if arg == "--send" || arg == "--show-in-taskbar" {
                    continue;
                }
                if arg == "--unregister-shell" {
                    result.unregister = true;
                    continue;
                }
                if arg.to_string_lossy().starts_with('-') {
                    bail!("未知参数：{}", arg.to_string_lossy());
                }
            }
            ensure!(!arg.is_empty(), "文件路径不能为空");
            result.files.push(PathBuf::from(arg));
        }
        ensure!(
            !result.unregister || result.files.is_empty(),
            "移除菜单时不能同时发送文件"
        );
        Ok(result)
    }
}

pub fn encode_paths(paths: &[PathBuf]) -> Result<Vec<u8>> {
    let mut units = Vec::new();
    for path in paths {
        let path: Vec<_> = path.as_os_str().encode_wide().collect();
        ensure!(!path.is_empty() && !path.contains(&0), "无效文件路径");
        units.extend(path);
        units.push(0);
    }
    units.push(0);
    if paths.is_empty() {
        units.push(0);
    }
    ensure!(units.len() * 2 <= MAX_ACTIVATION_BYTES, "选择的文件过多");
    Ok(units.into_iter().flat_map(u16::to_le_bytes).collect())
}

pub fn decode_paths(bytes: &[u8]) -> Result<Vec<PathBuf>> {
    ensure!(
        bytes.len() >= 4 && bytes.len() <= MAX_ACTIVATION_BYTES && bytes.len().is_multiple_of(2),
        "无效文件消息"
    );
    let units: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    ensure!(units.ends_with(&[0, 0]), "文件消息未结束");
    if units == [0, 0] {
        return Ok(Vec::new());
    }
    units[..units.len() - 2]
        .split(|u| *u == 0)
        .map(|path| {
            ensure!(!path.is_empty(), "空文件路径");
            Ok(PathBuf::from(OsString::from_wide(path)))
        })
        .collect()
}

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
fn set_value(key: &str, name: &str, value: &str) -> Result<()> {
    unsafe {
        let mut handle = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut handle,
            None,
        )
        .ok()?;
        let key = Key(handle);
        let data: Vec<_> = value
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        RegSetValueExW(key.0, &HSTRING::from(name), None, REG_SZ, Some(&data)).ok()?;
    }
    Ok(())
}
fn shortcut_path() -> Result<PathBuf> {
    unsafe {
        let name = SHGetKnownFolderPath(&FOLDERID_SendTo, KF_FLAG_DEFAULT, None)?;
        let path = PathBuf::from(OsString::from_wide(name.as_wide())).join(LINK);
        CoTaskMemFree(Some(name.0.cast()));
        Ok(path)
    }
}
fn command(exe: &Path) -> String {
    format!("\"{}\" --send -- \"%1\"", exe.display())
}

pub fn register() -> Result<()> {
    let _apartment = crate::picker::Apartment::new()?;
    let exe = std::env::current_exe()?;
    // Explorer may invoke a legacy verb once per selected file. The running
    // instance coalesces those activations into one selection and one window.
    set_value(VERB, "", "通过 Omdrop 发送")?;
    set_value(VERB, "Icon", &format!("\"{}\",0", exe.display()))?;
    set_value(VERB, "MultiSelectModel", "Player")?;
    set_value(&format!(r"{VERB}\command"), "", &command(&exe))?;
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        link.SetArguments(w!("--send --"))?;
        link.SetDescription(w!("使用 Omdrop 向附近的设备发送文件"))?;
        link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
        let persist: IPersistFile = link.cast()?;
        persist.Save(&HSTRING::from(shortcut_path()?.as_os_str()), true)?;
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
    Ok(())
}

pub fn unregister() -> Result<()> {
    unsafe {
        let result = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(VERB));
        if result != ERROR_FILE_NOT_FOUND {
            result.ok()?;
        }
    }
    let path = shortcut_path()?;
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        result => result?,
    }
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_mode_is_explicit_and_does_not_consume_explorer_paths() {
        assert!(matches!(
            Mode::parse([OsString::from("--service")]).unwrap(),
            Mode::Service
        ));
        let mode = Mode::parse(["--send", "--", "--service"].map(OsString::from)).unwrap();
        let Mode::Desktop(launch) = mode else {
            panic!("Explorer activation must stay in the user session");
        };
        assert_eq!(launch.files, [PathBuf::from("--service")]);
        assert!(matches!(Mode::parse([]).unwrap(), Mode::Desktop(_)));
        for args in [
            vec!["--service", "--console"],
            vec!["--service", "--help"],
            vec!["--service", "--send"],
            vec!["--send", "--service"],
        ] {
            assert!(Mode::parse(args.into_iter().map(OsString::from)).is_err());
        }
    }

    #[test]
    fn explorer_paths_roundtrip_without_shell_interpretation() {
        let paths = vec![
            PathBuf::from(r"C:\文件夹\a & b $(x).png"),
            PathBuf::from(r"D:\照片\📷.jpg"),
            PathBuf::from(OsString::from_wide(&[
                b'C' as u16,
                b':' as u16,
                b'\\' as u16,
                0xd800,
            ])),
        ];
        assert_eq!(decode_paths(&encode_paths(&paths).unwrap()).unwrap(), paths);
        assert!(
            decode_paths(&encode_paths(&[]).unwrap())
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn invalid_activation_is_rejected() {
        for payload in [
            vec![],
            vec![0],
            vec![1, 0, 0, 0],
            vec![0; MAX_ACTIVATION_BYTES + 2],
        ] {
            assert!(decode_paths(&payload).is_err());
        }
    }
    #[test]
    fn send_to_accepts_multiple_paths_and_option_like_names() {
        let launch =
            Launch::parse(["--send", "--", "C:\\file one.txt", "--photo.png"].map(OsString::from))
                .unwrap();
        assert_eq!(
            launch.files,
            vec![
                PathBuf::from("C:\\file one.txt"),
                PathBuf::from("--photo.png")
            ]
        );
        assert_eq!(
            command(Path::new(r"C:\Program Files\Omdrop\omdrop.exe")),
            r#""C:\Program Files\Omdrop\omdrop.exe" --send -- "%1""#
        );
        assert!(Launch::parse([OsString::from("--unknown")]).is_err());
    }
}
