use posthog_core::event::{Event, InnerEvent};
use reqwest::header::CONTENT_TYPE;
use reqwest::Client as HttpClient;

use crate::client_options::ClientOptions;
use crate::error::Error;

pub struct Client {
    options: ClientOptions,
    http_client: HttpClient,
}

impl Client {
    pub(crate) fn new(options: ClientOptions) -> Self {
        let http_client = HttpClient::builder()
            .timeout(options.timeout)
            .build()
            .unwrap(); // Unwrap here is as safe as `HttpClient::new`
        Client {
            options,
            http_client,
        }
    }

    pub async fn capture(&self, event: Event) -> Result<(), Error> {
        let inner_event = InnerEvent::new(event, self.options.api_key.clone());
        let _res = self
            .http_client
            .post(self.options.api_endpoint.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_string(&inner_event).expect("unwrap here is safe"))
            .send()
            .await
            .map_err(|source| Error::Connection { source })?;
        Ok(())
    }

    pub async fn capture_batch(&self, events: Vec<Event>) -> Result<(), Error> {
        // TODO: Use batch endpoint
        for event in events {
            self.capture(event).await?;
        }
        Ok(())
    }
}
