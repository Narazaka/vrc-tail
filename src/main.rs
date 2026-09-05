use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct Config {
    filter: Option<String>,
    case_sensitive: bool,
    ignore_blank_lines: bool,
    colored_log_level: bool,
    suppress_log_date: bool,
    watch_new_files: bool,
    group_period_secs: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LogEntry {
    path: PathBuf,
    time: i64,
}

fn parse_log_name(name: &OsStr, path: PathBuf) -> Option<LogEntry> {
    let name = name.to_str()?;
    let value = name.strip_prefix("output_log_")?.strip_suffix(".txt")?;
    let bytes = value.as_bytes();
    if bytes.len() != 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'_'
        || bytes[13] != b'-'
        || bytes[16] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| !matches!(index, 4 | 7 | 10 | 13 | 16) && !byte.is_ascii_digit())
    {
        return None;
    }
    let [year, month, day, hour, minute, second] = [
        &value[0..4],
        &value[5..7],
        &value[8..10],
        &value[11..13],
        &value[14..16],
        &value[17..19],
    ]
    .map(|field| field.parse().ok())
    .into_iter()
    .collect::<Option<Vec<u32>>>()?
    .try_into()
    .ok()?;
    Some(LogEntry {
        path,
        time: civil_seconds(year.into(), month, day, hour, minute, second)?,
    })
}

fn civil_seconds(
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<i64> {
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 || day == 0 {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day > days_in_month[month as usize - 1] {
        return None;
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146097 + day_of_era - 719468;
    Some(days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))
}

fn latest_group(mut entries: Vec<LogEntry>, period: i64) -> Vec<LogEntry> {
    entries.sort_by_key(|entry| entry.time);
    let start = entries
        .windows(2)
        .rposition(|pair| pair[1].time - pair[0].time > period)
        .map_or(0, |index| index + 1);
    entries.drain(..start);
    entries
}

#[allow(dead_code)]
fn scan_group(dir: &Path, period: i64) -> io::Result<Vec<LogEntry>> {
    let mut entries = Vec::new();
    for item in dir.read_dir()? {
        let item = item?;
        if let Some(entry) = parse_log_name(&item.file_name(), item.path()) {
            entries.push(entry);
        }
    }
    Ok(latest_group(entries, period))
}

fn main() {}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(hour: u32, minute: u32, second: u32) -> i64 {
        civil_seconds(2026, 9, 5, hour, minute, second).unwrap()
    }

    fn entry(time: &str) -> LogEntry {
        let name = format!("output_log_2026-09-05_{time}.txt");
        parse_log_name(OsStr::new(&name), PathBuf::from(name.clone())).unwrap()
    }

    #[test]
    fn rejects_non_log_name() {
        assert!(parse_log_name(OsStr::new("readme.txt"), PathBuf::from("readme.txt")).is_none());
    }

    #[test]
    fn rejects_log_name_with_wrong_delimiters() {
        let name = "output_log_2026_09_05_12_00_00.txt";
        assert!(parse_log_name(OsStr::new(name), PathBuf::from(name)).is_none());
    }

    #[test]
    fn parses_log_name() {
        assert_eq!(
            parse_log_name(
                OsStr::new("output_log_2026-09-05_12-00-00.txt"),
                PathBuf::from("output_log_2026-09-05_12-00-00.txt")
            )
            .unwrap()
            .time,
            seconds(12, 0, 0)
        );
    }

    #[test]
    fn selects_only_the_newest_contiguous_group() {
        let entries = vec![entry("12-01-00"), entry("12-00-20"), entry("12-00-00")];
        let group = latest_group(entries, 30);
        assert_eq!(
            group.iter().map(|e| e.time).collect::<Vec<_>>(),
            vec![seconds(12, 1, 0)]
        );
    }

    #[test]
    fn retains_contiguous_entries_in_chronological_order() {
        let entries = vec![entry("12-01-00"), entry("12-00-20"), entry("12-00-00")];
        let group = latest_group(entries, 60);
        assert_eq!(
            group.iter().map(|e| e.time).collect::<Vec<_>>(),
            vec![seconds(12, 0, 0), seconds(12, 0, 20), seconds(12, 1, 0)]
        );
    }

    #[test]
    fn validates_leap_days() {
        assert!(civil_seconds(2024, 2, 29, 0, 0, 0).is_some());
        assert!(civil_seconds(2025, 2, 29, 0, 0, 0).is_none());
    }
}
