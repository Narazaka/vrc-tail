use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
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
struct ActiveFile {
    entry: LogEntry,
    index: usize,
    file: File,
    offset: u64,
    pending: Vec<u8>,
}

#[allow(dead_code)]
struct TailSet {
    files: Vec<ActiveFile>,
    read_buffer: Box<[u8; 16 * 1024]>,
}

impl Default for TailSet {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            read_buffer: Box::new([0; 16 * 1024]),
        }
    }
}

#[allow(dead_code)]
impl TailSet {
    fn open_initial(mut group: Vec<LogEntry>) -> io::Result<Self> {
        group.sort_by_key(|entry| entry.time);
        let mut tails = Self::default();
        for (index, entry) in group.into_iter().enumerate() {
            let mut file = File::open(&entry.path)?;
            let offset = file.seek(SeekFrom::End(0))?;
            tails.files.push(ActiveFile {
                entry,
                index,
                file,
                offset,
                pending: Vec::new(),
            });
        }
        Ok(tails)
    }

    fn reconcile<W: Write>(
        &mut self,
        mut group: Vec<LogEntry>,
        warnings: &mut W,
    ) -> io::Result<()> {
        group.sort_by_key(|entry| entry.time);
        let reset = !self.files.is_empty()
            && !group.iter().any(|entry| {
                self.files
                    .iter()
                    .any(|active| active.entry.path == entry.path)
            });
        if reset {
            self.files.clear();
            writeln!(warnings, "{}", "-".repeat(79))?;
        }

        let mut previous = std::mem::take(&mut self.files);
        for (index, entry) in group.into_iter().enumerate() {
            if let Some(position) = previous
                .iter()
                .position(|active| active.entry.path == entry.path)
            {
                let mut active = previous.remove(position);
                active.entry = entry;
                active.index = index;
                self.files.push(active);
                continue;
            }
            match File::open(&entry.path) {
                Ok(file) => self.files.push(ActiveFile {
                    entry,
                    index,
                    file,
                    offset: 0,
                    pending: Vec::new(),
                }),
                Err(error) => {
                    writeln!(warnings, "failed to open {}: {error}", entry.path.display())?
                }
            }
        }
        Ok(())
    }

    fn drain<W: Write>(
        &mut self,
        config: &Config,
        color_output: bool,
        out: &mut W,
    ) -> io::Result<()> {
        for active in &mut self.files {
            if active.file.metadata()?.len() < active.offset {
                active.file.seek(SeekFrom::Start(0))?;
                active.offset = 0;
                active.pending.clear();
            }
            active.file.seek(SeekFrom::Start(active.offset))?;
            loop {
                let read = active.file.read(&mut *self.read_buffer)?;
                if read == 0 {
                    break;
                }
                active.offset += read as u64;
                consume_lines(&mut active.pending, &self.read_buffer[..read], |line| {
                    write_line(
                        out,
                        line,
                        active.index,
                        config,
                        color_output,
                        &formatted_timestamp(),
                    )
                })?;
            }
        }
        Ok(())
    }
}

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
        let lowered = line.to_lowercase();
        if filter.is_empty() {
            return true;
        }
        lowered.char_indices().any(|(start, _)| {
            let mut candidate = lowered[start..].chars();
            let mut needle = filter.chars().flat_map(char::to_lowercase);
            loop {
                match (needle.next(), candidate.next()) {
                    (None, _) => return true,
                    (Some(expected), Some(actual)) if expected == actual => {}
                    _ => return false,
                }
            }
        })
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
    use std::fs;

    fn test_dir(suffix: &str) -> PathBuf {
        let root = std::env::temp_dir();
        let dir = root.join(format!("vrc-tail-test-{}-{suffix}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let root = root.canonicalize().unwrap();
        assert!(dir.canonicalize().unwrap().starts_with(root));
        dir
    }

    fn remove_test_dir(dir: PathBuf) {
        let root = std::env::temp_dir().canonicalize().unwrap();
        assert!(dir.canonicalize().unwrap().starts_with(root));
        fs::remove_dir_all(dir).unwrap();
    }

    fn test_log(dir: &Path, index: usize) -> LogEntry {
        let path = dir.join(format!("output_log_2026-09-05_12-00-{index:02}.txt"));
        fs::write(&path, []).unwrap();
        LogEntry {
            path,
            time: index as i64,
        }
    }

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
            "WARN Ångström",
            &config(Some("å"), false, false, false)
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

    #[test]
    fn initial_content_is_skipped_and_appended_content_is_printed_once() {
        let dir = test_dir("initial-and-append");
        let entry = test_log(&dir, 0);
        fs::write(&entry.path, "old\n").unwrap();
        let mut tails = TailSet::open_initial(vec![entry.clone()]).unwrap();
        let mut output = Vec::new();
        tails
            .drain(&config(None, true, false, false), false, &mut output)
            .unwrap();
        assert!(output.is_empty());

        fs::write(&entry.path, "old\nnew\n").unwrap();
        tails
            .drain(&config(None, true, false, false), false, &mut output)
            .unwrap();
        tails
            .drain(&config(None, true, false, false), false, &mut output)
            .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("old\n"));
        assert_eq!(output.matches("new\n").count(), 1);
        remove_test_dir(dir);
    }

    #[test]
    fn newly_discovered_file_is_read_from_byte_zero() {
        let dir = test_dir("new-file");
        let first = test_log(&dir, 0);
        let second = test_log(&dir, 1);
        fs::write(&second.path, "new file\n").unwrap();
        let mut tails = TailSet::open_initial(vec![first]).unwrap();
        let mut output = Vec::new();
        tails.reconcile(vec![second], &mut io::sink()).unwrap();
        tails
            .drain(&config(None, true, false, false), false, &mut output)
            .unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .ends_with(" [0] new file\n")
        );
        remove_test_dir(dir);
    }

    #[test]
    fn truncation_reads_the_replacement_from_byte_zero() {
        let dir = test_dir("truncation");
        let entry = test_log(&dir, 0);
        fs::write(&entry.path, "before\n").unwrap();
        let mut tails = TailSet::open_initial(vec![entry.clone()]).unwrap();
        fs::write(&entry.path, "after\n").unwrap();
        let mut output = Vec::new();
        tails
            .drain(&config(None, true, false, false), false, &mut output)
            .unwrap();
        assert!(String::from_utf8(output).unwrap().ends_with(" [0] after\n"));
        remove_test_dir(dir);
    }

    #[test]
    fn repeated_rotations_retain_only_the_current_group() {
        let dir = test_dir("rotations");
        let mut tails = TailSet::default();
        for i in 0..1_000 {
            tails
                .reconcile(vec![test_log(&dir, i)], &mut io::sink())
                .unwrap();
            assert_eq!(tails.files.len(), 1);
        }
        remove_test_dir(dir);
    }
}
