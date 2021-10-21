use std::collections::HashMap;
use chrono::{NaiveDateTime};
use reqwest::blocking::Client as HttpClient;
use reqwest::header::CONTENT_TYPE;
use serde::{Serialize};
use std::time::Duration;

extern crate serde_json;

const API_ENDPOINT: &str = "https://app.posthog.com/capture/";
const TIMEOUT: &Duration = &Duration::from_millis(800); // This should be specified by the user

pub fn client<C: Into<ClientOptions>>(options: C) -> Client {
    let client = HttpClient::builder().timeout(Some(TIMEOUT.clone())).build().unwrap(); // Unwrap here is as safe as `HttpClient::new`
    Client {
        options: options.into(),
        client,
    }
}

#[derive(Debug)]
pub enum Error {
    Connection(String),
    Serialization(String)
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
    client: HttpClient,
}

impl Client {
    pub fn capture(&self, event: Event) -> Result<(), Error> {
        let inner_event = InnerEvent::new(event, self.options.api_key.clone());
        let _res = self.client.post(self.options.api_endpoint.clone())
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
        Self {
            api_key,
            event: event.event,
            properties: event.properties,
            timestamp: event.timestamp,
        }
    }
}


pub struct Event {
    event: String,
    properties: Properties,
    timestamp: Option<NaiveDateTime>,
}

#[derive(Serialize)]
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