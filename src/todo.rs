// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::fmt;
use std::fs;
use std::io;
use std::iter;
use std::ops;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone as _, Utc};
use derive_builder::Builder;
use itertools::Itertools as _;
use thiserror::Error;
use uuid::Uuid;
use vobject::{Component, Property};

/// Errors that can occur when reading or writing `.ics` todo files.
#[derive(Debug, Error)]
pub enum TodoError {
    /// Failed to read a `.ics` file from disk.
    #[error("failed to read file {}", path.display())]
    ReadFile {
        /// Path of the file that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// Failed to write a `.ics` file to disk.
    #[error("failed to write file {}", path.display())]
    WriteFile {
        /// Path of the file that could not be written.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// Failed to parse a vObject component from the file contents.
    #[error("failed to parse vobject component")]
    ParseComponent {
        #[from]
        /// The underlying parse error.
        source: vobject::error::VObjectError,
    },
}

impl TodoError {
    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Construct a `ReadFile` error.
    const fn read_file(path: PathBuf, source: io::Error) -> Self {
        Self::ReadFile {
            path,
            source,
        }
    }

    /// Construct a `WriteFile` error.
    const fn write_file(path: PathBuf, source: io::Error) -> Self {
        Self::WriteFile {
            path,
            source,
        }
    }
}

/// Convenience alias for `Result<T, TodoError>`.
type TodoResult<T> = Result<T, TodoError>;

/// An on-disk `.ics` file paired with the parsed [`TodoItem`] it contains.
pub struct TodoFile {
    /// Path to the `.ics` file on disk.
    path: PathBuf,
    /// The raw vObject component tree read from (or written to) `path`.
    component: Component,
    /// The parsed todo item derived from the VTODO subcomponent.
    pub item: TodoItem,
}

/// PRODID prefix written into every VCALENDAR component we create.
static PRODID_PREFIX: &str = concat!("-//IDN benboeckel.net//", env!("CARGO_PKG_NAME"), "/",);
/// PRODID suffix appended after the prefix.
static PRODID_SUFFIX: &str = concat!(env!("CARGO_PKG_VERSION"), " vobject", "//EN",);

/// Whether a [`TodoFile`] needs to be flushed back to disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Updated {
    /// The item was modified and the file must be re-written.
    Yes,
    /// Nothing changed; no write is needed.
    No,
}

impl TodoFile {
    #[expect(clippy::single_call_fn, reason = "function size")]
    /// Create a new `.ics` file in `dir` for the given `item` and return a [`TodoFile`] for it.
    pub fn from_item<P>(dir: P, item: TodoItem) -> TodoResult<Self>
    where
        P: AsRef<Path>,
    {
        Self::from_item_impl(dir.as_ref(), item)
    }

    #[expect(clippy::single_call_fn, reason = "monomorphization")]
    /// Monomorphized implementation of [`from_item`](Self::from_item).
    fn from_item_impl(dir: &Path, item: TodoItem) -> TodoResult<Self> {
        let path = dir.join(format!("{}.ics", item.uid.0));
        let subcomponent = item.vtodo();
        let mut component = Component::new("VCALENDAR");
        component.set(Property::new("VERSION", "2.0"));
        component.set(Property::new(
            "PRODID",
            format!("{PRODID_PREFIX}{PRODID_SUFFIX}"),
        ));
        component.subcomponents.push(subcomponent);

        fs::write(&path, vobject::write_component(&component).as_bytes())
            .map_err(|err| TodoError::write_file(path.clone(), err))?;

        Ok(Self {
            path,
            component,
            item,
        })
    }

    /// Flush any pending changes back to the `.ics` file on disk.
    pub fn write(&mut self) -> TodoResult<()> {
        if self.sync() == Updated::Yes {
            fs::write(
                &self.path,
                vobject::write_component(&self.component).as_bytes(),
            )
            .map_err(|err| TodoError::write_file(self.path.clone(), err))?;
        }

        Ok(())
    }

    /// Propagate in-memory changes to the vObject component tree; returns whether anything changed.
    fn sync(&mut self) -> Updated {
        if self.item.updated {
            let vtodo = Self::extract_component_as_mut(&mut self.component)
                .expect("How did the component become invalid?");
            self.item.update_component(vtodo);
            self.item.updated = false;

            Updated::Yes
        } else {
            Updated::No
        }
    }

    #[expect(clippy::single_call_fn, reason = "convenience constructor")]
    /// Read a `.ics` file at `path`; returns `None` if the file is not a devtodo component.
    pub fn from_path<P>(path: P) -> TodoResult<Option<Self>>
    where
        P: Into<PathBuf>,
    {
        Self::from_path_impl(path.into())
    }

