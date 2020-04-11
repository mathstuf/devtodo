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
    #[serde(default)]
    pub accounts: BTreeMap<String, Account>,
    #[serde(default)]
    pub targets: BTreeMap<String, SyncTarget>,
    #[serde(default)]
    pub default_targets: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Account {
    pub service: String,
    #[serde(default)]
    hostname: Option<String>,
    secret: String,
}

#[derive(Debug, Deserialize)]
pub struct SyncTarget {
    pub directory: PathBuf,
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Deserialize)]
pub struct Profile {
    pub account: String,
    pub target: QueryTarget,
    #[serde(default)]
    pub filters: Vec<Filter>,
}

#[derive(Debug, Deserialize)]
pub enum QueryTarget {
    #[serde(rename = "self")]
    SelfUser,
    #[serde(rename = "projects")]
    Projects(Vec<String>),
}

#[derive(Debug, Deserialize)]
pub enum Filter {
    #[serde(rename = "label")]
    Label(String),
}
