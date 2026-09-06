pub(crate) const DEFAULT_GROUP_PERIOD_SECS: i64 = 30;

#[derive(clap::Parser)]
#[command(version, about)]
pub(crate) struct Cli {
    #[arg(short, long)]
    filter: Option<String>,
    #[arg(short, long)]
    case_sensitive: bool,
    #[arg(short = 's', long)]
    ignore_blank_lines: bool,
    #[arg(short = 'L', long = "no-colored-log-level")]
    no_colored_log_level: bool,
    #[arg(short = 'd', long)]
    suppress_log_date: bool,
    #[arg(
        short = 'g',
        long = "group-period",
        default_value_t = DEFAULT_GROUP_PERIOD_SECS,
        value_parser = clap::value_parser!(i64).range(1..)
    )]
    group_period_secs: i64,
    #[arg(long = "no-watch")]
    no_watch: bool,
}

struct Filter {
    text: String,
    normalized: String,
}

pub(crate) struct Config {
    filter: Option<Filter>,
    pub(crate) case_sensitive: bool,
    pub(crate) ignore_blank_lines: bool,
    pub(crate) colored_log_level: bool,
    pub(crate) suppress_log_date: bool,
    pub(crate) watch_new_files: bool,
    pub(crate) group_period_secs: i64,
}

impl Config {
    pub(crate) fn set_filter(&mut self, text: Option<String>) {
        self.filter = text.map(|text| Filter {
            normalized: text.to_lowercase(),
            text,
        });
    }

    pub(crate) fn filter_text(&self) -> Option<&str> {
        self.filter.as_ref().map(|filter| filter.text.as_str())
    }

    pub(crate) fn line_matches(&self, line: &str) -> bool {
        let Some(filter) = &self.filter else {
            return true;
        };
        if self.case_sensitive {
            line.contains(&filter.text)
        } else {
            line.to_lowercase().contains(&filter.normalized)
        }
    }
}

impl From<Cli> for Config {
    fn from(cli: Cli) -> Self {
        let mut config = Self {
            filter: None,
            case_sensitive: cli.case_sensitive,
            ignore_blank_lines: cli.ignore_blank_lines,
            colored_log_level: !cli.no_colored_log_level,
            suppress_log_date: cli.suppress_log_date,
            watch_new_files: !cli.no_watch,
            group_period_secs: cli.group_period_secs,
        };
        config.set_filter(cli.filter);
        config
    }
}

#[cfg(test)]
pub(crate) fn test_config(
    filter: Option<&str>,
    case_sensitive: bool,
    ignore_blank_lines: bool,
    suppress_log_date: bool,
) -> Config {
    use clap::Parser;

    let mut config = Config::from(Cli::try_parse_from(["vrc-tail", "--no-watch"]).unwrap());
    config.case_sensitive = case_sensitive;
    config.ignore_blank_lines = ignore_blank_lines;
    config.suppress_log_date = suppress_log_date;
    config.set_filter(filter.map(str::to_owned));
    config
}

#[cfg(test)]
mod tests {
    use super::{Cli, Config};
    use clap::{Parser, error::ErrorKind};

    #[test]
    fn uses_literal_runtime_defaults() {
        let config = Config::from(Cli::try_parse_from(["vrc-tail"]).unwrap());

        assert_eq!(config.filter_text(), None);
        assert!(!config.case_sensitive);
        assert!(!config.ignore_blank_lines);
        assert!(config.colored_log_level);
        assert!(!config.suppress_log_date);
        assert!(config.watch_new_files);
        assert_eq!(config.group_period_secs, 30);
    }

    #[test]
    fn maps_short_options_to_runtime_config() {
        let config = Config::from(
            Cli::try_parse_from([
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
            .unwrap(),
        );

        assert_eq!(config.filter_text(), Some("Error"));
        assert!(config.case_sensitive);
        assert!(config.ignore_blank_lines);
        assert!(!config.colored_log_level);
        assert!(config.suppress_log_date);
        assert!(!config.watch_new_files);
        assert_eq!(config.group_period_secs, 12);
    }

    #[test]
    fn maps_long_options_to_runtime_config() {
        let config = Config::from(
            Cli::try_parse_from([
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
            .unwrap(),
        );

        assert_eq!(config.filter_text(), Some("Warn"));
        assert!(config.case_sensitive);
        assert!(config.ignore_blank_lines);
        assert!(!config.colored_log_level);
        assert!(config.suppress_log_date);
        assert!(!config.watch_new_files);
        assert_eq!(config.group_period_secs, 9);
    }

    #[test]
    fn rejects_zero_negative_and_invalid_group_periods() {
        for value in ["0", "-1", "no"] {
            assert!(Cli::try_parse_from(["vrc-tail", "--group-period", value]).is_err());
        }
    }

    #[test]
    fn rejects_missing_values_and_unknown_options() {
        for args in [
            vec!["vrc-tail", "--filter"],
            vec!["vrc-tail", "--group-period"],
            vec!["vrc-tail", "--unknown"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn recognizes_help_and_version_options() {
        for option in ["-h", "--help"] {
            assert_eq!(
                Cli::try_parse_from(["vrc-tail", option])
                    .err()
                    .unwrap()
                    .kind(),
                ErrorKind::DisplayHelp
            );
        }
        for option in ["-V", "--version"] {
            assert_eq!(
                Cli::try_parse_from(["vrc-tail", option])
                    .err()
                    .unwrap()
                    .kind(),
                ErrorKind::DisplayVersion
            );
        }
    }

    #[test]
    fn matching_uses_the_current_case_sensitivity() {
        let mut config =
            Config::from(Cli::try_parse_from(["vrc-tail", "--filter", "Error"]).unwrap());

        assert!(config.line_matches("error"));
        config.case_sensitive = true;
        assert!(config.line_matches("Error"));
        assert!(!config.line_matches("error"));
    }
}
