use crate::{FileGuard, identity_dir};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

pub const MAX_TRANSFER_HISTORY_RECORDS: usize = 200;
const MAX_TRANSFER_HISTORY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_PEER_NAME_CHARS: usize = 256;

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
    write_to(path, &entries)?;
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
    Ok(())
}

fn write_to(path: &Path, entries: &[TransferHistoryRecord]) -> Result<()> {
    let parent = path.parent().context("无效的传输历史目录")?;
    fs::create_dir_all(parent).context("无法创建传输历史目录")?;
    let history = HistoryFile {
        version: 1,
        entries: entries.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&history)?;
    ensure!(
        bytes.len() as u64 <= MAX_TRANSFER_HISTORY_BYTES,
        "传输历史文件过大"
    );
    let mut temporary = tempfile::NamedTempFile::new_in(parent).context("无法创建历史临时文件")?;
    use std::io::Write;
    temporary.write_all(&bytes)?;
    temporary.write_all(b"\n")?;
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
        }
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
}
