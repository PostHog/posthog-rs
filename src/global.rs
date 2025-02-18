use std::sync::OnceLock;

use crate::{client, Client, ClientOptions, Error, Event};

pub static GLOBAL_CLIENT: OnceLock<Client> = OnceLock::new();

#[cfg(feature = "async-client")]
pub async fn init_global_client<C: Into<ClientOptions>>(options: C) -> Result<(), Error> {
    let client = client(options).await;
    GLOBAL_CLIENT
        .set(client)
        .map_err(|_| Error::AlreadyInitialized)
}

#[cfg(not(feature = "async-client"))]
pub fn init_global_client<C: Into<ClientOptions>>(options: C) -> Result<(), Error> {
    let client = client(options);
    GLOBAL_CLIENT
        .set(client)
        .map_err(|_| Error::AlreadyInitialized)
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
