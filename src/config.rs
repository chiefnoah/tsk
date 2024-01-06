const APP_PREFIX: &'static str = "tsk";
const DATABASE: &'static str = "tsk.db";
const CONFIG: &'static str = "config.toml";
use std::path::PathBuf;
use xdg;

const XDG_DIRS: xdg::BaseDirectories = xdg::BaseDirectories::with_prefix(APP_PREFIX).unwrap();

pub(super) fn get_database_file() -> Result<PathBuf> {
    XDG_DIRS.place_state_file(&DATABASE)
}

pub fn get_config_file() -> Result<PathBuf> {
    XDG_DIRS.place_config_file(&CONFIG)
}
