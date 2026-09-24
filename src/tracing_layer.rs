use crate::{Client, Event};
use serde_json::{Map, Value};
use std::{cell::Cell, fmt::Debug, sync::Arc};
use tracing::{
    field::{Field, Visit},
    Event as TracingEvent, Metadata, Subscriber,
};
use tracing_subscriber::layer::{Context, Layer};

thread_local! {
    static IS_CAPTURING: Cell<bool> = const { Cell::new(false) };
}

struct CapturingGuard;

impl CapturingGuard {
    #[inline]
    fn try_acquire() -> Option<Self> {
        IS_CAPTURING.with(|capturing| {
            if capturing.get() {
                None
            } else {
                capturing.set(true);
                Some(Self)
            }
        })
    }
}

impl Drop for CapturingGuard {
    #[inline]
    fn drop(&mut self) {
        IS_CAPTURING.with(|capturing| capturing.set(false));
    }
}

#[derive(Default)]
struct Visitor {
    fields: Map<String, Value>,
}

impl Visit for Visitor {
    #[inline]
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(field.name().to_string(), value.into());
    }

    #[inline]
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(field.name().to_string(), value.into());
    }

    #[inline]
    fn record_i128(&mut self, field: &Field, value: i128) {
        let val = serde_json::Number::from_f64(value as f64)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(value.to_string()));
        self.fields.insert(field.name().to_string(), val);
    }

    #[inline]
    fn record_u128(&mut self, field: &Field, value: u128) {
        let val = serde_json::Number::from_f64(value as f64)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(value.to_string()));
        self.fields.insert(field.name().to_string(), val);
    }

    #[inline]
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.insert(field.name().to_string(), value.into());
    }

    #[inline]
    fn record_f64(&mut self, field: &Field, value: f64) {
        if let Some(num) = serde_json::Number::from_f64(value) {
            self.fields
                .insert(field.name().to_string(), Value::Number(num));
        } else {
            self.fields.insert(field.name().to_string(), Value::Null);
        }
    }

    #[inline]
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields
            .insert(field.name().to_string(), Value::String(value.to_string()));
    }

    #[inline]
    fn record_bytes(&mut self, field: &Field, value: &[u8]) {
        self.fields.insert(
            field.name().to_string(),
            Value::String(String::from_utf8_lossy(value).into_owned()),
        );
    }

    #[inline]
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.fields
            .insert(field.name().to_string(), Value::String(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.fields.insert(
            field.name().to_string(),
            Value::String(format!("{value:?}")),
        );
    }
}

/// Controls how tracing metadata is turned into a PostHog event name.
pub enum EventNamer {
    /// Use the tracing target as the event name.
    Target,
    /// Use the tracing callsite name as the event name.
    Name,
    /// Combine the tracing target and callsite name.
    TargetAndName,
    /// Derive the event name from the tracing metadata.
    Custom(Arc<dyn Fn(&Metadata<'_>) -> String + Send + Sync>),
}

impl Default for EventNamer {
    fn default() -> Self {
        Self::Target
    }
}

impl EventNamer {
    fn name(&self, metadata: &Metadata<'_>) -> String {
        match self {
            Self::Target => metadata.target().to_string(),
            Self::Name => metadata.name().to_string(),
            Self::TargetAndName => format!("{}.{}", metadata.target(), metadata.name()),
            Self::Custom(namer) => namer(metadata),
        }
    }
}

pub enum DistinctIdSource {
    Field,
    Static(String),
    Provider(Arc<dyn Fn() -> Option<String> + Send + Sync>),
    Anonymous,
}

impl Default for DistinctIdSource {
    fn default() -> Self {
        Self::Field
    }
}

impl DistinctIdSource {
    fn resolve(&self, field: Option<Value>) -> Option<String> {
        match self {
            Self::Field => field.and_then(|value| match value {
                Value::String(s) if !s.is_empty() => Some(s),
                Value::Null => None,
                value => Some(value.to_string()),
            }),
            Self::Static(value) => Some(value.clone()),
            Self::Provider(provider) => provider(),
            Self::Anonymous => None,
        }
    }
}

/// Captures selected `tracing` events as PostHog events.
///
/// Tracing fields are stored as PostHog properties, while the existing
/// PostHog client handles batching and delivery.
///
/// Use `tracing_subscriber` filters to control which events are captured.
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use tracing_subscriber::layer::SubscriberExt;
///
/// let layer = posthog_rs::PostHogLayer::new(Arc::new(client));
///
/// tracing_subscriber::registry()
///     .with(layer)
///     .init();
///
/// tracing::info!(
///     target: "posthog",
///     distinct_id = "user-123",
///     feature = "checkout",
///     "checkout started",
/// );
/// ```
///
/// SDK internal `posthog_rs` targets are ignored automatically.
///
/// For short lived progrms, flush the client before exiting.
///
/// ```no_run
/// client.flush();
/// ```
///
/// With the async client:
///
/// ```no_run
/// client.flush().await;
/// ```
pub struct PostHogLayer {
    client: Arc<Client>,
    event_namer: EventNamer,
    distinct_id: DistinctIdSource,
    properties: Map<String, Value>,
}

impl PostHogLayer {
    /// Creates a layer backed by the given PostHog client.
    ///
    /// By default, the tracing target is used as the PostHog event name and
    /// `distinct_id` is read from the tracing fields.
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            client,
            event_namer: EventNamer::Target,
            distinct_id: DistinctIdSource::Field,
            properties: Map::new(),
        }
    }

