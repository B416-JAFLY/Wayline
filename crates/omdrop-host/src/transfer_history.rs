use crate::{FileGuard, identity_dir};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
};

pub const MAX_TRANSFER_HISTORY_RECORDS: usize = 200;
const MAX_TRANSFER_HISTORY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PEER_NAME_CHARS: usize = 256;
const MAX_HISTORY_FILES: usize = 4096;
const MAX_HISTORY_PATH_CHARS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    Send,
    Receive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferOutcome {
    Completed,
    Declined,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferFailureReason {
    Connection,
    Validation,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferHistoryRecord {
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub direction: TransferDirection,
    pub peer: String,
    pub file_count: usize,
    pub file_bytes: Option<u64>,
    pub transferred_bytes: u64,
    pub duration_ms: u64,
    pub outcome: TransferOutcome,
    pub failure_reason: Option<TransferFailureReason>,
    pub destination: Option<PathBuf>,
    #[serde(default)]
    pub file_names: Vec<String>,
    #[serde(default)]
    pub file_paths: Vec<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryFile {
    version: u32,
    entries: Vec<TransferHistoryRecord>,
}

pub fn load_transfer_history() -> Result<Vec<TransferHistoryRecord>> {
    load_from(&transfer_history_path()?)
}

pub fn append_transfer_history(
    record: TransferHistoryRecord,
) -> Result<Vec<TransferHistoryRecord>> {
    let path = transfer_history_path()?;
    let _guard = FileGuard::acquire(&path.with_extension("lock"))?;
    append_to(&path, record)
}

pub fn clear_transfer_history() -> Result<()> {
    let path = transfer_history_path()?;
    let _guard = FileGuard::acquire(&path.with_extension("lock"))?;
    write_to(&path, &[])
}

/// Delete only records in the caller's snapshot, leaving concurrently appended transfers intact.
/// File removal is opt-in and never recurses into directories or follows reparse points.
pub fn delete_transfer_history(
    records: &[TransferHistoryRecord],
    delete_files: bool,
) -> Result<Vec<TransferHistoryRecord>> {
    let path = transfer_history_path()?;
    let _guard = FileGuard::acquire(&path.with_extension("lock"))?;
    delete_from(&path, records, delete_files)
}

fn delete_from(
    path: &Path,
    records: &[TransferHistoryRecord],
    delete_files: bool,
) -> Result<Vec<TransferHistoryRecord>> {
    let mut entries = load_from(path)?;
    let mut selected = vec![false; entries.len()];
    for record in records {
        if let Some(index) = entries
            .iter()
            .enumerate()
            .position(|(index, entry)| !selected[index] && entry == record)
        {
            selected[index] = true;
        }
    }
    if delete_files {
        let mut files = Vec::new();
        for record in entries
            .iter()
            .zip(&selected)
            .filter_map(|(record, selected)| selected.then_some(record))
        {
            for file in &record.file_paths {
                if validate_deletion_target(record, file)? && !files.contains(file) {
                    files.push(file.clone());
                }
            }
        }
        // Check the entire selection before making changes. If deletion later fails, the
        // original history remains available; missing files are accepted on a retry.
        for file in files {
            match fs::remove_file(&file) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("无法删除传输文件 {}", file.display()));
                }
            }
        }
    }
    let mut selected = selected.into_iter();
    entries.retain(|_| {
        !selected
            .next()
            .expect("one selection flag per history entry")
    });
    write_to(path, &entries)?;
    Ok(entries)
}

