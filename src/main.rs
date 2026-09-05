use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows_sys::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetNumberOfConsoleInputEvents,
    GetStdHandle, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    SetConsoleMode,
};
use windows_sys::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};

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
    if let Some(newest) = entries.last().map(|entry| entry.time) {
        entries.retain(|entry| entry.time >= newest.saturating_sub(period));
    }
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

#[derive(Debug)]
enum CliAction {
    Run(Config),
    Help,
    Version,
}

fn parse_args<I, S>(args: I) -> Result<CliAction, String>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut args = args.into_iter().map(|arg| arg.into());
    let _program = args.next();
    let mut config = Config {
        filter: None,
        case_sensitive: false,
        ignore_blank_lines: false,
        colored_log_level: true,
        suppress_log_date: false,
        watch_new_files: true,
        group_period_secs: 30,
    };
    while let Some(argument) = args.next() {
        let argument = argument
            .into_string()
            .map_err(|_| "arguments must be Unicode".to_owned())?;
        match argument.as_str() {
            "-h" | "--help" => return Ok(CliAction::Help),
            "-V" | "--version" => return Ok(CliAction::Version),
            "-f" | "--filter" => {
                config.filter = Some(next_arg(&mut args, &argument)?);
            }
            "-c" | "--case-sensitive" => config.case_sensitive = true,
            "-s" | "--ignore-blank-lines" => config.ignore_blank_lines = true,
            "-L" | "--no-colored-log-level" => config.colored_log_level = false,
            "-d" | "--suppress-log-date" => config.suppress_log_date = true,
            "-g" | "--group-period" => {
                let value = next_arg(&mut args, &argument)?;
                config.group_period_secs = value
                    .parse()
                    .ok()
                    .filter(|period: &i64| *period > 0)
                    .ok_or_else(|| "group period must be a positive number".to_owned())?;
            }
            "--no-watch" => config.watch_new_files = false,
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    Ok(CliAction::Run(config))
}

fn next_arg<I>(args: &mut I, option: &str) -> Result<String, String>
where
    I: Iterator<Item = OsString>,
{
    args.next()
        .ok_or_else(|| format!("missing value for {option}"))?
        .into_string()
        .map_err(|_| "arguments must be Unicode".to_owned())
}

#[derive(Debug, Eq, PartialEq)]
enum InputAction {
    Continue,
    Quit,
}

#[derive(Default)]
struct InputState {
    entering_filter: bool,
    text: String,
    high_surrogate: Option<u16>,
}

impl InputState {
    fn handle_utf16<W: Write>(
        &mut self,
        unit: u16,
        config: &mut Config,
        out: &mut W,
    ) -> io::Result<InputAction> {
        if let Some(high) = self.high_surrogate.take() {
            let mut action = InputAction::Continue;
            for decoded in char::decode_utf16([high, unit]) {
                action =
                    self.handle_char(decoded.unwrap_or(char::REPLACEMENT_CHARACTER), config, out)?;
            }
            Ok(action)
        } else if (0xD800..=0xDBFF).contains(&unit) {
            self.high_surrogate = Some(unit);
            Ok(InputAction::Continue)
        } else {
            self.handle_char(
                char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER),
                config,
                out,
            )
        }
    }

    fn handle_char<W: Write>(
        &mut self,
        character: char,
        config: &mut Config,
        out: &mut W,
    ) -> io::Result<InputAction> {
        if self.entering_filter {
            if matches!(character, '\r' | '\n') {
                self.entering_filter = false;
                config.filter = Some(std::mem::take(&mut self.text));
                writeln!(
                    out,
                    "\n> filter = {}",
                    config.filter.as_deref().unwrap_or_default()
                )?;
            } else {
                self.text.push(character);
                write!(out, "{character}")?;
            }
            return Ok(InputAction::Continue);
        }
        match character {
            '\u{3}' | 'q' => Ok(InputAction::Quit),
            '?' => write_help(out).map(|()| InputAction::Continue),
            '\r' | '\n' => writeln!(out).map(|()| InputAction::Continue),
            'c' => {
                config.case_sensitive = !config.case_sensitive;
                writeln!(out, "> caseSensitive = {}", config.case_sensitive)?;
                Ok(InputAction::Continue)
            }
            's' => {
                config.ignore_blank_lines = !config.ignore_blank_lines;
                writeln!(out, "> ignoreBlankLines = {}", config.ignore_blank_lines)?;
                Ok(InputAction::Continue)
            }
            'l' => {
                config.colored_log_level = !config.colored_log_level;
                writeln!(out, "> coloredLogLevel = {}", config.colored_log_level)?;
                Ok(InputAction::Continue)
            }
            'd' => {
                config.suppress_log_date = !config.suppress_log_date;
                writeln!(out, "> suppressLogDate = {}", config.suppress_log_date)?;
                Ok(InputAction::Continue)
            }
            'r' => {
                config.filter = None;
                writeln!(out, "> filter cleared!")?;
                Ok(InputAction::Continue)
            }
            '/' => {
                self.entering_filter = true;
                self.text.clear();
                write!(out, "/")?;
                Ok(InputAction::Continue)
            }
            _ => Ok(InputAction::Continue),
        }
    }
}

