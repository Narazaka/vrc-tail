use clap::Parser;
use cli::{Cli, Config};
use crossterm::style::Colored;
use events::{AppEvent, Events};
use input::{InputAction, InputState};
use log_entry::scan_group;
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use tail::TailSet;

mod cli;
mod events;
mod input;
mod log_entry;
mod output;
mod tail;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_vrchat_under_local_low_sibling() {
        let local = PathBuf::from(r"C:\Users\test\AppData\Local");
        let expected = PathBuf::from(r"C:\Users\test\AppData\LocalLow\VRChat\VRChat");
        assert_eq!(
            vrchat_log_dir_from_local_app_data(&local).unwrap(),
            expected
        );
        assert_ne!(
            vrchat_log_dir_from_local_app_data(&local).unwrap(),
            local.join("Low").join("VRChat").join("VRChat")
        );
    }
}