fn validate_deletion_target(record: &TransferHistoryRecord, file: &Path) -> Result<bool> {
    validate_file_path(file)?;
    // Inspect every component, including directories, so an intermediate link cannot
    // redirect a recorded path outside its original location.
    for ancestor in file.ancestors() {
        let metadata = match fs::symlink_metadata(ancestor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("无法检查传输文件 {}", ancestor.display()));
            }
        };
        ensure!(
            !is_link(&metadata),
            "不能删除符号链接或重解析点中的传输文件：{}",
            ancestor.display()
        );
        if ancestor == file {
            ensure!(
                metadata.is_file(),
                "传输记录中的路径不是普通文件：{}",
                file.display()
            );
        }
    }
    if record.direction == TransferDirection::Receive {
        let destination = record
            .destination
            .as_ref()
            .context("接收记录缺少保存目录")?;
        let resolved_destination = fs::canonicalize(destination)
            .with_context(|| format!("无法检查接收目录 {}", destination.display()))?;
        let resolved_file = fs::canonicalize(file)
            .with_context(|| format!("无法检查接收文件 {}", file.display()))?;
        ensure!(
            resolved_file.starts_with(&resolved_destination)
                && resolved_file != resolved_destination,
            "传输文件不在记录的接收目录内：{}",
            file.display()
        );
    }
    Ok(true)
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn validate_file_path(path: &Path) -> Result<()> {
    ensure!(path.is_absolute(), "传输历史文件路径必须是绝对路径");
    ensure!(
        !path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir)),
        "传输历史文件路径包含相对目录"
    );
    #[cfg(windows)]
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                use std::path::Prefix;
                ensure!(
                    matches!(
                        prefix.kind(),
                        Prefix::Disk(_)
                            | Prefix::VerbatimDisk(_)
                            | Prefix::UNC(_, _)
                            | Prefix::VerbatimUNC(_, _)
                    ),
                    "传输历史文件路径不能是设备路径"
                );
            }
            Component::Normal(name) => {
                ensure!(
                    !name.to_string_lossy().contains(':'),
                    "传输历史文件路径不能包含备用数据流"
                );
            }
            _ => {}
        }
    }
    let text = path.to_str().context("传输历史文件路径不是有效文本")?;
    ensure!(
        text.chars().count() <= MAX_HISTORY_PATH_CHARS,
        "传输历史文件路径过长"
    );
    ensure!(
        !text.chars().any(char::is_control),
        "传输历史文件路径包含控制字符"
    );
    Ok(())
}

fn transfer_history_path() -> Result<PathBuf> {
    Ok(identity_dir()?.join("transfer-history.json"))
}

fn load_from(path: &Path) -> Result<Vec<TransferHistoryRecord>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("无法读取传输历史"),
    };
    ensure!(
        file.metadata()?.len() <= MAX_TRANSFER_HISTORY_BYTES,
        "传输历史文件过大"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_TRANSFER_HISTORY_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_TRANSFER_HISTORY_BYTES,
        "传输历史文件过大"
    );
    let history: HistoryFile = serde_json::from_slice(&bytes).context("传输历史格式不正确")?;
    ensure!(history.version == 1, "不支持的传输历史版本");
    ensure!(
        history.entries.len() <= MAX_TRANSFER_HISTORY_RECORDS,
        "传输历史记录数量超出上限"
    );
    for record in &history.entries {
        validate_record(record)?;
    }
    Ok(history.entries)
}

fn append_to(path: &Path, record: TransferHistoryRecord) -> Result<Vec<TransferHistoryRecord>> {
    validate_record(&record)?;
    let mut entries = load_from(path)?;
    entries.insert(0, record);
    entries.truncate(MAX_TRANSFER_HISTORY_RECORDS);
    let mut bytes = encode_history(&entries)?;
    while bytes.len() as u64 > MAX_TRANSFER_HISTORY_BYTES {
        ensure!(entries.len() > 1, "单条传输历史记录过大");
        entries.pop();
        bytes = encode_history(&entries)?;
    }
    write_bytes_to(path, &bytes)?;
    Ok(entries)
}

