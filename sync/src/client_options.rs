use std::time::Duration;

use crate::client::Client;

const API_ENDPOINT: &str = "https://app.posthog.com";
const TIMEOUT: Duration = Duration::from_millis(800); // This should be specified by the user

pub struct ClientOptions {
    pub(crate) api_endpoint: String,
    pub(crate) api_key: String,
    pub(crate) timeout: Duration,
}

impl ClientOptions {
    pub fn new(api_key: impl ToString) -> ClientOptions {
        ClientOptions {
            api_endpoint: API_ENDPOINT.to_string(),
            api_key: api_key.to_string(),
            timeout: TIMEOUT,
        }
    }

    pub fn api_endpoint(&mut self, api_endpoint: impl ToString) -> &mut Self {
        self.api_endpoint = api_endpoint.to_string();
        self
    }

    pub fn timeout(&mut self, timeout: Duration) -> &mut Self {
        self.timeout = timeout;
        self
    }

    pub fn build(self) -> Client {
        Client::new(self)
    }
}

impl From<&str> for ClientOptions {
    fn from(api_key: &str) -> Self {
        ClientOptions::new(api_key)
    }
}
