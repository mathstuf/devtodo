// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use lazy_init::LazyTransform;
use log::error;
use once_cell::sync::OnceCell;

use crate::account::prelude::*;
use crate::todo::{Due, TodoKind, TodoStatus};

mod client;
mod queries;

struct ConnInfo {
    host: String,
    token: String,
}

pub struct GithubQuery {
    client: LazyTransform<ConnInfo, client::GithubResult<client::Github>>,
    init_error_cell: OnceCell<()>,
}

struct GithubItem {
    due: Option<Due>,
    summary: String,
    description: Option<String>,
    kind: TodoKind,
    status: TodoStatus,
    url: String,
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

    fn query_user(client: &client::Github, filters: &[Filter]) -> Result<Vec<GithubItem>, ItemError> {
        unimplemented!()
    }

    fn query_projects(client: &client::Github, projects: &[String], filters: &[Filter]) -> Result<Vec<GithubItem>, ItemError> {
        unimplemented!()
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

        let results = match target {
            QueryTarget::SelfUser => {
                Self::query_user(client, filters)
            },
            QueryTarget::Projects(projects) => {
                Self::query_projects(client, projects, filters)
            },
        };

        Ok(results?
            .into_iter()
            .filter_map(|result| {
                if let Some(&item) = existing_items(&result.url) {
                    if let Some(due) = result.due {
                        item.set_due(due);
                    }
                    item.set_status(result.status);
                    item.set_summary(result.summary);
                    if let Some(description) = result.description {
                        item.set_description(description);
                    }

                    None
                } else {
                    let mut item = TodoItem::builder();

                    item.kind(result.kind)
                        .status(result.status)
                        .url(result.url)
                        .summary(result.summary);

                    if let Some(due) = result.due {
                        item.due(due);
                    }
                    if let Some(description) = result.description {
                        item.description(description);
                    }

                    let item = item.build()
                        .expect("all item fields should be provided");

                    Some(item)
                }
            })
            .collect())
    }
}
