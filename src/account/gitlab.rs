// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! GitLab integration using the `gitlab` crate (REST API).

use chrono::NaiveDate;
use gitlab::api::{self, issues, merge_requests, projects, Query};
use gitlab::Gitlab;
use log::{error, warn};
use serde::Deserialize;

use crate::account::prelude::*;
use crate::todo::{Due, TodoKind, TodoStatus};

#[derive(Debug, Deserialize)]
struct GitlabUser {}

#[derive(Debug, Deserialize)]
struct GitlabMilestone {
    due_date: Option<NaiveDate>,
}

#[derive(Debug, Deserialize)]
struct GitlabIssue {
    title: String,
    description: Option<String>,
    state: String,
    web_url: String,
    assignees: Vec<GitlabUser>,
    due_date: Option<NaiveDate>,
    milestone: Option<GitlabMilestone>,
}

#[derive(Debug, Deserialize)]
struct GitlabMergeRequest {
    title: String,
    description: Option<String>,
    state: String,
    web_url: String,
    assignees: Vec<GitlabUser>,
    milestone: Option<GitlabMilestone>,
}

struct GitlabItem {
    due: Option<Due>,
    summary: String,
    description: String,
    kind: TodoKind,
    status: TodoStatus,
    url: String,
}

impl From<GitlabIssue> for GitlabItem {
    fn from(issue: GitlabIssue) -> Self {
        let due = issue
            .due_date
            .or_else(|| issue.milestone.as_ref().and_then(|m| m.due_date))
            .map(Due::Date);
        let kind = TodoKind::Issue;
        let status = match issue.state.as_str() {
            "closed" => TodoStatus::Completed,
            "opened" => {
                if issue.assignees.is_empty() {
                    TodoStatus::NeedsAction
                } else {
                    TodoStatus::InProcess
                }
            },
            state => {
                warn!("unknown gitlab issue state: {state}");
                TodoStatus::NeedsAction
            },
        };

        Self {
            due,
            summary: issue.title,
            description: issue.description.unwrap_or_default(),
            kind,
            status,
            url: issue.web_url,
        }
    }
}

impl From<GitlabMergeRequest> for GitlabItem {
    fn from(mr: GitlabMergeRequest) -> Self {
        let due = mr.milestone.and_then(|m| m.due_date).map(Due::Date);
        let kind = TodoKind::PullRequest;
        let status = match mr.state.as_str() {
            "closed" => TodoStatus::Cancelled,
            "merged" => TodoStatus::Completed,
            "opened" => {
                if mr.assignees.is_empty() {
                    TodoStatus::NeedsAction
                } else {
                    TodoStatus::InProcess
                }
            },
            state => {
                warn!("unknown gitlab merge request state: {state}");
                TodoStatus::NeedsAction
            },
        };

        Self {
            due,
            summary: mr.title,
            description: mr.description.unwrap_or_default(),
            kind,
            status,
            url: mr.web_url,
        }
    }
}

pub struct GitlabQuery {
    client: Result<Gitlab, gitlab::GitlabError>,
}

impl GitlabQuery {
    pub fn new(host: Option<String>, token: String) -> Self {
        let host = host.unwrap_or_else(|| "gitlab.com".into());
        let client = Gitlab::new(&host, token);

        GitlabQuery {
            client,
        }
    }

