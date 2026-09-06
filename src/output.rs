use crate::cli::Config;
use crossterm::QueueableCommand;
use crossterm::style::{Color, ResetColor, SetForegroundColor};
use std::io::{self, Write};

const LOG_DATE_PREFIX_LEN: usize = 20;
const FILE_COLORS: [Color; 6] = [
    Color::DarkGreen,
    Color::DarkBlue,
    Color::DarkMagenta,
    Color::DarkCyan,
    Color::Grey,
    Color::DarkGrey,
];

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

pub(crate) fn write_line<W: Write>(
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
mod tests {
    use super::*;
    use crate::cli::test_config as config;

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
        crossterm::style::Colored::set_ansi_color_disabled(false);
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
        let colors = output
            .lines()
            .map(|line| line.split_once('m').unwrap().0)
            .collect::<Vec<_>>();
        assert_eq!(
            colors,
            [
                "\x1b[38;5;2",
                "\x1b[38;5;4",
                "\x1b[38;5;5",
                "\x1b[38;5;6",
                "\x1b[38;5;7",
                "\x1b[38;5;8",
                "\x1b[38;5;2",
            ]
        );
    }
}
