// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! GitLab integration using the `gitlab` crate (REST API).

use chrono::NaiveDate;
use gitlab::api::{self, issues, merge_requests, projects, Query as _};
use gitlab::Gitlab;
use log::{error, warn};
use serde::Deserialize;

use crate::account::prelude::*;
use crate::todo::{Due, TodoKind, TodoStatus};

/// Placeholder for a GitLab user in deserialized API responses.
#[derive(Debug, Deserialize)]
struct GitlabUser;

/// A GitLab milestone associated with an issue or merge request.
#[derive(Debug, Deserialize)]
struct GitlabMilestone {
    /// Milestone title.
    title: String,
    /// Optional due date of the milestone.
    due_date: Option<NaiveDate>,
}

/// A GitLab issue as returned by the REST API.
#[derive(Debug, Deserialize)]
struct GitlabIssue {
    /// Title of the issue.
    title: String,
    /// Body text of the issue.
    description: Option<String>,
    /// Current state (e.g. `"opened"` or `"closed"`).
    state: String,
    /// Canonical URL of the issue.
    web_url: String,
    /// Users currently assigned to the issue.
    assignees: Vec<GitlabUser>,
    /// Optional start date.
    start_date: Option<NaiveDate>,
    /// Optional due date.
    due_date: Option<NaiveDate>,
    /// Associated milestone, if any.
    milestone: Option<GitlabMilestone>,
    /// Labels applied to the issue.
    labels: Vec<String>,
}

/// A GitLab merge request as returned by the REST API.
#[derive(Debug, Deserialize)]
struct GitlabMergeRequest {
    /// Title of the merge request.
    title: String,
    /// Body text of the merge request.
    description: Option<String>,
    /// Current state (`"opened"`, `"closed"`, or `"merged"`).
    state: String,
    /// Canonical URL of the merge request.
    web_url: String,
    /// Users currently assigned to the merge request.
    assignees: Vec<GitlabUser>,
    /// Associated milestone, if any.
    milestone: Option<GitlabMilestone>,
    /// Labels applied to the merge request.
    labels: Vec<String>,
    #[serde(default)]
    /// Whether the merge request is a draft.
    draft: bool,
}

/// A single item retrieved from the GitLab API, prior to conversion into a [`TodoItem`].
struct GitlabItem {
    /// Optional start date.
    start: Option<Due>,
    /// Optional due date.
    due: Option<Due>,
    /// Short title of the item.
    summary: String,
    /// Body text of the item.
    description: String,
    /// The kind of upstream item.
    kind: TodoKind,
    /// Current completion status.
    status: TodoStatus,
    /// Canonical URL of the item on GitLab.
    url: String,
    /// Labels applied to the item.
    labels: Vec<String>,
    /// Milestone title, if any.
    milestone: Option<String>,
    /// Whether this is a draft merge request.
    draft: bool,
}

impl From<GitlabIssue> for GitlabItem {
    fn from(issue: GitlabIssue) -> Self {
        let start = issue.start_date.map(Due::Date);
        let due = issue
            .due_date
            .or_else(|| issue.milestone.as_ref()?.due_date)
            .map(Due::Date);
        let milestone = issue
            .milestone
            .as_ref()
            .map(|milestone| milestone.title.clone());
        // TODO: Determine whether this is assigned or not.
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
            start,
            due,
            summary: issue.title,
            description: issue.description.unwrap_or_default(),
            kind,
            status,
            url: issue.web_url,
            labels: issue.labels,
            milestone,
            draft: false,
        }
    }
}

impl From<GitlabMergeRequest> for GitlabItem {
    fn from(mr: GitlabMergeRequest) -> Self {
        let due = mr
            .milestone
            .as_ref()
            .and_then(|milestone| milestone.due_date)
            .map(Due::Date);
        let milestone = mr
            .milestone
            .as_ref()
            .map(|milestone| milestone.title.clone());
        let draft = mr.draft;
        // TODO: Determine whether this is assigned or not.
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
            start: None,
            due,
            summary: mr.title,
            description: mr.description.unwrap_or_default(),
            kind,
            status,
            url: mr.web_url,
            labels: mr.labels,
            milestone,
            draft,
        }
    }
}

/// An [`ItemSource`] that queries a GitLab instance.
pub struct GitlabQuery {
    /// The GitLab client, or the initialization error if construction failed.
    client: Result<Gitlab, gitlab::GitlabError>,
}

impl GitlabQuery {
    /// Create a new `GitlabQuery` for the given optional `host` and `token`.
    #[expect(clippy::single_call_fn, reason = "used from dispatching code")]
    pub fn new(host: Option<String>, token: String) -> Self {
        let actual_host = host.unwrap_or_else(|| "gitlab.com".into());
        let client = Gitlab::new(&actual_host, token);

        Self {
            client,
        }
    }

