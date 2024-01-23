const APP_PREFIX: &'static str = "tsk";
const DATABASE: &'static str = "tsk.db";
const CONFIG: &'static str = "config.toml";
use crate::error::{Error, Result};
use std::path::PathBuf;
use xdg;

pub(crate) struct Config {
    pub num_top_tasks: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self { num_top_tasks: 20 }
    }
}

impl From<xdg::BaseDirectoriesError> for Error {
    fn from(value: xdg::BaseDirectoriesError) -> Self {
        Error::Internal(format!("Error getting XDG_DIRS: {value:?}"))
    }
}

pub(super) fn get_database_file() -> Result<PathBuf> {
    let xdg_dirs = xdg::BaseDirectories::with_prefix(APP_PREFIX)?;
    Ok(xdg_dirs.place_state_file(&DATABASE)?)
}

pub fn get_config_file() -> Result<PathBuf> {
    let xdg_dirs = xdg::BaseDirectories::with_prefix(APP_PREFIX)?;
    Ok(xdg_dirs.place_config_file(&CONFIG)?)
}
