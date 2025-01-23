use crate::errors::{Error, Result};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use nix::fcntl::{Flock, FlockArg};

pub fn flopen(path: PathBuf, mode: FlockArg) -> Result<Flock<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)?;
    Flock::lock(file, mode).map_err(|(_, errno)| Error::Lock(errno))
}

/// Recursively searches upwards for a directory
pub fn find_parent_with_dir(
    dir: PathBuf,
    searching_for: impl AsRef<Path>,
) -> Result<Option<PathBuf>> {
    // Create a new pathbuf to modify, we slap a segment onto the end but then pop it off right
    // away
    let mut d = dir.join(&searching_for);
    while d.pop() {
        let check = d.join(&searching_for);
        if check.exists() {
            if fs::metadata(&check)?.dev() != fs::metadata(&dir)?.dev() {
                // we hit a filesystem boundary
                return Ok(None);
            }
            return Ok(Some(check));
        }
    }
    Ok(None)
}
