// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::cell::{LazyCell, OnceCell};

use graphql_client::GraphQLQuery;
use log::{error, warn};

use crate::account::prelude::*;
use crate::todo::{Due, TodoKind, TodoStatus};

mod client;
mod queries;

struct ConnInfo {
    host: String,
    token: String,
}

pub struct GithubQuery {
    client: LazyCell<
        client::GithubResult<client::Github>,
        Box<dyn Fn() -> client::GithubResult<client::Github>>,
    >,
    init_error_cell: OnceCell<()>,
}

struct GithubItem {
    due: Option<Due>,
    summary: String,
    description: String,
    kind: TodoKind,
    status: TodoStatus,
    url: String,
}

macro_rules! impl_issue_filter {
    ($type:path) => {
        impl $type {
            fn add_filter(&mut self, filter: &Filter) {
                match filter {
                    Filter::Label(label) => {
                        self.labels.get_or_insert_with(Vec::new).push(label.into())
                    },
                }
            }
        }
    };
}

impl_issue_filter!(queries::viewer_issues::IssueFilters);

macro_rules! impl_issue {
    ($type:path, $state:path) => {
        impl From<$type> for GithubItem {
            fn from(issue: $type) -> Self {
                let due = issue.milestone.and_then(|m| m.due_on).map(Due::DateTime);
                // TODO: Determine whether this is assigned or not.
                let kind = TodoKind::Issue;
                let status = match issue.state {
                    <$state>::CLOSED => TodoStatus::Completed,
                    <$state>::OPEN => {
                        if issue
                            .assignees
                            .assignees
                            .map(|v| v.is_empty())
                            .unwrap_or(true)
                        {
                            TodoStatus::NeedsAction
                        } else {
                            TodoStatus::InProcess
                        }
                    },
                    state => {
                        warn!("unknown github issue state: {:?}", state);
                        TodoStatus::NeedsAction
                    },
                };

                Self {
                    due,
                    summary: issue.title,
                    description: issue.body,
                    kind,
                    status,
                    url: issue.url,
                }
            }
        }
    };
}

impl_issue!(
    queries::viewer_issues::IssueInfo,
    queries::viewer_issues::IssueState
);

macro_rules! impl_pull_request {
    ($type:path, $state:path) => {
        impl From<$type> for GithubItem {
            fn from(pr: $type) -> Self {
                let due = pr.milestone.and_then(|m| m.due_on).map(Due::DateTime);
                // TODO: Determine whether this is assigned or not.
                let kind = TodoKind::PullRequest;
                let status = match pr.state {
                    <$state>::CLOSED => TodoStatus::Cancelled,
                    <$state>::MERGED => TodoStatus::Completed,
                    <$state>::OPEN => {
                        if pr.assignees.assignees.map(|v| v.is_empty()).unwrap_or(true) {
                            TodoStatus::NeedsAction
                        } else {
                            TodoStatus::InProcess
                        }
                    },
                    state => {
                        warn!("unknown github pr state: {:?}", state);
                        TodoStatus::NeedsAction
                    },
                };

                Self {
                    due,
                    summary: pr.title,
                    description: pr.body,
                    kind,
                    status,
                    url: pr.url,
                }
            }
        }
    };
}

impl_pull_request!(
    queries::viewer_pull_requests::PullRequestInfo,
    queries::viewer_pull_requests::PullRequestState
);

impl GithubQuery {
    pub fn new(host: Option<String>, token: String) -> Self {
        let conninfo = ConnInfo {
            host: host.unwrap_or_else(|| "api.github.com".into()),
            token,
        };
        GithubQuery {
            client: LazyCell::new(Box::new(move || {
                client::Github::new(&conninfo.host, &conninfo.token)
            })),
            init_error_cell: OnceCell::new(),
        }
    }

    /// Check the rate limiting for a query.
    fn check_rate_limits<R>(rate_limit: &Option<R>, name: &str)
    where
        R: Into<queries::RateLimitInfo> + Clone,
    {
        if let Some(info) = rate_limit.as_ref() {
            info.clone().into().inspect(name);
        }
    }

