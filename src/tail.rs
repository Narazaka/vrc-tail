use crate::cli::Config;
use crate::log_entry::{LogEntry, formatted_timestamp};
use crate::output::write_line;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};

const READ_BUFFER_SIZE: usize = 16 * 1024;
const RETAINED_PENDING_BUFFER_SIZE: usize = 4 * 1024;
const SEPARATOR_WIDTH: usize = 79;

struct ActiveFile {
    entry: LogEntry,
    index: usize,
    file: File,
    offset: u64,
    pending: Vec<u8>,
}

pub(crate) struct TailSet {
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
    pub(crate) fn open_initial<W: Write>(
        mut group: Vec<LogEntry>,
        warnings: &mut W,
    ) -> io::Result<Self> {
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

    pub(crate) fn reconcile<W: Write>(
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

    pub(crate) fn reconcile_fixed<W: Write>(
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

    pub(crate) fn drain<W: Write>(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_config as config;
    use crate::log_entry::{self, LogEntry};
    use chrono::{NaiveDate, NaiveDateTime, TimeDelta};
    use std::ffi::OsString;
    use std::fs::{self, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
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
}
