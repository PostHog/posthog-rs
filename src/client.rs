use reqwest::blocking::Client as HttpClient;
use reqwest::header::CONTENT_TYPE;

use crate::client_options::ClientOptions;
use crate::error::Error;
use crate::event::{Event, InnerEvent};

pub struct Client {
    options: ClientOptions,
    http_client: HttpClient,
}

impl Client {
    pub(crate) fn new(options: ClientOptions) -> Self {
        let http_client = HttpClient::builder()
            .timeout(Some(options.timeout))
            .build()
            .unwrap(); // Unwrap here is as safe as `HttpClient::new`
        Client {
            options,
            http_client,
        }
    }

    pub fn capture(&self, event: Event) -> Result<(), Error> {
        let inner_event = InnerEvent::new(event, self.options.api_key.clone());
        let _res = self
            .http_client
            .post(self.options.api_endpoint.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_string(&inner_event).expect("unwrap here is safe"))
            .send()
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(())
    }

    pub fn capture_batch(&self, events: Vec<Event>) -> Result<(), Error> {
        for event in events {
            self.capture(event)?;
        }
        Ok(())
    }
}
