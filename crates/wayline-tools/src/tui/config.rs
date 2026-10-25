use super::language::{validate_text, InputError, Strings};
use crate::firmware_package;
use std::path::{Path, PathBuf};

pub struct Config {
    pub board: usize,
    pub release: bool,
    pub tests: bool,
    pub windows_package: bool,
    // Text values correspond to Strings.fields[4..]; empty paths use displayed defaults.
    values: [String; 9],
    target_dir: PathBuf,
}

impl Config {
    pub fn new(target_dir: PathBuf) -> Self {
        Self {
            board: 0,
            release: false,
            tests: false,
            windows_package: cfg!(windows),
            values: [
                String::new(),
                String::new(),
                "dist/wayline".into(),
                env!("CARGO_PKG_VERSION").into(),
                String::new(),
                String::new(),
                ".cache/downloads".into(),
                String::new(),
                String::new(),
            ],
            target_dir,
        }
    }

    pub fn board_id(&self) -> &'static str {
        wayline_flash::supported_profiles()[self.board].id
    }

    pub fn mode(&self) -> &'static str {
        if self.release {
            "release"
        } else {
            "debug"
        }
    }

    pub fn platform(&self) -> &'static str {
        if self.windows_package {
            "windows-x64"
        } else {
            "linux-x64"
        }
    }

    pub fn text(&self, field: usize) -> &str {
        &self.values[field - 4]
    }

    pub fn set_text(&mut self, field: usize, value: String) -> Result<(), InputError> {
        validate_text(&value)?;
        if matches!(field, 6 | 7 | 10) && value.trim().is_empty() {
            return Err(InputError::Required);
        }
        self.values[field - 4] = value;
        Ok(())
    }

    pub fn cycle(&mut self, field: usize, forward: bool) {
        match field {
            0 => {
                let count = wayline_flash::supported_profiles().len();
                self.board = (self.board + if forward { 1 } else { count - 1 }) % count;
            }
            1 => self.release = !self.release,
            2 => self.tests = !self.tests,
            3 => self.windows_package = !self.windows_package,
            _ => {}
        }
    }

    fn host_dir(&self) -> String {
        if self.text(4).is_empty() {
            self.target_dir
                .join(self.mode())
                .to_string_lossy()
                .into_owned()
        } else {
            self.text(4).into()
        }
    }

    fn firmware_dir(&self) -> String {
        if self.text(5).is_empty() {
            firmware_package::default_build_dir(Path::new(""), self.board_id())
                .expect("supported board")
                .to_string_lossy()
                .into_owned()
        } else {
            self.text(5).into()
        }
    }

    pub fn display(&self, field: usize, strings: &Strings) -> String {
        match field {
            0 => self.board_id().into(),
            1 => self.mode().into(),
            2 => if self.tests {
                &strings.ui.enabled
            } else {
                &strings.ui.disabled
            }
            .clone(),
            3 => self.platform().into(),
            4 if self.text(4).is_empty() => {
                format!("{}: {}", strings.ui.automatic, self.host_dir())
            }
            5 if self.text(5).is_empty() => {
                format!("{}: {}", strings.ui.automatic, self.firmware_dir())
            }
            _ => {
                let value = self.text(field);
                if value.is_empty() {
                    strings.ui.default_value.clone()
                } else {
                    value.into()
                }
            }
        }
    }

    pub fn args(&self, action: usize) -> Vec<String> {
        let command = [
            "firmware-check",
            "host-build",
            "firmware-build",
            "firmware-package",
            "tool-package",
            "package-build",
            "release-verify",
        ][action];
        let mut args = vec![command.to_owned()];
        let mut option = |name: &str, value: &str| {
            args.push(name.into());
            args.push(value.into());
        };
        match action {
            0 | 2 => {
                if action == 2 {
                    option("--board", self.board_id());
                    option("--output", &self.firmware_dir());
                }
                for (name, field) in [("--idf", 8), ("--python", 9)] {
                    if !self.text(field).is_empty() {
                        option(name, self.text(field));
                    }
                }
            }
            1 => {
                if self.release {
                    args.push("--release".into());
                }
                if self.tests {
                    args.push("--test".into());
                }
            }
            3 => {
                option("--board", self.board_id());
                option("--build-dir", &self.firmware_dir());
                option("--output", self.text(6));
            }
            4 | 5 => {
                if action == 5 {
                    option("--board", self.board_id());
                    option("--host-dir", &self.host_dir());
                    option("--firmware-build", &self.firmware_dir());
                    option("--version", self.text(7));
                    if self.windows_package && !self.text(12).is_empty() {
                        option("--wintun-archive", self.text(12));
                    }
                }
                option("--output", self.text(6));
                option("--platform", self.platform());
                option("--cache-dir", self.text(10));
                if !self.text(11).is_empty() {
                    option(
                        if action == 4 {
                            "--archive"
                        } else {
                            "--esptool-archive"
                        },
                        self.text(11),
                    );
                }
            }
            6 => args.push(self.text(6).into()),
            _ => unreachable!(),
        }
        args
    }
}

// This is a display of argv, never a shell command to be evaluated.
pub fn command_text(args: &[String]) -> String {
    let mut text = "wayline-tools".to_owned();
    for arg in args {
        text.push(' ');
        if arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_/.:".contains(c))
        {
            text.push_str(arg);
        } else {
            text.push_str(&format!("{arg:?}"));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_selection_keeps_firmware_and_package_paths_in_sync() {
        let mut config = Config::new("custom target".into());
        for board in 0..wayline_flash::supported_profiles().len() {
            config.board = board;
            let build = config.args(2);
            let package = config.args(5);
            assert_eq!(&build[1..3], &["--board", config.board_id()]);
            assert_eq!(build[4], package[6]);
            assert!(build[4].contains(config.board_id()));
        }
        config.cycle(1, true);
        assert!(config.args(1).contains(&"--release".into()));
        assert_eq!(
            config.args(5)[4],
            Path::new("custom target").join("release").to_string_lossy()
        );
        config.set_text(4, "manually built".into()).unwrap();
        config.cycle(1, true);
        assert_eq!(config.args(5)[4], "manually built");
    }

    #[test]
    fn paths_remain_single_arguments_and_linux_omits_wintun() {
        let mut config = Config::new("target".into());
        config
            .set_text(6, "输出 with spaces & $literal".into())
            .unwrap();
        config.set_text(12, "Wintun archive.zip".into()).unwrap();
        config.windows_package = false;
        assert!(config
            .args(5)
            .contains(&"输出 with spaces & $literal".into()));
        assert!(!config.args(5).contains(&"--wintun-archive".into()));
        config.windows_package = true;
        assert!(config.args(5).contains(&"Wintun archive.zip".into()));
        assert!(config.set_text(6, "\n".into()).is_err());
        assert!(config.set_text(7, "".into()).is_err());
    }
}
