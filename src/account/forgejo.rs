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
use crate::todo::{Due, LinkedIssue, LinkedIssueRelation, TodoKind, TodoStatus};

/// A single item retrieved from the Forgejo API, prior to conversion into a [`TodoItem`].
struct ForgejoItem {
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
    /// Canonical URL of the item on Forgejo.
    url: String,
    /// Labels applied to the item.
    labels: Vec<String>,
    /// Milestone title, if any.
    milestone: Option<String>,
    /// Whether this is a draft pull request.
    draft: bool,
    /// Issues linked via RELATED-TO with an X-RELATION parameter.
    linked_issues: Vec<LinkedIssue>,
}

impl ForgejoItem {
    /// Construct a [`ForgejoItem`] from a raw Forgejo `Issue`, treating it as an issue or PR.
    fn from_issue(client: &Forgejo, issue: Issue, is_pull_request: bool) -> Self {
        let kind = if is_pull_request {
            TodoKind::PullRequest
        } else {
            TodoKind::Issue
        };

        let state = issue.state.unwrap_or(StateType::Open);
        let has_assignees = issue
            .assignees
            .as_ref()
            .is_some_and(|assignees| !assignees.is_empty());

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
            .and_then(|milestone| milestone.due_on.as_ref())
            .map(|dt| {
                let date = dt.date();
                NaiveDate::from_ymd_opt(
                    date.year(),
                    u8::from(date.month()).into(),
                    date.day().into(),
                )
                .expect("valid date from API")
            })
            .map(Due::Date);

        // Extract milestone title
        let milestone = issue
            .milestone
            .as_ref()
            .and_then(|milestone| milestone.title.clone());

        // Extract labels from issue
        let labels = issue
            .labels
            .unwrap_or_default()
            .into_iter()
            .filter_map(|label| label.name)
            .collect();

        // Extract draft status from pull request metadata
        let draft = issue
            .pull_request
            .as_ref()
            .and_then(|pr| pr.draft)
            .unwrap_or(false);

        let linked_issues = issue
            .number
            .zip(
                issue
                    .repository
                    .as_ref()
                    .and_then(|repo| repo.owner.as_ref().zip(repo.name.as_ref())),
            )
            .map(|(number, (owner, repo))| {
                ForgejoQuery::fetch_linked_issues(client, owner, repo, number)
            })
            .unwrap_or_default();

        Self {
            due,
            summary: issue.title.unwrap_or_default(),
            description: issue.body.unwrap_or_default(),
            kind,
            status,
            url: issue
                .html_url
                .map(|url| url.to_string())
                .unwrap_or_default(),
            labels,
            milestone,
            draft,
            linked_issues,
        }
    }
}

/// An [`ItemSource`] that queries a Forgejo instance.
pub struct ForgejoQuery {
    /// The Forgejo client, or the initialization error if construction failed.
    client: Result<Forgejo, forgejo_api::ForgejoError>,
}

impl ForgejoQuery {
    /// Create a new `ForgejoQuery` for the given optional `host` and API `token`.
    #[expect(clippy::single_call_fn, reason = "used from dispatching code")]
    pub fn new(host: Option<String>, token: &str) -> Self {
        let actual_host = host.unwrap_or_else(|| "codeberg.org".into());
        let url = Url::parse(&format!("https://{actual_host}")).unwrap_or_else(|_| {
            // Fallback if the host is malformed
            Url::parse("https://codeberg.org").expect("codeberg.org is a valid URL")
        });

        let client = Forgejo::new(Auth::Token(token), url);

        Self {
            client,
        }
    }

