use crate::errors::Result;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Recursively searches upwards for a directory
pub fn find_parent_with_dir(
    dir: PathBuf,
    searching_for: impl AsRef<Path>,
) -> Result<Option<PathBuf>> {
    let mut d = dir.join(&searching_for);
    while d.pop() {
        let check = d.join(&searching_for);
        if check.exists() {
            if fs::metadata(&check)?.dev() != fs::metadata(&dir)?.dev() {
                return Ok(None);
            }
            return Ok(Some(check));
        }
    }
    Ok(None)
}
