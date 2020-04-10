// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_derive::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    accounts: BTreeMap<String, Account>,
    targets: BTreeMap<String, SyncTarget>,
    default_targets: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Account {
    service: String,
    hostname: Option<String>,
    secret: String,
}

#[derive(Debug, Deserialize)]
pub struct SyncTarget {
    directory: PathBuf,
    profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Deserialize)]
pub struct Profile {
    account: String,
    target: QueryTarget,
    filters: Vec<Filter>,
}

#[derive(Debug, Deserialize)]
pub enum QueryTarget {
    User,
    Projects(Vec<String>),
}

#[derive(Debug, Deserialize)]
pub enum Filter {
    Label(String),
}
