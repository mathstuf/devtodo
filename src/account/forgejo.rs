// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Forgejo integration using the `forgejo-api` crate (REST API with sync feature).

use chrono::NaiveDate;
use forgejo_api::structs::{
    CommitStatusState, Issue, IssueSearchIssuesQuery, IssueSearchIssuesQueryState,
    IssueSearchIssuesQueryType, ListActionRunsQuery, StateType,
};
use forgejo_api::sync::Forgejo;
use forgejo_api::Auth;
use log::{error, warn};
use url::Url;

use crate::account::prelude::*;
use crate::todo::{
    CiStatus, Due, LinkedIssue, LinkedIssueRelation, ReviewStatus, TodoKind, TodoStatus,
};

/// A normalised view of a single action run's status for aggregation.
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq))]
enum AggregatedRunStatus {
    /// The run completed successfully.
    Success,
    /// The run failed.
    Failure,
    /// The run is still in-flight or waiting to start.
    Pending,
    /// The run was skipped (or cancelled), contributing nothing to the
    /// aggregate.
    Neutral,
}

impl AggregatedRunStatus {
    #[expect(clippy::single_call_fn, reason = "abstraction")]
    /// Combine two run statuses into the overall aggregate.
    ///
    /// Order:
    ///   - `Failure` is sticky (wins over everything).
    ///   - `Pending` is the next-stickiest.
    ///   - `Neutral` is transparent (passes the other through).
    ///   - `Success` + `Success` remains `Success`.
    const fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Failure, _) | (_, Self::Failure) => Self::Failure,
            (Self::Pending, _) | (_, Self::Pending) => Self::Pending,
            (Self::Neutral, status) | (status, Self::Neutral) => status,
            (Self::Success, Self::Success) => Self::Success,
        }
    }

    #[expect(
        clippy::allow_attributes,
        reason = "call counts depend on feature selection"
    )]
    #[allow(clippy::single_call_fn, reason = "abstraction")]
    /// Map a raw Forgejo Actions run to its normalised status.
    fn from_run_status(run_status: &str) -> Self {
        match run_status {
            "success" => Self::Success,
            "failure" => Self::Failure,
            "cancelled" | "skipped" => Self::Neutral,
            "waiting" | "running" | "blocked" | "unknown" => Self::Pending,
            other => {
                warn!("unrecognised Forgejo Actions run status: {other}");
                Self::Pending
            },
        }
    }
}

impl From<AggregatedRunStatus> for CiStatus {
    fn from(value: AggregatedRunStatus) -> Self {
        match value {
            // A neutral status means all runs were skipped or cancelled. Consider this a success.
            AggregatedRunStatus::Success | AggregatedRunStatus::Neutral => Self::Success,
            AggregatedRunStatus::Failure => Self::Failure,
            AggregatedRunStatus::Pending => Self::Pending,
        }
    }
}

#[expect(clippy::single_call_fn, reason = "abstraction")]
fn forgejo_issue_status(issue: &Issue, is_pull_request: bool) -> TodoStatus {
    match issue.state.unwrap_or(StateType::Open) {
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
            let has_assignees = issue
                .assignees
                .as_ref()
                .is_some_and(|assignees| !assignees.is_empty());
            if has_assignees {
                TodoStatus::InProcess
            } else {
                TodoStatus::NeedsAction
            }
        },
    }
}

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
    /// Review status if this is a merge request with reviews enabled.
    review_status: Option<ReviewStatus>,
    /// CI/CD pipeline status for the upstream item.
    ci_status: Option<CiStatus>,
}

