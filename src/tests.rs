use super::*;
use chrono::{NaiveDate, NaiveDateTime, TimeDelta};
use clap::Parser;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicU64, Ordering};

fn test_dir(suffix: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().canonicalize().unwrap();
    loop {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = root.join(format!(
            "vrc-tail-test-{}-{suffix}-{id}",
            std::process::id()
        ));
        match fs::create_dir(&dir) {
            Ok(()) => return dir,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("failed to create {}: {error}", dir.display()),
        }
    }
}

fn remove_test_dir(dir: PathBuf) {
    let root = std::env::temp_dir().canonicalize().unwrap();
    assert!(dir.canonicalize().unwrap().starts_with(root));
    fs::remove_dir_all(dir).unwrap();
}

fn test_time(offset_secs: i64) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 9, 5)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        + TimeDelta::seconds(offset_secs)
}

fn test_log(dir: &Path, index: usize) -> LogEntry {
    let path = dir.join(format!("output_log_2026-09-05_12-00-{index:02}.txt"));
    fs::write(&path, []).unwrap();
    LogEntry {
        path,
        time: test_time(index as i64),
    }
}

#[test]
fn resolves_vrchat_under_local_low_sibling() {
    let root = test_dir("local-low");
    let local = root.join("Local");
    let expected = root.join("LocalLow").join("VRChat").join("VRChat");
    fs::create_dir(&local).unwrap();
    fs::create_dir_all(&expected).unwrap();
    assert_eq!(
        vrchat_log_dir_from_local_app_data(&local).unwrap(),
        expected
    );
    assert_ne!(
        vrchat_log_dir_from_local_app_data(&local).unwrap(),
        local.join("Low").join("VRChat").join("VRChat")
    );
    remove_test_dir(root);
}

