#![allow(dead_code)]
//! The Task stack. Tasks created with `push` end up at the top here. It is invalid for a task that
//! has been completed/archived to be on the stack.

use crate::errors::{Error, Result};
use crate::util;
use std::collections::VecDeque;
use std::fmt::Display;
use std::io::{self, BufRead, BufReader, Seek, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs::File, path::PathBuf};

use nix::fcntl::{Flock, FlockArg};

use crate::workspace::{Id, Task};

const TASKSFOLDER: &str = "tasks";
const INDEXFILE: &str = "index";

pub(crate) struct StackItem {
    id: Id,
    title: String,
    modify_time: SystemTime,
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

impl StackItem {
    /// Parses a [`StackItem`] from a string. The expected format is a tab-delimited line with the
    /// files: task id	title
    fn from_line(workspace_path: &PathBuf, line: String) -> Result<Self> {
        let mut parts = line.split("\t");
        let id: Id = parts
            .next()
            .ok_or(Error::Parse(format!(
                "Incomplete index line. Missing tsk ID"
            )))?
            .parse()?;
        let mut title: String = parts
            .next()
            .ok_or(Error::Parse(format!(
                "Incomplete index line. Missing title."
            )))?
            .trim()
            .to_string();
        // parse the timestamp as an integer
        let index_epoch: u64 = parts.next().unwrap_or("0").parse()?;
        // get a usable system time from the UNIX epoch, defaulting to the UNIX_EPOCH if there's
        // any failures. This means that if there's errors, we will always read the title and
        // modify_time from the task file.
        let mut modify_time = UNIX_EPOCH
            .checked_add(Duration::from_secs(index_epoch))
            .unwrap_or(UNIX_EPOCH);
        let modify_epoch = modify_time
            .duration_since(UNIX_EPOCH)
            .expect("We're before the dawn of time!?")
            .as_secs();
        let task = util::flopen(
            workspace_path.join(TASKSFOLDER).join(id.to_string()),
            FlockArg::LockExclusive,
        )?;
        let task_modify_time = task.metadata()?.modified()?;
        // if the task file has been modified since we last looked at it, re-read the title and
        // metadata
        if modify_epoch > index_epoch {
            title.clear();
            BufReader::new(&*task).read_line(&mut title)?;
            modify_time = task_modify_time;
        }
        Ok(Self {
            id,
            title,
            modify_time,
        })
    }
}

pub struct TaskStack {
    /// All items within the stack
    all: VecDeque<StackItem>,
    file: Flock<File>,
}

impl Display for TaskStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for task in self.all.iter() {
            write!(f, "{task}\n")?;
        }
        Ok(())
    }
}

impl TaskStack {
    pub fn from_tskdir(workspace_path: &PathBuf, count: Option<usize>) -> Result<Self> {
        let file = util::flopen(workspace_path.join(INDEXFILE), FlockArg::LockExclusive)?;
        let index = BufReader::new(&*file).lines();
        let mut all = VecDeque::new();
        if let Some(count) = count {
            for line in index.take(count) {
                let line = line?;
                let stack_item = StackItem::from_line(workspace_path, line)?;
                all.push_back(stack_item);
            }
        } else {
            for line in index {
                let stack_item = StackItem::from_line(workspace_path, line?)?;
                all.push_back(stack_item);
            }
        };
        Ok(Self { all, file })
    }

    /// Saves the task stack to disk.
    pub fn save(mut self) -> Result<()> {
        // Clear the file
        self.file.seek(std::io::SeekFrom::Start(0))?;
        self.file.set_len(0)?;
        for item in self.all.iter() {
            self.file.write_all(format!("{item}\n").as_bytes())?;
        }
        Ok(())
    }

    pub fn push(&mut self, item: StackItem) {
        self.all.push_front(item);
    }

    pub fn pop(&mut self) -> Option<StackItem> {
        self.all.pop_front()
    }

    pub fn swap(&mut self) {
        let tip = self.all.pop_front();
        let second = self.all.pop_front();
        if tip.is_some() && second.is_some() {
            self.all.push_front(tip.unwrap());
            self.all.push_front(second.unwrap());
        }
    }

    pub fn empty(&self) -> bool {
        self.all.is_empty()
    }
}
