//! The Task stack. Tasks created with `push` end up at the top here. It is invalid for a task that
//! has been completed/archived to be on the stack.

use crate::errors::{Error, Result};
use crate::util;
use std::io::{self, BufRead, Read};
use std::{fs::File, path::PathBuf};

use nix::fcntl::{Flock, FlockArg};

use crate::workspace::{Id, Workspace};

struct StackItem {
    id: Id,
    title: String,
    next: Id,
}

fn eof() -> Error {
    Error::Io(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "Unexpected end of file",
    ))
}

impl StackItem {
    fn from_reader(workspace_path: &PathBuf, reader: &mut impl BufRead) -> Result<Self> {
        let mut buf = String::new();
        reader.read_line(&mut buf)?;
        if buf.is_empty() {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Empty line",
            )));
        }
        let (id, next) = Self::parse(&buf)?;
        let title = util::flopen(workspace_path.join("tasks").join(id), mode)
        todo!();
    }

    fn parse(line: &str) -> Result<(Id, Id)> {
        let mut split = line.split("->");
        let curr = split.next().ok_or(eof())?;
        let next = split.next().ok_or(eof())?;
        if let Some(rest) = split.next() {
            Err(Error::Parse(format!(
                "Got unexpected data in index item: {rest}"
            )))
        } else {
            Ok((curr.parse()?, next.parse()?))
        }
    }
}

pub struct TaskStack {
    /// The index into `all` that is the top of the stack
    top: usize,
    all: Vec<StackItem>,
    file: Flock<File>,
}

impl TaskStack {
    fn from_tskdir(path: &PathBuf) -> Result<Self> {
        todo!()
    }
}
