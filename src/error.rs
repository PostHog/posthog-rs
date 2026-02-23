use std::fmt::{Display, Formatter};

impl std::error::Error for Error {}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connection(msg) => write!(f, "Connection Error: {msg}"),
            Error::Serialization(msg) => write!(f, "Serialization Error: {msg}"),
            Error::AlreadyInitialized => write!(f, "Client already initialized"),
            Error::NotInitialized => write!(f, "Client not initialized"),
            Error::InvalidTimestamp(msg) => write!(f, "Invalid Timestamp: {msg}"),
            Error::InconclusiveMatch(msg) => write!(f, "Inconclusive Match: {msg}"),
            Error::RateLimit => write!(f, "Rate limited"),
            Error::BadRequest(msg) => write!(f, "Bad Request: {msg}"),
            Error::ServerError { status, message } => {
                write!(f, "Server Error (HTTP {status}): {message}")
            }
        }
    }
}

/// Errors that can occur when using the PostHog client.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Network or HTTP error when communicating with PostHog API
    Connection(String),
    /// Error serializing or deserializing JSON data
    Serialization(String),
    /// Global client was already initialized via `init_global`
    AlreadyInitialized,
    /// Global client was not initialized before use
    NotInitialized,
    /// Timestamp could not be parsed or is invalid
    InvalidTimestamp(String),
    /// Flag evaluation was inconclusive (e.g., missing required properties, unknown operator)
    InconclusiveMatch(String),
    /// HTTP 429 — the server is rate limiting requests
    RateLimit,
    /// HTTP 400 or 413 — the request was malformed or too large
    BadRequest(String),
    /// HTTP 5xx — the server encountered an error
    ServerError { status: u16, message: String },
}

impl Error {
    /// Construct an error from an HTTP response's status and body.
    /// Returns `None` for success (2xx) status codes.
    pub(crate) fn from_http_response(status: u16, body: String) -> Option<Self> {
        match status {
            200..=299 => None,
            429 => Some(Error::RateLimit),
            400 | 413 => Some(Error::BadRequest(body)),
            500..=599 => Some(Error::ServerError {
                status,
                message: body,
            }),
            _ => Some(Error::Connection(format!(
                "Unexpected HTTP status {status}: {body}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_returns_none() {
        assert!(Error::from_http_response(200, String::new()).is_none());
        assert!(Error::from_http_response(201, String::new()).is_none());
        assert!(Error::from_http_response(299, String::new()).is_none());
    }

    #[test]
    fn rate_limit() {
        let err = Error::from_http_response(429, String::new()).unwrap();
        assert!(matches!(err, Error::RateLimit));
    }

    #[test]
    fn bad_request_preserves_body() {
        let err = Error::from_http_response(400, "invalid payload".to_string()).unwrap();
        match err {
            Error::BadRequest(msg) => assert_eq!(msg, "invalid payload"),
            _ => panic!("expected BadRequest"),
        }
    }

    #[test]
    fn payload_too_large() {
        let err = Error::from_http_response(413, "too large".to_string()).unwrap();
        match err {
            Error::BadRequest(msg) => assert_eq!(msg, "too large"),
            _ => panic!("expected BadRequest for 413"),
        }
    }

    #[test]
    fn server_error_preserves_status_and_body() {
        let err = Error::from_http_response(503, "unavailable".to_string()).unwrap();
        match err {
            Error::ServerError { status, message } => {
                assert_eq!(status, 503);
                assert_eq!(message, "unavailable");
            }
            _ => panic!("expected ServerError"),
        }
    }

    #[test]
    fn unexpected_status_becomes_connection_error() {
        let err = Error::from_http_response(302, "redirect".to_string()).unwrap();
        match err {
            Error::Connection(msg) => {
                assert!(msg.contains("302"));
                assert!(msg.contains("redirect"));
            }
            _ => panic!("expected Connection"),
        }
    }
}
