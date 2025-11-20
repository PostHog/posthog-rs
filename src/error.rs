use std::time::Duration;
use thiserror::Error;

/// Main error type for the PostHog Rust SDK.
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

    #[deprecated(since = "0.4.0", note = "Use Error::Initialization instead")]
    #[error("Uninitialized field: {0}")]
    UninitializedField(&'static str),

    #[deprecated(since = "0.4.0", note = "Use Error::Validation instead")]
    #[error("Validation error: {0}")]
    ValidationError(String),

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
    /// Returns true if this error can be retried.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Transport(e) => e.is_retryable(),
            _ => false,
        }
    }

    /// Returns true if this error is due to invalid usage (4xx, validation, or config errors).
    pub fn is_client_error(&self) -> bool {
        match self {
            Error::Validation(_) | Error::Initialization(_) => true,
            Error::Transport(e) => e.is_client_error(),
            _ => false,
        }
    }
}

/// Network transport and HTTP errors.
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
    /// Returns true if this error can be retried (timeouts, 5xx, 429).
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

    fn is_client_error(&self) -> bool {
        matches!(self, TransportError::HttpError(400..=499, _))
    }
}

impl From<reqwest::Error> for TransportError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            return TransportError::Timeout(Duration::from_secs(30));
        }

        if err.is_connect() {
            return err
                .url()
                .and_then(|u| u.host_str())
                .map(|host| TransportError::DnsResolution(host.to_string()))
                .unwrap_or(TransportError::NetworkUnreachable);
        }

        if let Some(status) = err.status() {
            return TransportError::HttpError(status.as_u16(), err.to_string());
        }

        let err_str = err.to_string();
        if err_str.contains("tls") || err_str.contains("ssl") {
            TransportError::TlsError(err_str)
        } else {
            TransportError::NetworkUnreachable
        }
    }
}

/// Event validation and data integrity errors.
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

/// Client initialization and configuration errors.
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

    /// Personal API key is required when local evaluation is enabled
    #[error("Personal API key is required when enable_local_evaluation is true")]
    MissingPersonalApiKey,
}
