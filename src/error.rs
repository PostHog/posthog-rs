use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub enum Error {
    Connection(String),
    Serialization(String),
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connection(msg) => write!(f, "Connection Error: {}", msg),
            Error::Serialization(msg) => write!(f, "Serialization Error: {}", msg),
        }
    }
}

impl std::error::Error for Error {}
