use clap::Parser;
use cli::{Cli, Config};
use log_entry::{LogEntry, formatted_timestamp, scan_group};
use std::env;
use std::fs::{self, File};
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

mod cli;
mod log_entry;

const READ_BUFFER_SIZE: usize = 16 * 1024;
const RETAINED_PENDING_BUFFER_SIZE: usize = 4 * 1024;
const LOG_DATE_PREFIX_LEN: usize = 20;
const SEPARATOR_WIDTH: usize = 79;
const LEGACY_FILE_COLORS: [usize; 6] = [32, 34, 35, 36, 37, 90];

#[derive(Debug, PartialEq)]
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
        if unit == 0 {
            Ok(InputAction::Continue)
        } else if let Some(high) = self.high_surrogate.take() {
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
        if character == '\u{3}' {
            return Ok(InputAction::Quit);
        }
        if self.entering_filter {
            if matches!(character, '\r' | '\n') {
                self.entering_filter = false;
                config.set_filter(Some(std::mem::take(&mut self.text)));
                writeln!(
                    out,
                    "\n> filter = {}",
                    config.filter_text().unwrap_or_default()
                )?;
            } else {
                self.text.push(character);
                write!(out, "{character}")?;
            }
            return Ok(InputAction::Continue);
        }
        match character {
            'q' => Ok(InputAction::Quit),
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
                config.set_filter(None);
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
    writeln!(out, ">   d - toggle suppress log date")?;
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
        let mut modes = Self {
            input,
            output: None,
            input_changed,
            output_changed: false,
        };
        let (output, output_changed) =
            set_console_mode(output, |mode| mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING)?;
        modes.output = output;
        modes.output_changed = output_changed;
        Ok(modes)
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
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set"))
        .and_then(|path| vrchat_log_dir_from_local_app_data(&path))
}

fn vrchat_log_dir_from_local_app_data(path: &Path) -> io::Result<PathBuf> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "LOCALAPPDATA has no name"))?
        .to_os_string();
    name.push("Low");
    Ok(path.with_file_name(name).join("VRChat").join("VRChat"))
}

fn run_in_dir(mut config: Config, dir: &Path) -> io::Result<()> {
    let notification = ChangeNotification::new(dir)?;
    let group = scan_group(dir, config.group_period_secs)?;
    if group.is_empty() && !config.watch_new_files {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No log files found",
        ));
    }
    let mut fixed_group = (!config.watch_new_files).then(|| group.clone());
    let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let console_output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let modes = ConsoleModes::new(stdin, console_output)?;
    let console_input = modes.input.map(|(handle, _)| handle);
    let mut handles = vec![notification.0];
    if let Some(handle) = console_input {
        handles.push(handle);
    }
    let color_output = modes.output.is_some() && io::stdout().is_terminal();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut tails = TailSet::open_initial(group, &mut output)?;
    let mut input = InputState::default();

    loop {
        output.flush()?;
        let result =
            unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE) };
        if result == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        if result == WAIT_OBJECT_0 {
            notification.rearm()?;
            if config.watch_new_files {
                tails.reconcile(scan_group(dir, config.group_period_secs)?, &mut output)?;
            } else if let Some(group) = fixed_group.as_mut() {
                tails.reconcile_fixed(group, &mut output)?;
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
    let config = Config::from(Cli::parse());
    let result = vrchat_log_dir().and_then(|dir| run_in_dir(config, &dir));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vrc-tail: {error}");
            ExitCode::FAILURE
        }
    }
}

struct ActiveFile {
    entry: LogEntry,
    index: usize,
    file: File,
    offset: u64,
    pending: Vec<u8>,
}

struct TailSet {
    files: Vec<ActiveFile>,
    startup_pending: Vec<LogEntry>,
    separator_pending: bool,
    read_buffer: Box<[u8; READ_BUFFER_SIZE]>,
}

impl Default for TailSet {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            startup_pending: Vec::new(),
            separator_pending: false,
            read_buffer: Box::new([0; READ_BUFFER_SIZE]),
        }
    }
}

impl TailSet {
    fn open_initial<W: Write>(mut group: Vec<LogEntry>, warnings: &mut W) -> io::Result<Self> {
        group.sort_by_key(|entry| entry.time);
        let mut tails = Self::default();
        for (index, entry) in group.into_iter().enumerate() {
            match File::open(&entry.path) {
                Ok(mut file) => match file.seek(SeekFrom::End(0)) {
                    Ok(offset) => tails.files.push(ActiveFile {
                        entry,
                        index,
                        file,
                        offset,
                        pending: Vec::new(),
                    }),
                    Err(error) => {
                        writeln!(warnings, "failed to seek {}: {error}", entry.path.display())?;
                        tails.startup_pending.push(entry);
                    }
                },
                Err(error) => {
                    writeln!(warnings, "failed to open {}: {error}", entry.path.display())?;
                    tails.startup_pending.push(entry);
                }
            }
        }
        Ok(tails)
    }

