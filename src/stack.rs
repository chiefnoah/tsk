#![allow(dead_code)]
//! The Task stack. Tasks created with `push` end up at the top here. It is invalid for a task that
//! has been completed/archived to be on the stack.
//!
//! The stack is persisted as a single blob keyed `index` in the workspace's
//! [`Store`](crate::backend::Store). Each line is `tsk-N\ttitle\ttimestamp`.

use crate::backend::Store;
use crate::errors::{Error, Result};
use std::collections::VecDeque;
use std::collections::vec_deque::Iter;
use std::fmt::Display;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::workspace::{Id, Task};

#[derive(Clone)]
pub struct StackItem {
    pub id: Id,
    pub title: String,
    pub modify_time: SystemTime,
}

impl Display for StackItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}\t{}", self.id, self.title.trim())
    }
}

impl From<&Task> for StackItem {
    fn from(value: &Task) -> Self {
        Self {
            id: value.id,
            title: value.title.replace("\t", " "),
            modify_time: SystemTime::now(),
        }
    }
}

impl From<Task> for StackItem {
    fn from(value: Task) -> Self {
        Self::from(&value)
    }
}

impl FromStr for StackItem {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let mut parts = s.trim().split('\t');
        let id: Id = parts
            .next()
            .ok_or(Error::Parse("Incomplete index line. Missing tsk ID".into()))?
            .parse()?;
        let title = parts
            .next()
            .ok_or(Error::Parse("Incomplete index line. Missing title.".into()))?
            .trim()
            .to_string();
        let index_epoch: u64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let modify_time = UNIX_EPOCH
            .checked_add(Duration::from_secs(index_epoch))
            .unwrap_or(UNIX_EPOCH);
        Ok(Self {
            id,
            title,
            modify_time,
        })
    }
}

pub struct TaskStack {
    pub all: VecDeque<StackItem>,
}

impl TaskStack {
    pub fn parse(text: &str) -> Result<Self> {
        let mut all = VecDeque::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            all.push_back(line.parse()?);
        }
        Ok(Self { all })
    }

    pub fn load(store: &dyn Store) -> Result<Self> {
        let raw = store.read("index")?.unwrap_or_default();
        Self::parse(&String::from_utf8_lossy(&raw))
    }

    pub fn serialize(&self) -> String {
        let mut s = String::new();
        for item in &self.all {
            let ts = item
                .modify_time
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            s.push_str(&format!("{item}\t{ts}\n"));
        }
        s
    }

    pub fn save(&self, store: &dyn Store) -> Result<()> {
        store.write("index", self.serialize().as_bytes())
    }

    pub fn push(&mut self, item: StackItem) {
        self.all.push_front(item);
    }

    pub fn push_back(&mut self, item: StackItem) {
        self.all.push_back(item);
    }

    pub fn pop(&mut self) -> Option<StackItem> {
        self.all.pop_front()
    }

    pub fn swap(&mut self) {
        let tip = self.all.pop_front();
        let second = self.all.pop_front();
        if let Some((tip, second)) = tip.zip(second) {
            self.all.push_front(tip);
            self.all.push_front(second);
        }
    }

    pub fn empty(&self) -> bool {
        self.all.is_empty()
    }

    pub fn remove(&mut self, index: usize) -> Option<StackItem> {
        self.all.remove(index)
    }

    pub fn iter(&self) -> Iter<'_, StackItem> {
        self.all.iter()
    }

    pub fn get(&self, index: usize) -> Option<&StackItem> {
        self.all.get(index)
    }

    pub fn position(&self, id: Id) -> Option<usize> {
        self.all.iter().position(|i| i.id == id)
    }

    /// Refresh stack item titles from authoritative task content.
    pub fn refresh_titles(&mut self, store: &dyn Store) -> Result<()> {
        for item in self.all.iter_mut() {
            if let Some((title, _, _)) = crate::backend::read_task(store, item.id)? {
                item.title = title.replace('\t', " ");
            }
        }
        Ok(())
    }
}

impl IntoIterator for TaskStack {
    type Item = StackItem;
    type IntoIter = std::collections::vec_deque::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.all.into_iter()
    }
}
