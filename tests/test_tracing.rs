#![cfg(feature = "tracing-subscriber")]

use std::{
    fmt::{self, Debug, Display},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use httpmock::prelude::*;
use posthog_rs::{
    Client,
    ClientOptions,
    ClientOptionsBuilder,
    Event,
    EventNamer,
    PostHogLayer,
};
use serde_json::Value;
use tracing::{Metadata, Subscriber};
use tracing_subscriber::{
    filter::{LevelFilter, Targets},
    layer::{Context, Layer, SubscriberExt},
};

async fn create_client(options: ClientOptions) -> Client {
    #[cfg(feature = "async-client")]
    {
        posthog_rs::client(options).await
    }

    #[cfg(not(feature = "async-client"))]
    {
        posthog_rs::client(options)
    }
}

async fn flush(client: &Client) {
    #[cfg(feature = "async-client")]
    {
        client.flush().await;
    }

    #[cfg(not(feature = "async-client"))]
    {
        client.flush();
    }
}

async fn client_with_hook<F>(host: &str, hook: F) -> Client
where
    F: FnMut(Event) -> Option<Event> + Send + 'static,
{
    let mut builder = ClientOptionsBuilder::default();

    builder
        .api_key("phc_test".to_string())
        .host(host.to_string())
        .before_send(hook);

    create_client(builder.build().unwrap()).await
}

async fn client_without_hook(host: &str) -> Client {
    let mut builder = ClientOptionsBuilder::default();

    builder
        .api_key("phc_test".to_string())
        .host(host.to_string());

    create_client(builder.build().unwrap()).await
}

fn capture_events() -> Arc<Mutex<Vec<Value>>> {
    Arc::new(Mutex::new(Vec::new()))
}

fn capture_hook(events: Arc<Mutex<Vec<Value>>>) -> impl FnMut(Event) -> Option<Event> + Send {
    move |event| {
        events
            .lock()
            .unwrap()
            .push(serde_json::to_value(event).unwrap());
        None
    }
}

struct DisplayValue;

impl Display for DisplayValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("display-value")
    }
}

struct DebugValue;

impl Debug for DebugValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("debug-value")
    }
}

#[tokio::test]
async fn captures_event_with_field_and_context_fidelity() {
    let events = capture_events();
    let client = Arc::new(
        client_with_hook(
            "http://127.0.0.1:1",
            capture_hook(Arc::clone(&events)),
        )
        .await,
    );

    let layer = PostHogLayer::new(Arc::clone(&client))
      .with_event_namer(EventNamer::Custom(Arc::new(|_| {"trace_event".to_owned()})))
      .with_property("service", "static");

    let subscriber = tracing_subscriber::registry().with(layer);

    let display_value = DisplayValue;
    let debug_value = DebugValue;

    tracing::subscriber::with_default(subscriber, || {
      tracing::trace!(
          src = "posthog",
          field1 = "one",
          field2 = "two",
          distinct_id = "user-123",
          service = "dynamic",
          signed = i64::MIN,
          unsigned = u64::MAX,
          amount = 49.95_f64,
          successful = true,
          display_field = %display_value,
          debug_field = ?debug_value,
          "payment completed"
      );
    });

    flush(&client).await;

    let events = events.lock().unwrap();
    assert_eq!(events.len(), 1);

    let event = &events[0];

    assert_eq!(event["event"], "trace_event");
    assert_eq!(event["distinct_id"], "user-123");

    let properties = &event["properties"];
    assert_eq!(properties["src"], "posthog");
    assert_eq!(properties["field1"], "one");
    assert_eq!(properties["field2"], "two");
    assert_eq!(properties["service"], "dynamic");
    assert_eq!(properties["signed"], i64::MIN);
    assert_eq!(properties["unsigned"], u64::MAX);
    assert_eq!(properties["amount"], 49.95);
    assert_eq!(properties["successful"], true);
    assert_eq!(properties["display_field"], "display-value");
    assert_eq!(properties["debug_field"], "debug-value");
    assert_eq!(properties["message"], "payment completed");

    assert!(properties.get("distinct_id").is_none());
}

struct CountingLayer {
    count: Arc<AtomicUsize>,
}

impl<S> Layer<S> for CountingLayer
where
    S: Subscriber,
{
  fn on_event(&self, _event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
      self.count.fetch_add(1, Ordering::Relaxed);
  }
}

