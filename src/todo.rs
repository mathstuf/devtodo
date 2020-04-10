// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::fmt;
use std::fs;
use std::io;
use std::ops;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use derive_builder::Builder;
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
    item: TodoItem,
}

static PRODID_PREFIX: &str = concat!("-//IDN benboeckel.net//", env!("CARGO_PKG_NAME"), "/",);
static PRODID_SUFFIX: &str = concat!(env!("CARGO_PKG_VERSION"), " vobject", "//EN",);

impl TodoFile {
    pub fn from_item<P>(dir: P, item: TodoItem) -> TodoResult<Self>
    where
        P: AsRef<Path>,
    {
        Self::from_item_impl(dir.as_ref(), item)
    }

    fn from_item_impl(dir: &Path, item: TodoItem) -> TodoResult<Self> {
        let path = dir.join(format!("{}.ical", item.uid.0));
        let subcomponent = item.vtodo();
        let mut component = Component::new("VCALENDAR");
        component.set(Property::new("VERSION", "2.0"));
        component.set(Property::new("PRODID", format!("{}{}", PRODID_PREFIX, PRODID_SUFFIX)));
        component.subcomponents.push(subcomponent);

        fs::write(&path, vobject::write_component(&component).as_bytes())
            .map_err(|err| TodoError::write_file(path.clone(), err))?;

        Ok(Self {
            path,
            component,
            item,
        })
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

        Ok(
            if let Some(item) =
                Self::extract_component(&component).and_then(TodoItem::from_component)
            {
                Some(Self {
                    path,
                    component,
                    item,
                })
            } else {
                None
            },
        )
    }

    fn extract_component(component: &Component) -> Option<Component> {
        let prodid = component.get_only("PRODID")?;
        if !prodid.value_as_string().starts_with(PRODID_PREFIX) {
            return None;
        }
        if component.subcomponents.len() != 1 {
            return None;
        }
        let subcomponent = &component.subcomponents[0];
        if subcomponent.name != "VTODO" {
            return None;
        }

        Some(subcomponent.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    NeedsAction,
    Completed,
    InProgress,
    Cancelled,
}

impl AsRef<str> for TodoStatus {
    fn as_ref(&self) -> &str {
        match self {
            Self::NeedsAction => "NEEDS-ACTION",
            Self::Completed => "COMPLETED",
            Self::InProgress => "IN-PROGRESS",
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
        Some(match DateTime::parse_from_str(s, DATE_TIME_FMT) {
            Ok(dt) => Due::DateTime(dt.with_timezone(&Utc)),
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

        Self(format!("{}", uuid.to_hyphenated()))
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

    #[builder(default)]
    last_modified: Option<DateTime<Utc>>,
}

impl TodoItem {
    fn from_component(component: Component) -> Option<Self> {
        let uid = Uid(component.get_only("UID")?.value_as_string());
        let kind = {
            let categories_value = component.get_only("CATEGORIES")?.value_as_string();
            let categories = categories_value.split(',').collect::<Vec<_>>();
            ALL_TODO_KINDS
                .iter()
                .find(|kind| categories.contains(&kind.category()))?
                .clone()
        };
        let created = {
            let dtstamp = component.get_only("DTSTAMP")?.value_as_string();
            DateTime::parse_from_str(&dtstamp, DATE_TIME_FMT)
                .ok()?
                .with_timezone(&Utc)
        };
        let due = if let Some(due) = component.get_only("DUE") {
            Some(Due::from_str(&due.value_as_string())?)
        } else {
            None
        };
        let status = match component.get_only("STATUS")?.value_as_string().as_ref() {
            "NEEDS-ACTION" => TodoStatus::NeedsAction,
            "COMPLETED" => TodoStatus::Completed,
            "IN-PROGRESS" => TodoStatus::InProgress,
            "CANCELLED" => TodoStatus::Cancelled,
            _ => return None,
        };
        let url = component.get_only("URL")?.value_as_string();
        let summary = component.get_only("SUMMARY")?.value_as_string();
        let description = component.get_only("DESCRIPTION")?.value_as_string();
        let last_modified = if let Some(last_modified) = component.get_only("LAST-MODIFIED") {
            Some(
                DateTime::parse_from_str(&last_modified.value_as_string(), DATE_TIME_FMT)
                    .ok()?
                    .with_timezone(&Utc),
            )
        } else {
            None
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
        })
    }

    fn vtodo(&self) -> Component {
        let mut component = Component::new("VTODO");
        component.set(Property::new("DTSTAMP", format!("{}", Utc::now().format(DATE_TIME_FMT))));
        component.set(Property::new("UID", format!("{}", self.uid.0)));
        component.set(Property::new("CREATED", format!("{}", self.created.format(DATE_TIME_FMT))));
        component.set(Property::new("LAST-MODIFIED", format!("{}", self.last_modified.as_ref().unwrap_or(&self.created).format(DATE_TIME_FMT))));
        component.set(Property::new("SUMMARY", &self.summary));
        component.set(Property::new("DESCRIPTION", &self.description));
        component.set(Property::new("CLASS", "CONFIDENTIAL"));
        component.set(Property::new("STATUS", self.status));
        if let Some(due) = self.due {
            component.set(Property::new("DUE", format!("{}", due)));
        }
        component.set(Property::new("URL", &self.url));
        component.set(Property::new("CATEGORIES", self.kind));

        component
    }
}
