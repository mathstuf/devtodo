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
use crate::todo::{Due, LinkedIssue, LinkedIssueRelation, TodoKind, TodoStatus};

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
    /// The project the issue belongs to.
    project_id: u64,
    /// The project-level IID of the issue.
    iid: u64,
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
    /// The project the merge request belongs to.
    project_id: u64,
    /// The project-level IID of the merge request.
    iid: u64,
}

/// Minimal merge request returned from linked-item endpoints.
#[derive(Debug, Deserialize)]
struct GitlabLinkedMr {
    /// The URL of the linked MR.
    web_url: String,
}

/// Minimal issue returned from linked-item endpoints.
#[derive(Debug, Deserialize)]
struct GitlabLinkedIssue {
    /// The URL of the linked issue.
    web_url: String,
}

/// Issue returned from issue link endpoints.
#[derive(Debug, Deserialize)]
struct GitlabIssueLinkIssue {
    /// The internal ID of the linked issue.
    iid: u64,
    /// The project of the issue.
    project_id: u64,
    /// The URL of the issue.
    web_url: String,
}

/// An issue link returned from the issue links endpoint.
#[derive(Debug, Deserialize)]
struct GitlabIssueLink {
    /// The source of the link.
    source_issue: GitlabIssueLinkIssue,
    /// The target of the link.
    target_issue: GitlabIssueLinkIssue,
    /// The type of the link.
    link_type: String,
}

impl GitlabIssueLink {
    /// Determine the relation type of a GitLab issue link.
    fn relation_type(&self, is_source: bool) -> LinkedIssueRelation {
        match self.link_type.as_str() {
            "blocks" => {
                if is_source {
                    LinkedIssueRelation::Blocks
                } else {
                    LinkedIssueRelation::DependsOn
                }
            },
            "is_blocked_by" => {
                if is_source {
                    LinkedIssueRelation::DependsOn
                } else {
                    LinkedIssueRelation::Blocks
                }
            },
            "relates_to" => LinkedIssueRelation::Referenced,
            other => {
                warn!("unrecognised GitLab issue link type: {other}");
                LinkedIssueRelation::Referenced
            },
        }
    }
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
    /// Linked issues to the item.
    linked_issues: Vec<LinkedIssue>,
}

