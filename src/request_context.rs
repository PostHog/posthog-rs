use std::collections::HashMap;
#[cfg(feature = "async-client")]
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde_json::Value;

#[cfg(feature = "async-client")]
use crate::client::TransportHandle;
#[cfg(feature = "error-tracking")]
use crate::event::fill_property;
use crate::{Error, Event};

/// At most this many events wait for a held scope's session; later ones are
/// dropped, as the TypeScript SDK does.
#[cfg(feature = "async-client")]
const MAX_HELD_EVENTS: usize = 1_000;

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
    #[cfg(feature = "async-client")]
    pub(crate) held: Option<HeldEvents>,
}

/// Events captured in a scope whose session is known only when the scope
/// ends, such as an MCP tool call whose new conversation handle may never
/// reach the agent. Once released, events captured later in the scope, such as
/// by work it spawned, take the released context at once.
#[cfg(feature = "async-client")]
#[derive(Clone)]
pub(crate) struct HeldEvents(Arc<Mutex<Hold>>);

#[cfg(feature = "async-client")]
#[cfg_attr(not(feature = "mcp"), allow(dead_code))]
enum Hold {
    Waiting {
        events: Vec<(Event, Arc<TransportHandle>)>,
        dropped: bool,
    },
    /// The properties the scope deferred, which fill each later event.
    Released(HashMap<String, Value>),
}

#[cfg(feature = "async-client")]
impl HeldEvents {
    #[cfg(feature = "mcp")]
    pub(crate) fn new() -> Self {
        let waiting = Hold::Waiting {
            events: Vec::new(),
            dropped: false,
        };
        Self(Arc::new(Mutex::new(waiting)))
    }

    /// Gives the event back, with the released context, once there is one.
    /// A held event takes its queue slot now; the mutex is never held while
    /// logging, since a subscriber that captures would re-enter it.
    fn hold(&self, mut event: Event, transport: &Arc<TransportHandle>) -> Option<Event> {
        let first_drop = match &mut *self.lock() {
            Hold::Released(deferred) => {
                fill_properties(&mut event, deferred);
                return Some(event);
            }
            Hold::Waiting { events, dropped } if events.len() >= MAX_HELD_EVENTS => {
                Some(!std::mem::replace(dropped, true))
            }
            Hold::Waiting { .. } => None,
        };
        if let Some(first_drop) = first_drop {
            if first_drop {
                tracing::warn!(
                    "a scope held {MAX_HELD_EVENTS} events waiting for their session; dropping the rest"
                );
            }
            return None;
        }
        if !transport.reserve_for_later() {
            return None;
        }
        event.ensure_timestamp(chrono::Utc::now());
        match &mut *self.lock() {
            Hold::Released(deferred) => {
                fill_properties(&mut event, deferred);
                transport.send_later(event);
            }
            Hold::Waiting { events, .. } if events.len() < MAX_HELD_EVENTS => {
                events.push((event, Arc::clone(transport)));
            }
            Hold::Waiting { .. } => transport.unreserve(),
        }
        None
    }

    /// Sends the held events with the properties the scope deferred, which
    /// fill only what each event leaves unset. Each event already took the
    /// rest of its context when it was held, and may have been finalized since
    /// (a `before_send` hook may have removed a property), so nothing else is
    /// applied again. Only the first release counts.
    #[cfg(feature = "mcp")]
    pub(crate) fn release(&self, deferred: HashMap<String, Value>) {
        let events = {
            let mut hold = self.lock();
            let Hold::Waiting { events, .. } = &mut *hold else {
                return;
            };
            let events = std::mem::take(events);
            *hold = Hold::Released(deferred.clone());
            events
        };
        for (mut event, transport) in events {
            fill_properties(&mut event, &deferred);
            transport.send_later(event);
        }
    }

    fn lock(&self) -> MutexGuard<'_, Hold> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(feature = "async-client")]
impl PartialEq for HeldEvents {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(feature = "async-client")]
impl Eq for HeldEvents {}

#[cfg(feature = "async-client")]
impl std::fmt::Debug for HeldEvents {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HeldEvents")
    }
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

    /// Sets a capture option on every event in this context. An option the event sets wins,
    /// and a `null` value sets nothing. A `process_person_profile` option does not set
    /// `process_person_profile` on an identify-style event, such as `$identify` or
    /// `$create_alias`, whose distinct ID the SDK did not generate; see
    /// [`RequestContext::apply_to_event`].
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

    #[cfg(feature = "async-client")]
    pub(crate) fn current_ids() -> (Option<String>, Option<String>) {
        CURRENT
            .try_with(|context| {
                (
                    context.get_distinct_id().map(str::to_owned),
                    context.get_session_id().map(str::to_owned),
                )
            })
            .unwrap_or_default()
    }

    /// Returns a snapshot. Spawned tasks must receive and scope an explicit clone.
    pub fn current() -> Option<Self> {
        Self::with_current(Clone::clone)
    }

