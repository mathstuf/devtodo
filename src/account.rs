// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::config::{Account, Filter, QueryTarget};
use crate::todo::TodoItem;

/// Shared imports used by all account backend modules.
mod prelude;

#[cfg(feature = "github")]
/// GitHub account backend.
mod github;

#[cfg(feature = "gitlab")]
mod gitlab;

#[cfg(feature = "forgejo")]
mod forgejo;

/// Errors that can occur while fetching items from an account backend.
#[derive(Debug, Error)]
#[error("failed to fetch items")]
pub enum ItemError {
    /// The backing service is unavailable or not compiled in.
    #[error("service error for {}", service)]
    ServiceError {
        /// The name of the service that encountered an error.
        service: &'static str,
    },
    /// A query to the backing service failed.
    #[error("query error for {}: {}", service, message)]
    QueryError {
        /// The name of the service that returned an error.
        service: &'static str,
        /// A human-readable description of what went wrong.
        message: String,
    },
}

impl ItemError {
    /// Construct a `QueryError` for the given `service` and `message`.
    pub fn query_error<M>(service: &'static str, message: M) -> Self
    where
        M: Into<String>,
    {
        Self::QueryError {
            service,
            message: message.into(),
        }
    }
}

/// Map from item URL to a mutable reference to the existing [`TodoItem`] for that URL.
pub type ItemLookup<'item> = BTreeMap<String, &'item mut TodoItem>;

/// Trait implemented by each account backend that can fetch todo items.
pub trait ItemSource {
    /// Fetch items from this source, updating `existing_items` in place and returning new ones.
    fn fetch_items(
        &self,
        target: &QueryTarget,
        filters: &[Filter],
        existing_items: &mut ItemLookup,
    ) -> Result<Vec<TodoItem>, ItemError>;
}

/// Errors that can occur when connecting to an account backend.
#[derive(Debug, Error)]
pub enum AccountError {
    /// The requested service exists but was not compiled in.
    #[cfg(not(all(feature = "github", feature = "gitlab", feature = "forgejo")))]
    #[error("unsupported service: {}", service)]
    UnsupportedService {
        /// The name of the unsupported service.
        service: &'static str,
    },
    /// The requested service name is not recognised at all.
    #[error("unknown service: {}", service)]
    UnknownService {
        /// The unrecognised service name from the configuration file.
        service: String,
    },
}

/// Connect to the account described by `account` and return an [`ItemSource`] for it.
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
                &account.secret,
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
