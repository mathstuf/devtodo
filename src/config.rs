// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Configuration file types deserialized from the YAML config.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

/// Top-level configuration read from the devtodo YAML file.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Named account credentials, keyed by a user-chosen account name.
    #[serde(default)]
    pub accounts: BTreeMap<String, Account>,
    /// Named sync targets, keyed by a user-chosen target name.
    #[serde(default)]
    pub targets: BTreeMap<String, SyncTarget>,
    /// Names of targets to sync when no explicit `--target` flag is given.
    #[serde(default)]
    pub default_targets: Vec<String>,
}

/// Credentials for a single service account.
#[derive(Debug, Deserialize)]
pub struct Account {
    /// The service type (e.g. `"github"`, `"gitlab"`, `"forgejo"`).
    pub service: String,
    /// Optional API hostname override (defaults to the public service host).
    #[serde(default)]
    pub hostname: Option<String>,
    /// API token or other secret used to authenticate with the service.
    pub secret: String,
}

/// A directory on disk that is kept in sync with one or more source profiles.
#[derive(Debug, Deserialize)]
pub struct SyncTarget {
    /// Path to the local directory containing `.ics` todo files.
    pub directory: PathBuf,
    /// Named query profiles that feed items into this target, keyed by profile name.
    pub profiles: BTreeMap<String, Profile>,
}

/// A single query profile that maps an account to a query and optional filters.
#[derive(Debug, Deserialize)]
pub struct Profile {
    /// Name of the account (key in [`Config::accounts`]) to query.
    pub account: String,
    /// What to query on that account (the viewer's own items or specific projects).
    pub target: QueryTarget,
    /// Optional filters applied to the query results.
    #[serde(default)]
    pub filters: Vec<Filter>,
}

/// Specifies what to query on a given account.
#[derive(Debug, Deserialize)]
pub enum QueryTarget {
    /// Query items assigned to or created by the authenticated user.
    #[serde(rename = "self")]
    SelfUser,
    /// Query items in the given list of `owner/repo` projects.
    #[serde(rename = "projects")]
    Projects(Vec<String>),
}

/// A filter that can be applied to narrow query results.
#[derive(Debug, Deserialize)]
pub enum Filter {
    /// Only include items carrying the given label.
    #[serde(rename = "label")]
    Label(String),
}
