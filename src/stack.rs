#![allow(dead_code)]
//! The Task stack. Tasks created with `push` end up at the top here. It is invalid for a task that
//! has been completed/archived to be on the stack.

use crate::errors::{Error, Result};
use crate::util;
use std::collections::VecDeque;
use std::collections::vec_deque::Iter;
use std::fmt::Display;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, Write};
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nix::fcntl::{Flock, FlockArg};

use crate::workspace::{Id, Task};

const TASKSFOLDER: &str = "tasks";
const INDEXFILE: &str = "index";

pub struct StackItem {
    pub id: Id,
    pub title: String,
    pub modify_time: SystemTime,
}

impl Display for StackItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // .trim is used here on the title because there may be a newline in here if we read the
        // title from the task file.
        write!(
            f,
            // NOTE: we do NOT print the access time.
            "{}\t{}",
            self.id,
            self.title.trim(),
        )
    }
}

impl TryFrom<Task> for StackItem {
    type Error = Error;

    fn try_from(value: Task) -> std::result::Result<Self, Self::Error> {
        let modify_time = value.file.metadata()?.modified()?;
        Ok(Self {
            id: value.id,
            // replace tabs with spaces, they're not valid in StackItem titles.
            title: value.title.replace("\t", " "),
            modify_time,
        })
    }
}

fn eof() -> Error {
    Error::Io(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "Unexpected end of file",
    ))
}

impl FromStr for StackItem {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let mut parts = s.trim().split("\t");
        let id: Id = parts
            .next()
            .ok_or(Error::Parse(
                "Incomplete index line. Missing tsk ID".to_owned(),
            ))?
            .parse()?;
        let title: String = parts
            .next()
            .ok_or(Error::Parse(
                "Incomplete index line. Missing title.".to_owned(),
            ))?
            .trim()
            .to_string();
        // parse the timestamp as an integer
        let index_epoch: u64 = parts.next().unwrap_or("0").parse()?;
        // get a usable system time from the UNIX epoch, defaulting to the UNIX_EPOCH if there's
        // any failures. This means that if there's errors, we will always read the title and
        // modify_time from the task file.
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

impl StackItem {
    /// Parses a [`StackItem`] from a string. The expected format is a tab-delimited line with the
    /// files: task id title
    fn from_line(workspace_path: &Path, line: String) -> Result<Self> {
        let mut stack_item: StackItem = line.parse()?;

        let task = util::flopen(
            workspace_path
                .join(TASKSFOLDER)
                .join(stack_item.id.filename()),
            FlockArg::LockExclusive,
        )?;
        let task_modify_time = task.metadata()?.modified()?;
        // if the task file has been modified since we last looked at it, re-read the title and
        // metadata
        if (task_modify_time - Duration::from_secs(1)) > stack_item.modify_time {
            stack_item.title.clear();
            BufReader::new(&*task).read_line(&mut stack_item.title)?;
            stack_item.modify_time = task_modify_time;
        }
        Ok(stack_item)
    }
}

pub struct TaskStack {
    /// All items within the stack
    all: VecDeque<StackItem>,
    file: Flock<File>,
}

impl TaskStack {
    pub fn from_tskdir(workspace_path: &Path) -> Result<Self> {
        let file = util::flopen(workspace_path.join(INDEXFILE), FlockArg::LockExclusive)?;
        let index = BufReader::new(&*file).lines();
        let mut all = VecDeque::new();
        for line in index {
            let stack_item = StackItem::from_line(workspace_path, line?)?;
            all.push_back(stack_item);
        }
        Ok(Self { all, file })
    }

    /// Saves the task stack to disk.
    pub fn save(mut self) -> Result<()> {
        // Clear the file
        self.file.seek(std::io::SeekFrom::Start(0))?;
        self.file.set_len(0)?;
        for item in self.all.iter() {
            let time = item.modify_time.duration_since(UNIX_EPOCH)?.as_secs();
            self.file
                .write_all(format!("{item}\t{}\n", time).as_bytes())?;
        }
        Ok(())
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
}

impl IntoIterator for TaskStack {
    type Item = StackItem;

    type IntoIter = std::collections::vec_deque::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.all.into_iter()
    }
}