fn validate_record(record: &TransferHistoryRecord) -> Result<()> {
    ensure!(!record.peer.trim().is_empty(), "传输历史缺少设备名称");
    ensure!(
        !record.peer.chars().any(char::is_control),
        "传输历史设备名称包含控制字符"
    );
    ensure!(
        record.peer.chars().count() <= MAX_PEER_NAME_CHARS,
        "传输历史设备名称过长"
    );
    ensure!(
        record.finished_at_unix_ms >= record.started_at_unix_ms,
        "传输历史时间范围无效"
    );
    ensure!(
        (record.outcome == TransferOutcome::Failed) == record.failure_reason.is_some(),
        "传输历史结果与失败原因不匹配"
    );
    ensure!(
        record.direction == TransferDirection::Receive || record.destination.is_none(),
        "发送记录不能包含接收目录"
    );
    if let Some(destination) = &record.destination {
        ensure!(destination.is_absolute(), "传输历史保存目录必须是绝对路径");
        let text = destination.to_string_lossy();
        ensure!(text.chars().count() <= 4096, "传输历史保存目录过长");
        ensure!(
            !text.chars().any(char::is_control),
            "传输历史保存目录包含控制字符"
        );
    }
    ensure!(
        record.file_names.len() <= MAX_HISTORY_FILES,
        "传输历史文件名称数量超出上限"
    );
    ensure!(
        record.file_paths.len() <= MAX_HISTORY_FILES,
        "传输历史文件路径数量超出上限"
    );
    for name in &record.file_names {
        ensure!(!name.trim().is_empty(), "传输历史文件名称为空");
        ensure!(
            name.chars().count() <= MAX_HISTORY_PATH_CHARS,
            "传输历史文件名称过长"
        );
        ensure!(
            !name.chars().any(char::is_control),
            "传输历史文件名称包含控制字符"
        );
    }
    for path in &record.file_paths {
        validate_file_path(path)?;
        if record.direction == TransferDirection::Receive {
            ensure!(
                record.outcome == TransferOutcome::Completed,
                "未完成的接收记录不能包含文件路径"
            );
            let destination = record
                .destination
                .as_ref()
                .context("接收记录缺少保存目录")?;
            ensure!(
                path.starts_with(destination) && path != destination,
                "传输历史文件不在接收目录内"
            );
        }
    }
    Ok(())
}

fn write_to(path: &Path, entries: &[TransferHistoryRecord]) -> Result<()> {
    let bytes = encode_history(entries)?;
    write_bytes_to(path, &bytes)
}

