use crate::API_ENDPOINT;

mod blocking;

pub use blocking::client;
pub use blocking::Client;

pub struct ClientOptions {
    api_endpoint: String,
    api_key: String,
}

impl From<&str> for ClientOptions {
    fn from(api_key: &str) -> Self {
        ClientOptions {
            api_endpoint: API_ENDPOINT.to_string(),
            api_key: api_key.to_string(),
        }
    }
}