    /// Query all issues and pull requests assigned to or created by the authenticated user.
    #[expect(clippy::single_call_fn, reason = "function size")]
    fn query_user(client: &Forgejo, filters: &[Filter]) -> Result<Vec<ForgejoItem>, ItemError> {
        let mut items = Vec::new();

        // Build label filter string (comma-separated)
        let labels: Option<String> = {
            let label_list: Vec<&str> = filters
                .iter()
                .map(|filter| {
                    match filter {
                        Filter::Label(label) => label.as_str(),
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
                ItemError::query_error("forgejo", format!("failed to query assigned issues: {err}"))
            })?;

            items.extend(
                assigned_issues
                    .into_iter()
                    .map(|i| ForgejoItem::from_issue(client, i, false)),
            );
        };

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
                ItemError::query_error("forgejo", format!("failed to query created issues: {err}"))
            })?;

            items.extend(
                created_issues
                    .into_iter()
                    .map(|i| ForgejoItem::from_issue(client, i, false)),
            );
        };

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
                ItemError::query_error(
                    "forgejo",
                    format!("failed to query assigned pull requests: {err}"),
                )
            })?;

            items.extend(
                assigned_prs
                    .into_iter()
                    .map(|i| ForgejoItem::from_issue(client, i, true)),
            );
        };

        // Query pull requests created by the API user.
        {
            let query = IssueSearchIssuesQuery {
                created: Some(true),
                state: Some(IssueSearchIssuesQueryState::Open),
                r#type: Some(IssueSearchIssuesQueryType::Pulls),
                labels,
                ..Default::default()
            };

            let (_, created_prs) = client.issue_search_issues(query).send().map_err(|err| {
                error!("failed to query created pull requests: {err:?}");
                ItemError::query_error(
                    "forgejo",
                    format!("failed to query created pull requests: {err}"),
                )
            })?;

            items.extend(
                created_prs
                    .into_iter()
                    .map(|i| ForgejoItem::from_issue(client, i, true)),
            );
        };

        Ok(items)
    }

    /// Query issues and pull requests for a list of `owner/repo` project paths.
    #[expect(clippy::single_call_fn, reason = "function size")]
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
                .map(|filter| {
                    match filter {
                        Filter::Label(label) => label.as_str(),
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
            let Some((owner, repo)) = project_path.split_once('/') else {
                warn!("invalid project path (expected owner/repo): {project_path}");
                continue;
            };

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
                        ItemError::query_error(
                            "forgejo",
                            format!("failed to query project {project_path} issues: {err}"),
                        )
                    })?;

                items.extend(
                    project_issues
                        .into_iter()
                        .map(|i| ForgejoItem::from_issue(client, i, false)),
                );
            };

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
                        ItemError::query_error(
                            "forgejo",
                            format!("failed to query project {project_path} pull requests: {err}"),
                        )
                    })?;

                items.extend(
                    project_prs
                        .into_iter()
                        .map(|i| ForgejoItem::from_issue(client, i, true)),
                );
            }
        }

        Ok(items)
    }

    /// Fetch linked issues for a single issue.
    #[expect(clippy::single_call_fn, reason = "function size")]
    fn fetch_linked_issues(
        client: &Forgejo,
        owner: &str,
        repo: &str,
        number: i64,
    ) -> Vec<LinkedIssue> {
        let mut links = Vec::new();

        // Issues this issue blocks.
        if let Ok(issues) = client.issue_list_blocks(owner, repo, number).send() {
            for issue in issues {
                if let Some(html_url) = issue.html_url {
                    links.push(LinkedIssue {
                        url: html_url.into(),
                        relation: Some(LinkedIssueRelation::Blocks),
                    });
                }
            }
        } else {
            warn!("failed to fetch blocks for {owner}/{repo}#{number}");
        }

        // Issues this issue depends on (block this issue).
        if let Ok(issues) = client
            .issue_list_issue_dependencies(owner, repo, number)
            .send()
        {
            for issue in issues {
                if let Some(html_url) = issue.html_url {
                    links.push(LinkedIssue {
                        url: html_url.into(),
                        relation: Some(LinkedIssueRelation::DependsOn),
                    });
                }
            }
        } else {
            warn!("failed to fetch dependencies for {owner}/{repo}#{number}");
        }

        links
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