    #[expect(clippy::single_call_fn, reason = "monomorphization")]
    /// Monomorphized implementation of [`from_path`](Self::from_path).
    fn from_path_impl(path: PathBuf) -> TodoResult<Option<Self>> {
        let contents =
            fs::read_to_string(&path).map_err(|err| TodoError::read_file(path.clone(), err))?;
        let component = vobject::parse_component(&contents)?;

        Ok(Self::extract_component(&component)
            .and_then(|comp| TodoItem::from_component(&comp))
            .map(|item| {
                Self {
                    path,
                    component,
                    item,
                }
            }))
    }

    /// Return `Some(())` iff `component` was written by this program (correct PRODID, one subcomponent).
    fn is_our_component(component: &Component) -> Option<()> {
        let prodid = component.get_only("PRODID")?;
        if !prodid.value_as_string().starts_with(PRODID_PREFIX) {
            return None;
        }
        if component.subcomponents.len() != 1 {
            return None;
        }

        Some(())
    }

    #[expect(clippy::single_call_fn, reason = "function size")]
    /// Extract the VTODO subcomponent as a mutable reference, or `None` if this is not our file.
    fn extract_component_as_mut(component: &mut Component) -> Option<&mut Component> {
        Self::is_our_component(component)?;
        let subcomponent = component.subcomponents.get_mut(0)?;
        if subcomponent.name != "VTODO" {
            return None;
        }

        Some(subcomponent)
    }

    #[expect(clippy::single_call_fn, reason = "convenience accessor")]
    /// Extract the VTODO subcomponent as a shared reference, or `None` if this is not our file.
    fn extract_component_as_ref(component: &Component) -> Option<&Component> {
        Self::is_our_component(component)?;
        let subcomponent = component.subcomponents.first()?;
        if subcomponent.name != "VTODO" {
            return None;
        }

        Some(subcomponent)
    }

    #[expect(clippy::single_call_fn, reason = "convenience accessor")]
    /// Clone and return the VTODO subcomponent, or `None` if this is not our file.
    fn extract_component(component: &Component) -> Option<Component> {
        Self::extract_component_as_ref(component).cloned()
    }
}

/// The completion status of a todo item, corresponding to the iCalendar `STATUS` property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    /// The item has not been started (`NEEDS-ACTION`).
    NeedsAction,
    /// The item has been fully completed (`COMPLETED`).
    Completed,
    /// The item is actively being worked on (`IN-PROCESS`).
    InProcess,
    /// The item was cancelled and will not be completed (`CANCELLED`).
    Cancelled,
}

impl AsRef<str> for TodoStatus {
    fn as_ref(&self) -> &str {
        match self {
            Self::NeedsAction => "NEEDS-ACTION",
            Self::Completed => "COMPLETED",
            Self::InProcess => "IN-PROCESS",
            Self::Cancelled => "CANCELLED",
        }
    }
}

/// The type of item retrieved from a code-hosting service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoKind {
    /// An issue (not personally assigned).
    Issue,
    /// An issue that is assigned to the authenticated user.
    AssignedIssue,
    /// A pull/merge request (not personally assigned).
    PullRequest,
    /// A pull/merge request assigned to the authenticated user.
    AssignedPullRequest,
    /// A pull/merge request for which a review has been requested.
    ReviewRequested,
    /// A generic todo item (e.g. from a Forgejo task list).
    Todo,
}

/// All [`TodoKind`] variants in order, used when searching CATEGORIES for a matching kind.
static ALL_TODO_KINDS: &[TodoKind] = &[
    TodoKind::Issue,
    TodoKind::AssignedIssue,
    TodoKind::PullRequest,
    TodoKind::AssignedPullRequest,
    TodoKind::ReviewRequested,
    TodoKind::Todo,
];

impl TodoKind {
    /// Return the iCalendar CATEGORIES string used to represent this kind.
    const fn category(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::AssignedIssue => "assigned-issue",
            Self::PullRequest => "pull-request",
            Self::AssignedPullRequest => "assigned-pull-request",
            Self::ReviewRequested => "review-requested",
            Self::Todo => "todo",
        }
    }
}

impl AsRef<str> for TodoKind {
    fn as_ref(&self) -> &str {
        self.category()
    }
}

/// Format string for iCalendar date-time values (UTC, no fractional seconds).
pub const DATE_TIME_FMT: &str = "%Y%m%dT%H%M%SZ";
/// Format string for iCalendar date-only values.
pub const DATE_FMT: &str = "%Y%m%d";

