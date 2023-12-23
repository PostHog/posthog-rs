use posthog_core::event::{Event, InnerEvent, InnerEventBatch};
use posthog_core::group_identify::GroupIdentify;
use reqwest::{Client as HttpClient, Method};
use serde::{de::DeserializeOwned, Serialize};

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

    async fn send_request<P: AsRef<str>, Body: Serialize, Res: DeserializeOwned>(
        &self,
        method: Method,
        path: P,
        body: &Body,
    ) -> Result<Res, Error> {
        let res = self
            .http_client
            .request(
                method,
                format!("{}{}", self.options.api_endpoint, path.as_ref()),
            )
            .json(body)
            .send()
            .await
            .map_err(|source| Error::SendRequest { source })?
            .error_for_status()
            .map_err(|source| Error::ResponseStatus { source })?
            .json::<Res>()
            .await
            .map_err(|source| Error::DecodeResponse { source })?;
        Ok(res)
    }

    pub async fn capture(&self, event: Event) -> Result<(), Error> {
        let inner_event = InnerEvent::new(event, self.options.api_key.clone());
        self.send_request::<_, _, serde_json::Value>(Method::POST, "/capture/", &inner_event)
            .await?;
        Ok(())
    }

    pub async fn capture_batch(&self, events: Vec<Event>) -> Result<(), Error> {
        let inner_event_batch = InnerEventBatch::new(events, self.options.api_key.clone());
        self.send_request::<_, _, serde_json::Value>(Method::POST, "/batch/", &inner_event_batch)
            .await?;
        Ok(())
    }

    pub async fn group_identify(&self, identify: GroupIdentify) -> Result<(), Error> {
        let inner_event = InnerEvent::new(
            identify
                .try_into()
                .map_err(|source| Error::PostHogCore { source })?,
            self.options.api_key.clone(),
        );
        self.send_request::<_, _, serde_json::Value>(Method::POST, "/capture/", &inner_event)
            .await?;
        Ok(())
    }
}
