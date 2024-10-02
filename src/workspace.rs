#![allow(dead_code)]
use nix::fcntl::{Flock, FlockArg};

use crate::errors::{Error, Result};
use crate::stack::TaskStack;
use crate::util;
use std::fmt::Display;
use std::fs::File;
use std::io::{BufRead as _, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::str::FromStr;
use std::{fs::OpenOptions, io::Write};

const INDEXFILE: &str = "index";
const TITLECACHEFILE: &str = "cache";
/// A unique identifier for a task. When referenced in text, it is prefixed with `tsk-`.
pub struct Id(u32);

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
    pub fn to_string(&self) -> String {
        format!("tsk-{}.tsk", self.0)
    }
}

pub struct Workspace {
    /// The path to the workspace root, excluding the .tsk directory. This should *contain* the
    /// .tsk directory.
    path: PathBuf,
}

impl Workspace {
    pub fn init(path: PathBuf) -> Result<()> {
        let tsk_dir = path.join(".tsk");
        if tsk_dir.exists() {
            return Err(Error::AlreadyInitialized);
        }
        std::fs::create_dir(&tsk_dir)?;
        // Create the tasks directory
        std::fs::create_dir(&tsk_dir.join("tasks"))?;
        let mut next = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(tsk_dir.join("next"))?;
        next.write_all(b"1\n")?;
        Ok(())
    }

    pub fn from_path(path: PathBuf) -> Result<Self> {
        // TODO: recursively walk up the path until we find a .tsk dir or error if we can't find
        // one / cross a filesystem boundary
        let tsk_dir = path.join(".tsk");
        if !tsk_dir.exists() {
            return Err(Error::Uninitialized);
        } else {
            Ok(Self { path: tsk_dir })
        }
    }

    pub fn next_id(&self) -> Result<Id> {
        let mut file = util::flopen(self.path.join("next"), FlockArg::LockExclusive)?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        let id = buf.trim().parse::<u32>()?;
        // reset the files contents
        file.set_len(0)?;
        // TODO: figure out if this is necessary
        file.seek(SeekFrom::Start(0))?;
        // store the *next* if
        file.write_all(format!("{}\n", id + 1).as_bytes())?;
        Ok(Id(id))
    }

    pub fn new_task(&self, title: String, body: String) -> Result<Task> {
        // TODO: we could improperly increment the id if the task is not written to disk/errors
        let id = self.next_id()?;
        let mut file = util::flopen(
            self.path.join("tasks").join(format!("tsk-{}.tsk", id.0)),
            FlockArg::LockExclusive,
        )?;
        file.write_all(format!("{title}\n\n{body}").as_bytes())?;
        Ok(Task {
            id,
            title,
            body,
            file,
        })
    }

    pub fn task(&self, id: Id) -> Result<Task> {
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

    pub fn read_stack(&self, count: Option<usize>) -> Result<TaskStack> {
        TaskStack::from_tskdir(&self.path, count)
    }

    pub fn push_task(&self, task: Task) -> Result<()> {
        let mut stack = TaskStack::from_tskdir(&self.path, None)?;
        stack.push(task.try_into()?);
        stack.save()?;
        Ok(())
    }

    pub fn swap_top(&self) -> Result<()> {
        let mut stack = TaskStack::from_tskdir(&self.path, None)?;
        stack.swap();
        stack.save()?;
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