fn config(
    filter: Option<&str>,
    case_sensitive: bool,
    ignore_blank_lines: bool,
    suppress_log_date: bool,
) -> Config {
    let mut config = Config::from(Cli::try_parse_from(["vrc-tail", "--no-watch"]).unwrap());
    config.case_sensitive = case_sensitive;
    config.ignore_blank_lines = ignore_blank_lines;
    config.suppress_log_date = suppress_log_date;
    config.set_filter(filter.map(str::to_owned));
    config
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
    assert_eq!(config.filter_text(), Some("日本😀"));
    state
        .handle_utf16('/' as u16, &mut config, &mut output)
        .unwrap();
    for unit in "Error".encode_utf16() {
        state.handle_utf16(unit, &mut config, &mut output).unwrap();
    }
    state
        .handle_utf16('\r' as u16, &mut config, &mut output)
        .unwrap();
    assert!(config.line_matches("error"));
    state
        .handle_utf16('c' as u16, &mut config, &mut output)
        .unwrap();
    assert!(config.case_sensitive);
    assert!(!config.line_matches("error"));
    for command in ['?', 's', 'l', 'd'] {
        state
            .handle_utf16(command as u16, &mut config, &mut output)
            .unwrap();
    }
    assert!(config.ignore_blank_lines && !config.colored_log_level && config.suppress_log_date);
    state
        .handle_utf16('r' as u16, &mut config, &mut output)
        .unwrap();
    assert_eq!(config.filter_text(), None);
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
fn zero_unicode_key_events_do_not_enter_the_filter() {
    let mut state = InputState::default();
    let mut config = config(None, false, false, false);
    let mut output = Vec::new();
    for unit in ['/' as u16, 0, 'x' as u16, '\r' as u16] {
        state.handle_utf16(unit, &mut config, &mut output).unwrap();
    }
    assert_eq!(config.filter_text(), Some("x"));
}

#[test]
fn control_c_quits_while_q_remains_literal_in_a_filter() {
    let mut state = InputState::default();
    let mut config = config(None, false, false, false);
    let mut output = Vec::new();
    state
        .handle_utf16('/' as u16, &mut config, &mut output)
        .unwrap();
    assert_eq!(
        state
            .handle_utf16('q' as u16, &mut config, &mut output)
            .unwrap(),
        InputAction::Continue
    );
    assert_eq!(state.text, "q");
    assert_eq!(
        state.handle_utf16(3, &mut config, &mut output).unwrap(),
        InputAction::Quit
    );
}

#[test]
fn interactive_help_spells_suppress_correctly() {
    let mut output = Vec::new();
    write_help(&mut output).unwrap();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("suppress log date")
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
    assert!(sensitive.line_matches("Warn here"));
    assert!(!sensitive.line_matches("warn here"));
    assert!(!config(Some("missing"), true, true, false).line_matches("anything"));
    assert!(config(Some("å"), false, false, false).line_matches("WARN Ångström"));
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
fn cycles_through_the_legacy_file_colors() {
    let mut output = Vec::new();
    for index in 0..7 {
        write_line(
            &mut output,
            "line",
            index,
            &config(None, true, false, false),
            true,
            "now",
        )
        .unwrap();
    }
    let output = String::from_utf8(output).unwrap();
    let colors = output.lines().map(|line| &line[2..4]).collect::<Vec<_>>();
    assert_eq!(colors, ["32", "34", "35", "36", "37", "90", "32"]);
}

#[test]
fn initial_content_is_skipped_and_appended_content_is_printed_once() {
    let dir = test_dir("initial-and-append");
    let entry = test_log(&dir, 0);
    fs::write(&entry.path, "old\n").unwrap();
    let mut tails = TailSet::open_initial(vec![entry.clone()], &mut io::sink()).unwrap();
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
fn initial_open_failure_warns_and_is_retried_by_reconciliation() {
    let dir = test_dir("initial-open-recovery");
    let missing = test_log(&dir, 0);
    let good = test_log(&dir, 1);
    fs::remove_file(&missing.path).unwrap();
    let group = vec![missing.clone(), good];
    let mut warnings = Vec::new();
    let mut tails = TailSet::open_initial(group.clone(), &mut warnings).unwrap();
    assert_eq!(tails.files.len(), 1);
    assert!(String::from_utf8_lossy(&warnings).contains("failed to open"));

    fs::write(&missing.path, "recovered\n").unwrap();
    tails.reconcile(group, &mut warnings).unwrap();
    assert_eq!(tails.files.len(), 2);
    remove_test_dir(dir);
}

#[test]
fn recovered_startup_file_starts_at_eof() {
    let dir = test_dir("startup-eof-recovery");
    let entry = test_log(&dir, 0);
    fs::write(&entry.path, "before startup\n").unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&entry.path)
        .unwrap();
    let mut warnings = Vec::new();
    let mut tails = TailSet::open_initial(vec![entry.clone()], &mut warnings).unwrap();
    assert!(tails.files.is_empty());
    drop(lock);

    tails.reconcile(vec![entry.clone()], &mut warnings).unwrap();
    let mut output = Vec::new();
    tails
        .drain(&config(None, true, false, false), false, &mut output)
        .unwrap();
    assert!(output.is_empty());

    fs::write(&entry.path, "before startup\nafter startup\n").unwrap();
    tails
        .drain(&config(None, true, false, false), false, &mut output)
        .unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(!output.contains("before startup"));
    assert!(output.contains("after startup"));
    remove_test_dir(dir);
}

#[test]
fn fixed_selection_drops_deleted_files_and_startup_state() {
    let dir = test_dir("fixed-deletion");
    let active = test_log(&dir, 0);
    let pending = test_log(&dir, 1);
    let lock = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&pending.path)
        .unwrap();
    let mut group = vec![active.clone(), pending.clone()];
    let mut warnings = Vec::new();
    let mut tails = TailSet::open_initial(group.clone(), &mut warnings).unwrap();
    assert_eq!(tails.files.len(), 1);
    drop(lock);

    fs::remove_file(&active.path).unwrap();
    fs::remove_file(&pending.path).unwrap();
    tails.reconcile_fixed(&mut group, &mut warnings).unwrap();
    assert!(group.is_empty());
    assert!(tails.files.is_empty());

    fs::write(&pending.path, "new generation\n").unwrap();
    tails.reconcile(vec![pending], &mut warnings).unwrap();
    let mut output = Vec::new();
    tails
        .drain(&config(None, true, false, false), false, &mut output)
        .unwrap();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("new generation")
    );
    remove_test_dir(dir);
}

#[test]
fn fixed_selection_keeps_unconfirmed_metadata_errors() {
    let mut group = vec![LogEntry {
        path: PathBuf::from(OsString::from("invalid\0path")),
        time: test_time(0),
    }];
    let mut warnings = Vec::new();
    TailSet::default()
        .reconcile_fixed(&mut group, &mut warnings)
        .unwrap();
    assert_eq!(group.len(), 1);
    assert!(String::from_utf8_lossy(&warnings).contains("failed to read metadata"));
}

#[test]
fn newly_discovered_file_is_read_from_byte_zero() {
    let dir = test_dir("new-file");
    let first = test_log(&dir, 0);
    let second = test_log(&dir, 1);
    fs::write(&second.path, "new file\n").unwrap();
    let mut tails = TailSet::open_initial(vec![first], &mut io::sink()).unwrap();
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
    let mut tails = TailSet::open_initial(vec![entry.clone()], &mut io::sink()).unwrap();
    fs::write(&entry.path, "after\n").unwrap();
    let mut output = Vec::new();
    tails
        .drain(&config(None, true, false, false), false, &mut output)
        .unwrap();
    assert!(String::from_utf8(output).unwrap().ends_with(" [0] after\n"));
    remove_test_dir(dir);
}