    fn query_user(
        client: &client::Github,
        filters: &[Filter],
    ) -> Result<Vec<GithubItem>, ItemError> {
        let mut issue_filters = queries::viewer_issues::IssueFilters {
            assignee: None,
            created_by: None,
            labels: None,
            mentioned: None,
            milestone: None,
            milestone_number: None,
            since: None,
            states: None,
            type_: None,
            viewer_subscribed: None,
        };
        for filter in filters {
            issue_filters.add_filter(filter);
        }

        let mut input = queries::viewer_issues::Variables {
            filter_by: issue_filters,
            cursor: None,
        };

        let mut items = Vec::new();

        // Query for issue information.
        loop {
            let query = queries::ViewerIssues::build_query(input.clone());
            let rsp = client
                .send::<queries::ViewerIssues>(&query)
                .map_err(|err| {
                    error!("failed to send viewer issue query: {err:?}");
                    let message = format!("failed to send viewer issue query: {err}");
                    ItemError::QueryError {
                        service: "github",
                        message,
                    }
                })?;

            Self::check_rate_limits(
                &rsp.rate_limit_info.rate_limit,
                queries::ViewerIssues::name(),
            );
            let (issues, page_info) = (rsp.viewer.issues.items, rsp.viewer.issues.page_info);
            if let Some(issues) = issues {
                items.extend(issues.into_iter().flatten().map(|issue| issue.into()));
            }

            if page_info.has_next_page {
                assert!(
                    page_info.end_cursor.is_some(),
                    "GitHub lied to us and said there is another page, but didn't give us an end \
                     cursor. Bailing to avoid an infinite loop.",
                );
                input.cursor = page_info.end_cursor;
            } else {
                break;
            }
        }

        let mut input = queries::viewer_pull_requests::Variables {
            labels: None,
            cursor: None,
        };
        for filter in filters {
            match filter {
                Filter::Label(label) => {
                    input
                        .labels
                        .get_or_insert_with(Vec::new)
                        .push(label.clone())
                },
            }
        }

        // Query for pull requests information.
        loop {
            let query = queries::ViewerPullRequests::build_query(input.clone());
            let rsp = client
                .send::<queries::ViewerPullRequests>(&query)
                .map_err(|err| {
                    error!("failed to send viewer pull request query: {err:?}");
                    let message = format!("failed to send viewer pull request query: {err}");
                    ItemError::QueryError {
                        service: "github",
                        message,
                    }
                })?;

            Self::check_rate_limits(
                &rsp.rate_limit_info.rate_limit,
                queries::ViewerIssues::name(),
            );
            let (prs, page_info) = (
                rsp.viewer.pull_requests.items,
                rsp.viewer.pull_requests.page_info,
            );
            if let Some(prs) = prs {
                items.extend(prs.into_iter().flatten().map(|pr| pr.into()));
            }

            if page_info.has_next_page {
                assert!(
                    page_info.end_cursor.is_some(),
                    "GitHub lied to us and said there is another page, but didn't give us an end \
                     cursor. Bailing to avoid an infinite loop.",
                );
                input.cursor = page_info.end_cursor;
            } else {
                break;
            }
        }

        Ok(items)
    }

    fn query_projects(
        client: &client::Github,
        projects: &[String],
        filters: &[Filter],
    ) -> Result<Vec<GithubItem>, ItemError> {
        unimplemented!()
    }
}

impl ItemSource for GithubQuery {
    fn fetch_items(
        &self,
        target: &QueryTarget,
        filters: &[Filter],
        existing_items: &mut ItemLookup,
    ) -> Result<Vec<TodoItem>, ItemError> {
        let client = self.client.as_ref().map_err(|err| {
            self.init_error_cell.get_or_init(|| {
                error!("failed to connect to github instance: {err:?}");
            });
            ItemError::ServiceError {
                service: "github",
            }
        })?;

        let results = match target {
            QueryTarget::SelfUser => Self::query_user(client, filters),
            QueryTarget::Projects(projects) => Self::query_projects(client, projects, filters),
        };

        Ok(results?
            .into_iter()
            .filter_map(|result| {
                if let Some(item) = existing_items.get_mut(&result.url) {
                    if let Some(due) = result.due {
                        item.set_due(due);
                    }
                    item.set_status(result.status);
                    item.set_summary(result.summary);
                    item.set_description(result.description);

                    None
                } else {
                    let mut item = TodoItem::builder();

                    item.kind(result.kind)
                        .status(result.status)
                        .url(result.url.clone())
                        .summary(result.summary)
                        .description(result.description);

                    if let Some(due) = result.due {
                        item.due(due);
                    }

                    let item = item.build().expect("all item fields should be provided");

                    Some(item)
                }
            })
            .collect())
    }
}
