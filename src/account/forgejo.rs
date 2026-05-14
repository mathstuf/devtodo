// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Forgejo integration using the `forgejo-api` crate (REST API with sync feature).

use chrono::NaiveDate;
use forgejo_api::structs::{
    Issue, IssueSearchIssuesQuery, IssueSearchIssuesQueryState, IssueSearchIssuesQueryType,
    StateType,
};
use forgejo_api::sync::Forgejo;
use forgejo_api::Auth;
use log::{error, warn};
use url::Url;

use crate::account::prelude::*;
use crate::todo::{Due, TodoKind, TodoStatus};

struct ForgejoItem {
    due: Option<Due>,
    summary: String,
    description: String,
    kind: TodoKind,
    status: TodoStatus,
    url: String,
}

impl ForgejoItem {
    fn from_issue(issue: Issue, is_pull_request: bool) -> Self {
        let kind = if is_pull_request {
            TodoKind::PullRequest
        } else {
            TodoKind::Issue
        };

        let state = issue.state.unwrap_or(StateType::Open);
        let has_assignees = issue
            .assignees
            .as_ref()
            .map(|a| !a.is_empty())
            .unwrap_or(false);

        let status = match state {
            StateType::Closed => {
                if is_pull_request {
                    // For PRs, check if it was merged
                    // The `pull_request` field contains merge info if it's a PR
                    if issue
                        .pull_request
                        .as_ref()
                        .and_then(|pr| pr.merged)
                        .unwrap_or(false)
                    {
                        TodoStatus::Completed
                    } else {
                        TodoStatus::Cancelled
                    }
                } else {
                    TodoStatus::Completed
                }
            },
            StateType::Open => {
                if has_assignees {
                    TodoStatus::InProcess
                } else {
                    TodoStatus::NeedsAction
                }
            },
        };

        // Extract due date from milestone if present
        // Convert from time::OffsetDateTime to chrono::NaiveDate
        let due = issue
            .milestone
            .as_ref()
            .and_then(|m| m.due_on.as_ref())
            .map(|dt| {
                let date = dt.date();
                NaiveDate::from_ymd_opt(date.year(), date.month() as u32, date.day() as u32)
                    .expect("valid date from API")
            })
            .map(Due::Date);

        Self {
            due,
            summary: issue.title.unwrap_or_default(),
            description: issue.body.unwrap_or_default(),
            kind,
            status,
            url: issue.html_url.map(|u| u.to_string()).unwrap_or_default(),
        }
    }
}

pub struct ForgejoQuery {
    client: Result<Forgejo, forgejo_api::ForgejoError>,
}

impl ForgejoQuery {
    pub fn new(host: Option<String>, token: String) -> Self {
        let host = host.unwrap_or_else(|| "codeberg.org".into());
        let url = Url::parse(&format!("https://{host}")).unwrap_or_else(|_| {
            // Fallback if the host is malformed
            Url::parse("https://codeberg.org").unwrap()
        });

        let client = Forgejo::new(Auth::Token(&token), url);

        ForgejoQuery {
            client,
        }
    }

    fn query_user(client: &Forgejo, filters: &[Filter]) -> Result<Vec<ForgejoItem>, ItemError> {
        let mut items = Vec::new();

        // Build label filter string (comma-separated)
        let labels: Option<String> = {
            let label_list: Vec<&str> = filters
                .iter()
                .map(|f| {
                    match f {
                        Filter::Label(l) => l.as_str(),
                    }
                })
                .collect();
            if label_list.is_empty() {
                None
            } else {
                Some(label_list.join(","))
            }
        };

        // Query issues assigned to the API user.
        {
            let query = IssueSearchIssuesQuery {
                assigned: Some(true),
                state: Some(IssueSearchIssuesQueryState::Open),
                r#type: Some(IssueSearchIssuesQueryType::Issues),
                labels: labels.clone(),
                ..Default::default()
            };

            let (_, assigned_issues) = client.issue_search_issues(query).send().map_err(|err| {
                error!("failed to query assigned issues: {err:?}");
                ItemError::QueryError {
                    service: "forgejo",
                    message: format!("failed to query assigned issues: {err}"),
                }
            })?;

            items.extend(
                assigned_issues
                    .into_iter()
                    .map(|i| ForgejoItem::from_issue(i, false)),
            );
        }

        // Query issues created by the API user.
        {
            let query = IssueSearchIssuesQuery {
                created: Some(true),
                state: Some(IssueSearchIssuesQueryState::Open),
                r#type: Some(IssueSearchIssuesQueryType::Issues),
                labels: labels.clone(),
                ..Default::default()
            };

            let (_, created_issues) = client.issue_search_issues(query).send().map_err(|err| {
                error!("failed to query created issues: {err:?}");
                ItemError::QueryError {
                    service: "forgejo",
                    message: format!("failed to query created issues: {err}"),
                }
            })?;

            items.extend(
                created_issues
                    .into_iter()
                    .map(|i| ForgejoItem::from_issue(i, false)),
            );
        }

        // Query pull requests assigned to the API user.
        {
            let query = IssueSearchIssuesQuery {
                assigned: Some(true),
                state: Some(IssueSearchIssuesQueryState::Open),
                r#type: Some(IssueSearchIssuesQueryType::Pulls),
                labels: labels.clone(),
                ..Default::default()
            };

            let (_, assigned_prs) = client.issue_search_issues(query).send().map_err(|err| {
                error!("failed to query assigned pull requests: {err:?}");
                ItemError::QueryError {
                    service: "forgejo",
                    message: format!("failed to query assigned pull requests: {err}"),
                }
            })?;

            items.extend(
                assigned_prs
                    .into_iter()
                    .map(|i| ForgejoItem::from_issue(i, true)),
            );
        }

        // Query pull requests created by the API user.
        {
            let query = IssueSearchIssuesQuery {
                created: Some(true),
                state: Some(IssueSearchIssuesQueryState::Open),
                r#type: Some(IssueSearchIssuesQueryType::Pulls),
                labels: labels.clone(),
                ..Default::default()
            };

            let (_, created_prs) = client.issue_search_issues(query).send().map_err(|err| {
                error!("failed to query created pull requests: {err:?}");
                ItemError::QueryError {
                    service: "forgejo",
                    message: format!("failed to query created pull requests: {err}"),
                }
            })?;

            items.extend(
                created_prs
                    .into_iter()
                    .map(|i| ForgejoItem::from_issue(i, true)),
            );
        }

        Ok(items)
    }

