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

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use derive_builder::Builder;
use itertools::Itertools;
use thiserror::Error;
use uuid::Uuid;
use vobject::{Component, Property};

#[derive(Debug, Error)]
pub enum TodoError {
    #[error("failed to read file {}", path.display())]
    ReadFile { path: PathBuf, source: io::Error },
    #[error("failed to write file {}", path.display())]
    WriteFile { path: PathBuf, source: io::Error },
    #[error("failed to parse vobject component")]
    ParseComponent {
        #[from]
        source: vobject::error::VObjectError,
    },
}

impl TodoError {
    fn read_file(path: PathBuf, source: io::Error) -> Self {
        Self::ReadFile {
            path,
            source,
        }
    }

    fn write_file(path: PathBuf, source: io::Error) -> Self {
        Self::WriteFile {
            path,
            source,
        }
    }
}

type TodoResult<T> = Result<T, TodoError>;

pub struct TodoFile {
    path: PathBuf,
    component: Component,
    pub item: TodoItem,
}

static PRODID_PREFIX: &str = concat!("-//IDN benboeckel.net//", env!("CARGO_PKG_NAME"), "/",);
static PRODID_SUFFIX: &str = concat!(env!("CARGO_PKG_VERSION"), " vobject", "//EN",);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Updated {
    Yes,
    No,
}

impl TodoFile {
    pub fn from_item<P>(dir: P, item: TodoItem) -> TodoResult<Self>
    where
        P: AsRef<Path>,
    {
        Self::from_item_impl(dir.as_ref(), item)
    }

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

    pub fn from_path<P>(path: P) -> TodoResult<Option<Self>>
    where
        P: Into<PathBuf>,
    {
        Self::from_path_impl(path.into())
    }

    fn from_path_impl(path: PathBuf) -> TodoResult<Option<Self>> {
        let contents =
            fs::read_to_string(&path).map_err(|err| TodoError::read_file(path.clone(), err))?;
        let component = vobject::parse_component(&contents)?;

        Ok(Self::extract_component(&component)
            .and_then(TodoItem::from_component)
            .map(|item| {
                Self {
                    path,
                    component,
                    item,
                }
            }))
    }

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

    fn extract_component_as_mut(component: &mut Component) -> Option<&mut Component> {
        Self::is_our_component(component)?;
        let subcomponent = &mut component.subcomponents[0];
        if subcomponent.name != "VTODO" {
            return None;
        }

        Some(subcomponent)
    }

    fn extract_component_as_ref(component: &Component) -> Option<&Component> {
        Self::is_our_component(component)?;
        let subcomponent = &component.subcomponents[0];
        if subcomponent.name != "VTODO" {
            return None;
        }

        Some(subcomponent)
    }

    fn extract_component(component: &Component) -> Option<Component> {
        Self::extract_component_as_ref(component).cloned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    NeedsAction,
    Completed,
    InProcess,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoKind {
    Issue,
    AssignedIssue,
    PullRequest,
    AssignedPullRequest,
    Todo,
}

static ALL_TODO_KINDS: &[TodoKind] = &[
    TodoKind::Issue,
    TodoKind::AssignedIssue,
    TodoKind::PullRequest,
    TodoKind::AssignedPullRequest,
    TodoKind::Todo,
];

impl TodoKind {
    fn category(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::AssignedIssue => "assigned-issue",
            Self::PullRequest => "pull-request",
            Self::AssignedPullRequest => "assigned-pull-request",
            Self::Todo => "todo",
        }
    }
}

impl AsRef<str> for TodoKind {
    fn as_ref(&self) -> &str {
        self.category()
    }
}

pub const DATE_TIME_FMT: &str = "%Y%m%dT%H%M%SZ";
pub const DATE_FMT: &str = "%Y%m%d";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    Date(NaiveDate),
    DateTime(DateTime<Utc>),
}

impl Due {
    fn from_str(s: &str) -> Option<Self> {
        Some(match NaiveDateTime::parse_from_str(s, DATE_TIME_FMT) {
            Ok(dt) => Due::DateTime(Utc.from_utc_datetime(&dt)),
            Err(_) => NaiveDate::parse_from_str(s, DATE_FMT).map(Due::Date).ok()?,
        })
    }
}

