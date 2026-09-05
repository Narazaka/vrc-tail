use std::ffi::OsStr;
use std::io::{self, Write};
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

#[allow(dead_code)]
fn consume_lines<E>(
    pending: &mut Vec<u8>,
    bytes: &[u8],
    mut emit: impl FnMut(&str) -> Result<(), E>,
) -> Result<(), E> {
    pending.extend_from_slice(bytes);
    let mut consumed = 0;
    while let Some(relative_end) = pending[consumed..].iter().position(|&byte| byte == b'\n') {
        let end = consumed + relative_end;
        let line_end = if end > consumed && pending[end - 1] == b'\r' {
            end - 1
        } else {
            end
        };
        let line = String::from_utf8_lossy(&pending[consumed..line_end]);
        emit(&line)?;
        consumed = end + 1;
    }
    if consumed != 0 {
        pending.drain(..consumed);
        if pending.capacity() > 64 * 1024 && pending.len() < 4 * 1024 {
            pending.shrink_to(4 * 1024);
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn line_matches(line: &str, config: &Config) -> bool {
    let Some(filter) = config.filter.as_deref() else {
        return true;
    };
    if config.case_sensitive {
        line.contains(filter)
    } else {
        line.to_lowercase().contains(&filter.to_lowercase())
    }
}

#[allow(dead_code)]
fn strip_log_date(line: &str) -> &str {
    if has_log_date_prefix(line) {
        &line[20..]
    } else {
        line
    }
}

#[allow(dead_code)]
fn log_level(line: &str) -> Option<(&str, &str)> {
    if !has_log_date_prefix(line) {
        return None;
    }
    let rest = &line[20..];
    ["Warning", "Exception", "Error", "Log"]
        .iter()
        .find_map(|level| rest.strip_prefix(level).map(|suffix| (*level, suffix)))
}

#[allow(dead_code)]
fn has_log_date_prefix(line: &str) -> bool {
    line.len() >= 20
        && line.as_bytes()[4] == b'.'
        && line.as_bytes()[7] == b'.'
        && line.as_bytes()[10] == b' '
        && line.as_bytes()[13] == b':'
        && line.as_bytes()[16] == b':'
        && line.as_bytes()[19] == b' '
        && line.as_bytes()[..19]
            .iter()
            .enumerate()
            .all(|(i, byte)| matches!(i, 4 | 7 | 10 | 13 | 16) || byte.is_ascii_digit())
}

#[allow(dead_code)]
fn formatted_timestamp() -> String {
    let mut now = windows_sys::Win32::Foundation::SYSTEMTIME {
        wYear: 0,
        wMonth: 0,
        wDayOfWeek: 0,
        wDay: 0,
        wHour: 0,
        wMinute: 0,
        wSecond: 0,
        wMilliseconds: 0,
    };
    unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut now) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:04}",
        now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond, now.wMilliseconds
    )
}

#[allow(dead_code)]
fn write_line<W: Write>(
    out: &mut W,
    line: &str,
    index: usize,
    config: &Config,
    color_output: bool,
    timestamp: &str,
) -> io::Result<()> {
    if !line_matches(line, config) || (config.ignore_blank_lines && line.is_empty()) {
        return Ok(());
    }
    let prefix = format!("{timestamp} [{index}] ");
    if color_output {
        let index_code = [32, 35, 36, 37, 90][index % 5];
        if config.colored_log_level
            && let Some((level, suffix)) = log_level(line)
        {
            let level_code = if matches!(level, "Error" | "Exception") {
                31
            } else if level == "Warning" {
                33
            } else {
                34
            };
            writeln!(
                out,
                "\x1b[{index_code}m{prefix}\x1b[0m\x1b[{level_code}m{}\x1b[0m\x1b[{index_code}m{}\x1b[0m",
                if config.suppress_log_date {
                    level
                } else {
                    &line[0..20 + level.len()]
                },
                suffix
            )?;
            return Ok(());
        }
        writeln!(
            out,
            "\x1b[{index_code}m{prefix}{}\x1b[0m",
            if config.suppress_log_date {
                strip_log_date(line)
            } else {
                line
            }
        )
    } else {
        writeln!(
            out,
            "{prefix}{}",
            if config.suppress_log_date {
                strip_log_date(line)
            } else {
                line
            }
        )
    }
}

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

    fn config(
        filter: Option<&str>,
        case_sensitive: bool,
        ignore_blank_lines: bool,
        suppress_log_date: bool,
    ) -> Config {
        Config {
            filter: filter.map(str::to_owned),
            case_sensitive,
            ignore_blank_lines,
            colored_log_level: true,
            suppress_log_date,
            watch_new_files: false,
            group_period_secs: 0,
        }
    }

    #[test]
    fn assembles_crlf_and_split_utf8_without_retaining_complete_lines() {
        let mut pending = Vec::new();
        let mut lines = Vec::new();
        consume_lines(&mut pending, b"hello\r", |line| {
            lines.push(line.to_owned());
            Ok::<_, io::Error>(())
        })
        .unwrap();
        consume_lines(&mut pending, b"\nwarn \xE3\x81", |line| {
            lines.push(line.to_owned());
            Ok::<_, io::Error>(())
        })
        .unwrap();
        consume_lines(&mut pending, b"\x82\n", |line| {
            lines.push(line.to_owned());
            Ok::<_, io::Error>(())
        })
        .unwrap();
        assert_eq!(lines, ["hello", "warn あ"]);
        assert!(pending.is_empty());

        consume_lines(&mut pending, b"bad \xff\n", |line| {
            lines.push(line.to_owned());
            Ok::<_, io::Error>(())
        })
        .unwrap();
        assert_eq!(lines.last().unwrap(), "bad �");
    }

    #[test]
    fn filters_dates_levels_blanks_and_formats_without_color() {
        let sensitive = config(Some("Warn"), true, true, false);
        assert!(line_matches("Warn here", &sensitive));
        assert!(!line_matches("warn here", &sensitive));
        assert!(!line_matches(
            "anything",
            &config(Some("missing"), true, true, false)
        ));
        assert!(line_matches(
            "WARN あ",
            &config(Some("あ"), false, false, false)
        ));
        assert_eq!(strip_log_date("2026.09.05 12:34:56 hello"), "hello");
        assert_eq!(strip_log_date("not dated"), "not dated");
        assert_eq!(
            log_level("2026.09.05 12:34:56 Warning details"),
            Some(("Warning", " details"))
        );
        assert_eq!(log_level("2026.09.05 12:34:56 Debug details"), None);
        let mut output = Vec::new();
        write_line(
            &mut output,
            "2026.09.05 12:34:56 Warning details",
            2,
            &config(None, true, false, true),
            false,
            "2026-09-05 12:34:56.0000",
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "2026-09-05 12:34:56.0000 [2] Warning details\n"
        );
        let mut output = Vec::new();
        write_line(
            &mut output,
            "",
            0,
            &config(None, true, true, false),
            false,
            "now",
        )
        .unwrap();
        assert!(output.is_empty());
    }
}