    fn query_user(client: &Gitlab, filters: &[Filter]) -> Result<Vec<GitlabItem>, ItemError> {
        let mut items = Vec::new();
        let labels = filters.iter().map(|filter| {
            match filter {
                Filter::Label(label) => label.as_str(),
            }
        });

        // Query issues assigned to the API user.
        {
            let endpoint = issues::Issues::builder()
                .scope(issues::IssueScope::AssignedToMe)
                .state(issues::IssueState::Opened)
                .labels(labels.clone())
                .build()
                .map_err(|err| {
                    ItemError::QueryError {
                        service: "gitlab",
                        message: format!("failed to build issues query: {err}"),
                    }
                })?;

            let assigned_issues: Vec<GitlabIssue> = api::paged(endpoint, api::Pagination::All)
                .query(client)
                .map_err(|err| {
                    error!("failed to query assigned issues: {err:?}");
                    ItemError::QueryError {
                        service: "gitlab",
                        message: format!("failed to query assigned issues: {err}"),
                    }
                })?;

            items.extend(assigned_issues.into_iter().map(GitlabItem::from));
        }

        // Query issues created by the API user.
        {
            let endpoint = issues::Issues::builder()
                .scope(issues::IssueScope::CreatedByMe)
                .state(issues::IssueState::Opened)
                .labels(labels.clone())
                .build()
                .map_err(|err| {
                    ItemError::QueryError {
                        service: "gitlab",
                        message: format!("failed to build issues query: {err}"),
                    }
                })?;

            let created_issues: Vec<GitlabIssue> = api::paged(endpoint, api::Pagination::All)
                .query(client)
                .map_err(|err| {
                    error!("failed to query created issues: {err:?}");
                    ItemError::QueryError {
                        service: "gitlab",
                        message: format!("failed to query created issues: {err}"),
                    }
                })?;

            items.extend(created_issues.into_iter().map(GitlabItem::from));
        }

        // Query merge requests assigned to the API user.
        {
            let endpoint = merge_requests::MergeRequests::builder()
                .scope(merge_requests::MergeRequestScope::AssignedToMe)
                .state(merge_requests::MergeRequestState::Opened)
                .labels(labels.clone())
                .build()
                .map_err(|err| {
                    ItemError::QueryError {
                        service: "gitlab",
                        message: format!("failed to build merge requests query: {err}"),
                    }
                })?;

            let assigned_mrs: Vec<GitlabMergeRequest> = api::paged(endpoint, api::Pagination::All)
                .query(client)
                .map_err(|err| {
                    error!("failed to query assigned merge requests: {err:?}");
                    ItemError::QueryError {
                        service: "gitlab",
                        message: format!("failed to query assigned merge requests: {err}"),
                    }
                })?;

            items.extend(assigned_mrs.into_iter().map(GitlabItem::from));
        }

        // Query merge requests created by the API user.
        {
            let endpoint = merge_requests::MergeRequests::builder()
                .scope(merge_requests::MergeRequestScope::CreatedByMe)
                .state(merge_requests::MergeRequestState::Opened)
                .labels(labels)
                .build()
                .map_err(|err| {
                    ItemError::QueryError {
                        service: "gitlab",
                        message: format!("failed to build merge requests query: {err}"),
                    }
                })?;

            let created_mrs: Vec<GitlabMergeRequest> = api::paged(endpoint, api::Pagination::All)
                .query(client)
                .map_err(|err| {
                    error!("failed to query created merge requests: {err:?}");
                    ItemError::QueryError {
                        service: "gitlab",
                        message: format!("failed to query created merge requests: {err}"),
                    }
                })?;

            items.extend(created_mrs.into_iter().map(GitlabItem::from));
        }

        Ok(items)
    }

    fn query_projects(
        client: &Gitlab,
        project_paths: &[String],
        filters: &[Filter],
    ) -> Result<Vec<GitlabItem>, ItemError> {
        let mut items = Vec::new();
        let labels = filters.iter().map(|filter| {
            match filter {
                Filter::Label(label) => label.as_str(),
            }
        });

        for project_path in project_paths {
            // Query project issues
            {
                let endpoint = issues::ProjectIssues::builder()
                    .project(project_path.as_str())
                    .state(issues::IssueState::Opened)
                    .labels(labels.clone())
                    .build()
                    .map_err(|err| {
                        ItemError::QueryError {
                            service: "gitlab",
                            message: format!("failed to build project issues query: {err}"),
                        }
                    })?;

                let project_issues: Vec<GitlabIssue> = api::paged(endpoint, api::Pagination::All)
                    .query(client)
                    .map_err(|err| {
                        error!("failed to query project {project_path} issues: {err:?}");
                        ItemError::QueryError {
                            service: "gitlab",
                            message: format!(
                                "failed to query project {project_path} issues: {err}",
                            ),
                        }
                    })?;

                items.extend(project_issues.into_iter().map(GitlabItem::from));
            }

            // Query project merge requests
            {
                let endpoint = projects::merge_requests::MergeRequests::builder()
                    .project(project_path.as_str())
                    .state(merge_requests::MergeRequestState::Opened)
                    .labels(labels.clone())
                    .build()
                    .map_err(|err| {
                        ItemError::QueryError {
                            service: "gitlab",
                            message: format!("failed to build project merge requests query: {err}"),
                        }
                    })?;

                let project_mrs: Vec<GitlabMergeRequest> =
                    api::paged(endpoint, api::Pagination::All)
                        .query(client)
                        .map_err(|err| {
                            error!(
                                "failed to query project {project_path} merge requests: {err:?}",
                            );
                            ItemError::QueryError {
                                service: "gitlab",
                                message: format!(
                                    "failed to query project {project_path} merge requests: {err}",
                                ),
                            }
                        })?;

                items.extend(project_mrs.into_iter().map(GitlabItem::from));
            }
        }

        Ok(items)
    }
}

impl ItemSource for GitlabQuery {
    fn fetch_items(
        &self,
        target: &QueryTarget,
        filters: &[Filter],
        existing_items: &mut ItemLookup,
    ) -> Result<Vec<TodoItem>, ItemError> {
        let client = self.client.as_ref().map_err(|err| {
            error!("failed to connect to gitlab instance: {err:?}");
            ItemError::ServiceError {
                service: "gitlab",
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
