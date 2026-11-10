//! A real PostHog client pointed at a mock endpoint, recording each event as
//! the client sends it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use httpmock::prelude::*;
use posthog::{Client, ClientOptionsBuilder, Event};
use serde_json::Value;

pub struct Recorder {
    pub client: Arc<Client>,
    events: Arc<Mutex<Vec<Event>>>,
    /// Events as sent, keyed by `uuid` so a matcher that runs more than once
    /// for a request records each event once.
    wire: Arc<Mutex<HashMap<String, Value>>>,
    _endpoint: MockServer,
}

impl Recorder {
    pub async fn new() -> Self {
        Self::with_status(200).await
    }

    /// An endpoint that answers every capture with `status`.
    pub async fn with_status(status: u16) -> Self {
        let endpoint = MockServer::start();
        let wire = Arc::new(Mutex::new(HashMap::new()));
        let sent = wire.clone();
        let api_key = format!("test-token-{}", uuid::Uuid::now_v7());
        let expected_authorization = format!("Bearer {api_key}");
        endpoint.mock(move |when, then| {
            when.is_true(move |request| {
                // Pooled endpoints can receive late requests from a previous client.
                if !request.headers_vec().iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("authorization") && value == &expected_authorization
                }) {
                    return false;
                }
                let body: Value = serde_json::from_slice(request.body_ref()).unwrap_or_default();
                let mut sent = sent.lock().unwrap();
                for event in body["batch"].as_array().cloned().unwrap_or_default() {
                    let uuid = event["uuid"].as_str().unwrap_or_default().to_string();
                    sent.insert(uuid, event);
                }
                true
            });
            then.status(status)
                .json_body(serde_json::json!({ "status": 1 }));
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        let options = ClientOptionsBuilder::default()
            .api_key(api_key)
            .host(endpoint.base_url())
            .before_send(move |event| {
                recorded.lock().unwrap().push(event.clone());
                Some(event)
            })
            .build()
            .unwrap();
        Self {
            client: Arc::new(posthog::client(options).await),
            events,
            wire,
            _endpoint: endpoint,
        }
    }

    /// The properties of the only `name` event as it was sent.
    pub async fn sent_properties(&self, name: &str) -> Value {
        self.sent(name).await["properties"].clone()
    }

    /// The only `name` event as it was sent.
    pub async fn sent(&self, name: &str) -> Value {
        let mut sent: Vec<Value> = self
            .sent_events()
            .await
            .into_iter()
            .filter(|event| event["event"] == name)
            .collect();
        assert_eq!(
            sent.len(),
            1,
            "expected exactly one {name} sent, got {sent:#?}"
        );
        sent.remove(0)
    }

    /// Every event as it was sent.
    pub async fn sent_events(&self) -> Vec<Value> {
        self.client.flush().await;
        self.wire.lock().unwrap().values().cloned().collect()
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

#[tokio::test]
async fn recorder_ignores_late_requests_from_another_client() {
    let recorder = Recorder::new().await;
    reqwest::Client::new()
        .post(format!(
            "{}/i/v1/analytics/events",
            recorder._endpoint.base_url()
        ))
        .bearer_auth("another-recorder")
        .json(&serde_json::json!({
            "batch": [{"event": "recorded-event", "uuid": "foreign", "properties": {}}]
        }))
        .send()
        .await
        .unwrap();
    recorder
        .client
        .capture(Event::new("recorded-event", "this-client"));
    let sent = recorder.sent("recorded-event").await;
    assert_eq!(sent["distinct_id"], "this-client");
    recorder.client.shutdown().await;
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
