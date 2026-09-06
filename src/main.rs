use clap::Parser;
use cli::{Cli, Config};
use crossterm::QueueableCommand;
use crossterm::style::{Color, Colored, ResetColor, SetForegroundColor};
use events::{AppEvent, Events};
use input::{InputAction, InputState};
use log_entry::{LogEntry, formatted_timestamp, scan_group};
use std::env;
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod cli;
mod events;
mod input;
mod log_entry;

const READ_BUFFER_SIZE: usize = 16 * 1024;
const RETAINED_PENDING_BUFFER_SIZE: usize = 4 * 1024;
const LOG_DATE_PREFIX_LEN: usize = 20;
const SEPARATOR_WIDTH: usize = 79;
const FILE_COLORS: [Color; 6] = [
    Color::DarkGreen,
    Color::DarkBlue,
    Color::DarkMagenta,
    Color::DarkCyan,
    Color::Grey,
    Color::DarkGrey,
];

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
    let events = Events::new(dir)?;
    let group = scan_group(dir, config.group_period_secs)?;
    if group.is_empty() && !config.watch_new_files {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No log files found",
        ));
    }
    let mut fixed_group = (!config.watch_new_files).then(|| group.clone());
    let color_output = io::stdout().is_terminal() && !Colored::ansi_color_disabled();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut tails = TailSet::open_initial(group, &mut output)?;
    let mut input = InputState::default();

    loop {
        output.flush()?;
        match events.recv()? {
            AppEvent::FilesChanged => {
                events.clear_file_event();
                if config.watch_new_files {
                    tails.reconcile(scan_group(dir, config.group_period_secs)?, &mut output)?;
                } else if let Some(group) = fixed_group.as_mut() {
                    tails.reconcile_fixed(group, &mut output)?;
                }
                tails.drain(&config, color_output, &mut output)?;
            }
            AppEvent::Key(key) => {
                if input.handle_key(key, &mut config, &mut output)? == InputAction::Quit {
                    return Ok(());
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
        let index_color = FILE_COLORS[index % FILE_COLORS.len()];
        out.queue(SetForegroundColor(index_color))?;
        write!(out, "{prefix}")?;
        if config.colored_log_level
            && let Some((level, suffix)) = log_level(line)
        {
            let level_color = if matches!(level, "Error" | "Exception") {
                Color::DarkRed
            } else if level == "Warning" {
                Color::DarkYellow
            } else {
                Color::DarkBlue
            };
            out.queue(ResetColor)?;
            out.queue(SetForegroundColor(level_color))?;
            write!(
                out,
                "{}",
                if config.suppress_log_date {
                    level
                } else {
                    &line[..LOG_DATE_PREFIX_LEN + level.len()]
                }
            )?;
            out.queue(ResetColor)?;
            out.queue(SetForegroundColor(index_color))?;
            write!(out, "{suffix}")?;
            out.queue(ResetColor)?;
            writeln!(out)?;
            return Ok(());
        }
        write!(
            out,
            "{}",
            if config.suppress_log_date {
                strip_log_date(line)
            } else {
                line
            }
        )?;
        out.queue(ResetColor)?;
        writeln!(out)
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
