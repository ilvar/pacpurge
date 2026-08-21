//! Command-line parsing.
//!
//! Hand-rolled rather than pulled from a crate: the option set is small, and
//! the strict profile favours a dependency-light binary. Unknown options are
//! an error rather than being ignored, because silently doing the wrong thing
//! to a package database is not an acceptable failure mode.

use std::fmt;
use std::path::PathBuf;

/// What the program should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Run the interactive terminal interface.
    Interactive,
    /// Print the analysis as one JSON document and exit.
    Json,
    /// Print a plain-text table and exit.
    List,
    /// Print the cleanup targets as plain text and exit.
    Clean,
    /// Explain what the last-use probe can and cannot see, and exit.
    Diagnose,
    /// Print usage and exit.
    Help,
    /// Print the version and exit.
    Version,
}

/// Parsed options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// What to do.
    pub mode: Mode,
    /// Filesystem root to analyse.
    pub root: PathBuf,
    /// Pacman database directory, when overridden.
    pub db_path: Option<PathBuf>,
    /// How many of the largest packages to probe for access times.
    /// Zero, the default, means all of them.
    pub probe_top: usize,
    /// Days without a read before a package counts as stale.
    pub stale_days: i64,
    /// Whether to probe access times.
    pub probe_usage: bool,
    /// Whether to measure directory sizes, which is the slow part of a scan.
    pub measure_directories: bool,
    /// Never execute anything; print what would run instead.
    pub dry_run: bool,
    /// Rows to print in `--list` mode.
    pub limit: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            mode: Mode::Interactive,
            root: PathBuf::from("/"),
            db_path: None,
            probe_top: 0,
            stale_days: 180,
            probe_usage: true,
            measure_directories: true,
            dry_run: false,
            limit: 40,
        }
    }
}

/// Why the command line could not be understood.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// An option this program does not have.
    Unknown {
        /// The offending argument.
        argument: String,
    },
    /// An option that needs a value did not get one.
    MissingValue {
        /// The option name.
        option: String,
    },
    /// A value that should have been a number was not.
    BadNumber {
        /// The option name.
        option: String,
        /// What was supplied.
        value: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unknown { argument } => {
                write!(
                    formatter,
                    "unknown option: {argument}\ntry `pacpurge --help`"
                )
            }
            Error::MissingValue { option } => {
                write!(formatter, "{option} needs a value")
            }
            Error::BadNumber { option, value } => {
                write!(formatter, "{option} expects a number, got `{value}`")
            }
        }
    }
}

/// The text printed by `--help`.
pub const HELP: &str = "\
pacpurge — find and reclaim space on an Arch system

USAGE
  pacpurge [OPTIONS]

By default pacpurge opens an interactive table of every installed package,
sorted largest first, annotated with how much removing it would really free
and when its files were last read.

MODES
  (default)          interactive terminal interface
  --list             print the package table and exit
  --clean            print the non-package cleanup targets and exit
  --diagnose         explain what the last-use probe can see on this system:
                     mount options, which evidence each source produced, and
                     what to change if the column is empty
  --json             print the whole analysis as one JSON document and exit
  -h, --help         print this text
  -V, --version      print the version

SCAN OPTIONS
  --root <PATH>      analyse an alternate filesystem root (default: /)
  --db-path <PATH>   pacman database directory (default: from pacman.conf)
  --top <N>          probe access times for only the N largest packages.
                     The default is 0, meaning every package: a full probe of
                     a 2000-package system costs about a third of a second,
                     and a bounded one leaves most of the table blank. When
                     set, AUR packages and orphans are probed regardless of
                     size.
  --stale-days <N>   days without a read before a package counts as stale
                     (default: 180)
  --no-usage         skip access-time probing entirely
  --quick            skip directory size measurement, which is the slow part
  --limit <N>        rows to print in --list mode (default: 40)

SAFETY
  --dry-run          never execute anything; print the commands instead

pacpurge never deletes a package itself. Removals are handed to
`pacman -Rns`, which applies its own dependency checks and its own
confirmation prompt.

SEARCH AND FILTERS
  Inside the interface, `/` filters by package name. `D` widens the search to
  descriptions as well. Filters compose with AND: `a` then `n` lists AUR
  packages that have not been read since they were installed.

LAST-USE DATA
  Package files carry an access time, which Arch updates at most once a day
  under the default `relatime` mount option. That is precise enough to tell
  software you use from software you installed once and forgot.

  Each package is judged by the strongest evidence it ships: its executables,
  or failing that its libraries, or failing that the data it installs. A font
  family or a TeX distribution ships no binary at all, so judging only by
  binaries would leave the biggest packages on the system with no verdict.
  Documentation is excluded: an indexer reading a man page says nothing about
  whether the software is used.

  Where a filesystem is mounted `noatime` the timestamps are frozen, and
  pacpurge disables the column rather than report a number that means nothing.
