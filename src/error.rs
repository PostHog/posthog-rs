use std::fmt::{Display, Formatter};

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connection(msg) => write!(f, "Connection Error: {}", msg),
            Error::Serialization(msg) => write!(f, "Serialization Error: {}", msg),
            Error::AlreadyInitialized => write!(f, "Client already initialized"),
            Error::NotInitialized => write!(f, "Client not initialized"),
        }
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Connection(String),
    Serialization(String),
    AlreadyInitialized,
    NotInitialized,
}
