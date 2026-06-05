// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::cell::{LazyCell, OnceCell};

use graphql_client::GraphQLQuery as _;
use log::{error, warn};

use crate::account::prelude::*;
use crate::todo::{Due, TodoKind, TodoStatus};

/// Low-level HTTP client for the GitHub GraphQL API.
mod client;
/// GraphQL query definitions and associated helpers.
mod queries;

/// Connection parameters for a GitHub instance.
struct ConnInfo {
    /// The API hostname (e.g. `"api.github.com"` or a GHES hostname).
    host: String,
    /// The personal access token used to authenticate requests.
    token: String,
}

/// An [`ItemSource`] that queries a GitHub instance.
pub struct GithubQuery {
    /// Lazily-initialized GitHub client; holds the construction result.
    client: LazyCell<
        client::GithubResult<client::Github>,
        Box<dyn Fn() -> client::GithubResult<client::Github>>,
    >,
    /// Stores the unit value once an initialization error has been logged.
    init_error_cell: OnceCell<()>,
}

/// A single item retrieved from the GitHub API, prior to conversion into a [`TodoItem`].
struct GithubItem {
    /// Due date derived from the item's milestone, if any.
    due: Option<Due>,
    /// Short title of the item.
    summary: String,
    /// Body text of the item.
    description: String,
    /// The kind of upstream item.
    kind: TodoKind,
    /// Current completion status of the item.
    status: TodoStatus,
    /// Canonical URL of the item on GitHub.
    url: String,
    /// Labels applied to the item.
    labels: Vec<String>,
    /// Milestone title associated with the item, if any.
    milestone: Option<String>,
    /// Whether this is a draft pull request.
    draft: bool,
}

/// Implement `add_filter` for a GraphQL issue-filter type.
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

