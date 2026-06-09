#[cfg(feature = "error-tracking")]
use std::error::Error as StdError;
use std::sync::OnceLock;

#[cfg(feature = "error-tracking")]
use crate::Exception;
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
/// # Errors
///
/// Returns [`Error::NotInitialized`] if `init_global` has not successfully run,
/// or any error returned by [`Client::capture`].
#[cfg(feature = "async-client")]
pub async fn capture(event: Event) -> Result<(), Error> {
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture(event).await
}

/// Capture an exception event using the global client.
#[cfg(all(feature = "async-client", feature = "error-tracking"))]
pub async fn capture_exception(exception: Exception) -> Result<(), Error> {
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture_exception(exception).await
}

/// Capture a Rust error using the global client.
#[cfg(all(feature = "async-client", feature = "error-tracking"))]
pub async fn capture_error<E, S>(error: &E, distinct_id: S) -> Result<(), Error>
where
    E: StdError + ?Sized,
    S: Into<String>,
{
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture_error(error, distinct_id).await
}

/// Capture a Rust error personlessly using the global client.
#[cfg(all(feature = "async-client", feature = "error-tracking"))]
pub async fn capture_error_anon<E>(error: &E) -> Result<(), Error>
where
    E: StdError + ?Sized,
{
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture_error_anon(error).await
}

/// Capture the provided event using the global client.
///
/// # Errors
///
/// Returns [`Error::NotInitialized`] if `init_global` has not successfully run,
/// or any error returned by [`Client::capture`].
#[cfg(not(feature = "async-client"))]
pub fn capture(event: Event) -> Result<(), Error> {
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture(event)
}

/// Capture an exception event using the global client.
#[cfg(all(not(feature = "async-client"), feature = "error-tracking"))]
pub fn capture_exception(exception: Exception) -> Result<(), Error> {
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture_exception(exception)
}

/// Capture a Rust error using the global client.
#[cfg(all(not(feature = "async-client"), feature = "error-tracking"))]
pub fn capture_error<E, S>(error: &E, distinct_id: S) -> Result<(), Error>
where
    E: StdError + ?Sized,
    S: Into<String>,
{
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture_error(error, distinct_id)
}

/// Capture a Rust error personlessly using the global client.
#[cfg(all(not(feature = "async-client"), feature = "error-tracking"))]
pub fn capture_error_anon<E>(error: &E) -> Result<(), Error>
where
    E: StdError + ?Sized,
{
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture_error_anon(error)
}
