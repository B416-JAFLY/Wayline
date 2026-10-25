use crate::artifacts;
use anyhow::{bail, ensure, Context, Result};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use wayline_flash::{FirmwareLink, Image, ImageKind, Manifest};

pub fn defaults(board: &str) -> Result<(&'static str, &'static str, bool)> {
    match board {
        "cardputer-adv" => Ok((
            "esp32s3",
            "sdkconfig.defaults;sdkconfig.defaults.cardputer",
            false,
        )),
        "esp32s3-uart0" => Ok((
            "esp32s3",
            "sdkconfig.defaults;sdkconfig.defaults.esp32s3-headless",
            true,
        )),
        "esp32s3-native-usb" => Ok((
            "esp32s3",
            "sdkconfig.defaults;sdkconfig.defaults.esp32s3-headless-usb",
            true,
        )),
        "esp32c5-uart0" => Ok((
            "esp32c5",
            "sdkconfig.defaults;sdkconfig.defaults.esp32c5",
            true,
        )),
        "esp32c5-native-usb" => Ok((
            "esp32c5",
            "sdkconfig.defaults;sdkconfig.defaults.esp32c5;sdkconfig.defaults.esp32c5-usb",
            true,
        )),
        _ => bail!("unsupported board profile: {board}"),
    }
}

pub fn default_build_dir(root: &Path, board: &str) -> Result<PathBuf> {
    defaults(board)?;
    Ok(root
        .join("firmware")
        .join(format!("build-{}-{board}", std::env::consts::OS)))
}

