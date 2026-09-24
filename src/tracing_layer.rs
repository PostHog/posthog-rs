use std::{cell::Cell, fmt::Debug, sync::Arc};
use serde_json::{Map, Value};
use tracing::{field::{Field, Visit}, Event as TracingEvent, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use crate::{Client, Event};

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
            self.fields.insert(field.name().to_string(), Value::Number(num));
        } else {
            self.fields.insert(field.name().to_string(), Value::Null);
        }
    }

    #[inline]
    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields.insert(field.name().to_string(), Value::String(value.to_string()));
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
        self.fields.insert(field.name().to_string(), Value::String(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.fields.insert(
            field.name().to_string(),
            Value::String(format!("{value:?}")),
        );
    }
}

pub enum EventNamer {
    Target,
    Name,
    TargetAndName,
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

pub struct PostHogLayer {
    client: Arc<Client>,
    event_namer: EventNamer,
    distinct_id: DistinctIdSource,
    properties: Map<String, Value>,
}

impl PostHogLayer {
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            client,
            event_namer: EventNamer::Target,
            distinct_id: DistinctIdSource::Field,
            properties: Map::new(),
        }
    }

    pub fn with_event_namer(mut self, event_namer: EventNamer) -> Self {
        self.event_namer = event_namer;
        self
    }

    pub fn with_distinct_id(mut self, distinct_id: impl Into<String>) -> Self {
        self.distinct_id = DistinctIdSource::Static(distinct_id.into());
        self
    }

    pub fn with_distinct_id_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn() -> Option<String> + Send + Sync + 'static,
    {
        self.distinct_id = DistinctIdSource::Provider(Arc::new(provider));
        self
    }

    pub fn anonymous(mut self) -> Self {
        self.distinct_id = DistinctIdSource::Anonymous;
        self
    }

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
