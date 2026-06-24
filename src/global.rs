#[cfg(feature = "error-tracking")]
use std::error::Error as StdError;
use std::sync::OnceLock;

#[cfg(feature = "error-tracking")]
use crate::error_tracking::CaptureExceptionOptions;

use crate::{client, Client, ClientOptions, Error, Event};

static GLOBAL_CLIENT: OnceLock<Client> = OnceLock::new();
static GLOBAL_DISABLE: OnceLock<bool> = OnceLock::new();

/// Initialize the global client singleton.
///
/// Use the crate-level `init_global` re-export when you don't need more than
/// one client instance and don't need to change client options at runtime.
///
/// # Parameters
///
/// - `options`: Project API key or [`ClientOptions`] used to construct the
///   global client.
///
/// # Errors
///
/// Returns [`Error::AlreadyInitialized`] if called more than once.
#[cfg(feature = "async-client")]
pub async fn init_global_client<C: Into<ClientOptions>>(options: C) -> Result<(), Error> {
    if is_disabled() {
        return Ok(());
    }

    let client = client(options).await;
    GLOBAL_CLIENT
        .set(client)
        .map_err(|_| Error::AlreadyInitialized)
}

/// Initialize the global client singleton.
///
/// Use the crate-level `init_global` re-export when you don't need more than
/// one client instance and don't need to change client options at runtime.
///
/// # Parameters
///
/// - `options`: Project API key or [`ClientOptions`] used to construct the
///   global client.
///
/// # Errors
///
/// Returns [`Error::AlreadyInitialized`] if called more than once.
#[cfg(not(feature = "async-client"))]
pub fn init_global_client<C: Into<ClientOptions>>(options: C) -> Result<(), Error> {
    if is_disabled() {
        return Ok(());
    }

    let client = client(options);
    GLOBAL_CLIENT
        .set(client)
        .map_err(|_| Error::AlreadyInitialized)
}

/// Prevent the global client from being initialized.
///
/// # Remarks
///
/// This does *not* prevent use of a global client that was already initialized.
pub fn disable() {
    let _ = GLOBAL_DISABLE.set(true);
}

/// Return `true` if global client initialization has been disabled.
///
/// # Remarks
///
/// A disabled global client can still be used if it was initialized before it
/// was disabled.
pub fn is_disabled() -> bool {
    *GLOBAL_DISABLE.get().unwrap_or(&false)
}

/// Capture the provided event using the global client.
///
/// # Remarks
///
/// Fire-and-forget, like [`Client::capture`]. No-op if `init_global` has not
/// run.
pub fn capture(event: Event) {
    if let Some(client) = GLOBAL_CLIENT.get() {
        client.capture(event);
    }
}

/// Flush the global client's queued events, awaiting the worker's next delivery
/// attempt. No-op if `init_global` has not run.
///
/// `capture` only enqueues, and the global client lives in a `static` whose
/// `Drop` never runs at process exit, so call this (or [`shutdown`]) before
/// exiting to avoid losing buffered events.
#[cfg(feature = "async-client")]
pub async fn flush() {
    if let Some(client) = GLOBAL_CLIENT.get() {
        client.flush().await;
    }
}

/// Flush and stop the global client's background worker. Idempotent; no-op if
/// `init_global` has not run.
#[cfg(feature = "async-client")]
pub async fn shutdown() {
    if let Some(client) = GLOBAL_CLIENT.get() {
        client.shutdown().await;
    }
}

/// Capture a Rust error personlessly using the global client.
#[cfg(all(feature = "async-client", feature = "error-tracking"))]
pub async fn capture_exception<E>(error: &E) -> Result<(), Error>
where
    E: StdError + ?Sized,
{
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture_exception(error).await
}

/// Capture a Rust error with optional context using the global client.
#[cfg(all(feature = "async-client", feature = "error-tracking"))]
pub async fn capture_exception_with<E>(
    error: &E,
    options: CaptureExceptionOptions,
) -> Result<(), Error>
where
    E: StdError + ?Sized,
{
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture_exception_with(error, options).await
}

/// Flush the global client's queued events, blocking until the worker's next
/// delivery attempt. No-op if `init_global` has not run.
///
/// `capture` only enqueues, and the global client lives in a `static` whose
/// `Drop` never runs at process exit, so call this (or [`shutdown`]) before
/// exiting to avoid losing buffered events.
#[cfg(not(feature = "async-client"))]
pub fn flush() {
    if let Some(client) = GLOBAL_CLIENT.get() {
        client.flush();
    }
}

/// Flush and stop the global client's background worker. Idempotent; no-op if
/// `init_global` has not run.
#[cfg(not(feature = "async-client"))]
pub fn shutdown() {
    if let Some(client) = GLOBAL_CLIENT.get() {
        client.shutdown();
    }
}

/// Capture a Rust error personlessly using the global client.
#[cfg(all(not(feature = "async-client"), feature = "error-tracking"))]
pub fn capture_exception<E>(error: &E) -> Result<(), Error>
where
    E: StdError + ?Sized,
{
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture_exception(error)
}

/// Capture a Rust error with optional context using the global client.
#[cfg(all(not(feature = "async-client"), feature = "error-tracking"))]
pub fn capture_exception_with<E>(error: &E, options: CaptureExceptionOptions) -> Result<(), Error>
where
    E: StdError + ?Sized,
{
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture_exception_with(error, options)
}
