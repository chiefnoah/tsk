//! The Task stack. Tasks created with `push` end up at the top here. It is invalid for a task that
//! has been completed/archived to be on the stack.

use crate::errors::{Error, Result};
use crate::util;
use std::io::{self, BufRead};
use std::num::ParseIntError;
use std::{fs::File, io::Read, path::PathBuf};

use nix::fcntl::{Flock, FlockArg};

use crate::workspace::Id;

const INDEXFILE: &str = "index";
const TITLECACHEFILE: &str = "cache";

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
    fn from_reader(reader: &mut impl BufRead) -> Result<Self> {
        let mut buf = String::new();
        reader.read_line(&mut buf)?;
        if buf.is_empty() {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Empty line",
            )));
        }
        let (id, next) = Self::parse(&buf)?;
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
    fn from_tskdir(path: &PathBuf) -> Self {
        let index = util::flopen(&path.join(INDEXFILE), FlockArg::LockExclusive);
        let cache = util::flopen(&path.join(TITLECACHEFILE), FlockArg::LockShared);

        todo!()
    }
}
