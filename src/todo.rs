// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::fs;
use std::io;
use std::path::PathBuf;

use chrono::{DateTime, NaiveDate, Utc};
use thiserror::Error;
use vobject::Component;

#[derive(Debug, Error)]
pub enum TodoError {
    #[error("failed to read file {}", path.display())]
    ReadFile { path: PathBuf, source: io::Error },
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

pub enum TodoStatus {
    NeedsAction,
    Completed,
    InProgress,
    Cancelled,
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

pub const DATE_TIME_FMT: &str = "%Y%m%dT%H%M%SZ";
pub const DATE_FMT: &str = "%Y%m%d";

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

pub struct TodoItem {
    uid: String,
    kind: TodoKind,
    created: DateTime<Utc>,
    due: Option<Due>,
    status: TodoStatus,
    url: String,
    summary: String,
    description: String,

    last_modified: Option<DateTime<Utc>>,
}

impl TodoItem {
    fn from_component(component: Component) -> Option<Self> {
        let uid = component.get_only("UID")?.value_as_string();
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
}
