use crate::cli::Config;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::io::{self, Write};

#[derive(Debug, PartialEq)]
pub(crate) enum InputAction {
    Continue,
    Quit,
}

#[derive(Default)]
pub(crate) struct InputState {
    entering_filter: bool,
    text: String,
}

impl InputState {
    pub(crate) fn handle_key<W: Write>(
        &mut self,
        key: KeyEvent,
        config: &mut Config,
        out: &mut W,
    ) -> io::Result<InputAction> {
        if matches!(key.code, KeyCode::Char('c' | 'C'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return Ok(InputAction::Quit);
        }
        if self.entering_filter {
            match key.code {
                KeyCode::Enter => {
                    self.entering_filter = false;
                    config.set_filter(Some(std::mem::take(&mut self.text)));
                    writeln!(
                        out,
                        "\n> filter = {}",
                        config.filter_text().unwrap_or_default()
                    )?;
                }
                KeyCode::Char(character) => {
                    self.text.push(character);
                    write!(out, "{character}")?;
                }
                _ => {}
            }
            return Ok(InputAction::Continue);
        }
        match key.code {
            KeyCode::Char('q') => Ok(InputAction::Quit),
            KeyCode::Char('?') => write_help(out).map(|()| InputAction::Continue),
            KeyCode::Enter => writeln!(out).map(|()| InputAction::Continue),
            KeyCode::Char('c') => {
                config.case_sensitive = !config.case_sensitive;
                writeln!(out, "> caseSensitive = {}", config.case_sensitive)?;
                Ok(InputAction::Continue)
            }
            KeyCode::Char('s') => {
                config.ignore_blank_lines = !config.ignore_blank_lines;
                writeln!(out, "> ignoreBlankLines = {}", config.ignore_blank_lines)?;
                Ok(InputAction::Continue)
            }
            KeyCode::Char('l') => {
                config.colored_log_level = !config.colored_log_level;
                writeln!(out, "> coloredLogLevel = {}", config.colored_log_level)?;
                Ok(InputAction::Continue)
            }
            KeyCode::Char('d') => {
                config.suppress_log_date = !config.suppress_log_date;
                writeln!(out, "> suppressLogDate = {}", config.suppress_log_date)?;
                Ok(InputAction::Continue)
            }
            KeyCode::Char('r') => {
                config.set_filter(None);
                writeln!(out, "> filter cleared!")?;
                Ok(InputAction::Continue)
            }
            KeyCode::Char('/') => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::test_config as config;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn plain_q_quits_only_outside_filter_entry() {
        let mut state = InputState::default();
        let mut config = config(None, false, false, false);
        let mut output = Vec::new();
        assert_eq!(
            state
                .handle_key(key(KeyCode::Char('q')), &mut config, &mut output)
                .unwrap(),
            InputAction::Quit
        );
        state
            .handle_key(key(KeyCode::Char('/')), &mut config, &mut output)
            .unwrap();
        assert_eq!(
            state
                .handle_key(key(KeyCode::Char('q')), &mut config, &mut output)
                .unwrap(),
            InputAction::Continue
        );
        state
            .handle_key(key(KeyCode::Enter), &mut config, &mut output)
            .unwrap();
        assert_eq!(config.filter_text(), Some("q"));
    }

    #[test]
    fn control_c_always_quits() {
        let mut state = InputState::default();
        let mut config = config(None, false, false, false);
        let mut output = Vec::new();
        state
            .handle_key(key(KeyCode::Char('/')), &mut config, &mut output)
            .unwrap();
        assert_eq!(
            state
                .handle_key(
                    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                    &mut config,
                    &mut output,
                )
                .unwrap(),
            InputAction::Quit
        );
    }

    #[test]
    fn enter_installs_unicode_filter() {
        let mut state = InputState::default();
        let mut config = config(None, false, false, false);
        let mut output = Vec::new();
        for code in [
            KeyCode::Char('/'),
            KeyCode::Char('日'),
            KeyCode::Char('本'),
            KeyCode::Char('😀'),
            KeyCode::Enter,
        ] {
            state
                .handle_key(key(code), &mut config, &mut output)
                .unwrap();
        }
        assert_eq!(config.filter_text(), Some("日本😀"));
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("> filter = 日本😀")
        );
    }

    #[test]
    fn interactive_commands_retain_their_effects() {
        let mut state = InputState::default();
        let mut config = config(Some("Error"), false, false, false);
        let mut output = Vec::new();
        for command in ['?', 'c', 's', 'l', 'd', 'r'] {
            state
                .handle_key(key(KeyCode::Char(command)), &mut config, &mut output)
                .unwrap();
        }
        assert!(config.case_sensitive);
        assert!(config.ignore_blank_lines);
        assert!(!config.colored_log_level);
        assert!(config.suppress_log_date);
        assert_eq!(config.filter_text(), None);
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("> Commands:"));
        assert!(output.contains("suppress log date"));
    }

    #[test]
    fn non_character_keys_do_nothing() {
        let mut state = InputState::default();
        let mut config = config(Some("keep"), false, false, false);
        let mut output = Vec::new();
        for code in [KeyCode::Backspace, KeyCode::Left, KeyCode::F(1)] {
            assert_eq!(
                state
                    .handle_key(key(code), &mut config, &mut output)
                    .unwrap(),
                InputAction::Continue
            );
        }
        assert_eq!(config.filter_text(), Some("keep"));
        assert!(output.is_empty());
    }
}
