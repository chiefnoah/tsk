#![allow(dead_code)]
use nix::fcntl::{Flock, FlockArg};

use crate::errors::{Error, Result};
use crate::stack::TaskStack;
use crate::{fzf, util};
use std::fmt::Display;
use std::fs::{self, File};
use std::io::{BufRead as _, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::str::FromStr;
use std::{fs::OpenOptions, io::Write};

const INDEXFILE: &str = "index";
const TITLECACHEFILE: &str = "cache";
/// A unique identifier for a task. When referenced in text, it is prefixed with `tsk-`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Id(pub u32);

impl FromStr for Id {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let s = s
            .strip_prefix("tsk-")
            .ok_or(Self::Err::Parse(format!("expected tsk- prefix. Got {s}")))?;
        Ok(Self(s.parse()?))
    }
}

impl Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tsk-{}", self.0)
    }
}

impl From<u32> for Id {
    fn from(value: u32) -> Self {
        Id(value)
    }
}

impl Id {
    pub fn to_filename(&self) -> String {
        format!("tsk-{}.tsk", self.0)
    }
}

pub enum TaskIdentifier {
    Id(Id),
    Relative(u32),
    Find,
}

impl From<Id> for TaskIdentifier {
    fn from(value: Id) -> Self {
        TaskIdentifier::Id(value)
    }
}

pub struct Workspace {
    /// The path to the workspace root, excluding the .tsk directory. This should *contain* the
    /// .tsk directory.
    path: PathBuf,
}

impl Workspace {
    pub fn init(path: PathBuf) -> Result<()> {
        // TODO: detect if in a git repo and add .tsk/ to `.git/info/exclude`
        let tsk_dir = path.join(".tsk");
        if tsk_dir.exists() {
            return Err(Error::AlreadyInitialized);
        }
        std::fs::create_dir(&tsk_dir)?;
        // Create the tasks directory
        std::fs::create_dir(&tsk_dir.join("tasks"))?;
        // Create the archive directory
        std::fs::create_dir(&tsk_dir.join("archive"))?;
        let mut next = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(tsk_dir.join("next"))?;
        next.write_all(b"1\n")?;
        Ok(())
    }

    pub fn from_path(path: PathBuf) -> Result<Self> {
        let tsk_dir = util::find_parent_with_dir(path, ".tsk")?.ok_or(Error::Uninitialized)?;
        Ok(Self { path: tsk_dir })
    }

    fn resolve(&self, identifier: TaskIdentifier) -> Result<Id> {
        match identifier {
            TaskIdentifier::Id(id) => Ok(id),
            TaskIdentifier::Relative(r) => {
                let stack = self.read_stack()?;
                let stack_item = stack.get(r as usize).ok_or(Error::NoTasks)?;
                Ok(stack_item.id)
            }
            TaskIdentifier::Find => self.search(None, false, false)?.ok_or(Error::NotSelected),
        }
    }

    pub fn next_id(&self) -> Result<Id> {
        let mut file = util::flopen(self.path.join("next"), FlockArg::LockExclusive)?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        let id = buf.trim().parse::<u32>()?;
        // reset the files contents
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        // store the *next* if
        file.write_all(format!("{}\n", id + 1).as_bytes())?;
        Ok(Id(id))
    }

    pub fn new_task(&self, title: String, body: String) -> Result<Task> {
        // WARN: we could improperly increment the id if the task is not written to disk/errors.
        // But who cares
        let id = self.next_id()?;
        let task_path = self.path.join("tasks").join(format!("tsk-{}.tsk", id.0));
        let mut file = util::flopen(task_path.clone(), FlockArg::LockExclusive)?;
        file.write_all(format!("{title}\n\n{body}").as_bytes())?;
        // create a hardlink to the archive dir
        fs::hard_link(
            task_path,
            self.path.join("archive").join(format!("tsk-{}.tsk", id.0)),
        )?;
        Ok(Task {
            id,
            title,
            body,
            file,
        })
    }

