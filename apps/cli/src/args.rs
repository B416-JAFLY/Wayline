use crate::output::Failure;
use omdrop_core::SendFormat;
use std::{
    net::{Ipv6Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

pub const HELP: &str = "omdrop-cli status [--json]\nomdrop-cli doctor [--json]\nomdrop-cli discover [--timeout 15] [--json]\nomdrop-cli send [--to NAME] [--format auto|gzip|dvzip] [--json] -- FILE...\nomdrop-cli receive [--accept-all] [--save DIR] [--decision-timeout 60] [--json]\nomdrop-cli receive --bind [::1]:8771 --accept-all [--save DIR]\nomdrop-cli firmware info [--manifest PATH] [--json]\nomdrop-cli firmware flash --port COMx --board cardputer-adv [--manifest PATH] [--tool PATH]\n                          [--reinstall --yes-replace] [--json]\n无线命令可用 --name NAME、--link-local ADDRESS --iface-index N 覆盖本次设置。\n发现和发送可用 --no-ble / --legacy-ble。默认自动选择 awdl0。\n--reinstall --yes-replace 会替换现有程序及分区布局，不保证保留其他固件数据。";

pub struct Args {
    pub command: String,
    pub json: bool,
    pub help: bool,
    pub name: Option<String>,
    pub model: String,
    pub address: Option<Ipv6Addr>,
    pub index: Option<u32>,
    pub timeout: Duration,
    pub decision_timeout: Duration,
    pub ble: bool,
    pub legacy_ble: bool,
    pub target: Option<String>,
    pub files: Vec<PathBuf>,
    pub format: SendFormat,
    pub bind: Option<SocketAddr>,
    pub save: Option<PathBuf>,
    pub accept_all: bool,
    pub manifest: Option<PathBuf>,
    pub tool: Option<PathBuf>,
    pub port: Option<String>,
    pub board: Option<String>,
    pub reinstall: bool,
    pub yes_replace: bool,
}

pub fn json_requested(values: &[String]) -> bool {
    values
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--json")
}

impl Args {
    pub fn parse(values: Vec<String>, terminal: bool) -> Result<Self, Failure> {
        let json = json_requested(&values);
        let mut args = values.into_iter().peekable();
        while args.peek().is_some_and(|arg| arg == "--json") {
            args.next();
        }
        let command = args.next().unwrap_or_else(|| "--help".into());
        let command = if command == "firmware" {
            format!(
                "firmware {}",
                args.next()
                    .ok_or_else(|| Failure::policy("firmware 需要 info 或 flash"))?
            )
        } else {
            command
        };
        let mut result = Self {
            help: matches!(command.as_str(), "--help" | "-h"),
            command,
            json,
            name: None,
            model: "Windows".into(),
            address: None,
            index: None,
            timeout: Duration::from_secs(15),
            decision_timeout: Duration::from_secs(60),
            ble: true,
            legacy_ble: false,
            target: None,
            files: Vec::new(),
            format: SendFormat::Auto,
            bind: None,
            save: None,
            accept_all: false,
            manifest: None,
            tool: None,
            port: None,
            board: None,
            reinstall: false,
            yes_replace: false,
        };
        while let Some(argument) = args.next() {
            if argument == "--" {
                result.files.extend(args.map(PathBuf::from));
                break;
            }
            if !argument.starts_with('-') && result.command == "send" {
                result.files.push(argument.into());
                continue;
            }
            let allowed = match result.command.as_str() {
                "discover" => [
                    "--name",
                    "--link-local",
                    "--iface-index",
                    "--timeout",
                    "--no-ble",
                    "--legacy-ble",
                ]
                .contains(&argument.as_str()),
                "send" => [
                    "--name",
                    "--model",
                    "--link-local",
                    "--iface-index",
                    "--timeout",
                    "--no-ble",
                    "--legacy-ble",
                    "--to",
                    "--format",
                ]
                .contains(&argument.as_str()),
                "receive" => [
                    "--name",
                    "--model",
                    "--link-local",
                    "--iface-index",
                    "--bind",
                    "--save",
                    "--accept-all",
                    "--decision-timeout",
                ]
                .contains(&argument.as_str()),
                "firmware info" => argument == "--manifest",
                "firmware flash" => [
                    "--manifest",
                    "--tool",
                    "--port",
                    "--board",
                    "--reinstall",
                    "--yes-replace",
                ]
                .contains(&argument.as_str()),
                _ => false,
            };
            if !allowed && !matches!(argument.as_str(), "--json" | "--help" | "-h") {
                return Err(Failure::policy(format!(
                    "{} 不支持参数 {argument}",
                    result.command
                )));
            }
            let mut value = || {
                args.next()
                    .ok_or_else(|| Failure::policy(format!("{argument} 需要参数值")))
            };
            match argument.as_str() {
                "--json" => result.json = true,
                "--help" | "-h" => result.help = true,
                "--name" => result.name = Some(value()?),
                "--model" => result.model = value()?,
                "--link-local" => result.address = Some(value()?.parse().map_err(Failure::policy)?),
                "--iface-index" => result.index = Some(value()?.parse().map_err(Failure::policy)?),
                "--timeout" => result.timeout = seconds(&value()?)?,
                "--decision-timeout" => result.decision_timeout = seconds(&value()?)?,
                "--no-ble" => result.ble = false,
                "--legacy-ble" => result.legacy_ble = true,
                "--to" => result.target = Some(value()?),
                "--format" => {
                    result.format = match value()?.as_str() {
                        "auto" => SendFormat::Auto,
                        "gzip" => SendFormat::Gzip,
                        "dvzip" => SendFormat::DvZip,
                        _ => return Err(Failure::policy("格式应为 auto、gzip 或 dvzip")),
                    }
                }
                "--bind" => result.bind = Some(value()?.parse().map_err(Failure::policy)?),
                "--save" => result.save = Some(value()?.into()),
                "--accept-all" => result.accept_all = true,
                "--manifest" => result.manifest = Some(value()?.into()),
                "--tool" => result.tool = Some(value()?.into()),
                "--port" => result.port = Some(value()?),
                "--board" => result.board = Some(value()?),
                "--reinstall" => result.reinstall = true,
                "--yes-replace" => result.yes_replace = true,
                _ => return Err(Failure::policy("未知参数")),
            }
        }
        result.validate(terminal)?;
        Ok(result)
    }

    fn validate(&self, terminal: bool) -> Result<(), Failure> {
        if self.help {
            return Ok(());
        }
        if self.address.is_some() != self.index.is_some() {
            return Err(Failure::policy(
                "--link-local 与 --iface-index 必须成对指定",
            ));
        }
        if self
            .address
            .is_some_and(|value| !value.is_unicast_link_local())
            || self.index == Some(0)
        {
            return Err(Failure::policy("需要有效链路本地 IPv6 和非零网卡索引"));
        }
        if !self.files.is_empty() && self.command != "send" {
            return Err(Failure::policy("此命令不接受文件参数"));
        }
        match self.command.as_str() {
            "status" | "doctor" | "discover" | "firmware info" => {}
            "send" => {
                if self.target.is_none() && (!terminal || self.json) {
                    return Err(Failure::policy(
                        "非交互或 JSON 模式必须提供 --to，未发送文件",
                    ));
                }
                if self.files.is_empty() {
                    return Err(Failure::policy("至少指定一个文件"));
                }
            }
            "receive" => {
                if !self.accept_all && (!terminal || self.json) {
                    return Err(Failure::policy(
                        "非交互或 JSON 接收必须显式提供 --accept-all，未启动接收",
                    ));
                }
                if self.bind.is_some() && self.address.is_some() {
                    return Err(Failure::policy(
                        "--bind 用于本地 TCP 诊断，不能同时指定无线网卡参数",
                    ));
                }
            }
            "firmware flash" => {
                if self.port.as_deref().is_none_or(str::is_empty)
                    || self.board.as_deref().is_none_or(str::is_empty)
                {
                    return Err(Failure::policy("烧录需要显式选择 --port 与 --board"));
                }
                if self.reinstall && !self.yes_replace {
                    return Err(Failure::policy(
                        "重新安装将替换程序和分区布局，需要 --reinstall --yes-replace 明确同意",
                    ));
                }
                if self.yes_replace && !self.reinstall {
                    return Err(Failure::policy("--yes-replace 必须与 --reinstall 同时使用"));
                }
            }
            _ => return Err(Failure::policy(format!("未知命令：{}", self.command))),
        }
        Ok(())
    }
}

fn seconds(value: &str) -> Result<Duration, Failure> {
    let seconds = value.parse::<u64>().map_err(Failure::policy)?;
    if !(1..=3600).contains(&seconds) {
        return Err(Failure::policy("超时必须在 1 到 3600 秒之间"));
    }
    Ok(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(values: &[&str], tty: bool) -> Result<Args, Failure> {
        Args::parse(values.iter().map(|s| s.to_string()).collect(), tty)
    }
    #[test]
    fn unattended_policy_is_checked_before_any_runtime_operation() {
        assert_eq!(parse(&["receive"], false).err().unwrap().code, 2);
        assert_eq!(parse(&["receive", "--json"], true).err().unwrap().code, 2);
        assert!(parse(&["receive", "--accept-all"], false).is_ok());
        assert!(parse(&["receive"], true).is_ok());
        assert!(parse(&["send", "--json", "a.txt"], true).is_err());
        assert!(parse(&["send", "a.txt"], false).is_err());
    }
    #[test]
    fn interface_pairs_and_explicit_firmware_choices_are_required() {
        assert!(parse(&["discover", "--iface-index", "1"], true).is_err());
        assert!(parse(&["discover", "--timeout", "0"], true).is_err());
        assert!(parse(
            &[
                "firmware",
                "flash",
                "--port",
                "COM4",
                "--board",
                "cardputer-adv",
                "--reinstall"
            ],
            true
        )
        .is_err());
        assert!(parse(
            &[
                "firmware",
                "flash",
                "--port",
                "COM4",
                "--board",
                "cardputer-adv",
                "--reinstall",
                "--yes-replace"
            ],
            true
        )
        .is_ok());
    }
    #[test]
    fn unicode_paths_and_literal_option_like_filenames_are_preserved() {
        let parsed = parse(
            &[
                "send",
                "--to",
                "我的手机",
                "--",
                "C:\\目录 有空格\\照片.jpg",
                "--json",
            ],
            false,
        )
        .unwrap();
        assert!(!parsed.json);
        assert_eq!(parsed.files[0], PathBuf::from("C:\\目录 有空格\\照片.jpg"));
        assert_eq!(parsed.files[1], PathBuf::from("--json"));
    }
}
