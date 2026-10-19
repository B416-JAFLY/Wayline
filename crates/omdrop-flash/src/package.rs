use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
};

pub const SUPPORTED_BOARD: &str = "cardputer-adv";
pub const TOOL_VERSION: &str = "5.0.2";
pub const FLASH_SIZE: u64 = 8 * 1024 * 1024;
const MANIFEST_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub format_version: u32,
    pub board: String,
    pub chip: String,
    pub flash_size: u64,
    pub firmware_version: String,
    pub source_revision: String,
    pub protocol_version: u8,
    pub idf_version: String,
    pub layout_id: String,
    pub tool_version: String,
    pub images: Vec<Image>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Image {
    pub path: String,
    pub offset: u32,
    pub length: u64,
    pub sha256: String,
    pub kind: ImageKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImageKind {
    Bootloader,
    PartitionTable,
    Application,
}
#[derive(Debug)]
pub struct ValidatedPackage {
    pub manifest: Manifest,
    pub(crate) paths: Vec<PathBuf>,
}

pub fn installed_manifest() -> Result<PathBuf> {
    Ok(installation_root()?.join("firmware/manifest.json"))
}
pub fn installed_tool() -> Result<PathBuf> {
    Ok(installation_root()?.join("tools/esptool/esptool.exe"))
}
fn installation_root() -> Result<PathBuf> {
    Ok(std::env::current_exe()?
        .parent()
        .context("应用程序目录不可用")?
        .to_path_buf())
}

pub fn validate_package(path: &Path) -> Result<ValidatedPackage> {
    let mut bytes = Vec::new();
    File::open(path)
        .context("无法打开固件包清单")?
        .take(MANIFEST_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MANIFEST_LIMIT, "固件包清单过大");
    let manifest: Manifest = serde_json::from_slice(&bytes).context("固件包清单格式不正确")?;
    ensure!(manifest.format_version == 1, "不支持的固件包格式版本");
    ensure!(
        manifest.board == SUPPORTED_BOARD
            && manifest.chip == "esp32s3"
            && manifest.flash_size == FLASH_SIZE,
        "仅支持 Cardputer Adv / ESP32-S3 / 8 MiB Flash"
    );
    ensure!(
        manifest.tool_version == TOOL_VERSION,
        "固件包要求的烧录工具版本不匹配"
    );
    ensure!(manifest.protocol_version == 2, "不支持的无线协议版本");
    for value in [
        &manifest.firmware_version,
        &manifest.source_revision,
        &manifest.idf_version,
        &manifest.layout_id,
    ] {
        ensure!(
            !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control),
            "固件包版本字段无效"
        );
    }
    ensure!(
        manifest.images.len() == 3,
        "固件包必须包含且仅包含 bootloader、分区表和应用"
    );
    let root = fs::canonicalize(path.parent().context("无效的固件包路径")?)?;
    let mut paths = Vec::new();
    let mut ranges = Vec::new();
    let mut kinds = HashSet::new();
    let mut seen_paths = HashSet::new();
    for image in &manifest.images {
        ensure!(kinds.insert(image.kind), "固件镜像类型重复");
        let relative = Path::new(&image.path);
        ensure!(
            !image.path.is_empty()
                && image.path.len() <= 240
                && !image.path.contains([':', '\\'])
                && relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
            "镜像路径必须是安全的相对路径"
        );
        let full = fs::canonicalize(root.join(relative))
            .with_context(|| format!("找不到固件镜像 {}", image.path))?;
        ensure!(
            full.starts_with(&root) && seen_paths.insert(full.clone()),
            "固件镜像路径越界或重复"
        );
        ensure!(
            image.length > 0 && image.length <= FLASH_SIZE,
            "镜像长度无效"
        );
        let end = u64::from(image.offset)
            .checked_add(image.length)
            .context("镜像地址溢出")?;
        ensure!(
            end <= FLASH_SIZE && image.offset % 4096 == 0,
            "镜像地址超出 Flash 或未按扇区对齐"
        );
        // esptool erases whole 4 KiB sectors. Include the padded final sector in overlap checks.
        ranges.push((u64::from(image.offset), end.div_ceil(4096) * 4096));
        let metadata = fs::metadata(&full)?;
        ensure!(
            metadata.is_file() && metadata.len() == image.length,
            "镜像长度与清单不符：{}",
            image.path
        );
        ensure!(
            image.sha256.len() == 64 && image.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "镜像 SHA-256 格式无效"
        );
        ensure!(
            hash_file(&full)? == image.sha256.to_ascii_lowercase(),
            "镜像校验失败：{}",
            image.path
        );
        if image.kind != ImageKind::PartitionTable {
            let mut header = [0u8; 24];
            File::open(&full)?.read_exact(&mut header)?;
            ensure!(
                header[0] == 0xe9 && u16::from_le_bytes([header[12], header[13]]) == 9,
                "镜像不是 ESP32-S3 可执行映像"
            );
        }
        paths.push(full);
    }
    ranges.sort_unstable();
    ensure!(
        ranges.windows(2).all(|r| r[0].1 <= r[1].0),
        "固件镜像地址或擦除扇区重叠"
    );
    let package = ValidatedPackage { manifest, paths };
    validate_layout(&package)?;
    Ok(package)
}

