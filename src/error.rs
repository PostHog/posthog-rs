use std::time::Duration;
use thiserror::Error;

/// Main error type for the PostHog Rust SDK.
///
/// This enum is non-exhaustive to discourage matching on specific error variants.
/// Instead, use the provided methods like `is_retryable()` to determine how to handle errors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    // Deprecated variants - kept for backward compatibility
    #[deprecated(since = "0.4.0", note = "Use Error::Connection instead")]
    #[error("Connection error: {0}")]
    Connection(String),

    #[deprecated(since = "0.4.0", note = "Use Error::Validation instead")]
    #[error("Serialization error: {0}")]
    Serialization(String),

    #[deprecated(
        since = "0.4.0",
        note = "Use Error::Initialization(InitializationError::AlreadyInitialized) instead"
    )]
    #[error("Global client already initialized")]
    AlreadyInitialized,

    #[deprecated(
        since = "0.4.0",
        note = "Use Error::Initialization(InitializationError::NotInitialized) instead"
    )]
    #[error("Global client not initialized")]
    NotInitialized,

    #[deprecated(since = "0.4.0", note = "Use Error::Validation instead")]
    #[error("Invalid timestamp: {0}")]
    InvalidTimestamp(String),

    // New error categories
    /// Transport-layer errors (network, HTTP, etc.)
    #[error(transparent)]
    Transport(#[from] TransportError),

    /// Validation errors for events and data
    #[error(transparent)]
    Validation(#[from] ValidationError),

    /// Initialization and configuration errors
    #[error(transparent)]
    Initialization(#[from] InitializationError),
}

impl Error {
    /// Returns true if this error is potentially recoverable via retry.
    ///
    /// # Examples
    ///
    /// ```
    /// use posthog_rs::{Error, TransportError};
    /// use std::time::Duration;
    ///
    /// let err = Error::Transport(TransportError::Timeout(Duration::from_secs(30)));
    /// assert!(err.is_retryable());
    ///
    /// let err = Error::Validation(posthog_rs::ValidationError::InvalidTimestamp("future".to_string()));
    /// assert!(!err.is_retryable());
    /// ```
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Transport(e) => e.is_retryable(),
            _ => false,
        }
    }

    /// Returns true if this error is due to invalid client usage or configuration.
    ///
    /// Client errors indicate a problem with how the SDK is being used (validation
    /// errors, missing configuration, 4xx HTTP errors) rather than transient network
    /// issues. These errors typically require fixing the code rather than retrying.
    ///
    /// # Examples
    ///
    /// ```
    /// use posthog_rs::{Error, ValidationError, InitializationError, TransportError};
    ///
    /// // Validation errors are client errors
    /// let err = Error::Validation(ValidationError::InvalidTimestamp("future".to_string()));
    /// assert!(err.is_client_error());
    ///
    /// // Initialization errors are client errors
    /// let err = Error::Initialization(InitializationError::MissingApiKey);
    /// assert!(err.is_client_error());
    ///
    /// // 4xx HTTP errors are client errors
    /// let err = Error::Transport(TransportError::HttpError(400, "Bad Request".to_string()));
    /// assert!(err.is_client_error());
    ///
    /// // 5xx errors are NOT client errors
    /// let err = Error::Transport(TransportError::HttpError(500, "Server Error".to_string()));
    /// assert!(!err.is_client_error());
    /// ```
    pub fn is_client_error(&self) -> bool {
        match self {
            Error::Validation(_) | Error::Initialization(_) => true,
            Error::Transport(e) => e.is_client_error(),
            _ => false,
        }
    }
}

/// Errors related to network transport and HTTP communication.
///
/// Non-exhaustive to allow adding new error types without breaking changes.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TransportError {
    /// The request timed out after the specified duration
    #[error("Request timed out after {0:?}")]
    Timeout(Duration),

    /// DNS resolution failed for the hostname
    #[error("DNS resolution failed: {0}")]
    DnsResolution(String),

    /// Network is unreachable
    #[error("Network is unreachable")]
    NetworkUnreachable,

    /// HTTP error with status code and message
    #[error("HTTP error {0}: {1}")]
    HttpError(u16, String),

    /// TLS/SSL error
    #[error("TLS error: {0}")]
    TlsError(String),
}

impl TransportError {
    /// Returns true if this error is potentially recoverable via retry.
    ///
    /// Retryable errors include:
    /// - Timeouts
    /// - Network unreachable
    /// - HTTP 5xx errors (server errors)
    /// - HTTP 429 (rate limiting)
    pub fn is_retryable(&self) -> bool {
        match self {
            TransportError::Timeout(_) => true,
            TransportError::NetworkUnreachable => true,
            TransportError::HttpError(status, _) => {
                // Retry on 5xx errors and 429 (rate limit)
                (*status >= 500 && *status < 600) || *status == 429
            }
            _ => false,
        }
    }

    // Internal helper for Error::is_client_error()
    fn is_client_error(&self) -> bool {
        matches!(self, TransportError::HttpError(400..=499, _))
    }
}

/// Convert from reqwest::Error to TransportError
impl From<reqwest::Error> for TransportError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            TransportError::Timeout(Duration::from_secs(30))
        } else if err.is_connect() {
            if let Some(url) = err.url() {
                TransportError::DnsResolution(url.host_str().unwrap_or("unknown").to_string())
            } else {
                TransportError::NetworkUnreachable
            }
        } else if let Some(status) = err.status() {
            TransportError::HttpError(status.as_u16(), err.to_string())
        } else if err.to_string().contains("tls") || err.to_string().contains("ssl") {
            TransportError::TlsError(err.to_string())
        } else {
            TransportError::NetworkUnreachable
        }
    }
}