impl fmt::Display for Due {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Due::Date(d) => write!(f, "{}", d.format(DATE_FMT)),
            Due::DateTime(dt) => write!(f, "{}", dt.format(DATE_TIME_FMT)),
        }
    }
}

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

#[derive(Builder)]
pub struct TodoItem {
    #[builder(default)]
    #[builder(setter(skip))]
    uid: Uid,
    kind: TodoKind,
    #[builder(default = "Utc::now()")]
    created: DateTime<Utc>,
    #[builder(default)]
    #[builder(setter(strip_option))]
    due: Option<Due>,
    status: TodoStatus,
    url: String,
    summary: String,
    #[builder(default)]
    description: String,

    #[builder(default = "Utc::now()")]
    #[builder(setter(skip))]
    last_modified: DateTime<Utc>,

    #[builder(default = "false")]
    #[builder(setter(skip))]
    updated: bool,
}

impl TodoItem {
    pub fn builder() -> TodoItemBuilder {
        TodoItemBuilder::default()
    }

    pub fn set_due(&mut self, new_due: Due) {
        if self.due.as_ref().map(|&due| due != new_due).unwrap_or(true) {
            self.due = Some(new_due);
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    pub fn set_status(&mut self, new_status: TodoStatus) {
        if self.status != new_status {
            self.status = new_status;
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    pub fn set_summary<S>(&mut self, new_summary: S)
    where
        S: Into<String>,
    {
        let new_summary = new_summary.into();
        if self.summary != new_summary {
            self.summary = new_summary;
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    pub fn set_description<D>(&mut self, new_description: D)
    where
        D: Into<String>,
    {
        let new_description = new_description.into();
        // Replace CR in the new description with nothing. These are lost upon reading them back
        // from the ical format.
        let new_description = new_description.replace('\r', "");
        if self.description != new_description {
            self.description = new_description;
            self.last_modified = Utc::now();
            self.updated = true;
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn from_component(component: Component) -> Option<Self> {
        let uid = Uid(component.get_only("UID")?.value_as_string());
        let kind = {
            let categories_value = component.get_only("CATEGORIES")?.value_as_string();
            let categories = categories_value.split(',').collect::<Vec<_>>();
            *ALL_TODO_KINDS
                .iter()
                .find(|kind| categories.contains(&kind.category()))?
        };
        let created = {
            let dtstamp = component.get_only("DTSTAMP")?.value_as_string();
            let dt = NaiveDateTime::parse_from_str(&dtstamp, DATE_TIME_FMT).ok()?;

            Utc.from_utc_datetime(&dt)
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

        Some(TodoItem {
            uid,
            kind,
            created,
            due,
            status,
            url,
            summary,
            description,
            last_modified,
            updated,
        })
    }

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

    fn update_component(&self, component: &mut Component) {
        component.set(Property::new("SUMMARY", &self.summary));
        component.set(Property::new("DESCRIPTION", &self.description));
        component.set(Property::new("URL", &self.url));
        if let Some(due) = self.due {
            component.set(Property::new("DUE", format!("{due}")));
        }

        component.set(Property::new(
            "LAST-MODIFIED",
            format!("{}", self.last_modified.format(DATE_TIME_FMT)),
        ));

        if let Some(prop) = component.get_only("CATEGORIES") {
            let value = prop.value_as_string();
            let categories = value.split(',');
            let all_categories = categories.clone();

            // See if we have any of the categories set.
            let kind_categories = categories
                .filter(|&category| {
                    ALL_TODO_KINDS
                        .iter()
                        .any(|kind| category == kind.category())
                })
                .collect::<Vec<_>>();

            // Check if we have the right category already set.
            if kind_categories.len() == 1 && kind_categories[0] == self.kind.category() {
                // OK
            } else {
                let new_categories = all_categories
                    .filter(|&category| {
                        ALL_TODO_KINDS
                            .iter()
                            .all(|kind| category != kind.category())
                    })
                    .chain(iter::once(self.kind.category()))
                    .format(",");
                component.set(Property::new("CATEGORIES", format!("{new_categories}")));
            }
        } else {
            component.set(Property::new("CATEGORIES", self.kind));
        };
    }
}
