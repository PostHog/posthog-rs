use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::{Error, Event};

#[cfg(feature = "async-client")]
tokio::task_local! {
    static CURRENT: RequestContext;
}

/// Analytics defaults for one request. Header identity is not proof of authentication.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestContext {
    pub(crate) distinct_id: Option<String>,
    pub(crate) properties: HashMap<String, Value>,
    pub(crate) options: HashMap<String, Value>,
}

impl RequestContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn distinct_id(mut self, distinct_id: impl Into<String>) -> Self {
        self.distinct_id = Some(distinct_id.into());
        self
    }

    pub fn session_id(mut self, session_id: impl Into<String>) -> Self {
        self.properties
            .insert("$session_id".into(), Value::String(session_id.into()));
        self
    }

    pub fn property(
        mut self,
        key: impl Into<String>,
        value: impl Serialize,
    ) -> Result<Self, Error> {
        let value =
            serde_json::to_value(value).map_err(|error| Error::Serialization(error.to_string()))?;
        self.properties.insert(key.into(), value);
        Ok(self)
    }

    /// Sets a capture option on every event in this context. An option the event sets wins.
    pub fn option(mut self, key: impl Into<String>, value: impl Serialize) -> Result<Self, Error> {
        let value =
            serde_json::to_value(value).map_err(|error| Error::Serialization(error.to_string()))?;
        self.options.insert(key.into(), value);
        Ok(self)
    }

    pub fn get_distinct_id(&self) -> Option<&str> {
        self.distinct_id
            .as_deref()
            .filter(|value| !value.is_empty())
    }

    pub fn get_session_id(&self) -> Option<&str> {
        self.properties
            .get("$session_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    }

    /// Returns a snapshot. Spawned tasks must receive and scope an explicit clone.
    pub fn current() -> Option<Self> {
        #[cfg(feature = "async-client")]
        {
            CURRENT.try_with(Clone::clone).ok()
        }
        #[cfg(not(feature = "async-client"))]
        {
            None
        }
    }

    /// Context exists only while the future is polled, including error and cancellation paths.
    #[cfg(feature = "async-client")]
    pub async fn scope<F: std::future::Future>(self, future: F) -> F::Output {
        CURRENT.scope(self, future).await
    }

    /// Without identity, this uses the SDK's ordinary personless event behavior.
    pub fn event(&self, name: impl Into<String>) -> Event {
        let name = name.into();
        let mut event = match self.get_distinct_id() {
            Some(identity) => Event::new(name, identity.to_string()),
            None => Event::new_anon(name),
        };
        self.apply_to_event(&mut event);
        event
    }

    /// Pins these defaults before delivery. Explicit event identity, properties and options win.
    pub fn apply_to_event(&self, event: &mut Event) {
        for (key, value) in &self.properties {
            event.insert_prop_default(key.clone(), value.clone());
        }
        for (key, value) in &self.options {
            event.insert_option_default(key.clone(), value.clone());
        }
        event.request_context_applied = true;
    }

    #[cfg(feature = "error-tracking")]
    pub fn exception_options(
        &self,
        mut options: crate::CaptureExceptionOptions,
    ) -> crate::CaptureExceptionOptions {
        if options.distinct_id.is_none() {
            options.distinct_id = self.get_distinct_id().map(str::to_string);
        }
        let mut properties: HashMap<_, _> = self.properties.clone();
        properties.extend(options.properties);
        options.properties = properties.into_iter().collect();
        for (key, value) in &self.options {
            // The last entry for a key wins; a missing or null last value counts as unset, as on events.
            let unset = options
                .options
                .iter()
                .rev()
                .find(|(set_key, _)| set_key == key)
                .map_or(true, |(_, set_value)| set_value.is_null());
            if unset {
                options.options.retain(|(set_key, _)| set_key != key);
                options.options.push((key.clone(), value.clone()));
            }
        }
        options
    }
}

pub(crate) fn apply_current_context(event: &mut Event) {
    if !event.request_context_applied {
        #[cfg(feature = "async-client")]
        let _ = CURRENT.try_with(|context| context.apply_to_event(event));
    }
}

#[cfg(all(test, feature = "error-tracking"))]
mod test {
    use serde_json::{json, Value};

    use super::RequestContext;
    use crate::CaptureExceptionOptions;

    #[test]
    fn exception_options_fill_only_unset_options() {
        let context = RequestContext::new()
            .option("cookieless_mode", true)
            .unwrap()
            .option("disable_skew_correction", true)
            .unwrap()
            .option("product_tour_id", "tour")
            .unwrap();
        let options = CaptureExceptionOptions::new()
            .option("cookieless_mode", false)
            .unwrap()
            .option("disable_skew_correction", Value::Null)
            .unwrap()
            .option("product_tour_id", "caller-tour")
            .unwrap()
            .option("product_tour_id", Value::Null)
            .unwrap();
        let mut merged = context.exception_options(options).options;
        merged.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            merged,
            vec![
                ("cookieless_mode".to_string(), json!(false)),
                ("disable_skew_correction".to_string(), json!(true)),
                ("product_tour_id".to_string(), json!("tour")),
            ]
        );
    }
}