#[tokio::test]
async fn filters_events_per_layer_and_excludes_sdk_targets() {
    let events = capture_events();
    let client = Arc::new(
        client_with_hook(
            "http://127.0.0.1:1",
            capture_hook(Arc::clone(&events)),
        )
        .await,
    );

    let seen_by_other_layer = Arc::new(AtomicUsize::new(0));

    let posthog_layer = PostHogLayer::new(Arc::clone(&client)).with_filter(
        Targets::new()
            .with_target("posthog", tracing::Level::INFO)
            .with_target("posthog_rs", tracing::Level::INFO)
            .with_default(LevelFilter::OFF),
    );

    let subscriber = tracing_subscriber::registry()
        .with(CountingLayer {
            count: Arc::clone(&seen_by_other_layer),
        })
        .with(posthog_layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(
            target: "posthog",
            marker = "captured",
            "capture this"
        );

        tracing::debug!(
            target: "posthog",
            marker = "debug",
            "filter this"
        );

        tracing::info!(
            target: "some_other_target",
            marker = "other",
            "filter this too"
        );

        tracing::info!(
            target: "posthog_rs::client",
            marker = "sdk",
            "never capture this"
        );

        assert_eq!(
            seen_by_other_layer.load(Ordering::Relaxed),
            4
        );
    });

    flush(&client).await;

    let events = events.lock().unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0]["properties"]["marker"],
        "captured"
    );
}

#[tokio::test]
async fn prevents_reentrant_capture_and_resets_guard() {
    let events = capture_events();
    let client = Arc::new(
        client_with_hook(
            "http://127.0.0.1:1",
            capture_hook(Arc::clone(&events)),
        )
        .await,
    );

    let provider_calls = Arc::new(AtomicUsize::new(0));

    let provider = {
        let provider_calls = Arc::clone(&provider_calls);

        move || {
            let call = provider_calls.fetch_add(1, Ordering::SeqCst);

            if call == 0 {
                tracing::info!(
                    target: "posthog",
                    marker = "nested",
                    "must not recurse"
                );

                Some("user-123".to_owned())
            } else {
                None
            }
        }
    };

    let layer = PostHogLayer::new(Arc::clone(&client))
        .with_distinct_id_provider(provider)
        .with_filter(
          Targets::new()
              .with_target("posthog", LevelFilter::INFO)
              .with_target("posthog_rs", LevelFilter::INFO)
              .with_default(LevelFilter::OFF),
        );

    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(
            target: "posthog",
            marker = "first",
            "first event"
        );

        tracing::info!(
            target: "posthog",
            marker = "second",
            "second event"
        );

        assert_eq!(
            provider_calls.load(Ordering::SeqCst),
            2
        );
    });

    flush(&client).await;

    let events = events.lock().unwrap();

    assert_eq!(events.len(), 2);

    assert_eq!(
        events[0]["distinct_id"],
        "user-123"
    );
    assert_eq!(
        events[0]["properties"]["marker"],
        "first"
    );

    assert_eq!(
        events[1]["properties"]["marker"],
        "second"
    );
    assert_eq!(
        events[1]["properties"]["$process_person_profile"],
        false
    );

    let second_distinct_id = events[1]["distinct_id"]
        .as_str()
        .expect("anonymous event should have a distinct_id");

    assert!(!second_distinct_id.is_empty());
    assert_ne!(second_distinct_id, "user-123");
}

#[tokio::test]
async fn captures_tracing_event_through_client_transport() {
    let server = MockServer::start();

    #[cfg(feature = "capture-v1")]
    let capture_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/i/v1/analytics/events")
            .body_includes(r#""event":"posthog""#)
            .body_includes(r#""distinct_id":"transport-user""#)
            .body_includes(r#""request_id":"req-123""#)
            .body_includes(r#""message":"transport smoke test""#);

        then.status(200);
    });

    #[cfg(not(feature = "capture-v1"))]
    let capture_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/batch/")
            .body_includes(r#""event":"posthog""#)
            .body_includes(r#""distinct_id":"transport-user""#)
            .body_includes(r#""request_id":"req-123""#)
            .body_includes(r#""message":"transport smoke test""#);

        then.status(200);
    });

    let client = Arc::new(
        client_without_hook(&server.base_url()).await
    );

    let layer = PostHogLayer::new(Arc::clone(&client)).with_filter(
        Targets::new()
            .with_target("posthog", tracing::Level::INFO)
            .with_default(LevelFilter::OFF),
    );

    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(
            target: "posthog",
            distinct_id = "transport-user",
            request_id = "req-123",
            "transport smoke test"
        );
    });

    flush(&client).await;

    capture_mock.assert_calls(1);
}
