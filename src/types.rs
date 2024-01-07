#![allow(dead_code)]
use std::mem::replace;

use crate::error::Error;
use chrono::{DateTime, Utc};
use uris::Uri;

#[repr(u8)]
pub(crate) enum TaskStatus {
    Todo = 0,
    InProgress = 1,
    Complete = 2,
    Cancelled = 3,
}

impl TryFrom<u8> for TaskStatus {
    type Error = Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::Todo,
            1 => Self::InProgress,
            2 => Self::Complete,
            3 => Self::Cancelled,
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

#[derive(Default)]
pub(crate) struct Task {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) status: TaskStatus,
    pub(crate) created: DateTime<Utc>,
    pub(crate) priority: u64,
    pub(crate) content: Option<TaskContent>,
}

pub(crate) struct TaskContent {
    pub(crate) body: Option<String>,
    pub(crate) link: Option<Uri>,
}

impl Task {
    pub(crate) fn new(
        id: u64,
        status: TaskStatus,
        title: String,
        created: DateTime<Utc>,
        priority: u64,
    ) -> Self {
        Task {
            id,
            status,
            title,
            created,
            priority,
            content: None,
        }
    }

    pub(crate) fn set_content(&mut self, content: TaskContent) -> Option<TaskContent> {
        replace(&mut self.content, Some(content))
    }
}
