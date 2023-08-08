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
use log::*;
use thiserror::Error;

mod account;
mod config;
mod todo;

use self::config::Config;
use self::todo::TodoFile;

#[derive(Debug, Error)]
enum LogError {
    #[error("unknown logger: {}", _0)]
    UnknownLogger(String),
}

enum Logger {
    Env,
}

#[derive(Debug, Error)]
enum SetupError {
    #[error("failed to determine project directories")]
    NoProjectDir,
    #[error("failed to read configuration file {}", path.display())]
    ReadConfig { path: PathBuf, source: io::Error },
    #[error("failed to parse configuration file {}", path.display())]
    ParseConfig {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("failed to handle merge keys in configuration file {}", path.display())]
    MergeKeys {
        path: PathBuf,
        source: yaml_merge_keys::MergeKeyError,
    },
    #[error("log error")]
    LogError {
        #[from]
        source: LogError,
    },
    #[error("account error for {}", name)]
    Account {
        name: String,
        source: account::AccountError,
    },
    #[error("failed to read directory {} for {}", path.display(), name)]
    ReadDir {
        path: PathBuf,
        name: String,
        source: io::Error,
    },
    #[error("failed to read file for {}", name)]
    ReadEntry { name: String, source: io::Error },
    #[error("failed to read todo information from {}", path.display())]
    TodoFile {
        path: PathBuf,
        source: todo::TodoError,
    },
    #[error("no such account {}", name)]
    NoSuchAccount { name: String },
    #[error(
        "failed to fetch items from the {} account for the {} profile",
        account,
        profile
    )]
    FetchItems {
        account: String,
        profile: String,
        source: account::ItemError,
    },
    #[error("failed to write {} items", errors.len())]
    WriteErrors {
        errors: Vec<(String, todo::TodoError)>,
    },
}

impl SetupError {
    fn read_config(path: PathBuf, source: io::Error) -> Self {
        Self::ReadConfig {
            path,
            source,
        }
    }

    fn parse_config(path: PathBuf, source: serde_yaml::Error) -> Self {
        Self::ParseConfig {
            path,
            source,
        }
    }

    fn merge_keys(path: PathBuf, source: yaml_merge_keys::MergeKeyError) -> Self {
        Self::MergeKeys {
            path,
            source,
        }
    }

    fn account(name: String, source: account::AccountError) -> Self {
        Self::Account {
            name,
            source,
        }
    }

    fn read_dir(path: PathBuf, name: String, source: io::Error) -> Self {
        Self::ReadDir {
            path,
            name,
            source,
        }
    }

    fn read_entry(name: String, source: io::Error) -> Self {
        Self::ReadEntry {
            name,
            source,
        }
    }

    fn todo_file(path: PathBuf, source: todo::TodoError) -> Self {
        Self::TodoFile {
            path,
            source,
        }
    }

    fn no_such_account(name: String) -> Self {
        Self::NoSuchAccount {
            name,
        }
    }

    fn fetch_items(account: String, profile: String, source: account::ItemError) -> Self {
        Self::FetchItems {
            account,
            profile,
            source,
        }
    }

    fn write_errors(errors: Vec<(String, todo::TodoError)>) -> Self {
        Self::WriteErrors {
            errors,
        }
    }
}

fn read_directory(dirpath: &Path, name: &str) -> Result<Vec<TodoFile>, SetupError> {
    let mut todo_files = Vec::new();
    let dir_iter = fs::read_dir(dirpath)
        .map_err(|err| SetupError::read_dir(dirpath.into(), name.into(), err))?;
    for entry in dir_iter {
        let entry = entry.map_err(|err| SetupError::read_entry(name.into(), err))?;
        let path = entry.path();

        // Only look at `.ics` files.
        if path.extension().map(|ext| ext != "ics").unwrap_or(true) {
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
                                "failed to read target metadata for {}: {}; ignoring",
                                path.display(),
                                err,
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
                    "failed to read metadata for {}: {}; ignoring",
                    path.display(),
                    err,
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
        let config_path = if let Some(config) = matches.get_one::<String>("CONFIG") {
            Path::new(config).into()
        } else {
            basedirs.config_dir().join("devtodo.yaml")
        };
        let contents = fs::read_to_string(&config_path)
            .map_err(|err| SetupError::read_config(config_path.clone(), err))?;
        let doc = serde_yaml::from_str(&contents)
            .map_err(|err| SetupError::parse_config(config_path.clone(), err))?;
        let doc = yaml_merge_keys::merge_keys_serde(doc)
            .map_err(|err| SetupError::merge_keys(config_path.clone(), err))?;
        serde_yaml::from_value(doc).map_err(|err| SetupError::parse_config(config_path, err))?
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
        for (name, profile) in target.profiles {
            let item_source = accounts
                .get(&profile.account)
                .ok_or_else(|| SetupError::no_such_account(profile.account.clone()))?;
            let new_items = item_source
                .fetch_items(&profile.target, &profile.filters, &mut url_map)
                .map_err(|err| SetupError::fetch_items(profile.account, name, err))?;
            all_new_items.extend(new_items);
        }

        let mut write_item = |url: String, item| {
            if let Err(err) = item {
                error!(
                    "failed to write todo for {} in the {} target: {:?}",
                    url, name, err,
                );
                errors.push((
                    format!(
                        "failed to write todo for {} in the {} target: {}",
                        url, name, err,
                    ),
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

    if let Err(err) = try_main() {
        error!("{:?}", err);
        panic!("{:?}", err);
    }
}
