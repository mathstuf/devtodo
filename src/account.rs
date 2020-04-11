// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use thiserror::Error;

use crate::config::{Account, Filter, QueryTarget};
use crate::todo::TodoItem;

mod prelude;

#[derive(Debug, Error)]
#[error("failed to fetch items")]
pub enum ItemError {
    #[error("unreachable host: {}", host)]
    UnreachableHost {
        host: String,
    },
    #[error("invalid secret")]
    InvalidSecrets,
}

pub trait ItemSource {
    fn fetch_items<'a, 'b>(&self, target: &QueryTarget, filters: &[Filter], existing_items: &dyn Fn(&'b str) -> Option<&&'a mut TodoItem>) -> Result<Vec<TodoItem>, ItemError>;
}

#[derive(Debug, Error)]
pub enum AccountError {
    #[error("unknown service: {}", service)]
    UnknownService {
        service: String,
    },
}

pub fn connect(account: Account) -> Result<Box<dyn ItemSource>, AccountError> {
    match account.service {
        service => {
            Err(AccountError::UnknownService {
                service,
            })
        },
    }
}
