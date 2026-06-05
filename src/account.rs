// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::config::{Account, Filter, QueryTarget};
use crate::todo::TodoItem;

mod prelude;

#[cfg(feature = "github")]
mod github;

#[cfg(feature = "gitlab")]
mod gitlab;

#[cfg(feature = "forgejo")]
mod forgejo;

#[derive(Debug, Error)]
#[error("failed to fetch items")]
pub enum ItemError {
    #[error("service error for {}", service)]
    ServiceError { service: &'static str },
    #[error("query error for {}: {}", service, message)]
    QueryError {
        service: &'static str,
        message: String,
    },
}

pub type ItemLookup<'item> = BTreeMap<String, &'item mut TodoItem>;

pub trait ItemSource {
    fn fetch_items(
        &self,
        target: &QueryTarget,
        filters: &[Filter],
        existing_items: &mut ItemLookup,
    ) -> Result<Vec<TodoItem>, ItemError>;
}

#[derive(Debug, Error)]
pub enum AccountError {
    #[cfg(not(all(feature = "github", feature = "gitlab", feature = "forgejo")))]
    #[error("unsupported service: {}", service)]
    UnsupportedService { service: &'static str },
    #[error("unknown service: {}", service)]
    UnknownService { service: String },
}

#[expect(clippy::single_call_fn, reason = "function size")]
pub fn connect(account: Account) -> Result<Box<dyn ItemSource>, AccountError> {
    match account.service.as_ref() {
        #[cfg(feature = "github")]
        "github" => {
            Ok(Box::new(github::GithubQuery::new(
                account.hostname,
                account.secret,
            )))
        },
        #[cfg(not(feature = "github"))]
        "github" => {
            Err(AccountError::UnsupportedService {
                service: "github",
            })
        },

        #[cfg(feature = "gitlab")]
        "gitlab" => {
            Ok(Box::new(gitlab::GitlabQuery::new(
                account.hostname,
                account.secret,
            )))
        },
        #[cfg(not(feature = "gitlab"))]
        "gitlab" => {
            Err(AccountError::UnsupportedService {
                service: "gitlab",
            })
        },

        #[cfg(feature = "forgejo")]
        "forgejo" => {
            Ok(Box::new(forgejo::ForgejoQuery::new(
                account.hostname,
                account.secret,
            )))
        },
        #[cfg(not(feature = "forgejo"))]
        "forgejo" => {
            Err(AccountError::UnsupportedService {
                service: "forgejo",
            })
        },

        service => {
            Err(AccountError::UnknownService {
                service: service.into(),
            })
        },
    }
}