fn encode_history(entries: &[TransferHistoryRecord]) -> Result<Vec<u8>> {
    let history = HistoryFile {
        version: 1,
        entries: entries.to_vec(),
    };
    let mut bytes = serde_json::to_vec_pretty(&history)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn write_bytes_to(path: &Path, bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() as u64 <= MAX_TRANSFER_HISTORY_BYTES,
        "传输历史文件过大"
    );
    let parent = path.parent().context("无效的传输历史目录")?;
    fs::create_dir_all(parent).context("无法创建传输历史目录")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).context("无法创建历史临时文件")?;
    use std::io::Write;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).context("无法原子保存传输历史")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(index: usize) -> TransferHistoryRecord {
        TransferHistoryRecord {
            started_at_unix_ms: index as u64,
            finished_at_unix_ms: index as u64 + 10,
            direction: TransferDirection::Send,
            peer: "iPhone".into(),
            file_count: 2,
            file_bytes: Some(2048),
            transferred_bytes: 1024,
            duration_ms: 10,
            outcome: TransferOutcome::Completed,
            failure_reason: None,
            destination: None,
            file_names: Vec::new(),
            file_paths: Vec::new(),
        }
    }

    fn record_with_encoded_size(index: usize, bytes: usize) -> TransferHistoryRecord {
        let mut entry = record(index);
        entry.file_names = vec!["x".into(); 1024];
        let mut padding = bytes - encode_history(std::slice::from_ref(&entry)).unwrap().len();
        for name in &mut entry.file_names {
            let added = padding.min(MAX_HISTORY_PATH_CHARS - name.len());
            name.push_str(&"x".repeat(added));
            padding -= added;
        }
        assert_eq!(padding, 0);
        validate_record(&entry).unwrap();
        assert_eq!(
            encode_history(std::slice::from_ref(&entry)).unwrap().len(),
            bytes
        );
        entry
    }

    #[test]
    fn appending_at_the_byte_limit_evicts_the_oldest_complete_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        let older = record_with_encoded_size(1, 2 * 1024 * 1024);
        let newer = record_with_encoded_size(2, 2 * 1024 * 1024);
        write_to(&path, &[newer.clone(), older]).unwrap();
        let newest = record(3);
        assert_eq!(
            append_to(&path, newest.clone()).unwrap(),
            vec![newest.clone(), newer.clone()]
        );
        assert_eq!(load_from(&path).unwrap(), vec![newest, newer]);
        assert!(fs::metadata(&path).unwrap().len() <= MAX_TRANSFER_HISTORY_BYTES);
    }

    #[test]
    fn byte_limit_includes_the_final_newline_and_oversized_records_preserve_history() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        let at_limit = record_with_encoded_size(1, MAX_TRANSFER_HISTORY_BYTES as usize);
        write_to(&path, std::slice::from_ref(&at_limit)).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().len(),
            MAX_TRANSFER_HISTORY_BYTES
        );
        assert_eq!(load_from(&path).unwrap(), vec![at_limit]);
        let before = fs::read(&path).unwrap();
        let too_large = record_with_encoded_size(2, MAX_TRANSFER_HISTORY_BYTES as usize + 1);
        assert!(write_to(&path, std::slice::from_ref(&too_large)).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(
            append_to(&path, too_large)
                .unwrap_err()
                .to_string()
                .contains("单条")
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        let mut invalid = before;
        invalid.push(b' ');
        fs::write(&path, invalid).unwrap();
        assert!(load_from(&path).is_err());
    }

    #[test]
    fn missing_history_is_empty_and_appends_keep_the_newest_records_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        assert!(load_from(&path).unwrap().is_empty());

        for index in 0..=MAX_TRANSFER_HISTORY_RECORDS {
            append_to(&path, record(index)).unwrap();
        }
        let entries = load_from(&path).unwrap();
        assert_eq!(entries.len(), MAX_TRANSFER_HISTORY_RECORDS);
        assert_eq!(
            entries[0].started_at_unix_ms,
            MAX_TRANSFER_HISTORY_RECORDS as u64
        );
        assert_eq!(
            entries.last().unwrap().started_at_unix_ms,
            1,
            "the oldest item is evicted"
        );
    }

    #[test]
    fn malformed_or_oversized_history_is_not_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        fs::write(&path, b"not json").unwrap();
        assert!(load_from(&path).is_err());
        assert!(append_to(&path, record(1)).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"not json");
    }

    #[test]
    fn clearing_history_persists_an_empty_versioned_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        append_to(&path, record(1)).unwrap();
        write_to(&path, &[]).unwrap();
        assert!(load_from(&path).unwrap().is_empty());
    }

    #[test]
    fn record_validation_rejects_invalid_paths_and_outcomes() {
        let mut invalid = record(1);
        invalid.direction = TransferDirection::Receive;
        invalid.destination = Some(PathBuf::from("folder\nname"));
        assert!(validate_record(&invalid).is_err());
        invalid.destination = Some(PathBuf::from("folder"));
        invalid.failure_reason = Some(TransferFailureReason::Other);
        assert!(validate_record(&invalid).is_err());
    }

    #[test]
    fn old_history_without_file_details_remains_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        let mut value = serde_json::to_value(record(1)).unwrap();
        value.as_object_mut().unwrap().remove("fileNames");
        value.as_object_mut().unwrap().remove("filePaths");
        fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"version": 1, "entries": [value]})).unwrap(),
        )
        .unwrap();
        assert_eq!(load_from(&path).unwrap(), vec![record(1)]);
    }

    #[test]
    fn deletion_matches_snapshot_and_keeps_files_unless_requested() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        let file = dir.path().join("photo.jpg");
        fs::write(&file, b"photo").unwrap();
        let mut selected = record(1);
        selected.file_names = vec!["photo.jpg".into()];
        selected.file_paths = vec![file.clone()];
        append_to(&path, selected.clone()).unwrap();
        let concurrent = record(2);
        append_to(&path, concurrent.clone()).unwrap();
        assert_eq!(
            delete_from(&path, &[selected], false).unwrap(),
            vec![concurrent]
        );
        assert_eq!(fs::read(file).unwrap(), b"photo");
    }

    #[test]
    fn stale_snapshot_cannot_delete_a_different_records_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        let file = dir.path().join("photo.jpg");
        fs::write(&file, b"photo").unwrap();
        let original = record(1);
        let mut changed = original.clone();
        changed.file_paths = vec![file.clone()];
        append_to(&path, changed.clone()).unwrap();
        assert_eq!(
            delete_from(&path, &[original], true).unwrap(),
            vec![changed]
        );
        assert_eq!(fs::read(file).unwrap(), b"photo");
    }

    #[test]
    fn deleting_one_duplicate_record_keeps_the_other_occurrences() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        let duplicate = record(1);
        for _ in 0..3 {
            append_to(&path, duplicate.clone()).unwrap();
        }
        assert_eq!(
            delete_from(&path, std::slice::from_ref(&duplicate), false).unwrap(),
            vec![duplicate.clone(), duplicate.clone()]
        );
        assert_eq!(load_from(&path).unwrap().len(), 2);
        assert!(
            delete_from(&path, &[duplicate.clone(), duplicate], false)
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(windows)]
    #[test]
    fn file_removal_failure_retains_history_and_can_be_retried() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        let file = dir.path().join("photo.jpg");
        fs::write(&file, b"photo").unwrap();
        let held_file = fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(&file)
            .unwrap();
        let mut selected = record(1);
        selected.file_paths = vec![file.clone()];
        append_to(&path, selected.clone()).unwrap();
        assert!(delete_from(&path, std::slice::from_ref(&selected), true).is_err());
        assert_eq!(load_from(&path).unwrap(), vec![selected.clone()]);
        assert!(file.exists());
        drop(held_file);
        assert!(delete_from(&path, &[selected], true).unwrap().is_empty());
        assert!(!file.exists());
    }

    #[test]
    fn deletion_removes_only_explicit_files_and_accepts_missing_files_on_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        let destination = dir.path().join("received");
        fs::create_dir(&destination).unwrap();
        let file = destination.join("photo.jpg");
        let retained = destination.join("unrelated.jpg");
        fs::write(&file, b"photo").unwrap();
        fs::write(&retained, b"unrelated").unwrap();
        let mut selected = record(1);
        selected.direction = TransferDirection::Receive;
        selected.destination = Some(destination.clone());
        selected.file_paths = vec![file.clone(), destination.join("already-missing.jpg")];
        append_to(&path, selected.clone()).unwrap();
        assert!(delete_from(&path, &[selected], true).unwrap().is_empty());
        assert!(!file.exists());
        assert_eq!(fs::read(retained).unwrap(), b"unrelated");
        assert!(destination.is_dir());
    }

    #[test]
    fn invalid_file_selection_keeps_history_and_prevalidates_every_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer-history.json");
        let file = dir.path().join("photo.jpg");
        let folder = dir.path().join("folder");
        fs::write(&file, b"photo").unwrap();
        fs::create_dir(&folder).unwrap();
        let mut selected = record(1);
        selected.file_paths = vec![file.clone(), folder.clone()];
        append_to(&path, selected.clone()).unwrap();
        assert!(delete_from(&path, std::slice::from_ref(&selected), true).is_err());
        assert_eq!(load_from(&path).unwrap(), vec![selected]);
        assert!(file.exists());
        assert!(folder.is_dir());
    }

    #[test]
    fn receive_file_paths_cannot_escape_the_destination_or_represent_unfinished_receives() {
        let dir = tempfile::tempdir().unwrap();
        let mut selected = record(1);
        selected.direction = TransferDirection::Receive;
        selected.destination = Some(dir.path().join("received"));
        selected.file_paths = vec![dir.path().join("outside.jpg")];
        assert!(validate_record(&selected).is_err());
        selected.file_paths = vec![dir.path().join("received").join("..").join("outside.jpg")];
        assert!(validate_record(&selected).is_err());
        selected.file_paths = vec![dir.path().join("received").join("inside.jpg")];
        assert!(validate_record(&selected).is_ok());
        selected.outcome = TransferOutcome::Cancelled;
        assert!(validate_record(&selected).is_err());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn deleting_through_a_symbolic_link_is_rejected_and_preserves_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("received");
        let outside = dir.path().join("outside");
        fs::create_dir(&destination).unwrap();
        fs::create_dir(&outside).unwrap();
        let file = outside.join("photo.jpg");
        fs::write(&file, b"photo").unwrap();
        let link = destination.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&outside, &link).unwrap();
        let mut selected = record(1);
        selected.direction = TransferDirection::Receive;
        selected.destination = Some(destination);
        selected.file_paths = vec![link.join("photo.jpg")];
        let path = dir.path().join("transfer-history.json");
        append_to(&path, selected.clone()).unwrap();
        assert!(delete_from(&path, std::slice::from_ref(&selected), true).is_err());
        assert_eq!(load_from(&path).unwrap(), vec![selected]);
        assert_eq!(fs::read(file).unwrap(), b"photo");
    }
}
