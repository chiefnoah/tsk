use std::io::Error as IOError;

pub(super) enum Error {
    Config(IOError),
    Database(String),
}

pub(super) type Result<T> = std::result::Result<T, Error>;

impl From<IOError> for Error {
    fn from(value: IOError) -> Self {
        Error::Config(value)
    }
}