";

/// Parse an argument list, excluding the program name.
pub fn parse(arguments: &[String]) -> Result<Options, Error> {
    let mut options = Options::default();
    let mut iterator = arguments.iter();

    while let Some(argument) = iterator.next() {
        let mut value_for = |option: &str| -> Result<String, Error> {
            iterator.next().cloned().ok_or(Error::MissingValue {
                option: option.to_owned(),
            })
        };

        match argument.as_str() {
            "-h" | "--help" | "help" => options.mode = Mode::Help,
            "-V" | "--version" => options.mode = Mode::Version,
            "--json" => options.mode = Mode::Json,
            "--list" => options.mode = Mode::List,
            "--clean" => options.mode = Mode::Clean,
            "--diagnose" => options.mode = Mode::Diagnose,
            "--no-usage" => options.probe_usage = false,
            "--quick" => options.measure_directories = false,
            "--dry-run" => options.dry_run = true,
            "--root" => options.root = PathBuf::from(value_for("--root")?),
            "--db-path" => options.db_path = Some(PathBuf::from(value_for("--db-path")?)),
            "--top" => options.probe_top = number("--top", &value_for("--top")?)?,
            "--limit" => options.limit = number("--limit", &value_for("--limit")?)?,
            "--stale-days" => {
                let raw = value_for("--stale-days")?;
                options.stale_days = raw.parse::<i64>().map_err(|_error| Error::BadNumber {
                    option: "--stale-days".to_owned(),
                    value: raw.clone(),
                })?;
            }
            other => {
                return Err(Error::Unknown {
                    argument: other.to_owned(),
                })
            }
        }
    }

    Ok(options)
}

/// Parse a `usize` option value.
fn number(option: &str, value: &str) -> Result<usize, Error> {
    value.parse::<usize>().map_err(|_error| Error::BadNumber {
        option: option.to_owned(),
        value: value.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{parse, Error, Mode};

    fn arguments(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn no_arguments_means_the_interactive_interface() {
        let options = parse(&[]).unwrap();
        assert_eq!(options.mode, Mode::Interactive);
        assert_eq!(options.root, PathBuf::from("/"));
        assert!(options.probe_usage);
        assert!(!options.dry_run);
    }

    #[test]
    fn modes_are_selected_by_flag() {
        assert_eq!(parse(&arguments(&["--json"])).unwrap().mode, Mode::Json);
        assert_eq!(parse(&arguments(&["--list"])).unwrap().mode, Mode::List);
        assert_eq!(parse(&arguments(&["--clean"])).unwrap().mode, Mode::Clean);
        assert_eq!(parse(&arguments(&["-h"])).unwrap().mode, Mode::Help);
        assert_eq!(parse(&arguments(&["-V"])).unwrap().mode, Mode::Version);
    }

    #[test]
    fn options_taking_values_consume_the_next_argument() {
        let options = parse(&arguments(&[
            "--root",
            "/mnt/arch",
            "--top",
            "50",
            "--stale-days",
            "30",
            "--db-path",
            "/mnt/arch/var/lib/pacman",
        ]))
        .unwrap();
        assert_eq!(options.root, PathBuf::from("/mnt/arch"));
        assert_eq!(options.probe_top, 50);
        assert_eq!(options.stale_days, 30);
        assert_eq!(
            options.db_path,
            Some(PathBuf::from("/mnt/arch/var/lib/pacman"))
        );
    }

    #[test]
    fn switches_turn_features_off() {
        let options = parse(&arguments(&["--no-usage", "--quick", "--dry-run"])).unwrap();
        assert!(!options.probe_usage);
        assert!(!options.measure_directories);
        assert!(options.dry_run);
    }

    #[test]
    fn an_unknown_option_is_an_error() {
        assert_eq!(
            parse(&arguments(&["--delete-everything"])),
            Err(Error::Unknown {
                argument: "--delete-everything".to_owned()
            })
        );
    }

    #[test]
    fn a_missing_value_is_an_error() {
        assert_eq!(
            parse(&arguments(&["--root"])),
            Err(Error::MissingValue {
                option: "--root".to_owned()
            })
        );
    }

    #[test]
    fn a_non_numeric_value_is_an_error() {
        assert_eq!(
            parse(&arguments(&["--top", "lots"])),
            Err(Error::BadNumber {
                option: "--top".to_owned(),
                value: "lots".to_owned()
            })
        );
    }

    #[test]
    fn the_help_text_documents_every_option() {
        for option in [
            "--root",
            "--db-path",
            "--top",
            "--stale-days",
            "--no-usage",
            "--quick",
            "--limit",
            "--dry-run",
            "--json",
            "--list",
            "--clean",
            "--diagnose",
        ] {
            assert!(
                super::HELP.contains(option),
                "help text does not mention {option}"
            );
        }
    }
}