    /// Query GitLab issues for a given scope.
    fn query_issues_scope(
        client: &Gitlab,
        scope: issues::IssueScope,
        filters: &[Filter],
        query_context: &str,
    ) -> Result<Vec<GitlabItem>, ItemError> {
        let labels = filters.iter().map(|filter| {
            match filter {
                Filter::Label(label) => label.as_str(),
            }
        });
        let endpoint = issues::Issues::builder()
            .scope(scope)
            .state(issues::IssueState::Opened)
            .labels(labels)
            .build()
            .map_err(|err| {
                ItemError::query_error("gitlab", format!("failed to build issues query: {err}"))
            })?;
        let result: Vec<GitlabIssue> = api::paged(endpoint, api::Pagination::All)
            .query(client)
            .map_err(|err| {
                error!("failed to query {query_context}: {err:?}");
                ItemError::query_error("gitlab", format!("failed to query {query_context}: {err}"))
            })?;
        Ok(result.into_iter().map(GitlabItem::from).collect())
    }

    /// Query GitLab merge requests for a given scope.
    fn query_merge_requests_scope(
        client: &Gitlab,
        scope: merge_requests::MergeRequestScope,
        filters: &[Filter],
        build_context: &str,
        query_context: &str,
    ) -> Result<Vec<GitlabItem>, ItemError> {
        let labels = filters.iter().map(|filter| {
            match filter {
                Filter::Label(label) => label.as_str(),
            }
        });
        let endpoint = merge_requests::MergeRequests::builder()
            .scope(scope)
            .state(merge_requests::MergeRequestState::Opened)
            .labels(labels)
            .build()
            .map_err(|err| {
                ItemError::query_error("gitlab", format!("failed to build {build_context}: {err}"))
            })?;
        let result: Vec<GitlabMergeRequest> = api::paged(endpoint, api::Pagination::All)
            .query(client)
            .map_err(|err| {
                error!("failed to query {query_context}: {err:?}");
                ItemError::query_error("gitlab", format!("failed to query {query_context}: {err}"))
            })?;
        Ok(result.into_iter().map(GitlabItem::from).collect())
    }

    /// Query all issues and merge requests for the authenticated user.
    #[expect(clippy::single_call_fn, reason = "function size")]
    fn query_user(client: &Gitlab, filters: &[Filter]) -> Result<Vec<GitlabItem>, ItemError> {
        let mut items = Vec::new();

        items.extend(Self::query_issues_scope(
            client,
            issues::IssueScope::AssignedToMe,
            filters,
            "assigned issues",
        )?);
        items.extend(Self::query_issues_scope(
            client,
            issues::IssueScope::CreatedByMe,
            filters,
            "created issues",
        )?);
        items.extend(Self::query_merge_requests_scope(
            client,
            merge_requests::MergeRequestScope::AssignedToMe,
            filters,
            "merge requests query",
            "assigned merge requests",
        )?);
        items.extend(Self::query_merge_requests_scope(
            client,
            merge_requests::MergeRequestScope::CreatedByMe,
            filters,
            "merge requests query",
            "created merge requests",
        )?);
        items.extend(Self::query_merge_requests_scope(
            client,
            merge_requests::MergeRequestScope::ReviewsForMe,
            filters,
            "reviewer merge requests query",
            "merge requests for review",
        )?);

        Ok(items)
    }

    /// Query issues and merge requests across multiple project paths.
    #[expect(clippy::single_call_fn, reason = "function size")]
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
                        ItemError::query_error(
                            "gitlab",
                            format!("failed to build project issues query: {err}"),
                        )
                    })?;

                let project_issues: Vec<GitlabIssue> = api::paged(endpoint, api::Pagination::All)
                    .query(client)
                    .map_err(|err| {
                        error!("failed to query project {project_path} issues: {err:?}");
                        ItemError::query_error(
                            "gitlab",
                            format!("failed to query project {project_path} issues: {err}"),
                        )
                    })?;

                items.extend(project_issues.into_iter().map(GitlabItem::from));
            };

            // Query project merge requests
            {
                let endpoint = projects::merge_requests::MergeRequests::builder()
                    .project(project_path.as_str())
                    .state(merge_requests::MergeRequestState::Opened)
                    .labels(labels.clone())
                    .build()
                    .map_err(|err| {
                        ItemError::query_error(
                            "gitlab",
                            format!("failed to build project merge requests query: {err}"),
                        )
                    })?;

                let project_mrs: Vec<GitlabMergeRequest> =
                    api::paged(endpoint, api::Pagination::All)
                        .query(client)
                        .map_err(|err| {
                            error!(
                                "failed to query project {project_path} merge requests: {err:?}",
                            );
                            ItemError::query_error(
                                "gitlab",
                                format!(
                                    "failed to query project {project_path} merge requests: {err}",
                                ),
                            )
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
                    if let Some(start) = result.start {
                        item.set_start(start);
                    }
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
                    // Create new item
                    let mut item = TodoItem::builder();

                    item.kind(result.kind)
                        .status(result.status)
                        .url(result.url.clone())
                        .summary(result.summary)
                        .description(result.description)
                        .labels(result.labels)
                        .draft(result.draft);

                    if let Some(start) = result.start {
                        item.start(start);
                    }
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
