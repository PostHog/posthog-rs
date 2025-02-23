use std::sync::OnceLock;

use crate::{client, Client, ClientOptions, Error, Event};

static GLOBAL_CLIENT: OnceLock<Client> = OnceLock::new();
static GLOBAL_DISABLE: OnceLock<bool> = OnceLock::new();

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

pub fn disable() {
    let _ = GLOBAL_DISABLE.set(true);
}

pub fn is_disabled() -> bool {
    *GLOBAL_DISABLE.get().unwrap_or(&false)
}

#[cfg(feature = "async-client")]
pub async fn capture(event: Event) -> Result<(), Error> {
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture(event).await
}

#[cfg(not(feature = "async-client"))]
pub fn capture(event: Event) -> Result<(), Error> {
    let client = GLOBAL_CLIENT.get().ok_or(Error::NotInitialized)?;
    client.capture(event)
}