/// Errors related to event validation and data integrity.
///
/// These errors should be raised eagerly when users construct events,
/// rather than during serialization. If an event is valid, it must be serializable.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ValidationError {
    /// Event property value is too large
    #[error("Property '{key}' is too large ({size} bytes)")]
    PropertyTooLarge { key: String, size: usize },

    /// Event property has invalid type
    #[error("Property '{key}' has invalid type (expected {expected})")]
    InvalidPropertyType { key: String, expected: String },

    /// Timestamp is invalid (e.g., in the future)
    #[error("Invalid timestamp: {0}")]
    InvalidTimestamp(String),

    /// Distinct ID is invalid or empty
    #[error("Invalid distinct_id: {0}")]
    InvalidDistinctId(String),

    /// Batch size exceeds maximum allowed
    #[error("Batch size {size} exceeds maximum {max}")]
    BatchSizeExceeded { size: usize, max: usize },

    /// Event name is too long
    #[error("Event name is too long ({length} chars, max {max})")]
    EventNameTooLong { length: usize, max: usize },

    /// JSON serialization failed (should rarely happen if validation is correct)
    #[error("Serialization failed: {0}")]
    SerializationFailed(String),
}

/// Errors related to client initialization and configuration.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum InitializationError {
    /// API key is missing or empty
    #[error("API key is missing or empty")]
    MissingApiKey,

    /// API endpoint URL is invalid
    #[error("Invalid endpoint: {0}")]
    InvalidEndpoint(String),

    /// Timeout value is invalid
    #[error("Invalid timeout: {0:?}")]
    InvalidTimeout(Duration),

    /// Global client is already initialized
    #[error("Global client is already initialized")]
    AlreadyInitialized,

    /// Global client is not initialized
    #[error("Global client is not initialized")]
    NotInitialized,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transport_error_is_retryable() {
        assert!(TransportError::Timeout(Duration::from_secs(30)).is_retryable());
        assert!(TransportError::NetworkUnreachable.is_retryable());
        assert!(TransportError::HttpError(500, "Internal Server Error".to_string()).is_retryable());
        assert!(TransportError::HttpError(503, "Service Unavailable".to_string()).is_retryable());
        assert!(TransportError::HttpError(429, "Too Many Requests".to_string()).is_retryable());

        assert!(!TransportError::HttpError(400, "Bad Request".to_string()).is_retryable());
        assert!(!TransportError::HttpError(404, "Not Found".to_string()).is_retryable());
        assert!(!TransportError::DnsResolution("example.com".to_string()).is_retryable());
        assert!(!TransportError::TlsError("TLS error".to_string()).is_retryable());
    }

    #[test]
    fn test_error_is_retryable() {
        let err = Error::Transport(TransportError::Timeout(Duration::from_secs(30)));
        assert!(err.is_retryable());

        let err = Error::Validation(ValidationError::InvalidTimestamp("future".to_string()));
        assert!(!err.is_retryable());

        let err = Error::Initialization(InitializationError::MissingApiKey);
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_error_display() {
        let err = Error::Transport(TransportError::Timeout(Duration::from_secs(30)));
        assert!(err.to_string().contains("timed out"));

        let err = Error::Validation(ValidationError::InvalidTimestamp("future".to_string()));
        assert!(err.to_string().contains("Invalid timestamp"));

        let err = Error::Initialization(InitializationError::MissingApiKey);
        assert!(err.to_string().contains("API key"));
    }

    #[test]
    #[allow(deprecated)]
    fn test_deprecated_errors_still_work() {
        let err = Error::Connection("test".to_string());
        assert!(err.to_string().contains("Connection error"));

        let err = Error::Serialization("test".to_string());
        assert!(err.to_string().contains("Serialization error"));

        let err = Error::AlreadyInitialized;
        assert!(err.to_string().contains("already initialized"));

        let err = Error::NotInitialized;
        assert!(err.to_string().contains("not initialized"));

        let err = Error::InvalidTimestamp("test".to_string());
        assert!(err.to_string().contains("Invalid timestamp"));
    }

    #[test]
    fn test_error_conversion_from_transport() {
        let transport_err = TransportError::Timeout(Duration::from_secs(30));
        let err: Error = transport_err.into();
        assert!(matches!(err, Error::Transport(_)));
    }

    #[test]
    fn test_error_conversion_from_validation() {
        let validation_err = ValidationError::InvalidTimestamp("test".to_string());
        let err: Error = validation_err.into();
        assert!(matches!(err, Error::Validation(_)));
    }

    #[test]
    fn test_error_conversion_from_initialization() {
        let init_err = InitializationError::MissingApiKey;
        let err: Error = init_err.into();
        assert!(matches!(err, Error::Initialization(_)));
    }

    #[test]
    fn test_error_is_client_error() {
        // Validation errors are client errors
        let err = Error::Validation(ValidationError::InvalidTimestamp("future".to_string()));
        assert!(err.is_client_error());

        // Initialization errors are client errors
        let err = Error::Initialization(InitializationError::MissingApiKey);
        assert!(err.is_client_error());

        // 4xx HTTP errors are client errors
        let err = Error::Transport(TransportError::HttpError(400, "Bad Request".to_string()));
        assert!(err.is_client_error());
        let err = Error::Transport(TransportError::HttpError(401, "Unauthorized".to_string()));
        assert!(err.is_client_error());
        let err = Error::Transport(TransportError::HttpError(404, "Not Found".to_string()));
        assert!(err.is_client_error());

        // 5xx errors are NOT client errors
        let err = Error::Transport(TransportError::HttpError(500, "Server Error".to_string()));
        assert!(!err.is_client_error());

        // Network errors are NOT client errors
        let err = Error::Transport(TransportError::Timeout(Duration::from_secs(30)));
        assert!(!err.is_client_error());
    }
}
