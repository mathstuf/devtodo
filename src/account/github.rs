// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use lazy_init::LazyTransform;
use log::error;
use once_cell::sync::OnceCell;

use crate::account::prelude::*;

mod client;

struct ConnInfo {
    host: String,
    token: String,
}

pub struct GithubQuery {
    client: LazyTransform<ConnInfo, client::GithubResult<client::Github>>,
    init_error_cell: OnceCell<()>,
}

impl GithubQuery {
    pub fn new(host: Option<String>, token: String) -> Self {
        GithubQuery {
            client: LazyTransform::new(ConnInfo {
                host: host.unwrap_or_else(|| "api.github.com".into()),
                token,
            }),
            init_error_cell: OnceCell::new(),
        }
    }
}

impl ItemSource for GithubQuery {
    fn fetch_items<'a, 'b>(&self, target: &QueryTarget, filters: &[Filter], existing_items: &dyn Fn(&'b str) -> Option<&&'a mut TodoItem>) -> Result<Vec<TodoItem>, ItemError> {
        let client = self.client
            .get_or_create(|info| client::Github::new(&info.host, &info.token))
            .as_ref()
            .map_err(|err| {
                self.init_error_cell.get_or_init(|| {
                    error!(
                        "failed to connect to github instance: {:?}",
                        err,
                    );
                });
                ItemError::ServiceError {
                    service: "github",
                }
            })?;

        unimplemented!()
    }
}
