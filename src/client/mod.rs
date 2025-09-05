use crate::API_ENDPOINT;
use derive_builder::Builder;

#[cfg(not(feature = "async-client"))]
mod blocking;
#[cfg(not(feature = "async-client"))]
pub use blocking::client;
#[cfg(not(feature = "async-client"))]
pub use blocking::Client;

#[cfg(feature = "async-client")]
mod async_client;
#[cfg(feature = "async-client")]
pub use async_client::client;
#[cfg(feature = "async-client")]
pub use async_client::Client;

#[derive(Builder)]
pub struct ClientOptions {
    #[builder(default = "API_ENDPOINT.to_string()")]
    api_endpoint: String,
    #[builder(default = "None")]
    api_key: Option<String>,

    #[builder(default = "30")]
    request_timeout_seconds: u64,
}

impl From<&str> for ClientOptions {
    fn from(api_key: &str) -> Self {
        ClientOptionsBuilder::default()
            .api_key(Some(api_key.to_string()))
            .build()
            .expect("We always set the API key, so this is infallible")
    }
}
