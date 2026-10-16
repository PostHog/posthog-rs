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

    /// Pins these defaults before delivery. Explicit event identity and properties win.
    pub fn apply_to_event(&self, event: &mut Event) {
        for (key, value) in &self.properties {
            event.insert_prop_default(key.clone(), value.clone());
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
        options
    }
}

pub(crate) fn apply_current_context(event: &mut Event) {
    if !event.request_context_applied {
        if let Some(context) = RequestContext::current() {
            context.apply_to_event(event);
        }
    }
}