fn write_help<W: Write>(out: &mut W) -> io::Result<()> {
    writeln!(out, "> Commands:")?;
    writeln!(out, ">   ? - show this help")?;
    writeln!(out, ">   q - quit")?;
    writeln!(out, ">   c - toggle case sensitive")?;
    writeln!(out, ">   s - toggle ignore blank lines")?;
    writeln!(out, ">   l - toggle colored log level")?;
    writeln!(out, ">   d - toggle supress log date")?;
    writeln!(out, ">   /<str> - filter")?;
    writeln!(out, ">   r - reset filter")
}

struct ChangeNotification(HANDLE);

impl ChangeNotification {
    fn new(dir: &Path) -> io::Result<Self> {
        let path = dir
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let handle = unsafe {
            FindFirstChangeNotificationW(
                path.as_ptr(),
                0,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }

    fn rearm(&self) -> io::Result<()> {
        if unsafe { FindNextChangeNotification(self.0) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for ChangeNotification {
    fn drop(&mut self) {
        // Change-notification handles have their own close API.
        unsafe { FindCloseChangeNotification(self.0) };
    }
}

struct ConsoleModes {
    input: Option<(HANDLE, CONSOLE_MODE)>,
    output: Option<(HANDLE, CONSOLE_MODE)>,
    input_changed: bool,
    output_changed: bool,
}

impl ConsoleModes {
    fn new(input: HANDLE, output: HANDLE) -> io::Result<Self> {
        let (input, input_changed) = set_console_mode(input, |mode| {
            mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT)
        })?;
        let (output, output_changed) =
            set_console_mode(output, |mode| mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING)?;
        Ok(Self {
            input,
            output,
            input_changed,
            output_changed,
        })
    }
}

fn set_console_mode(
    handle: HANDLE,
    change: impl FnOnce(CONSOLE_MODE) -> CONSOLE_MODE,
) -> io::Result<(Option<(HANDLE, CONSOLE_MODE)>, bool)> {
    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return Ok((None, false));
    }
    let changed = change(mode);
    if unsafe { SetConsoleMode(handle, changed) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((Some((handle, mode)), changed != mode))
}

impl Drop for ConsoleModes {
    fn drop(&mut self) {
        if self.input_changed
            && let Some((handle, mode)) = self.input
        {
            unsafe { SetConsoleMode(handle, mode) };
        }
        if self.output_changed
            && let Some((handle, mode)) = self.output
        {
            unsafe { SetConsoleMode(handle, mode) };
        }
    }
}

fn vrchat_log_dir() -> io::Result<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("Low").join("VRChat").join("VRChat"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set"))
}

fn run_in_dir(mut config: Config, dir: &Path) -> io::Result<()> {
    let group = scan_group(dir, config.group_period_secs)?;
    if group.is_empty() && !config.watch_new_files {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No log files found",
        ));
    }
    let mut tails = TailSet::open_initial(group)?;
    let notification = ChangeNotification::new(dir)?;
    let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let modes = ConsoleModes::new(stdin, stdout)?;
    let console_input = modes.input.map(|(handle, _)| handle);
    let color_output = modes.output.is_some() && io::stdout().is_terminal();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut input = InputState::default();

    loop {
        let mut handles = vec![notification.0];
        if let Some(handle) = console_input {
            handles.push(handle);
        }
        let result =
            unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE) };
        if result == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        if result == WAIT_OBJECT_0 {
            notification.rearm()?;
            if config.watch_new_files {
                tails.reconcile(scan_group(dir, config.group_period_secs)?, &mut output)?;
            } else {
                tails.files.retain(|active| active.entry.path.exists());
            }
            tails.drain(&config, color_output, &mut output)?;
            continue;
        }
        if result == WAIT_OBJECT_0 + 1 {
            let Some(handle) = console_input else {
                continue;
            };
            let mut count = 0;
            if unsafe { GetNumberOfConsoleInputEvents(handle, &mut count) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut records = [INPUT_RECORD::default(); 32];
            while count != 0 {
                let mut read = 0;
                if unsafe {
                    ReadConsoleInputW(
                        handle,
                        records.as_mut_ptr(),
                        records.len() as u32,
                        &mut read,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                for record in &records[..read as usize] {
                    if record.EventType != KEY_EVENT as u16 {
                        continue;
                    }
                    let key = unsafe { record.Event.KeyEvent };
                    if key.bKeyDown == 0 {
                        continue;
                    }
                    let unit = unsafe { key.uChar.UnicodeChar };
                    for _ in 0..key.wRepeatCount {
                        if input.handle_utf16(unit, &mut config, &mut output)? == InputAction::Quit
                        {
                            return Ok(());
                        }
                    }
                }
                if unsafe { GetNumberOfConsoleInputEvents(handle, &mut count) } == 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        }
    }
}

fn main() -> ExitCode {
    let result = match parse_args(env::args_os()) {
        Ok(CliAction::Help) => print_help(&mut io::stdout()),
        Ok(CliAction::Version) => {
            writeln!(io::stdout(), "{}", env!("CARGO_PKG_VERSION"))
        }
        Ok(CliAction::Run(config)) => vrchat_log_dir().and_then(|dir| run_in_dir(config, &dir)),
        Err(error) => Err(io::Error::new(io::ErrorKind::InvalidInput, error)),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vrc-tail: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_help<W: Write>(out: &mut W) -> io::Result<()> {
    writeln!(out, "Usage: vrc-tail [OPTIONS]")?;
    writeln!(out, "  -f, --filter <str>          filter")?;
    writeln!(out, "  -c, --case-sensitive         case sensitive")?;
    writeln!(out, "  -s, --ignore-blank-lines     ignore blank lines")?;
    writeln!(out, "  -L, --no-colored-log-level   no colored log level")?;
    writeln!(out, "  -d, --suppress-log-date      suppress log date")?;
    writeln!(
        out,
        "  -g, --group-period <sec>     log group period (seconds)"
    )?;
    writeln!(
        out,
        "      --no-watch               do not add new log files"
    )?;
    writeln!(out, "  -h, --help                   print help")?;
    writeln!(out, "  -V, --version                print version")
}

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
        if pending.capacity() > 4 * 1024 && pending.len() < 4 * 1024 {
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
    fn keeps_only_the_fixed_window_below_the_newest_timestamp() {
        let entries = vec![
            entry("12-00-00"),
            entry("12-00-20"),
            entry("12-00-40"),
            entry("12-01-00"),
        ];
        let group = latest_group(entries, 30);
        assert_eq!(
            group.iter().map(|entry| entry.time).collect::<Vec<_>>(),
            vec![seconds(12, 0, 40), seconds(12, 1, 0)]
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
    fn parses_cli_flags_and_defaults() {
        let CliAction::Run(defaults) = parse_args(["vrc-tail"]).unwrap() else {
            panic!();
        };
        assert_eq!(defaults.group_period_secs, 30);
        assert!(defaults.watch_new_files);
        assert!(defaults.colored_log_level);

        let CliAction::Run(config) = parse_args([
            "vrc-tail",
            "-f",
            "Error",
            "-c",
            "-s",
            "-L",
            "-d",
            "-g",
            "12",
            "--no-watch",
        ])
        .unwrap() else {
            panic!();
        };
        assert_eq!(config.filter.as_deref(), Some("Error"));
        assert!(config.case_sensitive && config.ignore_blank_lines && config.suppress_log_date);
        assert!(!config.colored_log_level && !config.watch_new_files);
        assert_eq!(config.group_period_secs, 12);

        let CliAction::Run(config) = parse_args([
            "vrc-tail",
            "--filter",
            "Warn",
            "--case-sensitive",
            "--ignore-blank-lines",
            "--no-colored-log-level",
            "--suppress-log-date",
            "--group-period",
            "9",
            "--no-watch",
        ])
        .unwrap() else {
            panic!();
        };
        assert_eq!(config.filter.as_deref(), Some("Warn"));
        assert!(config.case_sensitive && config.ignore_blank_lines && config.suppress_log_date);
        assert!(!config.colored_log_level && !config.watch_new_files);
        assert_eq!(config.group_period_secs, 9);
    }

    #[test]
    fn rejects_invalid_cli_arguments_and_recognizes_meta_actions() {
        for args in [
            vec!["vrc-tail", "-f"],
            vec!["vrc-tail", "--filter"],
            vec!["vrc-tail", "-g"],
            vec!["vrc-tail", "-g", "0"],
            vec!["vrc-tail", "--group-period", "-1"],
            vec!["vrc-tail", "--group-period", "no"],
            vec!["vrc-tail", "--unknown"],
        ] {
            assert!(parse_args(args).is_err());
        }
        assert!(matches!(
            parse_args(["vrc-tail", "-h"]),
            Ok(CliAction::Help)
        ));
        assert!(matches!(
            parse_args(["vrc-tail", "--help"]),
            Ok(CliAction::Help)
        ));
        assert!(matches!(
            parse_args(["vrc-tail", "--version"]),
            Ok(CliAction::Version)
        ));
        assert!(matches!(
            parse_args(["vrc-tail", "-V"]),
            Ok(CliAction::Version)
        ));
    }

    #[test]
    fn initial_filter_respects_case_sensitivity() {
        let CliAction::Run(config) = parse_args(["vrc-tail", "-f", "Error", "-c"]).unwrap() else {
            panic!();
        };
        assert!(line_matches("Error", &config));
        assert!(!line_matches("error", &config));
    }

    #[test]
    fn console_input_filters_toggles_and_quits() {
        let mut state = InputState::default();
        let mut config = config(None, false, false, false);
        let mut output = Vec::new();
        assert_eq!(
            state
                .handle_utf16('/' as u16, &mut config, &mut output)
                .unwrap(),
            InputAction::Continue
        );
        for unit in "日本😀".encode_utf16() {
            state.handle_utf16(unit, &mut config, &mut output).unwrap();
        }
        state
            .handle_utf16('\r' as u16, &mut config, &mut output)
            .unwrap();
        assert_eq!(config.filter.as_deref(), Some("日本😀"));
        state
            .handle_utf16('/' as u16, &mut config, &mut output)
            .unwrap();
        for unit in "Error".encode_utf16() {
            state.handle_utf16(unit, &mut config, &mut output).unwrap();
        }
        state
            .handle_utf16('\r' as u16, &mut config, &mut output)
            .unwrap();
        assert!(line_matches("error", &config));
        state
            .handle_utf16('c' as u16, &mut config, &mut output)
            .unwrap();
        assert!(config.case_sensitive);
        assert!(!line_matches("error", &config));
        for command in ['?', 's', 'l', 'd'] {
            state
                .handle_utf16(command as u16, &mut config, &mut output)
                .unwrap();
        }
        assert!(config.ignore_blank_lines && !config.colored_log_level && config.suppress_log_date);
        state
            .handle_utf16('r' as u16, &mut config, &mut output)
            .unwrap();
        assert!(config.filter.is_none());
        assert_eq!(
            state.handle_utf16(3, &mut config, &mut output).unwrap(),
            InputAction::Quit
        );
        assert_eq!(
            state
                .handle_utf16('q' as u16, &mut config, &mut output)
                .unwrap(),
            InputAction::Quit
        );
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("> filter = 日本😀")
        );
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

    #[test]
    fn chained_groups_drop_handles_outside_the_fixed_window() {
        let dir = test_dir("bounded-window");
        let entries = (0..4)
            .map(|index| LogEntry {
                time: index * 20,
                ..test_log(&dir, index as usize)
            })
            .collect::<Vec<_>>();
        let mut tails = TailSet::default();
        for end in 0..entries.len() {
            let group = latest_group(entries[..=end].to_vec(), 30);
            tails.reconcile(group, &mut io::sink()).unwrap();
            assert!(tails.files.len() <= 2);
        }
        assert_eq!(
            tails
                .files
                .iter()
                .map(|file| file.entry.time)
                .collect::<Vec<_>>(),
            vec![40, 60]
        );
        remove_test_dir(dir);
    }

    #[test]
    fn drained_large_line_releases_pending_capacity() {
        let dir = test_dir("pending-capacity");
        let entry = test_log(&dir, 0);
        let mut tails = TailSet::open_initial(vec![entry.clone()]).unwrap();
        let mut line = vec![b'x'; 8 * 1024];
        line.push(b'\n');
        fs::write(&entry.path, line).unwrap();
        tails
            .drain(&config(None, true, false, false), false, &mut io::sink())
            .unwrap();
        assert!(tails.files[0].pending.capacity() <= 4 * 1024);
        remove_test_dir(dir);
    }
}
