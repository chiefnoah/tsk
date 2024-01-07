use std::io::Error as IOError;
use uris::Error as URIError;

#[derive(Debug)]
pub(super) enum Error {
    Config(IOError),
    Database(String),
    Internal(String),
    Bug(String),
    URIFormat(URIError)
}

pub(super) type Result<T> = std::result::Result<T, Error>;

impl From<IOError> for Error {
    fn from(value: IOError) -> Self {
        Error::Config(value)
    }
}

impl From<URIError> for Error {
    fn from(value: URIError) -> Self {
        Error::URIFormat(value)
    }
}
