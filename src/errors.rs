use std::fmt::{Display, Formatter};

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connection(msg) => write!(f, "Connection Error: {}", msg),
            Self::Serialization(msg) => write!(f, "Serialization Error: {}", msg),
            Self::EmptyReply(msg) => write!(f, "Response Error: {}", msg),
            Self::ClientOptionConfigError(msg) => {
                write!(f, "Error configuring client options: {}", msg)
            }
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug)]
pub enum Error {
    /// Connection error towards API
    Connection(String),
    /// Error in serialization of data
    Serialization(String),
    /// Error if API returns 0 result
    EmptyReply(String),
    /// Error while attempting to create a ClientOptions from Builder
    ClientOptionConfigError(String),
}