    pub(crate) fn with_current<R>(read: impl FnOnce(&Self) -> R) -> Option<R> {
        #[cfg(feature = "async-client")]
        {
            CURRENT.try_with(read).ok()
        }
        #[cfg(not(feature = "async-client"))]
        {
            let _ = read;
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
    ///
    /// An identify-style event, such as `$identify` or `$create_alias`, first gets
    /// `process_person_profile` from its own option or `$process_person_profile` property,
    /// else `true` unless the SDK generated its distinct ID, so the context cannot turn
    /// person processing off for it.
    pub fn apply_to_event(&self, event: &mut Event) {
        // Only before the first context fill, so a context value never counts as the caller's.
        if !event.request_context_applied {
            event.apply_identify_style_person_default();
        }
        fill_properties(event, &self.properties);
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
        let mut properties: HashMap<_, _> = options.properties.into_iter().collect();
        for (key, value) in &self.properties {
            fill_property(&mut properties, key.clone(), value.clone());
        }
        options.properties = properties.into_iter().collect();
        for (key, value) in self.options.iter().filter(|(_, value)| !value.is_null()) {
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

/// The current context's distinct id, if it has one that is not blank.
pub(crate) fn current_distinct_id() -> Option<String> {
    #[cfg(feature = "async-client")]
    {
        CURRENT
            .try_with(|context| {
                context
                    .get_distinct_id()
                    .filter(|id| !id.trim().is_empty())
                    .map(str::to_string)
            })
            .ok()
            .flatten()
    }
    #[cfg(not(feature = "async-client"))]
    {
        None
    }
}

/// Fills the properties `event` leaves unset.
fn fill_properties(event: &mut Event, properties: &HashMap<String, Value>) {
    let skip_session = event.event_name() == "$groupidentify";
    for (key, value) in properties {
        // A session on `$groupidentify` would reassign the session to the group's distinct id.
        if skip_session && key == "$session_id" {
            continue;
        }
        event.insert_prop_default(key.clone(), value.clone());
    }
}

/// Holds `event` when the current context holds events, else gives it back.
/// A held event takes the current context first, so a nested scope's values
/// survive; the released context then fills what is still unset.
#[cfg(feature = "async-client")]
pub(crate) fn hold_in_current(mut event: Event, transport: &Arc<TransportHandle>) -> Option<Event> {
    let held = CURRENT.try_with(|context| {
        let held = context.held.clone()?;
        if !event.request_context_applied {
            event.apply_identify_style_person_default();
            context.apply_to_event(&mut event);
        }
        Some(held)
    });
    match held {
        Ok(Some(held)) => held.hold(event, transport),
        _ => Some(event),
    }
}

/// The events of `events` the current context does not hold.
#[cfg(feature = "async-client")]
pub(crate) fn hold_all_in_current(
    events: Vec<Event>,
    transport: &Arc<TransportHandle>,
) -> Vec<Event> {
    events
        .into_iter()
        .filter_map(|event| hold_in_current(event, transport))
        .collect()
}

pub(crate) fn apply_current_context(event: &mut Event) {
    if !event.request_context_applied {
        // Also reached with no scoped context, such as from the blocking client.
        event.apply_identify_style_person_default();
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
    fn exception_options_fill_only_unset_set_keys() {
        let context = RequestContext::new()
            .property("$set", json!({"plan": "free", "source": "web"}))
            .unwrap()
            .property("route", "/work")
            .unwrap();
        let options = CaptureExceptionOptions::new()
            .property("$set", json!({"plan": "pro"}))
            .unwrap()
            .property("route", "/explicit")
            .unwrap();
        let mut merged = context.exception_options(options).properties;
        merged.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            merged,
            vec![
                ("$set".to_string(), json!({"plan": "pro", "source": "web"})),
                ("route".to_string(), json!("/explicit")),
            ]
        );
    }

    #[test]
    fn exception_options_fill_only_unset_options() {
        let context = RequestContext::new()
            .option("cookieless_mode", true)
            .unwrap()
            .option("disable_skew_correction", true)
            .unwrap()
            .option("product_tour_id", "tour")
            .unwrap()
            .option("process_person_profile", Value::Null)
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

#[cfg(all(test, feature = "mcp"))]
mod held_tests {
    use std::sync::Arc;

    use serde_json::Value;

    use super::HeldEvents;
    use crate::client::TransportHandle;
    use crate::{ClientOptionsBuilder, Event};

    #[test]
    fn held_events_count_against_the_queue_bound() {
        let options = ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .host("http://127.0.0.1:9".to_string())
            .flush_interval_ms(60_000u64)
            .max_queue_size(2usize)
            .build()
            .unwrap();
        let transport = Arc::new(TransportHandle::spawn(options));
        let held = HeldEvents::new();

        for _ in 0..3 {
            assert!(held.hold(Event::new_anon("held"), &transport).is_none());
        }

        assert!(
            !transport.reserve_for_later(),
            "two held events fill the queue"
        );
        let deferred = std::iter::once(("$session_id".to_string(), Value::from("ses_final")));
        held.release(deferred.collect());
        let cases = [
            ("late", Some(Value::from("ses_final"))),
            ("$groupidentify", None),
        ];
        for (name, session) in cases.iter().cloned() {
            let late = held.hold(Event::new_anon(name), &transport);
            let late = late.expect("a released scope sends at once");
            assert_eq!(
                late.properties().get("$session_id").cloned(),
                session,
                "{}",
                name
            );
        }
    }
}
