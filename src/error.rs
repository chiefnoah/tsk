use std::io::Error as IOError;

#[derive(Debug)]
pub(super) enum Error {
    Config(IOError),
    Database(String),
    Internal(String),
}

pub(super) type Result<T> = std::result::Result<T, Error>;

impl From<IOError> for Error {
    fn from(value: IOError) -> Self {
        Error::Config(value)
    }
}