/// Implement `From<$type> for GithubItem` for a GraphQL issue response type.
macro_rules! impl_issue {
    ($type:path, $state:path) => {
        impl From<$type> for GithubItem {
            fn from(issue: $type) -> Self {
                let due = issue
                    .milestone
                    .as_ref()
                    .and_then(|milestone| milestone.due_on)
                    .map(Due::DateTime);
                let milestone = issue
                    .milestone
                    .as_ref()
                    .map(|milestone| milestone.title.clone());
                // TODO: Determine whether this is assigned or not.
                let kind = TodoKind::Issue;
                let status = match issue.state {
                    <$state>::CLOSED => TodoStatus::Completed,
                    <$state>::OPEN => {
                        if issue
                            .assignees
                            .assignees
                            .map(|assignees| assignees.is_empty())
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
                let labels = issue
                    .labels
                    .and_then(|labels| labels.labels)
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                    .map(|label| label.name)
                    .collect();

                Self {
                    due,
                    summary: issue.title,
                    description: issue.body,
                    kind,
                    status,
                    url: issue.url,
                    labels,
                    milestone,
                    draft: false,
                }
            }
        }
    };
}

impl_issue!(
    queries::viewer_issues::IssueInfo,
    queries::viewer_issues::IssueState
);
impl_issue!(
    queries::repository_issues::IssueInfo,
    queries::repository_issues::IssueState
);

/// Implement `From<$type> for GithubItem` for a GraphQL pull-request response type.
macro_rules! impl_pull_request {
    ($type:path, $state:path) => {
        impl From<$type> for GithubItem {
            fn from(pr: $type) -> Self {
                let due = pr
                    .milestone
                    .as_ref()
                    .and_then(|milestone| milestone.due_on)
                    .map(Due::DateTime);
                let milestone = pr
                    .milestone
                    .as_ref()
                    .map(|milestone| milestone.title.clone());
                let draft = pr.is_draft;
                // TODO: Determine whether this is assigned or not.
                let kind = TodoKind::PullRequest;
                let status = match pr.state {
                    <$state>::CLOSED => TodoStatus::Cancelled,
                    <$state>::MERGED => TodoStatus::Completed,
                    <$state>::OPEN => {
                        if pr
                            .assignees
                            .assignees
                            .map(|assignees| assignees.is_empty())
                            .unwrap_or(true)
                        {
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
                let labels = pr
                    .labels
                    .and_then(|labels| labels.labels)
                    .unwrap_or_default()
                    .into_iter()
                    .flatten()
                    .map(|label| label.name)
                    .collect();

                Self {
                    due,
                    summary: pr.title,
                    description: pr.body,
                    kind,
                    status,
                    url: pr.url,
                    labels,
                    milestone,
                    draft,
                }
            }
        }
    };
}

impl_pull_request!(
    queries::viewer_pull_requests::PullRequestInfo,
    queries::viewer_pull_requests::PullRequestState
);
impl_pull_request!(
    queries::repository_pull_requests::PullRequestInfo,
    queries::repository_pull_requests::PullRequestState
);

impl GithubQuery {
    /// Create a new `GithubQuery` for the given optional `host` and `token`.
    #[expect(clippy::single_call_fn, reason = "used from dispatching code")]
    pub fn new(host: Option<String>, token: String) -> Self {
        let conninfo = ConnInfo {
            host: host.unwrap_or_else(|| "api.github.com".into()),
            token,
        };
        Self {
            client: LazyCell::new(Box::new(move || {
                client::Github::new(&conninfo.host, &conninfo.token)
            })),
            init_error_cell: OnceCell::new(),
        }
    }

    /// Check the rate limiting for a query.
    fn check_rate_limits<R>(rate_limit: Option<&R>, name: &str)
    where
        R: Into<queries::RateLimitInfo> + Clone,
    {
        if let Some(info) = rate_limit {
            info.clone().into().inspect(name);
        }
    }

    #[expect(clippy::single_call_fn, reason = "function size")]
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

        let mut issues_input = queries::viewer_issues::Variables {
            filter_by: issue_filters,
            cursor: None,
        };

        let mut items = Vec::new();

        // Query for issue information.
        loop {
            let query = queries::ViewerIssues::build_query(issues_input.clone());
            let rsp = client
                .send::<queries::ViewerIssues>(&query)
                .map_err(|err| {
                    error!("failed to send viewer issue query: {err:?}");
                    ItemError::query_error(
                        "github",
                        format!("failed to send viewer issue query: {err}"),
                    )
                })?;

            Self::check_rate_limits(
                rsp.rate_limit_info.rate_limit.as_ref(),
                queries::ViewerIssues::name(),
            );
            let (gql_issues, page_info) = (rsp.viewer.issues.items, rsp.viewer.issues.page_info);
            if let Some(issues) = gql_issues {
                items.extend(issues.into_iter().flatten().map(Into::into));
            }

            if page_info.has_next_page {
                if page_info.end_cursor.is_none() {
                    return Err(ItemError::query_error(
                        "github",
                        "GitHub reported another page of issues but provided no end \
                                  cursor; bailing to avoid an infinite loop.",
                    ));
                }
                issues_input.cursor = page_info.end_cursor;
            } else {
                break;
            }
        }

        let mut prs_input = queries::viewer_pull_requests::Variables {
            labels: None,
            cursor: None,
        };
        for filter in filters {
            match filter {
                Filter::Label(label) => {
                    prs_input
                        .labels
                        .get_or_insert_with(Vec::new)
                        .push(label.clone());
                },
            }
        }

        // Query for pull requests information.
        loop {
            let query = queries::ViewerPullRequests::build_query(prs_input.clone());
            let rsp = client
                .send::<queries::ViewerPullRequests>(&query)
                .map_err(|err| {
                    error!("failed to send viewer pull request query: {err:?}");
                    ItemError::query_error(
                        "github",
                        format!("failed to send viewer pull request query: {err}"),
                    )
                })?;

            Self::check_rate_limits(
                rsp.rate_limit_info.rate_limit.as_ref(),
                queries::ViewerPullRequests::name(),
            );
            let (gql_prs, page_info) = (
                rsp.viewer.pull_requests.items,
                rsp.viewer.pull_requests.page_info,
            );
            if let Some(prs) = gql_prs {
                items.extend(prs.into_iter().flatten().map(Into::into));
            }

            if page_info.has_next_page {
                if page_info.end_cursor.is_none() {
                    return Err(ItemError::query_error(
                        "github",
                        "GitHub reported another page of pull requests but provided no \
                                  end cursor; bailing to avoid an infinite loop.",
                    ));
                }
                prs_input.cursor = page_info.end_cursor;
            } else {
                break;
            }
        }

        Ok(items)
    }

    /// Query issues and pull requests across multiple repositories.
    #[expect(clippy::single_call_fn, reason = "function size")]
    fn query_projects(
        client: &client::Github,
        projects: &[String],
        filters: &[Filter],
    ) -> Result<Vec<GithubItem>, ItemError> {
        let mut items = Vec::new();

        // Collect labels from filters
        let labels: Option<Vec<String>> = {
            let label_list: Vec<String> = filters
                .iter()
                .map(|Filter::Label(label)| label.clone())
                .collect();
            if label_list.is_empty() {
                None
            } else {
                Some(label_list)
            }
        };

        for project in projects {
            // Parse "owner/repo" format
            let (owner, name) = if let Some((owner, name)) = project.split_once('/') {
                (owner.to_owned(), name.to_owned())
            } else {
                error!("invalid project format (expected owner/repo): {project}");
                return Err(ItemError::query_error(
                    "github",
                    format!("invalid project format (expected owner/repo): {project}"),
                ));
            };

            // Query for repository issues
            let mut issues_input = queries::repository_issues::Variables {
                owner: owner.clone(),
                name: name.clone(),
                labels: labels.clone(),
                states: Some(vec![queries::repository_issues::IssueState::OPEN]),
                cursor: None,
            };

            loop {
                let query = queries::RepositoryIssues::build_query(issues_input.clone());
                let rsp = client
                    .send::<queries::RepositoryIssues>(&query)
                    .map_err(|err| {
                        error!("failed to send repository issue query for {project}: {err:?}");
                        ItemError::query_error(
                            "github",
                            format!("failed to send repository issue query for {project}: {err}"),
                        )
                    })?;

                Self::check_rate_limits(
                    rsp.rate_limit_info.rate_limit.as_ref(),
                    queries::RepositoryIssues::name(),
                );

                if let Some(repo) = rsp.repository {
                    let (gql_issues, page_info) = (repo.issues.items, repo.issues.page_info);
                    if let Some(issues) = gql_issues {
                        items.extend(issues.into_iter().flatten().map(GithubItem::from));
                    }

                    if page_info.has_next_page {
                        if page_info.end_cursor.is_none() {
                            return Err(ItemError::query_error(
                                "github",
                                "GitHub reported another page of issues but provided no \
                                          end cursor; bailing to avoid an infinite loop.",
                            ));
                        }
                        issues_input.cursor = page_info.end_cursor;
                    } else {
                        break;
                    }
                } else {
                    warn!("repository {project} not found or not accessible");
                    break;
                }
            }

            // Query for repository pull requests
            let mut prs_input = queries::repository_pull_requests::Variables {
                owner: owner.clone(),
                name: name.clone(),
                labels: labels.clone(),
                states: Some(vec![
                    queries::repository_pull_requests::PullRequestState::OPEN,
                ]),
                cursor: None,
            };

            loop {
                let query = queries::RepositoryPullRequests::build_query(prs_input.clone());
                let rsp = client
                    .send::<queries::RepositoryPullRequests>(&query)
                    .map_err(|err| {
                        error!(
                            "failed to send repository pull request query for {project}: {err:?}",
                        );
                        ItemError::query_error(
                            "github",
                            format!(
                                "failed to send repository pull request query for {project}: {err}",
                            ),
                        )
                    })?;

                Self::check_rate_limits(
                    rsp.rate_limit_info.rate_limit.as_ref(),
                    queries::RepositoryPullRequests::name(),
                );

                if let Some(repo) = rsp.repository {
                    let (gql_prs, page_info) =
                        (repo.pull_requests.items, repo.pull_requests.page_info);
                    if let Some(prs) = gql_prs {
                        items.extend(prs.into_iter().flatten().map(GithubItem::from));
                    }

                    if page_info.has_next_page {
                        if page_info.end_cursor.is_none() {
                            return Err(ItemError::query_error(
                                "github",
                                "GitHub reported another page of pull requests but \
                                          provided no end cursor; bailing to avoid an infinite \
                                          loop.",
                            ));
                        }
                        prs_input.cursor = page_info.end_cursor;
                    } else {
                        break;
                    }
                } else {
                    // Already warned above for issues query
                    break;
                }
            }
        }

        Ok(items)
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
                    item.set_labels(result.labels);
                    item.set_milestone(result.milestone);
                    item.set_draft(result.draft);

                    None
                } else {
                    let mut item = TodoItem::builder();

                    item.kind(result.kind)
                        .status(result.status)
                        .url(result.url.clone())
                        .summary(result.summary)
                        .description(result.description)
                        .labels(result.labels)
                        .draft(result.draft);

                    if let Some(due) = result.due {
                        item.due(due);
                    }
                    if let Some(milestone) = result.milestone {
                        item.milestone(milestone);
                    }

                    Some(item.build().expect("all item fields should be provided"))
                }
            })
            .collect())
    }
}
