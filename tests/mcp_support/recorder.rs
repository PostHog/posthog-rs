//! A real PostHog client pointed at a mock endpoint, recording each event as
//! the client sends it.

use std::sync::{Arc, Mutex};

use httpmock::prelude::*;
use posthog_rs::{Client, ClientOptionsBuilder, Event};
use serde_json::Value;

pub struct Recorder {
    pub client: Arc<Client>,
    events: Arc<Mutex<Vec<Event>>>,
    _endpoint: MockServer,
}

impl Recorder {
    pub async fn new() -> Self {
        Self::with_status(200).await
    }

    /// An endpoint that answers every capture with `status`.
    pub async fn with_status(status: u16) -> Self {
        let endpoint = MockServer::start();
        endpoint.mock(|when, then| {
            when.any_request();
            then.status(status)
                .json_body(serde_json::json!({ "status": 1 }));
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let options = ClientOptionsBuilder::default()
            .api_key("test-token".to_string())
            .host(endpoint.base_url())
            .before_send(move |event| {
                recorded.lock().unwrap().push(event.clone());
                Some(event)
            })
            .build()
            .unwrap();
        Self {
            client: Arc::new(posthog_rs::client(options).await),
            events,
            _endpoint: endpoint,
        }
    }

    pub async fn events(&self) -> Vec<Event> {
        self.client.flush().await;
        self.events.lock().unwrap().clone()
    }

    pub async fn named(&self, name: &str) -> Vec<Event> {
        let events = self.events().await;
        events
            .into_iter()
            .filter(|event| event.event_name() == name)
            .collect()
    }

    pub async fn only(&self, name: &str) -> Event {
        let mut events = self.named(name).await;
        assert_eq!(
            events.len(),
            1,
            "expected exactly one {name}, got {events:#?}"
        );
        events.remove(0)
    }
}

pub fn property<'a>(event: &'a Event, key: &str) -> &'a Value {
    event.properties().get(key).unwrap_or_else(|| {
        panic!(
            "{} has no {key}: {:#?}",
            event.event_name(),
            event.properties()
        )
    })
}

pub fn text_property<'a>(event: &'a Event, key: &str) -> &'a str {
    property(event, key)
        .as_str()
        .unwrap_or_else(|| panic!("{} is not a string", key))
}

pub fn serialized_properties_size(event: &Event) -> usize {
    serde_json::to_vec(event.properties()).unwrap().len()
}