    /// Sets how tracing events are named in PostHog.
    ///
    /// # Example
    ///
    /// ```no_run
    /// let layer = PostHogLayer::new(client)
    ///     .with_event_namer(EventNamer::TargetAndName);
    /// ```
    pub fn with_event_namer(mut self, event_namer: EventNamer) -> Self {
        self.event_namer = event_namer;
        self
    }

    /// Sets a fixed distinct ID for all captured events.
    ///
    /// # Example
    ///
    /// ```no_run
    /// let layer = PostHogLayer::new(client)
    ///     .with_distinct_id("user-123");
    /// ```
    pub fn with_distinct_id(mut self, distinct_id: impl Into<String>) -> Self {
        self.distinct_id = DistinctIdSource::Static(distinct_id.into());
        self
    }

    /// Gets the distinct ID for each captured event from a callback.
    ///
    /// Return `None` to create an anonymous PostHog event.
    ///
    /// # Example
    ///
    /// ```no_run
    /// let layer = PostHogLayer::new(client)
    ///     .with_distinct_id_provider(|| {
    ///         Some("user-123".to_owned())
    ///     });
    /// ```
    pub fn with_distinct_id_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> Option<String> + Send + Sync + 'static,
    {
        self.distinct_id = DistinctIdSource::Provider(Arc::new(provider));
        self
    }

    /// Captures events without a caller-provided distinct ID.
    pub fn anonymous(mut self) -> Self {
        self.distinct_id = DistinctIdSource::Anonymous;
        self
    }

    /// Adds a property to every captured PostHog event.
    ///
    /// Event fields with the same name override shared properties.
    ///
    /// # Example
    ///
    /// ```no_run
    /// let layer = PostHogLayer::new(client)
    ///     .with_property("service", "checkout");
    ///
    /// tracing::info!(service = "api", "request handled");
    /// // The event contains `service = "api"`.
    /// ```
    pub fn with_property(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }
}

#[inline]
fn is_sdk_target(target: &str) -> bool {
    target == "posthog_rs" || target.starts_with("posthog_rs::")
}

impl<S> Layer<S> for PostHogLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &TracingEvent<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();

        if is_sdk_target(metadata.target()) {
            return;
        }

        // for OOMs
        let _guard = match CapturingGuard::try_acquire() {
            Some(guard) => guard,
            None => return,
        };

        let mut visitor = Visitor::default();
        event.record(&mut visitor);

        let distinct_id = self
            .distinct_id
            .resolve(visitor.fields.remove("distinct_id"));

        let event_name = self.event_namer.name(metadata);

        let mut posthog_event = match distinct_id {
            Some(id) => Event::new(event_name, id),
            None => Event::new_anon(event_name),
        };

        let mut fields = self.properties.clone();
        fields.extend(visitor.fields);

        for (key, value) in fields {
            _ = posthog_event.insert_prop(key, value);
        }

        self.client.capture(posthog_event);
    }
}
