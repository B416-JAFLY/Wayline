use crate::{
    artifacts, firmware_package,
    tool_package::{self, Platform},
};
use anyhow::{ensure, Context, Result};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

struct Options(BTreeMap<String, String>);
impl Options {
    fn parse(args: &[String], allowed: &[&str]) -> Result<Self> {
        ensure!(
            args.len().is_multiple_of(2),
            "every packaging option requires a value"
        );
        let mut values = BTreeMap::new();
        for pair in args.as_chunks::<2>().0 {
            ensure!(
                allowed.contains(&pair[0].as_str()),
                "unknown option: {}",
                pair[0]
            );
            ensure!(
                !pair[1].is_empty() && !pair[1].starts_with("--"),
                "missing value for {}",
                pair[0]
            );
            ensure!(
                values.insert(pair[0].clone(), pair[1].clone()).is_none(),
                "duplicate option: {}",
                pair[0]
            );
        }
        Ok(Self(values))
    }
    fn required(&self, key: &str) -> Result<&str> {
        self.0
            .get(key)
            .map(String::as_str)
            .with_context(|| format!("{key} is required"))
    }
    fn path(&self, key: &str, root: &Path) -> Option<PathBuf> {
        self.0.get(key).map(|value| root.join(value))
    }
    fn platform(&self) -> Result<Platform> {
        match self.0.get("--platform") {
            Some(name) => Platform::parse(name),
            None => Platform::native(),
        }
    }
}

pub fn firmware(args: &[String], root: &Path) -> Result<()> {
    let options = Options::parse(args, &["--build-dir", "--board", "--output"])?;
    let board = options.required("--board")?;
    let build = options
        .path("--build-dir", root)
        .unwrap_or(firmware_package::default_build_dir(root, board)?);
    let output = root.join(options.required("--output")?);
    artifacts::publish(&output, |stage| {
        firmware_package::write(&build, board, stage)
    })?;
    println!("{}", output.display());
    Ok(())
}

pub fn tool(args: &[String], root: &Path) -> Result<()> {
    let options = Options::parse(
        args,
        &["--platform", "--output", "--archive", "--cache-dir"],
    )?;
    let output = root.join(options.required("--output")?);
    let platform = options.platform()?;
    let cache = options
        .path("--cache-dir", root)
        .unwrap_or(root.join(".cache/downloads"));
    let archive = options.path("--archive", root);
    artifacts::publish(&output, |stage| {
        tool_package::write(platform, &cache, archive.as_deref(), stage)
    })?;
    println!("{}", output.display());
    Ok(())
}

pub fn bundle(args: &[String], root: &Path) -> Result<()> {
    let options = Options::parse(
        args,
        &[
            "--host-dir",
            "--firmware-build",
            "--board",
            "--platform",
            "--output",
            "--version",
            "--cache-dir",
            "--esptool-archive",
            "--wintun-archive",
        ],
    )?;
    let host = root.join(options.required("--host-dir")?);
    let board = options.required("--board")?;
    let build = options
        .path("--firmware-build", root)
        .unwrap_or(firmware_package::default_build_dir(root, board)?);
    let platform = options.platform()?;
    let output = root.join(options.required("--output")?);
    let version = options.required("--version")?;
    ensure!(
        version.len() <= 128 && !version.chars().any(char::is_control),
        "invalid package version"
    );
    let cache = options
        .path("--cache-dir", root)
        .unwrap_or(root.join(".cache/downloads"));
    let esptool = options.path("--esptool-archive", root);
    let wintun = options.path("--wintun-archive", root);
    ensure!(
        platform == Platform::Windows || wintun.is_none(),
        "--wintun-archive only applies to windows-x64 packages"
    );
    artifacts::publish(&output, |stage| {
        let names: &[&str] = if platform == Platform::Windows {
            &[
                "wayline-desktop",
                "wayline-bridge",
                "wayline-service",
                "wayline-cli",
                "wayline-radio",
            ]
        } else {
            &["wayline-cli", "wayline-radio"]
        };
        for name in names {
            let name = platform.executable(name);
            let source = artifacts::source_file(&host, &name)?;
            tool_package::validate_executable(&source, platform)?;
            artifacts::copy(&source, &stage.join(name))?;
        }
        let firmware = if board == wayline_flash::SUPPORTED_BOARD {
            stage.join("firmware")
        } else {
            stage.join("firmware/profiles").join(board)
        };
        firmware_package::write(&build, board, &firmware)
            .with_context(|| format!("package firmware from {}", build.display()))?;
        tool_package::write(
            platform,
            &cache,
            esptool.as_deref(),
            &stage.join("tools/esptool"),
        )?;
        if platform == Platform::Windows {
            tool_package::write_wintun(&cache, wintun.as_deref(), stage)?;
        }
        for (source, destination) in [
            ("LICENSE", "LICENSE"),
            ("vendor/owly-awdl/LICENSE", "licenses/owly-awdl/LICENSE"),
            (
                "vendor/owly-airdrop/LICENSE",
                "licenses/owly-airdrop/LICENSE",
            ),
        ] {
            artifacts::copy(&root.join(source), &stage.join(destination))?;
        }
        artifacts::write_json(
            &stage.join("package-info.json"),
            &serde_json::json!({
                "platform":platform.name(), "board":board,
                "desktopAvailable":platform == Platform::Windows,
                "wirelessServiceAvailable":platform == Platform::Windows,
                "installsService":false, "flashesDevice":false
            }),
        )?;
        artifacts::release_manifest(stage, platform.name(), version)
    })?;
    println!("{}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn malformed_and_ambiguous_options_are_rejected() {
        for values in [
            vec!["--output"],
            vec!["--bogus", "x"],
            vec!["--output", "--archive"],
            vec!["--output", "x", "--output", "y"],
        ] {
            assert!(Options::parse(&args(&values), &["--output"]).is_err());
        }
        let options =
            Options::parse(&args(&["--output", "directory with spaces"]), &["--output"]).unwrap();
        assert_eq!(
            options.required("--output").unwrap(),
            "directory with spaces"
        );
    }

    #[test]
    fn unknown_board_cannot_be_used_as_an_output_path() {
        let dir = tempfile::tempdir().unwrap();
        assert!(firmware(
            &args(&["--board", "../escape", "--output", "out"]),
            dir.path()
        )
        .is_err());
        assert!(!dir.path().join("out").exists());
    }
}
