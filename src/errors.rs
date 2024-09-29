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
    #[error("Unable to acquire locc: {0}")]
    Lock(nix::errno::Errno),
    #[error("Unable to parse id: {0}")]
    ParseId(#[from] std::num::ParseIntError),
    #[error("General parsing error: {0}")]
    Parse(String),
    #[error("An unexpected error occurred: {0}")]
    Oops(Box<dyn std::error::Error>),
}
