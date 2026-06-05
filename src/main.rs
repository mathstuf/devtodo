// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clap::builder::PossibleValuesParser;
use clap::{self, Arg, ArgAction, Command};
use directories::ProjectDirs;
use human_panic::setup_panic;
use log::{error, warn, LevelFilter};
use thiserror::Error;

/// Account backend integrations.
mod account;
/// Configuration file parsing.
mod config;
/// iCalendar todo-file reading and writing.
mod todo;

use self::config::Config;
use self::todo::TodoFile;

/// Errors that can occur when initialising the logging backend.
#[derive(Debug, Error)]
enum LogError {
    /// The user requested a logging backend that is not recognised.
    #[error("unknown logger: {}", _0)]
    UnknownLogger(String),
}

/// The active logging backend.
enum Logger {
    /// Log output driven by the `RUST_LOG` environment variable.
    Env,
}

/// Top-level errors that can occur during startup or sync.
#[derive(Debug, Error)]
enum SetupError {
    #[error("failed to determine project directories")]
    /// Could not determine the platform-specific project directory.
    NoProjectDir,
    #[error("failed to read configuration file {}", path.display())]
    /// Reading the configuration file from disk failed.
    ReadConfig {
        /// Path of the config file that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    #[error("failed to parse configuration file {}", path.display())]
    /// Deserializing the configuration file failed.
    ParseConfig {
        /// Path of the config file that could not be parsed.
        path: PathBuf,
        /// The underlying parse error.
        source: serde_saphyr::Error,
    },
    #[error("log error")]
    /// Setting up the logging backend failed.
    LogError {
        #[from]
        /// The underlying log-setup error.
        source: LogError,
    },
    #[error("account error for {}", name)]
    /// Connecting to a service account failed.
    Account {
        /// The name of the account from the configuration file.
        name: String,
        /// The underlying account-connection error.
        source: account::AccountError,
    },
    #[error("failed to read directory {} for {}", path.display(), name)]
    /// Reading the todo directory for a sync target failed.
    ReadDir {
        /// Path of the directory that could not be read.
        path: PathBuf,
        /// Name of the sync target whose directory could not be read.
        name: String,
        /// The underlying I/O error.
        source: io::Error,
    },
    #[error("failed to read file for {}", name)]
    /// Reading a directory entry within a sync target failed.
    ReadEntry {
        /// Name of the sync target in which the read failed.
        name: String,
        /// The underlying I/O error.
        source: io::Error,
    },
    #[error("failed to read todo information from {}", path.display())]
    /// Parsing a `.ics` file as a todo item failed.
    TodoFile {
        /// Path of the `.ics` file that could not be parsed.
        path: PathBuf,
        /// The underlying todo-parse error.
        source: todo::TodoError,
    },
    #[error("no such account {}", name)]
    /// A profile referenced an account name that is not in the configuration.
    NoSuchAccount {
        /// The missing account name.
        name: String,
    },
    #[error(
        "failed to fetch items from the {} account for the {} profile",
        account,
        profile
    )]
    /// Fetching items from a service account failed.
    FetchItems {
        /// The account name that was being queried.
        account: String,
        /// The profile name that triggered the fetch.
        profile: String,
        /// The underlying fetch error.
        source: account::ItemError,
    },
    #[error("failed to write {} items", errors.len())]
    /// One or more todo files could not be written back to disk.
    WriteErrors {
        /// Per-item error messages paired with the underlying [`todo::TodoError`].
        errors: Vec<(String, todo::TodoError)>,
    },
}

