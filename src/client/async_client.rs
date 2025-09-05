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
pub async fn client<C: Into<ClientOptions>>(options: C) -> Client {
    let options = options.into();
    let client = HttpClient::builder()
        .timeout(Duration::from_secs(options.request_timeout_seconds))
        .build()
        .unwrap(); // Unwrap here is as safe as `HttpClient::new`
    Client { options, client }
}

impl Client {
    /// Returns true if this client is disabled (has no API key).
    pub fn is_disabled(&self) -> bool {
        self.options.api_key.is_none()
    }

    /// Capture the provided event, sending it to PostHog.
    /// If the client is disabled (no API key), this method returns Ok(()) without sending anything.
    pub async fn capture(&self, event: Event) -> Result<(), Error> {
        if self.is_disabled() {
            return Ok(());
        }

        let api_key = self.options.api_key.as_ref().unwrap();
        let inner_event = InnerEvent::new(event, api_key.clone());

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
    /// If the client is disabled (no API key), this method returns Ok(()) without sending anything.
    pub async fn capture_batch(&self, events: Vec<Event>) -> Result<(), Error> {
        if self.is_disabled() {
            return Ok(());
        }

        let api_key = self.options.api_key.as_ref().unwrap();
        let events: Vec<_> = events
            .into_iter()
            .map(|event| InnerEvent::new(event, api_key.clone()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientOptionsBuilder, Event};

    #[tokio::test]
    async fn test_client_without_api_key_is_disabled() {
        let options = ClientOptionsBuilder::default().build().unwrap();
        let client = client(options).await;
        assert!(client.is_disabled());
    }

    #[tokio::test]
    async fn test_client_with_api_key_is_enabled() {
        let options = ClientOptionsBuilder::default()
            .api_key(Some("test_key".to_string()))
            .build()
            .unwrap();
        let client = client(options).await;
        assert!(!client.is_disabled());
    }

    #[tokio::test]
    async fn test_disabled_client_capture_returns_ok() {
        let options = ClientOptionsBuilder::default().build().unwrap();
        let client = client(options).await;

        let event = Event::new("test_event", "user_123");
        let result = client.capture(event).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_disabled_client_capture_batch_returns_ok() {
        let options = ClientOptionsBuilder::default().build().unwrap();
        let client = client(options).await;

        let events = vec![
            Event::new("test_event1", "user_123"),
            Event::new("test_event2", "user_456"),
        ];
        let result = client.capture_batch(events).await;
        assert!(result.is_ok());
    }
}