    fn reconcile<W: Write>(
        &mut self,
        mut group: Vec<LogEntry>,
        warnings: &mut W,
    ) -> io::Result<()> {
        group.sort_by_key(|entry| entry.time);
        let has_state = !self.files.is_empty() || !self.startup_pending.is_empty();
        if group.is_empty() && has_state {
            self.separator_pending = true;
        }
        let group_overlaps_state = group.iter().any(|entry| {
            self.files
                .iter()
                .any(|active| active.entry.path == entry.path)
                || self
                    .startup_pending
                    .iter()
                    .any(|pending| pending.path == entry.path)
        });
        let reset =
            !group.is_empty() && (self.separator_pending || (has_state && !group_overlaps_state));
        if reset {
            self.files.clear();
            self.startup_pending.clear();
            self.separator_pending = false;
            writeln!(warnings, "{}", "-".repeat(SEPARATOR_WIDTH))?;
        }

        self.startup_pending
            .retain(|pending| group.iter().any(|entry| entry.path == pending.path));

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
            let startup_position = self
                .startup_pending
                .iter()
                .position(|pending| pending.path == entry.path);
            match File::open(&entry.path) {
                Ok(mut file) => {
                    let offset = if startup_position.is_some() {
                        match file.seek(SeekFrom::End(0)) {
                            Ok(offset) => offset,
                            Err(error) => {
                                writeln!(
                                    warnings,
                                    "failed to seek {}: {error}",
                                    entry.path.display()
                                )?;
                                continue;
                            }
                        }
                    } else {
                        0
                    };
                    if let Some(position) = startup_position {
                        self.startup_pending.remove(position);
                    }
                    self.files.push(ActiveFile {
                        entry,
                        index,
                        file,
                        offset,
                        pending: Vec::new(),
                    });
                }
                Err(error) => {
                    writeln!(warnings, "failed to open {}: {error}", entry.path.display())?
                }
            }
        }
        Ok(())
    }

    fn reconcile_fixed<W: Write>(
        &mut self,
        group: &mut Vec<LogEntry>,
        warnings: &mut W,
    ) -> io::Result<()> {
        let mut warning_result = Ok(());
        group.retain(|entry| match fs::metadata(&entry.path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                if warning_result.is_ok() {
                    warning_result = writeln!(
                        warnings,
                        "failed to read metadata for {}: {error}",
                        entry.path.display()
                    );
                }
                true
            }
            Ok(_) => true,
        });
        warning_result?;
        self.reconcile(group.clone(), warnings)
    }

    fn drain<W: Write>(
        &mut self,
        config: &Config,
        color_output: bool,
        out: &mut W,
    ) -> io::Result<()> {
        for active in &mut self.files {
            let length = match active.file.metadata() {
                Ok(metadata) => metadata.len(),
                Err(error) => {
                    writeln!(
                        out,
                        "failed to read metadata for {}: {error}",
                        active.entry.path.display()
                    )?;
                    continue;
                }
            };
            if length < active.offset {
                if let Err(error) = active.file.seek(SeekFrom::Start(0)) {
                    writeln!(
                        out,
                        "failed to seek {}: {error}",
                        active.entry.path.display()
                    )?;
                    continue;
                }
                active.offset = 0;
                active.pending = Vec::new();
            }
            if let Err(error) = active.file.seek(SeekFrom::Start(active.offset)) {
                writeln!(
                    out,
                    "failed to seek {}: {error}",
                    active.entry.path.display()
                )?;
                continue;
            }
            loop {
                let read = match active.file.read(&mut *self.read_buffer) {
                    Ok(read) => read,
                    Err(error) => {
                        writeln!(
                            out,
                            "failed to read {}: {error}",
                            active.entry.path.display()
                        )?;
                        break;
                    }
                };
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
        if pending.capacity() > RETAINED_PENDING_BUFFER_SIZE
            && pending.len() < RETAINED_PENDING_BUFFER_SIZE
        {
            pending.shrink_to(RETAINED_PENDING_BUFFER_SIZE);
        }
    }
    Ok(())
}

fn strip_log_date(line: &str) -> &str {
    if has_log_date_prefix(line) {
        &line[LOG_DATE_PREFIX_LEN..]
    } else {
        line
    }
}

fn log_level(line: &str) -> Option<(&str, &str)> {
    if !has_log_date_prefix(line) {
        return None;
    }
    let rest = &line[LOG_DATE_PREFIX_LEN..];
    ["Warning", "Exception", "Error", "Log"]
        .iter()
        .find_map(|level| rest.strip_prefix(level).map(|suffix| (*level, suffix)))
}

fn has_log_date_prefix(line: &str) -> bool {
    line.len() >= LOG_DATE_PREFIX_LEN
        && line.as_bytes()[4] == b'.'
        && line.as_bytes()[7] == b'.'
        && line.as_bytes()[10] == b' '
        && line.as_bytes()[13] == b':'
        && line.as_bytes()[16] == b':'
        && line.as_bytes()[LOG_DATE_PREFIX_LEN - 1] == b' '
        && line.as_bytes()[..LOG_DATE_PREFIX_LEN - 1]
            .iter()
            .enumerate()
            .all(|(i, byte)| matches!(i, 4 | 7 | 10 | 13 | 16) || byte.is_ascii_digit())
}

fn write_line<W: Write>(
    out: &mut W,
    line: &str,
    index: usize,
    config: &Config,
    color_output: bool,
    timestamp: &str,
) -> io::Result<()> {
    if !config.line_matches(line) || (config.ignore_blank_lines && line.is_empty()) {
        return Ok(());
    }
    let prefix = format!("{timestamp} [{index}] ");
    if color_output {
        let index_code = LEGACY_FILE_COLORS[index % LEGACY_FILE_COLORS.len()];
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
                    &line[..LOG_DATE_PREFIX_LEN + level.len()]
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
mod tests;