impl SetupError {
    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `ReadConfig` error.
    const fn read_config(path: PathBuf, source: io::Error) -> Self {
        Self::ReadConfig {
            path,
            source,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `ParseConfig` error.
    const fn parse_config(path: PathBuf, source: serde_saphyr::Error) -> Self {
        Self::ParseConfig {
            path,
            source,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct an `Account` error.
    const fn account(name: String, source: account::AccountError) -> Self {
        Self::Account {
            name,
            source,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `ReadDir` error.
    const fn read_dir(path: PathBuf, name: String, source: io::Error) -> Self {
        Self::ReadDir {
            path,
            name,
            source,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `ReadEntry` error.
    const fn read_entry(name: String, source: io::Error) -> Self {
        Self::ReadEntry {
            name,
            source,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `TodoFile` error.
    const fn todo_file(path: PathBuf, source: todo::TodoError) -> Self {
        Self::TodoFile {
            path,
            source,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `NoSuchAccount` error.
    const fn no_such_account(name: String) -> Self {
        Self::NoSuchAccount {
            name,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `FetchItems` error.
    const fn fetch_items(account: String, profile: String, source: account::ItemError) -> Self {
        Self::FetchItems {
            account,
            profile,
            source,
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `WriteErrors` error.
    const fn write_errors(errors: Vec<(String, todo::TodoError)>) -> Self {
        Self::WriteErrors {
            errors,
        }
    }
}

#[expect(clippy::single_call_fn, reason = "function size")]
/// Read all `.ics` todo files from `dirpath`, skipping non-files and unrecognised entries.
fn read_directory(dirpath: &Path, name: &str) -> Result<Vec<TodoFile>, SetupError> {
    let mut todo_files = Vec::new();
    let dir_iter = fs::read_dir(dirpath)
        .map_err(|err| SetupError::read_dir(dirpath.into(), name.into(), err))?;
    for dir_entry in dir_iter {
        let entry = dir_entry.map_err(|err| SetupError::read_entry(name.into(), err))?;
        let path = entry.path();

        // Only look at `.ics` files.
        if path.extension().is_none_or(|ext| ext != "ics") {
            continue;
        }

        // Check the filetype.
        match entry.metadata() {
            Ok(md) => {
                let filetype = md.file_type();
                if filetype.is_dir() {
                    // Ignore directories.
                    continue;
                }
                // Get the actual file we're dealing with here.
                let real_filetype = if filetype.is_symlink() {
                    match path.metadata() {
                        Ok(real_md) => real_md.file_type(),
                        Err(err) => {
                            warn!(
                                "failed to read target metadata for {}: {err}; ignoring",
                                path.display(),
                            );
                            continue;
                        },
                    }
                } else {
                    filetype
                };
                // Ignore non-files.
                if !real_filetype.is_file() {
                    continue;
                }
            },
            Err(err) => {
                warn!(
                    "failed to read metadata for {}: {err}; ignoring",
                    path.display(),
                );
                continue;
            },
        }

        if let Some(todo_file) =
            TodoFile::from_path(&path).map_err(|err| SetupError::todo_file(path, err))?
        {
            todo_files.push(todo_file);
        }
    }

    Ok(todo_files)
}

#[expect(clippy::single_call_fn, reason = "separate concerns")]
/// Entry point with a `Result` return so that `main` can report errors uniformly.
fn try_main() -> Result<(), SetupError> {
    let matches = Command::new("devtodo")
        .version(clap::crate_version!())
        .author("Ben Boeckel <mathstuf@gmail.com>")
        .about("Query code hosting platforms for todo items to add to a calendar")
        .arg(
            Arg::new("CONFIG")
                .short('c')
                .long("config")
                .help("Path to the configuration file")
                .value_name("FILE")
                .action(ArgAction::Set),
        )
        .arg(
            Arg::new("ALL_TARGETS")
                .short('a')
                .long("all-targets")
                .help("Sync all targets")
                .conflicts_with("TARGET"),
        )
        .arg(
            Arg::new("TARGET")
                .short('t')
                .long("target")
                .help("Name of a target to sync")
                .action(ArgAction::Append)
                .number_of_values(1),
        )
        .arg(
            Arg::new("DEBUG")
                .short('d')
                .long("debug")
                .help("Increase verbosity")
                .action(ArgAction::Count),
        )
        .arg(
            Arg::new("LOGGER")
                .short('l')
                .long("logger")
                .default_value("env")
                .value_parser(PossibleValuesParser::new(["env"]))
                .help("Logging backend")
                .value_name("LOGGER")
                .action(ArgAction::Set),
        )
        .get_matches();

    let log_level = match matches.get_one::<u8>("DEBUG").copied().unwrap_or(0) {
        0 => LevelFilter::Error,
        1 => LevelFilter::Warn,
        2 => LevelFilter::Info,
        3 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };

    let _logger = match matches
        .get_one::<String>("LOGGER")
        .expect("logger should have a value")
        .as_ref()
    {
        "env" => {
            env_logger::Builder::new().filter(None, log_level).init();
            Logger::Env
        },

        logger => {
            return Err(LogError::UnknownLogger(logger.into()).into());
        },
    };

    log::set_max_level(log_level);

    let basedirs = ProjectDirs::from("net.benboeckel.devtodo", "", "devtodo")
        .ok_or(SetupError::NoProjectDir)?;
    let config: Config = {
        let config_path = matches.get_one::<String>("CONFIG").map_or_else(
            || basedirs.config_dir().join("devtodo.yaml"),
            |config| Path::new(config).into(),
        );
        let contents = fs::read_to_string(&config_path)
            .map_err(|err| SetupError::read_config(config_path.clone(), err))?;
        serde_saphyr::from_str(&contents)
            .map_err(|err| SetupError::parse_config(config_path, err))?
    };

    let accounts = config
        .accounts
        .into_iter()
        .map(|(name, account)| {
            let item_source =
                account::connect(account).map_err(|err| SetupError::account(name.clone(), err))?;
            Ok((name, item_source))
        })
        .collect::<Result<BTreeMap<_, _>, SetupError>>()?;

    let targets = if matches.get_flag("ALL_TARGETS") {
        config.targets.keys().cloned().collect()
    } else {
        matches
            .get_many::<String>("TARGET")
            .map(|values| values.map(Into::into).collect())
            .unwrap_or(config.default_targets)
    };

    let targets_to_use = config
        .targets
        .into_iter()
        .filter(|(name, _)| targets.iter().any(|target| target == name))
        .collect::<BTreeMap<_, _>>();

    let mut errors = Vec::new();
    for (name, target) in targets_to_use {
        let mut todo_files = read_directory(&target.directory, &name)?;
        let mut url_map = todo_files
            .iter_mut()
            .map(|todo_file| (todo_file.item.url().into(), &mut todo_file.item))
            .collect::<BTreeMap<String, _>>();

        let mut all_new_items = Vec::new();
        for (profile_name, profile) in target.profiles {
            let item_source = accounts
                .get(&profile.account)
                .ok_or_else(|| SetupError::no_such_account(profile.account.clone()))?;
            let new_items = item_source
                .fetch_items(&profile.target, &profile.filters, &mut url_map)
                .map_err(|err| SetupError::fetch_items(profile.account, profile_name, err))?;
            all_new_items.extend(new_items);
        }

        let mut write_item = |url: String, item| {
            if let Err(err) = item {
                error!("failed to write todo for {url} in the {name} target: {err:?}");
                errors.push((
                    format!("failed to write todo for {url} in the {name} target: {err}"),
                    err,
                ));
            }
        };

        for todo_item in all_new_items {
            let url = todo_item.url().into();
            write_item(
                url,
                TodoFile::from_item(&target.directory, todo_item).map(|_| ()),
            );
        }

        for mut todo_file in todo_files {
            let url = todo_file.item.url().into();
            write_item(url, todo_file.write());
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(SetupError::write_errors(errors))
    }
}

fn main() {
    setup_panic!();

    #[expect(clippy::panic, reason = "Surfacing any error during execution")]
    if let Err(err) = try_main() {
        error!("{err:?}");
        panic!("{:?}", err);
    }
}
