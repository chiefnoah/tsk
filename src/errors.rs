use std::{convert::Infallible, string::FromUtf8Error};

use thiserror::Error as ThisError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(ThisError, Debug)]
pub enum Error {
    #[error("The workspace is not initialized. Run `tsk init` to initialize it.")]
    Uninitialized,
    #[error("The tsk workspace is already initialized. No change.")]
    AlreadyInitialized,
    #[error("Unable to read file: {0}")]
    Io(#[from] std::io::Error),
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("Unable to parse id: {0}")]
    ParseId(#[from] std::num::ParseIntError),
    #[error("General parsing error: {0}")]
    Parse(String),
    #[error("Error parsing bytes as utf-8: {0}")]
    FromUtf8(#[from] FromUtf8Error),
    #[error("No tasks on stack")]
    NoTasks,
    #[allow(dead_code)]
    #[error("An unexpected error occurred: {0}")]
    Oops(Box<dyn std::error::Error>),
    #[error("System time/clock error: {0}")]
    SystemTime(#[from] std::time::SystemTimeError),
}

impl From<Infallible> for Error {
    fn from(_: Infallible) -> Self {
        unreachable!();
    }
}
