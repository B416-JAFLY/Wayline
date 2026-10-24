use crate::backend::Event;
use std::{
    path::PathBuf,
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};
use wayline_host::{
    TransferHistoryRecord, append_transfer_history, delete_transfer_history, load_transfer_history,
};

enum Command {
    Append(TransferHistoryRecord, Vec<PathBuf>),
    Delete(Vec<TransferHistoryRecord>, bool, bool),
    SaveDeletePreference(bool),
    Flush(Sender<()>),
}

pub struct StoreWorker(Sender<Command>);

impl StoreWorker {
    pub fn start(events: Sender<Event>) -> (Self, JoinHandle<()>) {
        let (commands, rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("wayline-history".into())
            .spawn(move || {
                let loaded = load_transfer_history().map_err(|error| format!("{error:#}"));
                let _ = events.send(Event::HistoryLoaded(loaded));
                for command in rx {
                    match command {
                        Command::Append(mut record, received_files) => {
                            if let Some(bytes) = received_file_bytes(&received_files) {
                                record.file_bytes = Some(bytes);
                            }
                            let result = append_transfer_history(record)
                                .map_err(|error| format!("{error:#}"));
                            let _ = events.send(Event::HistoryUpdated(result));
                        }
                        Command::Delete(records, delete_files, remember) => {
                            if remember {
                                let result = wayline_host::save_history_delete_files(delete_files)
                                    .map_err(|error| format!("{error:#}"));
                                let _ = events.send(Event::HistoryDeletePreferenceSaved(result));
                            }
                            let result = delete_transfer_history(&records, delete_files)
                                .map_err(|error| format!("{error:#}"));
                            let _ = events.send(Event::HistoryDeleted(result));
                        }
                        Command::SaveDeletePreference(value) => {
                            let result = wayline_host::save_history_delete_files(value)
                                .map_err(|error| format!("{error:#}"));
                            let _ = events.send(Event::HistoryDeletePreferenceSaved(result));
                        }
                        Command::Flush(complete) => {
                            let _ = complete.send(());
                        }
                    }
                }
            })
            .expect("spawn transfer history worker");
        (Self(commands), worker)
    }

    pub fn append(&self, record: TransferHistoryRecord, received_files: Vec<PathBuf>) {
        let _ = self.0.send(Command::Append(record, received_files));
    }

    pub fn delete(&self, records: Vec<TransferHistoryRecord>, delete_files: bool, remember: bool) {
        let _ = self
            .0
            .send(Command::Delete(records, delete_files, remember));
    }

    pub fn save_delete_preference(&self, value: bool) {
        let _ = self.0.send(Command::SaveDeletePreference(value));
    }

    pub fn flush_handle(&self) -> JoinHandle<()> {
        let (complete, wait) = mpsc::channel();
        let _ = self.0.send(Command::Flush(complete));
        thread::spawn(move || {
            let _ = wait.recv();
        })
    }
}

fn received_file_bytes(paths: &[PathBuf]) -> Option<u64> {
    if paths.is_empty() {
        return None;
    }
    paths.iter().try_fold(0u64, |total, path| {
        let metadata = std::fs::symlink_metadata(path).ok()?;
        // Received paths describe top-level items. Directory metadata length is
        // not its content size, so preserve the offered total for directory transfers.
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return None;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return None;
            }
        }
        total.checked_add(metadata.len())
    })
}

pub fn unix_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub fn average_bytes_per_second(record: &TransferHistoryRecord) -> Option<u64> {
    (record.duration_ms > 0).then(|| {
        (u128::from(record.transferred_bytes) * 1000 / u128::from(record.duration_ms))
            .min(u128::from(u64::MAX)) as u64
    })
}

pub fn format_timestamp_utc(timestamp_ms: u64) -> String {
    let seconds = timestamp_ms / 1000;
    let days = (seconds / 86_400).min((i64::MAX - 719_468) as u64) as i64;
    let seconds_in_day = seconds % 86_400;
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = seconds_in_day / 3600;
    let minute = seconds_in_day % 3600 / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(transferred_bytes: u64, duration_ms: u64) -> TransferHistoryRecord {
        TransferHistoryRecord {
            started_at_unix_ms: 0,
            finished_at_unix_ms: duration_ms,
            direction: wayline_host::TransferDirection::Receive,
            peer: "iPhone".into(),
            file_count: 1,
            file_bytes: Some(transferred_bytes),
            transferred_bytes,
            duration_ms,
            outcome: wayline_host::TransferOutcome::Completed,
            failure_reason: None,
            destination: None,
            channel: None,
            format: None,
            file_names: Vec::new(),
            file_paths: Vec::new(),
        }
    }

    #[test]
    fn received_sizes_only_replace_offered_totals_for_plain_files() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        std::fs::write(&first, b"123").unwrap();
        std::fs::write(&second, b"45678").unwrap();
        assert_eq!(received_file_bytes(&[first.clone(), second]), Some(8));
        let folder = dir.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("nested.txt"), b"nested content").unwrap();
        assert_eq!(received_file_bytes(&[first.clone(), folder]), None);
        assert_eq!(
            received_file_bytes(&[first, dir.path().join("missing.txt")]),
            None
        );
        assert_eq!(received_file_bytes(&[]), None);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn received_sizes_do_not_follow_symbolic_links() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("original.txt");
        let link = dir.path().join("linked.txt");
        std::fs::write(&file, b"file content").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&file, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&file, &link).unwrap();
        assert_eq!(received_file_bytes(&[link]), None);
    }

    #[test]
    fn average_speed_handles_zero_duration_fractional_seconds_and_overflow() {
        assert_eq!(average_bytes_per_second(&record(1024, 0)), None);
        assert_eq!(average_bytes_per_second(&record(0, 1000)), Some(0));
        assert_eq!(average_bytes_per_second(&record(3000, 1500)), Some(2000));
        assert_eq!(average_bytes_per_second(&record(1, 1500)), Some(0));
        assert_eq!(
            average_bytes_per_second(&record(u64::MAX, 1)),
            Some(u64::MAX)
        );
        assert_eq!(
            average_bytes_per_second(&record(u64::MAX, u64::MAX)),
            Some(1000)
        );
    }

    #[test]
    fn utc_timestamp_format_handles_epoch_and_calendar_boundaries() {
        assert_eq!(format_timestamp_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(
            format_timestamp_utc(1_798_761_600_000),
            "2027-01-01 00:00 UTC"
        );
    }
}
