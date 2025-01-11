#![cfg_attr(docsrs, feature(doc_auto_cfg))]

use chrono::NaiveDateTime;
#[cfg(feature = "blocking")]
use reqwest::blocking::Client as BlockingHttpClient;
use reqwest::header::CONTENT_TYPE;
use reqwest::Client as AsyncHttpClient;
use semver::Version;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::time::Duration;

extern crate serde_json;

const API_ENDPOINT: &str = "https://us.i.posthog.com/capture/";
const TIMEOUT: &Duration = &Duration::from_millis(800); // This should be specified by the user

pub fn client<C: Into<ClientOptions>>(options: C) -> Client {
    #[cfg(feature = "blocking")]
    let blocking_client = BlockingHttpClient::builder()
        .timeout(Some(*TIMEOUT))
        .build()
        .unwrap(); // Unwrap here is as safe as `HttpClient::new`
    let async_client = AsyncHttpClient::builder()
        .timeout(*TIMEOUT)
        .build()
        .unwrap(); // Unwrap here is as safe as `HttpClient::new`
    Client {
        options: options.into(),
        #[cfg(feature = "blocking")]
        blocking_client,
        async_client,
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Connection(msg) => write!(f, "Connection Error: {}", msg),
            Error::Serialization(msg) => write!(f, "Serialization Error: {}", msg),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug)]
pub enum Error {
    Connection(String),
    Serialization(String),
}

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

pub struct Client {
    options: ClientOptions,
    #[cfg(feature = "blocking")]
    blocking_client: BlockingHttpClient,
    async_client: AsyncHttpClient,
}

impl Client {
    #[cfg(feature = "blocking")]
    pub fn capture(&self, event: Event) -> Result<(), Error> {
        let inner_event = InnerEvent::new(event, self.options.api_key.clone());
        let _res = self
            .blocking_client
            .post(self.options.api_endpoint.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_string(&inner_event).expect("unwrap here is safe"))
            .send()
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(())
    }

    pub async fn async_capture(&self, event: Event) -> Result<(), Error> {
        let inner_event = InnerEvent::new(event, self.options.api_key.clone());
        let _res = self
            .async_client
            .post(self.options.api_endpoint.clone())
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_string(&inner_event).expect("unwrap here is safe"))
            .send()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(())
    }

    #[cfg(feature = "blocking")]
    pub fn capture_batch(&self, events: impl Iterator<Item = Event>) -> Result<(), Error> {
        for event in events {
            self.capture(event)?;
        }
        Ok(())
    }

    pub async fn async_capture_batch(
        &self,
        events: impl Iterator<Item = Event>,
    ) -> Result<(), Error> {
        for event in events {
            self.async_capture(event).await?;
        }
        Ok(())
    }
}

// This exists so that the client doesn't have to specify the API key over and over
#[derive(Serialize)]
struct InnerEvent {
    api_key: String,
    event: String,
    properties: Properties,
    timestamp: Option<NaiveDateTime>,
}

impl InnerEvent {
    fn new(event: Event, api_key: String) -> Self {
        let mut properties = event.properties;

        // Add $lib_name and $lib_version to the properties
        properties.props.insert(
            "$lib_name".into(),
            serde_json::Value::String("posthog-rs".into()),
        );

        let version_str = env!("CARGO_PKG_VERSION");
        properties.props.insert(
            "$lib_version".into(),
            serde_json::Value::String(version_str.into()),
        );

        if let Ok(version) = version_str.parse::<Version>() {
            properties.props.insert(
                "$lib_version__major".into(),
                serde_json::Value::Number(version.major.into()),
            );
            properties.props.insert(
                "$lib_version__minor".into(),
                serde_json::Value::Number(version.minor.into()),
            );
            properties.props.insert(
                "$lib_version__patch".into(),
                serde_json::Value::Number(version.patch.into()),
            );
        }

        Self {
            api_key,
            event: event.event,
            properties,
            timestamp: event.timestamp,
        }
    }
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Event {
    event: String,
    properties: Properties,
    timestamp: Option<NaiveDateTime>,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Properties {
    distinct_id: String,
    props: HashMap<String, serde_json::Value>,
}

impl Properties {
    fn new<S: Into<String>>(distinct_id: S) -> Self {
        Self {
            distinct_id: distinct_id.into(),
            props: Default::default(),
        }
    }
}

impl Event {
    pub fn new<S: Into<String>>(event: S, distinct_id: S) -> Self {
        Self {
            event: event.into(),
            properties: Properties::new(distinct_id),
            timestamp: None,
        }
    }

    /// Errors if `prop` fails to serialize
    pub fn insert_prop<K: Into<String>, P: Serialize>(
        &mut self,
        key: K,
        prop: P,
    ) -> Result<(), Error> {
        let as_json =
            serde_json::to_value(prop).map_err(|e| Error::Serialization(e.to_string()))?;
        let _ = self.properties.props.insert(key.into(), as_json);
        Ok(())
    }
}

#[cfg(test)]
mod test_setup {
    use ctor::ctor;
    use dotenv::dotenv;

    #[ctor]
    fn load_dotenv() {
        dotenv().ok(); // Load the .env file
        println!("Loaded .env for tests");
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    // see https://us.posthog.com/project/115809/ for the e2e project

    #[test]
    fn inner_event_adds_lib_properties_correctly() {
        // Arrange
        let mut event = Event::new("unit test event", "1234");
        event.insert_prop("key1", "value1").unwrap();
        let api_key = "test_api_key".to_string();

        // Act
        let inner_event = InnerEvent::new(event, api_key);

        // Assert
        let props = &inner_event.properties.props;
        assert_eq!(
            props.get("$lib_name"),
            Some(&serde_json::Value::String("posthog-rs".to_string()))
        );
    }

    #[cfg(feature = "e2e-test")]
    #[test]
    fn get_client() {
        use std::collections::HashMap;

        let api_key = std::env::var("POSTHOG_RS_E2E_TEST_API_KEY").unwrap();
        let client = crate::client(api_key.as_str());

        let mut child_map = HashMap::new();
        child_map.insert("child_key1", "child_value1");

        let mut event = Event::new("e2e test event", "1234");
        event.insert_prop("key1", "value1").unwrap();
        event.insert_prop("key2", vec!["a", "b"]).unwrap();
        event.insert_prop("key3", child_map).unwrap();

        client.capture(event).unwrap();
    }
}
