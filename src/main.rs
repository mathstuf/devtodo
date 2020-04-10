// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use clap::{self, App, Arg};
use human_panic::setup_panic;
use log::*;
use thiserror::Error;

mod config;
mod todo;

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
    #[error("log error")]
    LogError {
        #[from]
        source: LogError,
    },
}

fn try_main() -> Result<(), SetupError> {
    let matches = App::new("devtodo")
        .version(clap::crate_version!())
        .author("Ben Boeckel <mathstuf@gmail.com>")
        .about("Query code hosting platforms for todo items to add to a calendar")
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

    Ok(())
}

fn main() {
    setup_panic!();

    if let Err(err) = try_main() {
        error!("{:?}", err);
        panic!("{:?}", err);
    }
}
