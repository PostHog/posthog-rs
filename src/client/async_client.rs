use std::time::Duration;

use reqwest::{header::CONTENT_TYPE, Client as HttpClient};

use crate::{event::InnerEvent, Error, Event};

use super::ClientOptions;

/// A [`Client`] facilitates interactions with the PostHog API over HTTP.
pub struct Client {
    options: ClientOptions,
    client: HttpClient,
}

/// This function constructs a new client using the options provided.
pub fn client<C: Into<ClientOptions>>(options: C) -> Client {
    let options = options.into();
    let client = HttpClient::builder()
        .timeout(Duration::from_secs(options.request_timeout_seconds))
        .build()
        .unwrap(); // Unwrap here is as safe as `HttpClient::new`
    Client { options, client }
}

impl Client {
    /// Capture the provided event, sending it to PostHog.
    pub async fn capture(&self, event: Event) -> Result<(), Error> {
        let inner_event = InnerEvent::new(event, self.options.api_key.clone());

        let payload =
            serde_json::to_string(&inner_event).map_err(|e| Error::Serialization(e.to_string()))?;

        self.client
            .post(&self.options.api_endpoint)
            .header(CONTENT_TYPE, "application/json")
            .body(payload)
            .send()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        Ok(())
    }

    /// Capture a collection of events with a single request. This function may be
    /// more performant than capturing a list of events individually.
    pub async fn capture_batch(&self, events: Vec<Event>) -> Result<(), Error> {
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
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;

        Ok(())
    }
}
