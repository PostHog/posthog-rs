use std::cell::Cell;
use std::fmt::Debug;
use std::sync::Arc;
use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::Metadata;


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