    pub fn task(&self, identifier: TaskIdentifier) -> Result<Task> {
        let id = self.resolve(identifier)?;

        let file = util::flopen(
            self.path.join("tasks").join(format!("tsk-{}.tsk", id.0)),
            FlockArg::LockExclusive,
        )?;
        let mut title = String::new();
        let mut body = String::new();
        let mut reader = BufReader::new(&*file);
        reader.read_line(&mut title)?;
        reader.read_to_string(&mut body)?;
        drop(reader);
        Ok(Task {
            id,
            title,
            body,
            file,
        })
    }

    pub fn read_stack(&self) -> Result<TaskStack> {
        TaskStack::from_tskdir(&self.path)
    }

    pub fn push_task(&self, task: Task) -> Result<()> {
        let mut stack = TaskStack::from_tskdir(&self.path)?;
        stack.push(task.try_into()?);
        stack.save()?;
        Ok(())
    }

    pub fn swap_top(&self) -> Result<()> {
        let mut stack = TaskStack::from_tskdir(&self.path)?;
        stack.swap();
        stack.save()?;
        Ok(())
    }

    pub fn rot(&self) -> Result<()> {
        let mut stack = TaskStack::from_tskdir(&self.path)?;
        let top = stack.pop();
        let second = stack.pop();
        let third = stack.pop();

        if top.is_none() || second.is_none() || third.is_none() {
            return Ok(());
        }

        stack.push(second.unwrap());
        stack.push(top.unwrap());
        stack.push(third.unwrap());
        stack.save()?;
        Ok(())
    }

    /// The inverse of tor. Pushes the top item behind the second item, shifting #2 and #3 to #1
    /// and #2 respectively.
    pub fn tor(&self) -> Result<()> {
        let mut stack = TaskStack::from_tskdir(&self.path)?;
        let top = stack.pop();
        let second = stack.pop();
        let third = stack.pop();

        if top.is_none() || second.is_none() || third.is_none() {
            return Ok(());
        }

        stack.push(top.unwrap());
        stack.push(third.unwrap());
        stack.push(second.unwrap());
        stack.save()?;
        Ok(())
    }

    pub fn drop(&self) -> Result<Option<Id>> {
        let mut stack = self.read_stack()?;
        if let Some(stack_item) = stack.pop() {
            let task_path = self
                .path
                .join("tasks")
                .join(format!("{}.tsk", stack_item.id));
            fs::remove_file(task_path)?;
            stack.save()?;
            Ok(Some(stack_item.id))
        } else {
            Ok(None)
        }
    }

    pub fn search(
        &self,
        stack: Option<TaskStack>,
        _search_body: bool,
        _include_archived: bool,
    ) -> Result<Option<Id>> {
        let stack = if let Some(stack) = stack {
            stack
        } else {
            self.read_stack()?
        };
        Ok(fzf::select(stack)?.map(|si| si.id))
    }

    pub fn reprioritize(&self, identifier: TaskIdentifier) -> Result<()> {
        let id = self.resolve(identifier)?;
        let mut stack = self.read_stack()?;
        let index = &stack.iter().map(|i| i.id).position(|i| i == id);
        if let Some(index) = index {
            let prioritized_task = stack.remove(*index);
            // unwrap here is safe because we just searched for the index and know it exists
            stack.push(prioritized_task.unwrap());
            stack.save()?;
        }
        Ok(())
    }
}

pub struct Task {
    pub id: Id,
    pub title: String,
    pub body: String,
    pub file: Flock<File>,
}

impl Task {
    /// Consumes a task and saves it to disk.
    pub fn save(mut self) -> Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(self.title.trim().as_bytes())?;
        self.file.write_all(b"\n\n")?;
        self.file.write_all(self.body.trim().as_bytes())?;
        Ok(())
    }
}
