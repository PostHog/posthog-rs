use std::time::Duration;

use reqwest::{blocking::Client as HttpClient, header::CONTENT_TYPE};

use crate::{event::InnerEvent, Error, Event};

use super::ClientOptions;

pub struct Client {
    options: ClientOptions,
    client: HttpClient,
}

impl Client {
    pub fn capture(&self, event: Event) -> Result<(), Error> {
        let inner_event = InnerEvent::new(event, self.options.api_key.clone());

        let payload =
            serde_json::to_string(&inner_event).map_err(|e| Error::Serialization(e.to_string()))?;

        self.client
            .post(&self.options.api_endpoint)
            .header(CONTENT_TYPE, "application/json")
            .body(payload)
            .send()
            .map_err(|e| Error::Connection(e.to_string()))?;

        Ok(())
    }

    pub fn capture_batch(&self, events: Vec<Event>) -> Result<(), Error> {
        let events: Vec<_> = events
            .into_iter()
            .map(|event| InnerEvent::new(event, self.options.api_key.clone()))
            .collect();

        let payload =
            serde_json::to_string(&events).map_err(|e| Error::Serialization(e.to_string()))?;

        self.client
            .post(&self.options.api_endpoint)
            .header(CONTENT_TYPE, "application/json")
            .body(payload)
            .send()
            .map_err(|e| Error::Connection(e.to_string()))?;

        Ok(())
    }
}

pub fn client<C: Into<ClientOptions>>(options: C) -> Client {
    let options = options.into();
    let client = HttpClient::builder()
        .timeout(Duration::from_secs(options.request_timeout_seconds))
        .build()
        .unwrap(); // Unwrap here is as safe as `HttpClient::new`
    Client { options, client }
}