impl ForgejoItem {
    /// Construct a [`ForgejoItem`] from a raw Forgejo `Issue`, treating it as an issue or PR.
    fn from_issue(client: &Forgejo, issue: Issue, is_pull_request: bool) -> Self {
        let kind = if is_pull_request {
            TodoKind::PullRequest
        } else {
            TodoKind::Issue
        };

        let url = issue
            .html_url
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        let status = forgejo_issue_status(&issue, is_pull_request);

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

        let review_status = if is_pull_request {
            issue
                .number
                .zip(
                    issue
                        .repository
                        .as_ref()
                        .and_then(|repo| repo.owner.as_ref().zip(repo.name.as_ref())),
                )
                .and_then(|(number, (owner, repo))| {
                    ForgejoQuery::fetch_review_status(client, owner, repo, number)
                })
        } else {
            None
        };

        let ci_status = if is_pull_request {
            match issue
                .number
                .zip(
                    issue
                        .repository
                        .as_ref()
                        .and_then(|repo| repo.owner.as_ref().zip(repo.name.as_ref())),
                )
                .map(|(number, (owner, repo))| {
                    ForgejoQuery::fetch_ci_for_pr(client, owner, repo, number)
                })
                .transpose()
            {
                Ok(ci_status) => ci_status.flatten(),
                Err(err) => {
                    warn!("failed to determine CI status for {url}: {err:?}");
                    None
                },
            }
        } else {
            None
        };

        Self {
            due,
            summary: issue.title.unwrap_or_default(),
            description: issue.body.unwrap_or_default(),
            kind,
            status,
            url,
            labels,
            milestone,
            draft,
            linked_issues,
            review_status,
            ci_status,
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

    /// Fetch review status for a single pull request.
    #[expect(clippy::single_call_fn, reason = "abstraction")]
    fn fetch_review_status(
        client: &Forgejo,
        owner: &str,
        repo: &str,
        number: i64,
    ) -> Option<ReviewStatus> {
        let (_, reviews) = match client.repo_list_pull_reviews(owner, repo, number).send() {
            Ok(reviews) => reviews,
            Err(err) => {
                warn!("failed to fetch reviews for {owner}/{repo}#{number}: {err:?}");
                return None;
            },
        };

        reviews
            .iter()
            .filter_map(|review| {
                match review.state.as_deref()? {
                    "APPROVED" => Some(ReviewStatus::Approved),
                    "REQUEST_CHANGES" => Some(ReviewStatus::ChangesRequested),
                    "PENDING" => Some(ReviewStatus::Pending),
                    other => {
                        warn!("unrecognised Forgejo review state: {other}");
                        None
                    },
                }
            })
            .reduce(ReviewStatus::combine)
    }

    /// Fetch CI status for a single pull request by looking up actions runs first,
    /// then falling back to the combined commit status.
    #[expect(clippy::single_call_fn, reason = "function size")]
    fn fetch_ci_for_pr(
        client: &Forgejo,
        owner: &str,
        repo: &str,
        number: i64,
    ) -> Result<Option<CiStatus>, ItemError> {
        // Fetch full PR details to get the head commit SHA.
        let pr = client
            .repo_get_pull_request(owner, repo, number)
            .send()
            .map_err(|err| {
                ItemError::query_error(
                    "forgejo",
                    format!("failed to fetch PR {owner}/{repo}#{number} for CI status: {err:?}"),
                )
            })?;

        let head_sha = pr
            .head
            .as_ref()
            .ok_or_else(|| {
                ItemError::query_error(
                    "forgejo",
                    format!("missing HEAD information for {owner}/{repo}#{number}"),
                )
            })?
            .sha
            .as_ref()
            .ok_or_else(|| {
                ItemError::query_error(
                    "forgejo",
                    format!("missing SHA information for {owner}/{repo}#{number}"),
                )
            })?;

        // Try Forgejo Actions runs first.
        let query = ListActionRunsQuery {
            head_sha: Some(head_sha.clone()),
            ..Default::default()
        };

        let runs_response = client
            .list_action_runs(owner, repo, query)
            .send()
            .map_err(|err| {
                ItemError::query_error(
                    "forgejo",
                    format!("failed to list action runs for {owner}/{repo}#{number}: {err:?}"),
                )
            })?;

        let runs = runs_response.workflow_runs.unwrap_or_default();

        if !runs.is_empty() {
            return Ok(Some(
                runs.iter()
                    .filter_map(|run| run.status.as_deref())
                    .map(AggregatedRunStatus::from_run_status)
                    .fold(AggregatedRunStatus::Neutral, AggregatedRunStatus::combine)
                    .into(),
            ));
        }

        // Fall back to the combined commit status (e.g. for legacy CI or
        // webhook-based status).
        let (_, combined_status) = client
            .repo_get_combined_status_by_ref(owner, repo, head_sha)
            .send()
            .map_err(|err| {
                ItemError::query_error(
                    "forgejo",
                    format!("failed to get combined status for {owner}/{repo}@{head_sha}: {err:?}"),
                )
            })?;

        Ok(combined_status.state.map(Self::commit_status_to_ci))
    }

    /// Map a Forgejo [`CommitStatusState`] to our canonical [`CiStatus`].
    #[expect(clippy::single_call_fn, reason = "abstraction")]
    const fn commit_status_to_ci(state: CommitStatusState) -> CiStatus {
        match state {
            CommitStatusState::Success => CiStatus::Success,
            CommitStatusState::Failure => CiStatus::Failure,
            CommitStatusState::Error | CommitStatusState::Warning => CiStatus::Error,
            CommitStatusState::Pending => CiStatus::Pending,
        }
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
                    item.set_review_status(result.review_status);
                    item.set_ci_status(result.ci_status);

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
                    if let Some(review_status) = result.review_status {
                        item.review_status(review_status);
                    }
                    if let Some(ci_status) = result.ci_status {
                        item.ci_status(ci_status);
                    }

                    Some(item.build().expect("all item fields should be provided"))
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use crate::account::forgejo::AggregatedRunStatus;
    use crate::todo::CiStatus;

    #[test]
    fn test_aggregated_run_status_combine() {
        let cases = [
            (
                AggregatedRunStatus::Success,
                AggregatedRunStatus::Success,
                AggregatedRunStatus::Success,
            ),
            (
                AggregatedRunStatus::Success,
                AggregatedRunStatus::Failure,
                AggregatedRunStatus::Failure,
            ),
            (
                AggregatedRunStatus::Success,
                AggregatedRunStatus::Pending,
                AggregatedRunStatus::Pending,
            ),
            (
                AggregatedRunStatus::Success,
                AggregatedRunStatus::Neutral,
                AggregatedRunStatus::Success,
            ),
            (
                AggregatedRunStatus::Failure,
                AggregatedRunStatus::Failure,
                AggregatedRunStatus::Failure,
            ),
            (
                AggregatedRunStatus::Failure,
                AggregatedRunStatus::Pending,
                AggregatedRunStatus::Failure,
            ),
            (
                AggregatedRunStatus::Failure,
                AggregatedRunStatus::Neutral,
                AggregatedRunStatus::Failure,
            ),
            (
                AggregatedRunStatus::Pending,
                AggregatedRunStatus::Pending,
                AggregatedRunStatus::Pending,
            ),
            (
                AggregatedRunStatus::Pending,
                AggregatedRunStatus::Neutral,
                AggregatedRunStatus::Pending,
            ),
            (
                AggregatedRunStatus::Neutral,
                AggregatedRunStatus::Neutral,
                AggregatedRunStatus::Neutral,
            ),
        ];

        for (left, right, result) in cases {
            assert_eq!(left.combine(right), result);
            assert_eq!(right.combine(left), result);
        }
    }

    #[test]
    fn test_aggregated_run_status_from_run_status() {
        let cases = [
            ("success", AggregatedRunStatus::Success),
            ("failure", AggregatedRunStatus::Failure),
            ("cancelled", AggregatedRunStatus::Neutral),
            ("skipped", AggregatedRunStatus::Neutral),
            ("waiting", AggregatedRunStatus::Pending),
            ("running", AggregatedRunStatus::Pending),
            ("blocked", AggregatedRunStatus::Pending),
            ("unknown", AggregatedRunStatus::Pending),
            ("not a forgejo status", AggregatedRunStatus::Pending),
        ];

        for (run_status, expect) in cases {
            assert_eq!(AggregatedRunStatus::from_run_status(run_status), expect);
        }
    }

    #[test]
    fn test_from_aggregated_run_status_for_ci_status() {
        let cases = [
            (AggregatedRunStatus::Success, CiStatus::Success),
            (AggregatedRunStatus::Failure, CiStatus::Failure),
            (AggregatedRunStatus::Pending, CiStatus::Pending),
            (AggregatedRunStatus::Neutral, CiStatus::Success),
        ];

        for (run_status, ci_status) in cases {
            assert_eq!(CiStatus::from(run_status), ci_status);
        }
    }
}
