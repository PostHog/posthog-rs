use chrono::NaiveDateTime;
use reqwest::blocking::Client as HttpClient;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde::Serialize;
use std::fmt::{Display, Formatter};
use std::time::Duration;

pub mod event;
pub mod feature_flags;

const API_ENDPOINT: &str = "https://app.posthog.com";
const TIMEOUT: Duration = Duration::from_millis(800);

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
    pub api_endpoint: String,
    pub api_key: String,
    pub timeout: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            api_endpoint: API_ENDPOINT.to_string(),
            api_key: String::default(),
            timeout: TIMEOUT,
        }
    }
}

impl From<&str> for ClientOptions {
    fn from(api_key: &str) -> Self {
        ClientOptions {
            api_key: api_key.to_string(),
            ..Default::default()
        }
    }
}

pub struct Client {
    options: ClientOptions,
    client: HttpClient,
}

impl Client {
    pub fn new(options: ClientOptions) -> Self {
        let client = HttpClient::builder()
            .timeout(Some(options.timeout))
            .build()
            .unwrap(); // Unwrap here is as safe as `HttpClient::new`

        Self { options, client }
    }

    pub fn capture(&self, event: event::Event) -> Result<(), Error> {
        let inner_event = InnerEvent::new(event, self.options.api_key.clone());
        let _res = self
            .client
            .post(format!("{}/capture/", self.options.api_endpoint.clone()))
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_string(&inner_event).expect("unwrap here is safe"))
            .send()
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(())
    }

    pub fn capture_batch(&self, events: Vec<event::Event>) -> Result<(), Error> {
        for event in events {
            self.capture(event)?;
        }
        Ok(())
    }

    pub fn list_feature_flags(
        &self,
        project_id: &str,
    ) -> Result<Vec<feature_flags::FeatureFlag>, Error> {
        let res = self
            .client
            .get(format!(
                "{}/api/projects/{project_id}/feature_flags/",
                self.options.api_endpoint.clone()
            ))
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {}", self.options.api_key))
            .send()
            .map_err(|e| Error::Connection(e.to_string()))?;
        let response: feature_flags::FeatureFlagResponse = res
            .json::<feature_flags::FeatureFlagResponse>()
            .map_err(|e| Error::Serialization(e.to_string()))?;
        // TODO: if the response is paginated we should fetch the other pages
        Ok(response.results)
    }
}

// This exists so that the client doesn't have to specify the API key over and over
#[derive(Serialize)]
struct InnerEvent {
    api_key: String,
    event: String,
    properties: event::Properties,
    timestamp: Option<NaiveDateTime>,
}

impl InnerEvent {
    fn new(event: event::Event, api_key: String) -> Self {
        Self {
            api_key,
            event: event.event,
            properties: event.properties,
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
            props: Default::default()
        }
    }
}

impl Event {
    pub fn new<S: Into<String>>(event: S, distinct_id: S) -> Self {
        Self {
            event: event.into(),
            properties: Properties::new(distinct_id),
            timestamp: None
        }
    }

    /// Errors if `prop` fails to serialize
    pub fn insert_prop<K: Into<String>, P: Serialize>(&mut self, key: K, prop: P) -> Result<(), Error> {
        let as_json = serde_json::to_value(prop).map_err(|e| Error::Serialization(e.to_string()))?;
        let _ = self.properties.props.insert(key.into(), as_json);
        Ok(())
    }
}


#[cfg(test)]
pub mod tests {
    use super::*;
    use chrono::{Utc};

    #[test]
    fn get_client() {
        let client = crate::client(env!("POSTHOG_API_KEY"));

        let mut child_map = HashMap::new();
        child_map.insert("child_key1", "child_value1");


        let mut event = Event::new("test", "1234");
        event.insert_prop("key1", "value1").unwrap();
        event.insert_prop("key2", vec!["a", "b"]).unwrap();
        event.insert_prop("key3", child_map).unwrap();

        client.capture(event).unwrap();
    }
}