#![allow(dead_code)]
use nix::fcntl::{Flock, FlockArg};

use crate::errors::{Error, Result};
use std::fs::File;
use std::io::{Read, Seek};
use std::path::PathBuf;
use std::{fs::OpenOptions, io::Write};

pub struct Id(u32);

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
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .open(self.path.join("next"))?;
        let mut lock =
            Flock::lock(file, FlockArg::LockExclusive).map_err(|(_, errno)| Error::Lock(errno))?;
        let mut buf = String::new();
        lock.read_to_string(&mut buf)?;
        let id = buf.trim().parse::<u32>()?;
        // reset the files contents
        lock.set_len(0)?;
        // TODO: figure out if this is necessary
        lock.seek(std::io::SeekFrom::Start(0))?;
        // store the *next* if
        lock.write_all(format!("{}\n", id + 1).as_bytes())?;
        Ok(Id(id))
    }

    pub fn new_task(&self, title: String, body: String) -> Result<Task> {
        // TODO: we could improperly increment the id if the task is not written to disk/errors
        let id = self.next_id()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(self.path.join("tasks").join(format!("tsk-{}.tsk", id.0)))?;
        let mut file =
            Flock::lock(file, FlockArg::LockExclusive).map_err(|(_, errno)| Error::Lock(errno))?;
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