impl GitlabItem {
    /// Construct an item from an issue API object.
    fn from_issue(client: &Gitlab, issue: GitlabIssue) -> Self {
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

        let linked_issues = Self::fetch_issue_linked(client, &issue);

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
            linked_issues,
        }
    }

    /// Construct an item from a merge request API object.
    fn from_merge_request(client: &Gitlab, mr: GitlabMergeRequest) -> Self {
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

        let linked_issues = Self::fetch_mr_linked(client, &mr);

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
            linked_issues,
        }
    }

    #[expect(clippy::single_call_fn, reason = "abstraction")]
    /// Fetch linked items for an issue.
    fn fetch_issue_linked(client: &Gitlab, issue: &GitlabIssue) -> Vec<LinkedIssue> {
        let mut links = Vec::new();

        // MRs closing this issue.
        let closing_result: Result<Vec<GitlabLinkedMr>, _> = api::paged(
            projects::issues::MergeRequestsClosing::builder()
                .project(issue.project_id)
                .issue(issue.iid)
                .build()
                .expect("all fields provided"),
            api::Pagination::All,
        )
        .query(client);
        if let Ok(items) = closing_result {
            for item in items {
                links.push(LinkedIssue {
                    url: item.web_url,
                    relation: Some(LinkedIssueRelation::ClosedBy),
                });
            }
        } else {
            warn!(
                "failed to fetch MRs closing issue {}#{}",
                issue.project_id, issue.iid,
            );
        }

        // Related MRs.
        let related_result: Result<Vec<GitlabLinkedMr>, _> = api::paged(
            projects::issues::RelatedMergeRequests::builder()
                .project(issue.project_id)
                .issue(issue.iid)
                .build()
                .expect("all fields provided"),
            api::Pagination::All,
        )
        .query(client);
        if let Ok(items) = related_result {
            for item in items {
                links.push(LinkedIssue {
                    url: item.web_url,
                    relation: Some(LinkedIssueRelation::Referenced),
                });
            }
        } else {
            warn!(
                "failed to fetch related MRs for issue {}#{}",
                issue.project_id, issue.iid,
            );
        }

        // Issue links (issue-to-issue relations).
        let issue_links_result: Result<Vec<GitlabIssueLink>, _> = api::paged(
            projects::issues::links::IssueLinks::builder()
                .project(issue.project_id)
                .issue(issue.iid)
                .build()
                .expect("all fields provided"),
            api::Pagination::All,
        )
        .query(client);
        let Ok(issue_links) = issue_links_result else {
            warn!(
                "failed to fetch issue links for issue {}#{}",
                issue.project_id, issue.iid,
            );
            return links;
        };
        for link in issue_links {
            let (other, relation) = if link.source_issue.iid == issue.iid
                && link.source_issue.project_id == issue.project_id
            {
                // Our issue is the source.
                let relation = link.relation_type(true);
                (link.target_issue, relation)
            } else {
                // Our issue is the target.
                let relation = link.relation_type(false);
                (link.source_issue, relation)
            };
            links.push(LinkedIssue {
                url: other.web_url,
                relation: Some(relation),
            });
        }

        links
    }

    #[expect(clippy::single_call_fn, reason = "abstraction")]
    /// Fetch linked items for a merge request.
    fn fetch_mr_linked(client: &Gitlab, mr: &GitlabMergeRequest) -> Vec<LinkedIssue> {
        let mut links = Vec::new();

        // Issues this MR closes.
        let closed_result: Result<Vec<GitlabLinkedIssue>, _> = api::paged(
            projects::merge_requests::IssuesClosedBy::builder()
                .project(mr.project_id)
                .merge_request(mr.iid)
                .build()
                .expect("all fields provided"),
            api::Pagination::All,
        )
        .query(client);
        if let Ok(items) = closed_result {
            for item in items {
                links.push(LinkedIssue {
                    url: item.web_url,
                    relation: Some(LinkedIssueRelation::Closes),
                });
            }
        } else {
            warn!(
                "failed to fetch issues closed by MR {}!{}",
                mr.project_id, mr.iid,
            );
        }

        // MRs that block this MR (this MR depends on them).
        let blocks_result: Result<Vec<GitlabLinkedMr>, _> = api::paged(
            projects::merge_requests::blocks::MergeRequestBlocks::builder()
                .project(mr.project_id)
                .merge_request(mr.iid)
                .build()
                .expect("all fields provided"),
            api::Pagination::All,
        )
        .query(client);
        if let Ok(items) = blocks_result {
            for item in items {
                links.push(LinkedIssue {
                    url: item.web_url,
                    relation: Some(LinkedIssueRelation::DependsOn),
                });
            }
        } else {
            warn!(
                "failed to fetch blocking MRs for MR {}!{}",
                mr.project_id, mr.iid,
            );
        }

        // MRs blocked by this MR (this MR blocks them).
        let blockees_result: Result<Vec<GitlabLinkedMr>, _> = api::paged(
            projects::merge_requests::blocks::MergeRequestBlockees::builder()
                .project(mr.project_id)
                .merge_request(mr.iid)
                .build()
                .expect("all fields provided"),
            api::Pagination::All,
        )
        .query(client);
        if let Ok(items) = blockees_result {
            for item in items {
                links.push(LinkedIssue {
                    url: item.web_url,
                    relation: Some(LinkedIssueRelation::Blocks),
                });
            }
        } else {
            warn!(
                "failed to fetch blocked MRs for MR {}!{}",
                mr.project_id, mr.iid,
            );
        }

        links
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
        Ok(result
            .into_iter()
            .map(|issue| GitlabItem::from_issue(client, issue))
            .collect())
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
        Ok(result
            .into_iter()
            .map(|mr| GitlabItem::from_merge_request(client, mr))
            .collect())
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

                items.extend(
                    project_issues
                        .into_iter()
                        .map(|issue| GitlabItem::from_issue(client, issue)),
                );
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

                items.extend(
                    project_mrs
                        .into_iter()
                        .map(|mr| GitlabItem::from_merge_request(client, mr)),
                );
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
                    item.set_linked_issues(result.linked_issues);

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
                        .draft(result.draft)
                        .linked_issues(result.linked_issues);

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
