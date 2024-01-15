#![allow(dead_code)]
use std::{fmt::Display, mem::replace};

use crate::error::Error;
use chrono::{DateTime, Utc};
use uris::Uri;

#[repr(u8)]
pub(crate) enum TaskStatus {
    Todo = 0,
    InProgress = 1,
    Complete = 2,
    Cancelled = 3,
    Hidden = 4,
}

impl Display for TaskStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let c = match self {
            TaskStatus::Todo => ' ',
            TaskStatus::InProgress => '/',
            TaskStatus::Complete => 'x',
            TaskStatus::Cancelled => '-',
            TaskStatus::Hidden => '?',
        };
        write!(f, "[{c}]")
    }
}

impl TryFrom<u8> for TaskStatus {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::Todo,
            1 => Self::InProgress,
            2 => Self::Complete,
            3 => Self::Cancelled,
            4 => Self::Hidden,
            _ => {
                return Err(Error::Bug(format!(
                    "Invalid task status integer {value}, this is a bug."
                )))
            }
        })
    }
}

impl Default for TaskStatus {
    fn default() -> Self {
        Self::Todo
    }
}

pub(crate) type TaskId = u64;

pub(crate) struct Tag(String);

#[derive(Default)]
pub(crate) struct Task {
    pub(crate) id: TaskId,
    pub(crate) title: String,
    pub(crate) status: TaskStatus,
    pub(crate) created: DateTime<Utc>,
    pub(crate) content: Option<TaskContent>,
}

pub(crate) struct TaskContent {
    pub(crate) body: Option<String>,
    pub(crate) link: Option<Uri>,
}

impl Task {
    pub(crate) fn new(id: u64, status: TaskStatus, title: String, created: DateTime<Utc>) -> Self {
        Task {
            id,
            status,
            title,
            created,
            content: None,
        }
    }

    pub(crate) fn set_content(&mut self, content: TaskContent) -> Option<TaskContent> {
        replace(&mut self.content, Some(content))
    }
}

pub(crate) enum RelationshipSide {
    Left(TaskId),
    Right(TaskId),
}

/// `Query` represents a segment of a query when entering "query mode".
pub(crate) enum QueryArgs {
    /// Query tasks with a given tag
    Tag(bool, Tag),
    /// Query tasks of a certain status
    Status(bool, TaskStatus),
    /// Query tasks usint FTS
    Text(String),
    /// Query tasks with a certain relationship
    Relation(String, RelationshipSide),
}
