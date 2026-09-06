use chrono::{Local, NaiveDateTime, TimeDelta};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(crate) struct LogEntry {
    pub(crate) path: PathBuf,
    pub(crate) time: NaiveDateTime,
}

fn parse_log_name(name: &OsStr, path: PathBuf) -> Option<LogEntry> {
    let name = name.to_str()?;
    let value = name.strip_prefix("output_log_")?.strip_suffix(".txt")?;
    let bytes = value.as_bytes();
    let format = b"0000-00-00_00-00-00";
    if bytes.len() != format.len()
        || bytes.iter().zip(format).any(|(actual, expected)| {
            if expected.is_ascii_digit() {
                !actual.is_ascii_digit()
            } else {
                actual != expected
            }
        })
    {
        return None;
    }
    Some(LogEntry {
        path,
        time: NaiveDateTime::parse_from_str(value, "%Y-%m-%d_%H-%M-%S").ok()?,
    })
}

pub(crate) fn latest_group(mut entries: Vec<LogEntry>, period: i64) -> Vec<LogEntry> {
    entries.sort_by_key(|entry| entry.time);
    if let Some(newest) = entries.last().map(|entry| entry.time) {
        let cutoff = newest
            .checked_sub_signed(TimeDelta::seconds(period))
            .unwrap_or(NaiveDateTime::MIN);
        entries.retain(|entry| entry.time >= cutoff);
    }
    entries
}

pub(crate) fn scan_group(dir: &Path, period: i64) -> io::Result<Vec<LogEntry>> {
    let mut entries = Vec::new();
    for item in dir.read_dir()? {
        let item = item?;
        if let Some(entry) = parse_log_name(&item.file_name(), item.path()) {
            entries.push(entry);
        }
    }
    Ok(latest_group(entries, period))
}

pub(crate) fn formatted_timestamp() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S.0%3f").to_string()
}

#[cfg(test)]
mod tests {
    use super::{formatted_timestamp, latest_group, parse_log_name};
    use chrono::{NaiveDate, NaiveDateTime};
    use std::ffi::OsStr;
    use std::path::PathBuf;

    fn timestamp(hour: u32, minute: u32, second: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 9, 5)
            .unwrap()
            .and_hms_opt(hour, minute, second)
            .unwrap()
    }

    fn entry(time: &str) -> super::LogEntry {
        let name = format!("output_log_2026-09-05_{time}.txt");
        parse_log_name(OsStr::new(&name), PathBuf::from(&name)).unwrap()
    }

    #[test]
    fn rejects_non_log_names_and_wrong_delimiters() {
        for name in [
            "readme.txt",
            "output_log_2026_09_05_12_00_00.txt",
            "output_log_2026-9-05_12-00-00.txt",
        ] {
            assert!(parse_log_name(OsStr::new(name), PathBuf::from(name)).is_none());
        }
    }

    #[test]
    fn parses_exact_log_filename_into_naive_datetime() {
        let name = "output_log_2026-09-05_12-00-00.txt";
        let parsed = parse_log_name(OsStr::new(name), PathBuf::from(name)).unwrap();

        assert_eq!(parsed.time, timestamp(12, 0, 0));
    }

    #[test]
    fn validates_leap_days() {
        let valid = "output_log_2024-02-29_00-00-00.txt";
        let invalid = "output_log_2025-02-29_00-00-00.txt";

        assert!(parse_log_name(OsStr::new(valid), PathBuf::from(valid)).is_some());
        assert!(parse_log_name(OsStr::new(invalid), PathBuf::from(invalid)).is_none());
    }

    #[test]
    fn keeps_the_newest_relative_window_in_chronological_order() {
        let entries = vec![
            entry("12-01-00"),
            entry("12-00-20"),
            entry("12-00-00"),
            entry("12-00-40"),
        ];

        let group = latest_group(entries, 30);

        assert_eq!(
            group.iter().map(|entry| entry.time).collect::<Vec<_>>(),
            vec![timestamp(12, 0, 40), timestamp(12, 1, 0)]
        );
    }

    #[test]
    fn timestamp_has_fixed_zero_padded_shape() {
        let timestamp = formatted_timestamp();

        assert_eq!(timestamp.len(), 24);
        assert!(timestamp.bytes().enumerate().all(|(index, byte)| {
            match index {
                4 | 7 => byte == b'-',
                10 => byte == b' ',
                13 | 16 => byte == b':',
                19 => byte == b'.',
                _ => byte.is_ascii_digit(),
            }
        }));
        assert_eq!(timestamp.as_bytes()[20], b'0');
    }
}
