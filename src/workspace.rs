#![allow(dead_code)]
use nix::fcntl::{Flock, FlockArg};

use crate::errors::{Error, Result};
use crate::util;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::PathBuf;
use std::str::FromStr;
use std::{fs::OpenOptions, io::Write};

/// A unique identifier for a task. When referenced in text, it is prefixed with `tsk-`.
pub struct Id(u32);

impl FromStr for Id {
    type Err = Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        s.strip_prefix("tsk-")
            .ok_or(Self::Err::Parse("expected tsk- prefix ".to_string()))?;
        Ok(Self(s.parse()?))
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
        let mut file = util::flopen(&self.path.join("next"), FlockArg::LockExclusive)?;
        let mut buf = String::new();
        file.read_to_string(&mut buf)?;
        let id = buf.trim().parse::<u32>()?;
        // reset the files contents
        file.set_len(0)?;
        // TODO: figure out if this is necessary
        file.seek(std::io::SeekFrom::Start(0))?;
        // store the *next* if
        file.write_all(format!("{}\n", id + 1).as_bytes())?;
        Ok(Id(id))
    }

    pub fn new_task(&self, title: String, body: String) -> Result<Task> {
        // TODO: we could improperly increment the id if the task is not written to disk/errors
        let id = self.next_id()?;
        let mut file = util::flopen(
            &self.path.join("tasks").join(format!("tsk-{}.tsk", id.0)),
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
}

pub struct Task {
    id: Id,
    title: String,
    body: String,
    file: Flock<File>,
}

#[cfg(test)]
mod test {
    fn test_next_id() {}
}
