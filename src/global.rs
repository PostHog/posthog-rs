use std::sync::OnceLock;

use crate::{client, Client, ClientOptions, Error, Event};

static GLOBAL_CLIENT: OnceLock<Client> = OnceLock::new();
static GLOBAL_DISABLE: OnceLock<bool> = OnceLock::new();

/// [`init_global_client`] will initialize a globally available client singleton. This singleton
/// can be used when you don't need more than one instance and have no need to regularly change
/// the client options.
/// # Errors
/// This function returns [`Error::AlreadyInitialized`] if called more than once.
#[cfg(feature = "async-client")]
pub fn init_global_client<C: Into<ClientOptions>>(options: C) -> Result<(), Error> {
    if is_disabled() {
        return Ok(());
    }

    let client = client(options);
    GLOBAL_CLIENT
        .set(client)
        .map_err(|_| Error::AlreadyInitialized)
}

/// [`init_global_client`] will initialize a globally available client singleton. This singleton
/// can be used when you don't need more than one instance and have no need to regularly change
/// the client options.
/// # Errors
/// This function returns [`Error::AlreadyInitialized`] if called more than once.
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

/// [`disable`] prevents the global client from being initialized.
/// **NOTE:** It does *not* prevent use of the global client once initialized.
pub fn disable() {
    let _ = GLOBAL_DISABLE.set(true);
}

/// [`is_disabled`] returns true if the global client has been disabled.
/// **NOTE:** A disabled global client can still be used as long as it was
/// initialized before it was disabled.
pub fn is_disabled() -> bool {
    *GLOBAL_DISABLE.get().unwrap_or(&false)
}

/// Capture the provided event, sending it to PostHog using the global client.
#[cfg(feature = "async-client")]
pub async fn capture(event: Event) -> Result<(), Error> {
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture(event).await
}

/// Capture the provided event, sending it to PostHog using the global client.
#[cfg(not(feature = "async-client"))]
pub fn capture(event: Event) -> Result<(), Error> {
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture(event)
}
