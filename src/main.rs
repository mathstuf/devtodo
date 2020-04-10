// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use clap::{self, App, Arg};
use human_panic::setup_panic;
use thiserror::Error;

mod config;
mod todo;

#[derive(Debug, Error)]
#[error("setup error")]
enum SetupError {
}

fn try_main() -> Result<(), SetupError> {
    let matches = App::new("devtodo")
        .version(clap::crate_version!())
        .author("Ben Boeckel <mathstuf@gmail.com>")
        .about("Query code hosting platforms for todo items to add to a calendar")
        .get_matches();

    Ok(())
}

fn main() {
    setup_panic!();

    if let Err(err) = try_main() {
        panic!("{:?}", err);
    }
}
