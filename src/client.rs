use reqwest::header::CONTENT_TYPE;

use crate::{event::InnerEvent, Error, Event, API_ENDPOINT, TIMEOUT};

pub struct Client {
    options: ClientOptions,
    client: HttpClient,
}

pub struct ClientOptions {
    api_endpoint: String,
    api_key: String,
}

impl Client {
    pub fn capture(&self, event: Event) -> Result<(), Error> {
        let inner_event = InnerEvent::new(event, self.options.api_key.clone());
        let _res = self
            .client
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

impl From<&str> for ClientOptions {
    fn from(api_key: &str) -> Self {
        ClientOptions {
            api_endpoint: API_ENDPOINT.to_string(),
            api_key: api_key.to_string(),
        }
    }
}

pub fn client<C: Into<ClientOptions>>(options: C) -> Client {
    let client = HttpClient::builder()
        .timeout(Some(*TIMEOUT))
        .build()
        .unwrap(); // Unwrap here is as safe as `HttpClient::new`
    Client {
        options: options.into(),
        client,
    }
}
