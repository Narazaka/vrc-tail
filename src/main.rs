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

fn run_in_dir(config: Config, dir: &Path) -> io::Result<()> {
    let events = Events::new(dir)?;
    let color_output = io::stdout().is_terminal() && !Colored::ansi_color_disabled();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    run_loop(
        config,
        dir,
        || {
            let event = events.recv()?;
            if matches!(event, AppEvent::FilesChanged) {
                events.clear_file_event();
            }
            Ok(event)
        },
        color_output,
        &mut output,
    )
}

fn run_loop<W: Write>(
    mut config: Config,
    dir: &Path,
    mut recv: impl FnMut() -> io::Result<AppEvent>,
    color_output: bool,
    output: &mut W,
) -> io::Result<()> {
    let group = scan_group(dir, config.group_period_secs)?;
    if group.is_empty() && !config.watch_new_files {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No log files found",
        ));
    }
    let mut fixed_group = (!config.watch_new_files).then(|| group.clone());
    let mut tails = TailSet::open_initial(group, output)?;
    let mut input = InputState::default();

    loop {
        output.flush()?;
        match recv()? {
            AppEvent::FilesChanged => {
                if config.watch_new_files {
                    tails.reconcile(scan_group(dir, config.group_period_secs)?, output)?;
                } else if let Some(group) = fixed_group.as_mut() {
                    tails.reconcile_fixed(group, output)?;
                }
                tails.drain(&config, color_output, output)?;
            }
            AppEvent::Key(key) => {
                if input.handle_key(key, &mut config, output)? == InputAction::Quit {
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
    use crate::cli::test_config;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_dir(suffix: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vrc-tail-main-test-{}-{suffix}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        dir
    }

    fn quit_event() -> AppEvent {
        AppEvent::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))
    }

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

    #[test]
    fn watched_loop_tails_appends_and_switches_to_a_new_group() {
        let dir = test_dir("watched-loop");
        let first = dir.join("output_log_2026-09-06_12-00-00.txt");
        let second = dir.join("output_log_2026-09-06_12-00-31.txt");
        fs::write(&first, "old startup\n").unwrap();
        let mut config = test_config(None, true, false, false);
        config.watch_new_files = true;
        let mut calls = 0;
        let mut output = Vec::new();

        run_loop(
            config,
            &dir,
            || {
                let event = match calls {
                    0 => {
                        fs::write(&first, "old startup\nfirst append\n")?;
                        AppEvent::FilesChanged
                    }
                    1 => {
                        fs::write(&second, "new group\n")?;
                        AppEvent::FilesChanged
                    }
                    _ => quit_event(),
                };
                calls += 1;
                Ok(event)
            },
            false,
            &mut output,
        )
        .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("old startup"));
        assert!(output.contains(" [0] first append\n"));
        assert!(output.contains(&"-".repeat(79)));
        assert!(output.contains(" [0] new group\n"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn no_watch_loop_keeps_the_startup_file_set() {
        let dir = test_dir("fixed-loop");
        let first = dir.join("output_log_2026-09-06_12-00-00.txt");
        let second = dir.join("output_log_2026-09-06_12-00-31.txt");
        fs::write(&first, "old startup\n").unwrap();
        let mut calls = 0;
        let mut output = Vec::new();

        run_loop(
            test_config(None, true, false, false),
            &dir,
            || {
                let event = if calls == 0 {
                    fs::write(&first, "old startup\nfixed append\n")?;
                    fs::write(&second, "ignored new file\n")?;
                    AppEvent::FilesChanged
                } else {
                    quit_event()
                };
                calls += 1;
                Ok(event)
            },
            false,
            &mut output,
        )
        .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(!output.contains("old startup"));
        assert!(output.contains(" [0] fixed append\n"));
        assert!(!output.contains("ignored new file"));
        fs::remove_dir_all(dir).unwrap();
    }
}