    fn query_projects(
        client: &Forgejo,
        project_paths: &[String],
        filters: &[Filter],
    ) -> Result<Vec<ForgejoItem>, ItemError> {
        use forgejo_api::structs::{
            IssueListIssuesQuery, IssueListIssuesQueryState, IssueListIssuesQueryType,
        };

        let mut items = Vec::new();

        // Build label filter string (comma-separated)
        let labels: Option<String> = {
            let label_list: Vec<&str> = filters
                .iter()
                .map(|f| {
                    match f {
                        Filter::Label(l) => l.as_str(),
                    }
                })
                .collect();
            if label_list.is_empty() {
                None
            } else {
                Some(label_list.join(","))
            }
        };

        for project_path in project_paths {
            // Parse owner/repo from project path
            let parts: Vec<&str> = project_path.splitn(2, '/').collect();
            if parts.len() != 2 {
                warn!("invalid project path (expected owner/repo): {project_path}");
                continue;
            }
            let (owner, repo) = (parts[0], parts[1]);

            // Query project issues
            {
                let query = IssueListIssuesQuery {
                    state: Some(IssueListIssuesQueryState::Open),
                    r#type: Some(IssueListIssuesQueryType::Issues),
                    labels: labels.clone(),
                    ..Default::default()
                };

                let (_, project_issues) = client
                    .issue_list_issues(owner, repo, query)
                    .send()
                    .map_err(|err| {
                        error!("failed to query project {project_path} issues: {err:?}");
                        ItemError::QueryError {
                            service: "forgejo",
                            message: format!(
                                "failed to query project {project_path} issues: {err}",
                            ),
                        }
                    })?;

                items.extend(
                    project_issues
                        .into_iter()
                        .map(|i| ForgejoItem::from_issue(i, false)),
                );
            }

            // Query project pull requests
            {
                let query = IssueListIssuesQuery {
                    state: Some(IssueListIssuesQueryState::Open),
                    r#type: Some(IssueListIssuesQueryType::Pulls),
                    labels: labels.clone(),
                    ..Default::default()
                };

                let (_, project_prs) = client
                    .issue_list_issues(owner, repo, query)
                    .send()
                    .map_err(|err| {
                        error!("failed to query project {project_path} pull requests: {err:?}");
                        ItemError::QueryError {
                            service: "forgejo",
                            message: format!(
                                "failed to query project {project_path} pull requests: {err}",
                            ),
                        }
                    })?;

                items.extend(
                    project_prs
                        .into_iter()
                        .map(|i| ForgejoItem::from_issue(i, true)),
                );
            }
        }

        Ok(items)
    }
}

impl ItemSource for ForgejoQuery {
    fn fetch_items(
        &self,
        target: &QueryTarget,
        filters: &[Filter],
        existing_items: &mut ItemLookup,
    ) -> Result<Vec<TodoItem>, ItemError> {
        let client = self.client.as_ref().map_err(|err| {
            error!("failed to connect to forgejo instance: {err:?}");
            ItemError::ServiceError {
                service: "forgejo",
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
                    // Update existing item
                    if let Some(due) = result.due {
                        item.set_due(due);
                    }
                    item.set_status(result.status);
                    item.set_summary(result.summary);
                    item.set_description(result.description);

                    None
                } else {
                    // Create new item
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
