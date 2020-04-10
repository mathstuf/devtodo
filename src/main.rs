// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clap::{self, App, Arg};
use directories::ProjectDirs;
use human_panic::setup_panic;
use log::*;
use thiserror::Error;

mod account;
mod config;
mod todo;

use self::config::Config;

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
    ReadConfig {
        path: PathBuf,
        source: io::Error,
    },
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
}

fn try_main() -> Result<(), SetupError> {
    let matches = App::new("devtodo")
        .version(clap::crate_version!())
        .author("Ben Boeckel <mathstuf@gmail.com>")
        .about("Query code hosting platforms for todo items to add to a calendar")
        .arg(
            Arg::with_name("CONFIG")
                .short("c")
                .long("config")
                .help("Path to the configuration file")
                .value_name("FILE")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("DEBUG")
                .short("d")
                .long("debug")
                .help("Increase verbosity")
                .multiple(true),
        )
        .arg(
            Arg::with_name("LOGGER")
                .short("l")
                .long("logger")
                .default_value("env")
                .possible_values(&[
                    "env",
                ])
                .help("Logging backend")
                .value_name("LOGGER")
                .takes_value(true),
        )
        .get_matches();

    let log_level = match matches.occurrences_of("DEBUG") {
        0 => LevelFilter::Error,
        1 => LevelFilter::Warn,
        2 => LevelFilter::Info,
        3 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };

    let _logger = match matches
        .value_of("LOGGER")
        .expect("logger should have a value")
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
        let config_path = if let Some(config) = matches.value_of("CONFIG") {
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
        serde_yaml::from_value(doc)
            .map_err(|err| SetupError::parse_config(config_path, err))?
    };

    let accounts = config
        .accounts
        .into_iter()
        .map(|(name, account)| {
            let item_source = account::connect(account)
                .map_err(|err| SetupError::account(name.clone(), err))?;
            Ok((name, item_source))
        })
        .collect::<Result<BTreeMap<_, _>, SetupError>>()?;

    Ok(())
}

fn main() {
    setup_panic!();

    if let Err(err) = try_main() {
        error!("{:?}", err);
        panic!("{:?}", err);
    }
}
