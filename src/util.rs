use crate::errors::{Error, Result};
use std::{
    fs::{File, OpenOptions},
    path::PathBuf,
};

use nix::fcntl::{Flock, FlockArg};

pub fn flopen(path: &PathBuf, mode: FlockArg) -> Result<Flock<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)?;
    Ok(Flock::lock(file, mode).map_err(|(_, errno)| Error::Lock(errno))?)
}
