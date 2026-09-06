use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::io::{self, IsTerminal};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

pub(crate) enum AppEvent {
    FilesChanged,
    Key(KeyEvent),
}

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

pub(crate) struct Events {
    receiver: Receiver<io::Result<AppEvent>>,
    _watcher: RecommendedWatcher,
    _raw_mode: Option<RawModeGuard>,
    file_event_pending: Arc<AtomicBool>,
}

impl Events {
    pub(crate) fn new(dir: &Path) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let file_event_pending = Arc::new(AtomicBool::new(false));
        let callback_pending = Arc::clone(&file_event_pending);
        let callback_sender = sender.clone();
        let mut watcher = notify::recommended_watcher(move |result| match result {
            Ok(_) => {
                if !callback_pending.swap(true, Ordering::AcqRel) {
                    let _ = callback_sender.send(Ok(AppEvent::FilesChanged));
                }
            }
            Err(error) => send_error(&callback_sender, error),
        })
        .map_err(io::Error::other)?;
        watcher
            .watch(dir, RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;

        let raw_mode = if io::stdin().is_terminal() {
            let raw_mode = RawModeGuard::new()?;
            thread::Builder::new()
                .name("terminal-input".to_owned())
                .spawn(move || read_input(sender))?;
            Some(raw_mode)
        } else {
            None
        };

        Ok(Self {
            receiver,
            _watcher: watcher,
            _raw_mode: raw_mode,
            file_event_pending,
        })
    }

    pub(crate) fn recv(&self) -> io::Result<AppEvent> {
        self.receiver.recv().map_err(io::Error::other)?
    }

    pub(crate) fn clear_file_event(&self) {
        self.file_event_pending.store(false, Ordering::Release);
    }
}

fn read_input(sender: Sender<io::Result<AppEvent>>) {
    loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                if sender.send(Ok(AppEvent::Key(key))).is_err() {
                    return;
                }
            }
            Ok(_) => {}
            Err(error) => {
                let _ = sender.send(Err(error));
                return;
            }
        }
    }
}

fn send_error(sender: &Sender<io::Result<AppEvent>>, error: notify::Error) {
    let _ = sender.send(Err(io::Error::other(error)));
}