pub fn write(build: &Path, board: &str, output: &Path) -> Result<()> {
    let profile = wayline_flash::profile_for_board(board).context("unsupported firmware board")?;
    let description = artifacts::json(&build.join("project_description.json"))?;
    let flash = artifacts::json(&build.join("flasher_args.json"))?;
    let flash_files = flash["flash_files"]
        .as_object()
        .context("missing flash_files map")?;
    ensure!(
        flash_files.len() == 3,
        "firmware package requires exactly three flash images"
    );
    ensure!(
        artifacts::string(&description, "project_name")? == "owl-radio",
        "build is not a Wayline firmware project"
    );
    ensure!(
        artifacts::string(&description, "target")? == profile.chip
            && artifacts::string(&flash["extra_esptool_args"], "chip")? == profile.chip,
        "build chip does not match board {board}"
    );
    let size_text = artifacts::string(&flash["flash_settings"], "flash_size")?;
    let flash_size = size_text
        .strip_suffix("MB")
        .context("flash size must be an explicit MB value")?
        .parse::<u64>()?
        .checked_mul(1024 * 1024)
        .context("flash size overflow")?;
    ensure!(flash_size == profile.flash_size,
        "build flash size {size_text} does not match {board} ({} MB); align the SDK defaults and board profile before packaging", profile.flash_size / (1024 * 1024));

    let mut config = String::new();
    fs::File::open(build.join("sdkconfig"))?
        .take(2 * 1024 * 1024 + 1)
        .read_to_string(&mut config)?;
    ensure!(config.len() <= 2 * 1024 * 1024, "sdkconfig is too large");
    let enabled = |name: &str| {
        config
            .lines()
            .any(|line| line.trim() == format!("{name}=y"))
    };
    ensure!(
        enabled("CONFIG_OWL_RADIO_LINK_USB_SERIAL_JTAG")
            == (profile.link == FirmwareLink::NativeUsbSerialJtag),
        "USB/UART configuration does not match {board}"
    );
    ensure!(
        enabled("CONFIG_OWL_CARDPUTER_UI") == (board == wayline_flash::SUPPORTED_BOARD),
        "Cardputer UI configuration does not match {board}"
    );

    fs::create_dir_all(output)?;
    let mut images = Vec::new();
    for (key, name, kind) in [
        ("bootloader", "bootloader.bin", ImageKind::Bootloader),
        (
            "partition-table",
            "partition-table.bin",
            ImageKind::PartitionTable,
        ),
        ("app", "owl-radio.bin", ImageKind::Application),
    ] {
        let item = flash
            .get(key)
            .with_context(|| format!("missing {key} in flasher_args.json"))?;
        let file_name = artifacts::string(item, "file")?;
        let offset = artifacts::string(item, "offset")?;
        ensure!(
            flash_files.get(offset).and_then(serde_json::Value::as_str) == Some(file_name),
            "flash_files does not match the {key} image descriptor"
        );
        let source = artifacts::source_file(build, file_name)?;
        let length = fs::metadata(&source)?.len();
        ensure!(
            length > 0 && length <= flash_size,
            "image length exceeds the selected Flash: {file_name}"
        );
        let offset = u32::from_str_radix(
            offset
                .strip_prefix("0x")
                .context("expected hexadecimal image offset")?,
            16,
        )?;
        let target = output.join(name);
        artifacts::copy(&source, &target)?;
        if kind != ImageKind::PartitionTable {
            let mut header = [0; 4];
            fs::File::open(&target)?.read_exact(&mut header)?;
            let image_capacity = 1u64 << (20 + (header[3] >> 4));
            ensure!(
                image_capacity == flash_size,
                "image header Flash capacity does not match build metadata: {name}"
            );
        }
        images.push(Image {
            path: name.into(),
            offset,
            length: fs::metadata(&target)?.len(),
            sha256: artifacts::sha256(&target)?,
            kind,
        });
    }
    let version = artifacts::string(&description, "project_version")?.to_owned();
    // CMake records the source's git-describe label, including the dirty suffix.
    // Preserve it instead of substituting the packager checkout's unrelated HEAD.
    let manifest = Manifest {
        format_version: 1,
        board: board.into(),
        chip: profile.chip.into(),
        flash_size,
        firmware_version: version.clone(),
        source_revision: version,
        protocol_version: 2,
        idf_version: artifacts::string(&description, "git_revision")?
            .trim_start_matches('v')
            .into(),
        layout_id: images[1].sha256.clone(),
        tool_version: wayline_flash::TOOL_VERSION.into(),
        images,
    };
    let path = output.join("manifest.json");
    artifacts::write_json(&path, &manifest)?;
    wayline_flash::validate_package(&path)
        .context("generated firmware package failed validation")?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("sdkconfig"),
            "CONFIG_OWL_CARDPUTER_UI=y\nCONFIG_OWL_RADIO_LINK_USB_SERIAL_JTAG=y\n",
        )
        .unwrap();
        artifacts::write_json(&root.join("project_description.json"), &json!({"project_name":"owl-radio","project_version":"test-revision-dirty","target":"esp32s3","git_revision":"v6.0.3"})).unwrap();
        artifacts::write_json(&root.join("flasher_args.json"), &json!({"extra_esptool_args":{"chip":"esp32s3"},"flash_settings":{"flash_size":"8MB"},"flash_files":{"0x0":"boot.bin","0x8000":"table.bin","0x10000":"app.bin"},"bootloader":{"offset":"0x0","file":"boot.bin"},"partition-table":{"offset":"0x8000","file":"table.bin"},"app":{"offset":"0x10000","file":"app.bin"}})).unwrap();
        let mut image = [0u8; 64];
        image[0] = 0xe9;
        image[3] = 0x30;
        image[12] = 9;
        fs::write(root.join("boot.bin"), image).unwrap();
        fs::write(root.join("app.bin"), image).unwrap();
        let mut table = vec![0xff; 4096];
        for (index, kind, sub, offset, size) in [
            (0, 1, 2, 0x9000u32, 0x5000u32),
            (1, 0, 0, 0x10000, 0x100000),
        ] {
            let entry = &mut table[index * 32..(index + 1) * 32];
            entry.fill(0);
            entry[..4].copy_from_slice(&[0xaa, 0x50, kind, sub]);
            entry[4..8].copy_from_slice(&offset.to_le_bytes());
            entry[8..12].copy_from_slice(&size.to_le_bytes());
        }
        fs::write(root.join("table.bin"), table).unwrap();
        dir
    }

    #[test]
    fn image_capacity_cannot_be_overridden_by_manifest_metadata() {
        let build = fixture();
        let mut bytes = fs::read(build.path().join("app.bin")).unwrap();
        bytes[3] = 0x40; // 16 MB image inside an otherwise 8 MB build description.
        fs::write(build.path().join("app.bin"), bytes).unwrap();
        let parent = tempfile::tempdir().unwrap();
        let output = parent.path().join("result");
        assert!(
            artifacts::publish(&output, |stage| write(build.path(), "cardputer-adv", stage))
                .is_err()
        );
        assert!(!output.exists());
    }

    #[test]
    fn manifest_preserves_build_provenance_and_passes_runtime_validation() {
        let build = fixture();
        let parent = tempfile::tempdir().unwrap();
        let output = parent.path().join("package with spaces");
        artifacts::publish(&output, |stage| write(build.path(), "cardputer-adv", stage)).unwrap();
        let package = wayline_flash::validate_package(&output.join("manifest.json")).unwrap();
        assert_eq!(package.manifest.source_revision, "test-revision-dirty");
        assert_eq!(package.manifest.images.len(), 3);
        fs::write(output.join("owl-radio.bin"), "changed").unwrap();
        assert!(wayline_flash::validate_package(&output.join("manifest.json")).is_err());
    }

    #[test]
    fn wrong_capacity_and_link_mode_never_publish_a_package() {
        let build = fixture();
        let parent = tempfile::tempdir().unwrap();
        let output = parent.path().join("result");
        let path = build.path().join("flasher_args.json");
        let mut value = artifacts::json(&path).unwrap();
        value["flash_settings"]["flash_size"] = json!("16MB");
        artifacts::write_json(&path, &value).unwrap();
        assert!(
            artifacts::publish(&output, |stage| write(build.path(), "cardputer-adv", stage))
                .is_err()
        );
        assert!(!output.exists());
        value["flash_settings"]["flash_size"] = json!("8MB");
        artifacts::write_json(&path, &value).unwrap();
        fs::write(
            build.path().join("sdkconfig"),
            "CONFIG_OWL_CARDPUTER_UI=y\n",
        )
        .unwrap();
        assert!(
            artifacts::publish(&output, |stage| write(build.path(), "cardputer-adv", stage))
                .is_err()
        );
        assert!(!output.exists());
    }

    #[test]
    fn build_metadata_cannot_escape_its_directory_or_overwrite_existing_output() {
        let build = fixture();
        let parent = tempfile::tempdir().unwrap();
        let output = parent.path().join("result");
        let path = build.path().join("flasher_args.json");
        let mut value = artifacts::json(&path).unwrap();
        value["app"]["file"] = json!("../outside.bin");
        value["flash_files"]["0x10000"] = json!("../outside.bin");
        artifacts::write_json(&path, &value).unwrap();
        assert!(
            artifacts::publish(&output, |stage| write(build.path(), "cardputer-adv", stage))
                .is_err()
        );
        assert!(!output.exists());
        fs::create_dir(&output).unwrap();
        fs::write(output.join("keep"), "untouched").unwrap();
        assert!(artifacts::publish(&output, |_| Ok(())).is_err());
        assert_eq!(
            fs::read_to_string(output.join("keep")).unwrap(),
            "untouched"
        );
    }
}