/// A due date that may carry either a date or a full date-time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// A date-only due value (from the iCalendar `DUE` property without a time component).
    Date(NaiveDate),
    /// A full UTC date-time due value.
    DateTime(DateTime<Utc>),
}

impl Due {
    /// Parse a `Due` from an iCalendar date or date-time string; returns `None` on failure.
    fn from_str(str: &str) -> Option<Self> {
        Some(match NaiveDateTime::parse_from_str(str, DATE_TIME_FMT) {
            Ok(dt) => Self::DateTime(Utc.from_utc_datetime(&dt)),
            Err(_) => {
                NaiveDate::parse_from_str(str, DATE_FMT)
                    .map(Self::Date)
                    .ok()?
            },
        })
    }
}

impl fmt::Display for Due {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Date(date) => write!(f, "{}", date.format(DATE_FMT)),
            Self::DateTime(dt) => write!(f, "{}", dt.format(DATE_TIME_FMT)),
        }
    }
}

/// A unique identifier for a todo item, stored as the iCalendar `UID` property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uid(String);

impl ops::Deref for Uid {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ops::DerefMut for Uid {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Default for Uid {
    fn default() -> Self {
        let uuid = Uuid::new_v4();

        Self(format!("{}", uuid.hyphenated()))
    }
}

/// The relationship direction for a linked issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkedIssueRelation {
    /// This item blocks the linked issue.
    Blocks,
    /// This item depends on (is blocked by) the linked issue.
    DependsOn,
    /// This item will close the linked issue when it is merged.
    Closes,
    /// This item will be closed when the linked issue is merged.
    ClosedBy,
    /// This item is referenced by another item.
    Referenced,
}

impl LinkedIssueRelation {
    #[expect(clippy::single_call_fn, reason = "abstraction")]
    /// Return the relation type for an iCalendar parameter value.
    fn from_str(str: &str) -> Option<Self> {
        match str {
            "BLOCKS" => Some(Self::Blocks),
            "DEPENDS-ON" => Some(Self::DependsOn),
            "CLOSES" => Some(Self::Closes),
            "CLOSED-BY" => Some(Self::ClosedBy),
            "REFERENCES" => Some(Self::Referenced),
            _ => None,
        }
    }
}

impl AsRef<str> for LinkedIssueRelation {
    fn as_ref(&self) -> &str {
        match self {
            Self::Blocks => "BLOCKS",
            Self::DependsOn => "DEPENDS-ON",
            Self::Closes => "CLOSES",
            Self::ClosedBy => "CLOSED-BY",
            Self::Referenced => "REFERENCED",
        }
    }
}

/// A linked issue with its relationship direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedIssue {
    /// URL of the linked issue.
    pub url: String,
    /// How this item relates to the linked issue.
    pub relation: Option<LinkedIssueRelation>,
}

/// A single todo item derived from a code-hosting service item.
#[derive(Builder)]
pub struct TodoItem {
    /// Unique identifier for this item (iCalendar UID).
    #[builder(default)]
    #[builder(setter(skip))]
    uid: Uid,
    /// The kind of upstream item this represents.
    kind: TodoKind,
    /// When this item was first created.
    #[builder(default = "Utc::now()")]
    created: DateTime<Utc>,
    /// Optional due date, sourced from a milestone or similar.
    #[builder(default)]
    #[builder(setter(strip_option))]
    due: Option<Due>,
    /// Optional start date.
    #[builder(default)]
    #[builder(setter(strip_option))]
    start: Option<Due>,
    /// Current completion status.
    status: TodoStatus,
    /// Canonical URL of the upstream item.
    url: String,
    /// Short title / summary of the item.
    summary: String,
    /// Long-form body text.
    #[builder(default)]
    description: String,
    /// Labels applied to the upstream item.
    #[builder(default)]
    labels: Vec<String>,
    /// Milestone title associated with the upstream item, if any.
    #[builder(default)]
    #[builder(setter(strip_option))]
    milestone: Option<String>,
    /// Whether the upstream pull request is in draft state.
    #[builder(default)]
    draft: bool,
    /// Issues linked via RELATED-TO with an X-RELATION parameter.
    #[builder(default)]
    linked_issues: Vec<LinkedIssue>,

    /// Timestamp of the most recent modification (iCalendar LAST-MODIFIED).
    #[builder(default = "Utc::now()")]
    #[builder(setter(skip))]
    last_modified: DateTime<Utc>,

    /// Whether this item has unsaved in-memory changes that need to be flushed to disk.
    #[builder(default = "false")]
    #[builder(setter(skip))]
    updated: bool,
}