fn validate_layout(package: &ValidatedPackage) -> Result<()> {
    let find = |kind| {
        package
            .manifest
            .images
            .iter()
            .position(|i| i.kind == kind)
            .unwrap()
    };
    let boot = &package.manifest.images[find(ImageKind::Bootloader)];
    let table_index = find(ImageKind::PartitionTable);
    let table = &package.manifest.images[table_index];
    let application = &package.manifest.images[find(ImageKind::Application)];
    ensure!(
        boot.offset == 0
            && table.offset == 0x8000
            && table.length <= 4096
            && application.offset == 0x10000,
        "不支持的 Cardputer Adv 启动布局"
    );
    let bytes = fs::read(&package.paths[table_index])?;
    let mut partitions = Vec::new();
    let mut app_ok = false;
    let mut nvs_ok = false;
    for record in bytes.as_chunks::<32>().0 {
        if record[0..2] != [0xaa, 0x50] {
            break;
        }
        let offset = u32::from_le_bytes(record[4..8].try_into().unwrap()) as u64;
        let size = u32::from_le_bytes(record[8..12].try_into().unwrap()) as u64;
        let end = offset.checked_add(size).context("分区范围溢出")?;
        ensure!(
            size > 0
                && offset >= 0x9000
                && end <= FLASH_SIZE
                && offset.is_multiple_of(4096)
                && size.is_multiple_of(4096),
            "分区范围无效"
        );
        partitions.push((offset, end));
        if record[2] == 0 && offset == u64::from(application.offset) && application.length <= size {
            app_ok = true;
        }
        if record[2] == 1 && record[3] == 2 {
            nvs_ok = true;
        }
        if record[2] != 0 {
            for image in &package.manifest.images {
                let image_start = u64::from(image.offset);
                let image_end = (image_start + image.length).div_ceil(4096) * 4096;
                ensure!(
                    image_end <= offset || image_start >= end,
                    "固件镜像会擦除持久化数据分区"
                );
            }
        }
    }
    partitions.sort_unstable();
    ensure!(
        partitions.windows(2).all(|r| r[0].1 <= r[1].0),
        "固件分区表包含重叠分区"
    );
    ensure!(app_ok && nvs_ok, "固件分区表缺少匹配的应用或 NVS 分区");
    Ok(())
}

pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(total <= FLASH_SIZE, "镜像超过最大允许长度");
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn fixture() -> (tempfile::TempDir, PathBuf, Manifest) {
        let dir = tempfile::tempdir().unwrap();
        let mut boot = vec![0; 4096];
        boot[0] = 0xe9;
        boot[12] = 9;
        let mut app = vec![0; 8192];
        app[0] = 0xe9;
        app[12] = 9;
        let mut table = vec![0xff; 3072];
        for (slot, kind, subtype, offset, length, name) in [
            (0, 1, 2, 0x9000u32, 0x6000u32, "nvs"),
            (1, 1, 1, 0xf000, 0x1000, "phy_init"),
            (2, 0, 0, 0x10000, 0x7f0000, "factory"),
        ] {
            let record = &mut table[slot * 32..slot * 32 + 32];
            record.fill(0);
            record[..4].copy_from_slice(&[0xaa, 0x50, kind, subtype]);
            record[4..8].copy_from_slice(&offset.to_le_bytes());
            record[8..12].copy_from_slice(&length.to_le_bytes());
            record[12..12 + name.len()].copy_from_slice(name.as_bytes());
        }
        let mut images = Vec::new();
        for (name, offset, kind, bytes) in [
            ("bootloader.bin", 0, ImageKind::Bootloader, boot),
            (
                "partition-table.bin",
                0x8000,
                ImageKind::PartitionTable,
                table,
            ),
            ("owl-radio.bin", 0x10000, ImageKind::Application, app),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, &bytes).unwrap();
            images.push(Image {
                path: name.into(),
                offset,
                length: bytes.len() as u64,
                sha256: hash_file(&path).unwrap(),
                kind,
            });
        }
        let manifest = Manifest {
            format_version: 1,
            board: SUPPORTED_BOARD.into(),
            chip: "esp32s3".into(),
            flash_size: FLASH_SIZE,
            firmware_version: "0.1.0-test".into(),
            source_revision: "a1b2c3d".into(),
            protocol_version: 2,
            idf_version: "6.0".into(),
            layout_id: images[1].sha256.clone(),
            tool_version: TOOL_VERSION.into(),
            images,
        };
        let path = dir.path().join("manifest.json");
        save(&path, &manifest);
        (dir, path, manifest)
    }
    fn save(path: &Path, manifest: &Manifest) {
        fs::write(path, serde_json::to_vec(manifest).unwrap()).unwrap();
    }
    #[test]
    fn complete_package_keeps_three_images_outside_nvs() {
        let (_dir, path, _) = fixture();
        let package = validate_package(&path).unwrap();
        assert_eq!(package.paths.len(), 3);
        assert_eq!(package.manifest.flash_size, FLASH_SIZE);
    }
    #[test]
    fn rejects_path_escape_absolute_duplicate_and_unknown_board() {
        let (_dir, path, original) = fixture();
        for bad in [
            "../bootloader.bin",
            "C:/bootloader.bin",
            "/bootloader.bin",
            "a/../../bootloader.bin",
            "a\\bootloader.bin",
            "",
        ] {
            let mut manifest = original.clone();
            manifest.images[0].path = bad.into();
            save(&path, &manifest);
            assert!(validate_package(&path).is_err(), "{bad}");
        }
        let mut manifest = original.clone();
        manifest.images[1].path = manifest.images[0].path.clone();
        save(&path, &manifest);
        assert!(validate_package(&path).is_err());
        let mut manifest = original;
        manifest.board = "esp32-s3-devkit".into();
        save(&path, &manifest);
        assert!(validate_package(&path).is_err());
    }
    #[test]
    fn rejects_tampering_truncation_and_capacity_errors() {
        let (dir, path, original) = fixture();
        let boot = dir.path().join(&original.images[0].path);
        let bytes = fs::read(&boot).unwrap();
        fs::write(&boot, &bytes[..bytes.len() - 1]).unwrap();
        assert!(validate_package(&path).is_err());
        let mut altered = bytes.clone();
        altered[20] ^= 1;
        fs::write(&boot, &altered).unwrap();
        assert!(validate_package(&path).is_err());
        fs::write(&boot, bytes).unwrap();
        let mut manifest = original.clone();
        manifest.images[2].offset = FLASH_SIZE as u32;
        save(&path, &manifest);
        assert!(validate_package(&path).is_err());
        let mut manifest = original.clone();
        manifest.images[2].offset = 0x8000;
        save(&path, &manifest);
        assert!(validate_package(&path).is_err());
        let mut manifest = original;
        manifest.flash_size = 4 * 1024 * 1024;
        save(&path, &manifest);
        assert!(validate_package(&path).is_err());
    }
    #[test]
    fn rejects_unsupported_chip_and_persistent_partition_overlap() {
        let (dir, path, mut manifest) = fixture();
        let app = dir.path().join(&manifest.images[2].path);
        let mut bytes = fs::read(&app).unwrap();
        bytes[12] = 0;
        fs::write(&app, &bytes).unwrap();
        manifest.images[2].sha256 = hash_file(&app).unwrap();
        save(&path, &manifest);
        assert!(validate_package(&path).is_err());
        bytes[12] = 9;
        fs::write(&app, &bytes).unwrap();
        manifest.images[2].sha256 = hash_file(&app).unwrap();
        let table = dir.path().join(&manifest.images[1].path);
        let mut bytes = fs::read(&table).unwrap();
        bytes[4..8].copy_from_slice(&0x10000u32.to_le_bytes());
        fs::write(&table, bytes).unwrap();
        manifest.images[1].sha256 = hash_file(&table).unwrap();
        save(&path, &manifest);
        assert!(validate_package(&path).is_err());
    }
    #[test]
    fn rejects_oversize_manifest_without_reading_entire_input() {
        let (_dir, path, _) = fixture();
        fs::write(&path, vec![b' '; MANIFEST_LIMIT as usize + 1]).unwrap();
        assert!(
            validate_package(&path)
                .unwrap_err()
                .to_string()
                .contains("过大")
        );
    }
}