#[test]
fn truncation_releases_an_oversized_unfinished_line() {
    let dir = test_dir("truncation-capacity");
    let entry = test_log(&dir, 0);
    let mut tails = TailSet::open_initial(vec![entry.clone()], &mut io::sink()).unwrap();
    fs::write(&entry.path, vec![b'x'; 8 * 1024]).unwrap();
    tails
        .drain(&config(None, true, false, false), false, &mut io::sink())
        .unwrap();
    assert!(tails.files[0].pending.capacity() > RETAINED_PENDING_BUFFER_SIZE);

    fs::write(&entry.path, []).unwrap();
    tails
        .drain(&config(None, true, false, false), false, &mut io::sink())
        .unwrap();
    assert_eq!(tails.files[0].pending.capacity(), 0);
    remove_test_dir(dir);
}

#[test]
fn a_read_failure_does_not_stop_other_files_and_can_recover() {
    let dir = test_dir("read-recovery");
    let bad = test_log(&dir, 0);
    let good = test_log(&dir, 1);
    fs::write(&bad.path, "recovered\n").unwrap();
    fs::write(&good.path, "good\n").unwrap();
    let write_only = OpenOptions::new().write(true).open(&bad.path).unwrap();
    let mut tails = TailSet {
        files: vec![
            ActiveFile {
                entry: bad.clone(),
                index: 0,
                file: write_only,
                offset: 0,
                pending: Vec::new(),
            },
            ActiveFile {
                entry: good.clone(),
                index: 1,
                file: File::open(&good.path).unwrap(),
                offset: 0,
                pending: Vec::new(),
            },
        ],
        ..TailSet::default()
    };
    let mut output = Vec::new();
    tails
        .drain(&config(None, true, false, false), false, &mut output)
        .unwrap();
    let first = String::from_utf8_lossy(&output);
    assert!(first.contains("failed to read"));
    assert!(first.contains(" [1] good\n"));

    tails.files[0].file = File::open(&bad.path).unwrap();
    tails
        .drain(&config(None, true, false, false), false, &mut output)
        .unwrap();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains(" [0] recovered\n")
    );
    remove_test_dir(dir);
}

#[test]
fn test_directories_never_reuse_an_existing_path() {
    let first = test_dir("unique");
    fs::write(first.join("sentinel"), []).unwrap();
    let second = test_dir("unique");
    assert_ne!(first, second);
    assert!(first.join("sentinel").exists());
    remove_test_dir(first);
    remove_test_dir(second);
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
fn watched_empty_transition_prints_one_separator_between_groups() {
    let dir = test_dir("empty-transition");
    let old = test_log(&dir, 0);
    let new = test_log(&dir, 1);
    let mut tails = TailSet::default();
    let mut output = Vec::new();

    tails.reconcile(Vec::new(), &mut output).unwrap();
    tails.reconcile(vec![old], &mut output).unwrap();
    assert!(output.is_empty());
    tails.reconcile(Vec::new(), &mut output).unwrap();
    tails.reconcile(Vec::new(), &mut output).unwrap();
    assert!(output.is_empty());
    tails.reconcile(vec![new.clone()], &mut output).unwrap();
    tails.reconcile(vec![new], &mut output).unwrap();

    assert_eq!(
        String::from_utf8(output)
            .unwrap()
            .matches(&"-".repeat(SEPARATOR_WIDTH))
            .count(),
        1
    );
    remove_test_dir(dir);
}

#[test]
fn chained_groups_drop_handles_outside_the_fixed_window() {
    let dir = test_dir("bounded-window");
    let entries = (0..4)
        .map(|index| LogEntry {
            time: test_time(index * 20),
            ..test_log(&dir, index as usize)
        })
        .collect::<Vec<_>>();
    let mut tails = TailSet::default();
    for end in 0..entries.len() {
        let group = log_entry::latest_group(entries[..=end].to_vec(), 30);
        tails.reconcile(group, &mut io::sink()).unwrap();
        assert!(tails.files.len() <= 2);
    }
    assert_eq!(
        tails
            .files
            .iter()
            .map(|file| file.entry.time)
            .collect::<Vec<_>>(),
        vec![test_time(40), test_time(60)]
    );
    remove_test_dir(dir);
}

#[test]
fn drained_large_line_releases_pending_capacity() {
    let dir = test_dir("pending-capacity");
    let entry = test_log(&dir, 0);
    let mut tails = TailSet::open_initial(vec![entry.clone()], &mut io::sink()).unwrap();
    let mut line = vec![b'x'; 8 * 1024];
    line.push(b'\n');
    fs::write(&entry.path, line).unwrap();
    tails
        .drain(&config(None, true, false, false), false, &mut io::sink())
        .unwrap();
    assert!(tails.files[0].pending.capacity() <= RETAINED_PENDING_BUFFER_SIZE);
    remove_test_dir(dir);
}