impl TodoItem {
    #[expect(
        clippy::allow_attributes,
        reason = "call counts depend on feature selection"
    )]
    #[allow(clippy::single_call_fn, reason = "public builder API")]
    /// Return a fresh [`TodoItemBuilder`] for constructing a new item.
    pub fn builder() -> TodoItemBuilder {
        TodoItemBuilder::default()
    }

    #[cfg(feature = "gitlab")]
    pub fn set_start(&mut self, new_start: Due) {
        if self.start.as_ref().is_none_or(|&start| start != new_start) {
            self.start = Some(new_start);
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    /// Update the due date, marking the item as modified if it changed.
    pub fn set_due(&mut self, new_due: Due) {
        if self.due.as_ref().is_none_or(|&due| due != new_due) {
            self.due = Some(new_due);
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    /// Update the completion status, marking the item as modified if it changed.
    pub fn set_status(&mut self, new_status: TodoStatus) {
        if self.status != new_status {
            self.status = new_status;
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    /// Update the summary, marking the item as modified if it changed.
    pub fn set_summary<S>(&mut self, new_summary: S)
    where
        S: Into<String>,
    {
        let summary = new_summary.into();
        if self.summary != summary {
            self.summary = summary;
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    /// Update the description, marking the item as modified if it changed.
    ///
    /// Carriage-return characters are stripped because they are lost when the value is round-tripped
    /// through iCalendar format.
    pub fn set_description<D>(&mut self, new_description: D)
    where
        D: Into<String>,
    {
        // Replace CR in the new description with nothing. These are lost upon reading them back
        // from the ical format.
        let description = new_description.into().replace('\r', "");
        if self.description != description {
            self.description = description;
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    /// Replace the label list, marking the item as modified if it changed.
    pub fn set_labels(&mut self, new_labels: Vec<String>) {
        if self.labels != new_labels {
            self.labels = new_labels;
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    /// Update the milestone, marking the item as modified if it changed.
    pub fn set_milestone<M>(&mut self, new_milestone: Option<M>)
    where
        M: Into<String>,
    {
        let milestone = new_milestone.map(Into::into);
        if self.milestone != milestone {
            self.milestone = milestone;
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    /// Update the draft flag, marking the item as modified if it changed.
    pub fn set_draft(&mut self, new_draft: bool) {
        if self.draft != new_draft {
            self.draft = new_draft;
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    /// Replace the linked issues for this item.
    pub fn set_linked_issues(&mut self, new_linked_issues: Vec<LinkedIssue>) {
        if self.linked_issues != new_linked_issues {
            self.linked_issues = new_linked_issues;
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    /// Return the canonical URL of the upstream item.
    pub fn url(&self) -> &str {
        &self.url
    }

    #[expect(clippy::single_call_fn, reason = "function size")]
    /// Attempt to construct a [`TodoItem`] from a parsed VTODO `component`; returns `None` if any
    /// required property is missing or cannot be parsed.
    fn from_component(component: &Component) -> Option<Self> {
        let uid = Uid(component.get_only("UID")?.value_as_string());
        let (kind, labels, milestone) = {
            let categories_value = component.get_only("CATEGORIES")?.value_as_string();
            let categories = categories_value.split(',').collect::<Vec<_>>();
            let kind = *ALL_TODO_KINDS
                .iter()
                .find(|kind| categories.contains(&kind.category()))?;
            let labels = categories
                .iter()
                .filter_map(|cat| cat.strip_prefix("label-"))
                .map(String::from)
                .collect();
            let milestone = categories
                .iter()
                .find_map(|cat| cat.strip_prefix("milestone-"))
                .map(String::from);
            (kind, labels, milestone)
        };
        let created = {
            let dtstamp = component.get_only("DTSTAMP")?.value_as_string();
            let dt = NaiveDateTime::parse_from_str(&dtstamp, DATE_TIME_FMT).ok()?;

            Utc.from_utc_datetime(&dt)
        };
        let start = if let Some(start) = component.get_only("DTSTART") {
            Some(Due::from_str(&start.value_as_string())?)
        } else {
            None
        };
        let due = if let Some(due) = component.get_only("DUE") {
            Some(Due::from_str(&due.value_as_string())?)
        } else {
            None
        };
        let status = match component.get_only("STATUS")?.value_as_string().as_ref() {
            "NEEDS-ACTION" => TodoStatus::NeedsAction,
            "COMPLETED" => TodoStatus::Completed,
            "IN-PROCESS" => TodoStatus::InProcess,
            "CANCELLED" => TodoStatus::Cancelled,
            _ => return None,
        };
        let url = component.get_only("URL")?.value_as_string();
        let summary = component.get_only("SUMMARY")?.value_as_string();
        let description = component.get_only("DESCRIPTION")?.value_as_string();
        let (last_modified, updated) = if let Some(last_modified) =
            component.get_only("LAST-MODIFIED")
        {
            let dt = NaiveDateTime::parse_from_str(&last_modified.value_as_string(), DATE_TIME_FMT)
                .ok()?;

            (Utc.from_utc_datetime(&dt), false)
        } else {
            // Missing a time? Set it to now; we'll write it back later.
            (Utc::now(), true)
        };
        let draft = component
            .get_only("X-DRAFT")
            .is_some_and(|is_draft| is_draft.value_as_string() == "TRUE");
        let linked_issues = component
            .get_all("RELATED-TO")
            .iter()
            .map(|prop| {
                let issue_url = prop.value_as_string();
                let relation = prop
                    .params
                    .get("X-RELATION")
                    .and_then(|str| LinkedIssueRelation::from_str(str));
                LinkedIssue {
                    url: issue_url,
                    relation,
                }
            })
            .collect();

        Some(Self {
            uid,
            kind,
            created,
            due,
            start,
            status,
            url,
            summary,
            description,
            labels,
            milestone,
            draft,
            linked_issues,
            last_modified,
            updated,
        })
    }

    /// Serialise this item into a new VTODO [`Component`].
    fn vtodo(&self) -> Component {
        let mut component = Component::new("VTODO");

        // Initialize the component.
        component.set(Property::new(
            "DTSTAMP",
            format!("{}", Utc::now().format(DATE_TIME_FMT)),
        ));
        component.set(Property::new("UID", self.uid.0.clone()));
        component.set(Property::new(
            "CREATED",
            format!("{}", self.created.format(DATE_TIME_FMT)),
        ));
        component.set(Property::new("CLASS", "CONFIDENTIAL"));
        component.set(Property::new("STATUS", self.status));

        // Fill in the rest of the fields that we assume are controlled by the source of the item.
        self.update_component(&mut component);

        component
    }

    /// Write the current field values from this item into an existing VTODO `component`.
    fn update_component(&self, component: &mut Component) {
        component.set(Property::new("SUMMARY", &self.summary));
        component.set(Property::new("DESCRIPTION", &self.description));
        component.set(Property::new("URL", &self.url));
        if let Some(start) = self.start {
            component.set(Property::new("DTSTART", format!("{start}")));
        }
        if let Some(due) = self.due {
            component.set(Property::new("DUE", format!("{due}")));
        }

        component.set(Property::new(
            "LAST-MODIFIED",
            format!("{}", self.last_modified.format(DATE_TIME_FMT)),
        ));

        // Build the CATEGORIES value:
        // - Keep any existing categories that aren't kind, label, or milestone categories
        // - Add the current kind category
        // - Add label- prefixed categories for each label
        // - Add milestone- prefixed category if present
        let existing_categories: Vec<String> = component
            .get_only("CATEGORIES")
            .map(|prop| {
                prop.value_as_string()
                    .split(',')
                    .filter(|&cat| {
                        // Filter out kind categories, label- categories, and milestone- categories
                        !ALL_TODO_KINDS.iter().any(|kind| cat == kind.category())
                            && !cat.starts_with("label-")
                            && !cat.starts_with("milestone-")
                    })
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();

        let label_categories = self.labels.iter().map(|label| format!("label-{label}"));
        let milestone_category = self
            .milestone
            .iter()
            .map(|milestone| format!("milestone-{milestone}"));

        let new_categories = existing_categories
            .into_iter()
            .chain(iter::once(self.kind.category().to_owned()))
            .chain(label_categories)
            .chain(milestone_category)
            .format(",");

        component.set(Property::new("CATEGORIES", format!("{new_categories}")));

        // Write X-DRAFT property (only if true to avoid clutter)
        if self.draft {
            component.set(Property::new("X-DRAFT", "TRUE"));
        } else {
            component.remove("X-DRAFT");
        }

        // Write RELATED-TO properties for linked issues
        // First remove all existing RELATED-TO properties
        while component.get_only("RELATED-TO").is_some() {
            component.remove("RELATED-TO");
        }
        // Then add new ones
        for linked in &self.linked_issues {
            let mut prop = Property::new("RELATED-TO", &linked.url);
            if let Some(relation) = linked.relation {
                prop.params
                    .insert("X-RELATION".to_owned(), relation.as_ref().to_owned());
            }
            component.push(prop);
        }
    }
}
