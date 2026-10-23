use crate::backend::Event;
use omdrop_host::{
    TransferHistoryRecord, append_transfer_history, clear_transfer_history, load_transfer_history,
};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

enum Command {
    Append(TransferHistoryRecord, Vec<PathBuf>),
    Clear,
    Flush(Sender<()>),
}

pub struct StoreWorker(Sender<Command>);

impl StoreWorker {
    pub fn start(events: Sender<Event>) -> (Self, JoinHandle<()>) {
        let (commands, rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("omdrop-history".into())
            .spawn(move || {
                let loaded = load_transfer_history().map_err(|error| format!("{error:#}"));
                let _ = events.send(Event::HistoryLoaded(loaded));
                for command in rx {
                    match command {
                        Command::Append(mut record, received_files) => {
                            if !received_files.is_empty()
                                && let Some(bytes) =
                                    received_files.iter().try_fold(0u64, |total, path| {
                                        total.checked_add(std::fs::metadata(path).ok()?.len())
                                    })
                            {
                                record.file_bytes = Some(bytes);
                            }
                            let result = append_transfer_history(record)
                                .map_err(|error| format!("{error:#}"));
                            let _ = events.send(Event::HistoryUpdated(result));
                        }
                        Command::Clear => {
                            let result = clear_transfer_history()
                                .map(|()| Vec::new())
                                .map_err(|error| format!("{error:#}"));
                            let _ = events.send(Event::HistoryCleared(result));
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

    pub fn clear(&self) {
        let _ = self.0.send(Command::Clear);
    }

    pub fn flush_handle(&self) -> JoinHandle<()> {
        let (complete, wait) = mpsc::channel();
        let _ = self.0.send(Command::Flush(complete));
        thread::spawn(move || {
            let _ = wait.recv();
        })
    }
}

pub fn unix_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
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

    #[test]
    fn utc_timestamp_format_handles_epoch_and_calendar_boundaries() {
        assert_eq!(format_timestamp_utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(
            format_timestamp_utc(1_798_761_600_000),
            "2027-01-01 00:00 UTC"
        );
    }
}
