use std::{
    ffi::OsString,
    io,
    path::{Component, Path},
    time::Duration,
};
use tokio::process::Command;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

#[derive(Debug)]
pub struct Rule {
    pub name: String,
    pub settings: Vec<OsString>,
}

pub fn rules(exe: &Path) -> io::Result<Vec<Rule>> {
    let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let allowed = ["omdrop-cli.exe", "omdrop-desktop.exe"];
    if !allowed.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "firewall supports omdrop-cli.exe or omdrop-desktop.exe only",
        ));
    }
    // Accept local absolute drive paths only. netsh receives individual arguments,
    // but quotes/newlines also have no legitimate use in a Windows executable path.
    let local_drive = matches!(exe.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), std::path::Prefix::Disk(_)));
    if !local_drive
        || !exe.is_absolute()
        || exe.to_string_lossy().contains(['"', '\r', '\n', '\0'])
        || !exe.is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "firewall exe must be an existing absolute local path",
        ));
    }
    let mut program = OsString::from("program=");
    program.push(exe);
    Ok([("TCP", 8771), ("UDP", 5353)]
        .into_iter()
        .map(|(protocol, port)| Rule {
            name: format!(
                "omdrop Rust AirDrop (AWDL, link-local only): {} {protocol}",
                name.to_ascii_lowercase()
            ),
            settings: vec![
                "dir=in".into(),
                "action=allow".into(),
                program.clone(),
                format!("protocol={protocol}").into(),
                format!("localport={port}").into(),
                "remoteip=fe80::/10".into(),
                "profile=any".into(),
                "enable=yes".into(),
            ],
        })
        .collect())
}

pub async fn ensure(exe: &Path) -> io::Result<()> {
    for rule in rules(exe)? {
        let name = OsString::from(format!("name={}", rule.name));
        let existing = netsh(vec!["show".into(), "rule".into(), name.clone()]).await?;
        let mut args: Vec<OsString> = if existing.status.success() {
            vec!["set".into(), "rule".into(), name, "new".into()]
        } else {
            vec!["add".into(), "rule".into(), name]
        };
        args.extend(rule.settings);
        let result = netsh(args).await?;
        if !result.status.success() {
            return Err(io::Error::other(format!(
                "netsh failed for {} ({}): {} {}",
                rule.name,
                result.status,
                String::from_utf8_lossy(&result.stdout).trim(),
                String::from_utf8_lossy(&result.stderr).trim()
            )));
        }
    }
    Ok(())
}

async fn netsh(args: Vec<OsString>) -> io::Result<std::process::Output> {
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::other("SystemRoot is not defined"))?;
    let mut command = Command::new(Path::new(&root).join("System32/netsh.exe"));
    command
        .args(["advfirewall", "firewall"])
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "netsh exceeded 10 seconds"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builds_two_link_local_rules_and_does_not_modify_firewall() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("with spaces");
        std::fs::create_dir(&folder).unwrap();
        let exe = folder.join("omdrop-cli.exe");
        std::fs::write(&exe, b"test fixture").unwrap();
        let rules = rules(&exe).unwrap();
        assert_eq!(rules.len(), 2);
        assert_ne!(rules[0].name, rules[1].name);
        for rule in &rules {
            assert!(rule.settings.contains(&"remoteip=fe80::/10".into()));
            assert!(rule.settings.contains(&"dir=in".into()));
            assert_eq!(
                rule.settings
                    .iter()
                    .filter(|a| a.to_string_lossy().starts_with("program="))
                    .count(),
                1
            );
        }
        assert!(rules[0].settings.contains(&"localport=8771".into()));
        assert!(rules[1].settings.contains(&"localport=5353".into()));
        assert!(super::rules(&folder.join("powershell.exe")).is_err());
        assert!(super::rules(Path::new("omdrop-cli.exe")).is_err());
        let desktop = folder.join("omdrop-desktop.exe");
        std::fs::write(&desktop, b"test fixture").unwrap();
        assert_eq!(super::rules(&desktop).unwrap().len(), 2);
        for name in ["OmdropTray.exe", "omdrop.exe"] {
            let retired = folder.join(name);
            std::fs::write(&retired, b"test fixture").unwrap();
            assert!(super::rules(&retired).is_err());
        }
    }
}
