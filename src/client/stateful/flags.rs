//! Current-user loading uses the independent request worker and validated state owner.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::flag_state::{FlagCache, FlagCompletion, FlagContext};
use super::writer::StateAccess;
use super::Client;
use crate::client::transport::{flags, TransportHandle};
use crate::client::ClientOptions;

pub(super) struct FlagLoader {
    options: Arc<ClientOptions>,
    // Serialize request snapshotting and enqueueing, not network execution. An
    // older caller must not overwrite a newer caller's pending refresh.
    scheduling: Mutex<()>,
    pub(super) listeners: Arc<super::flag_listeners::Listeners>,
}

#[derive(Clone)]
struct Request {
    context: Arc<FlagContext>,
    revision: u64,
    sequence: u64,
}

impl Request {
    /// Reserve the next request for the current targeting.
    fn reserve(state: &mut super::Snapshot, disable_geoip: bool) -> Option<Self> {
        let sequence = state.flag_load.request.checked_add(1)?;
        state.flag_load.request = sequence;
        Some(Self {
            context: Arc::new(FlagContext::current(state, disable_geoip)),
            revision: state.flag_load.revision,
            sequence,
        })
    }

    fn payload(&self, api_key: &str) -> Value {
        json!({
            "api_key": api_key,
            "distinct_id": self.context.distinct_id,
            "$anon_distinct_id": self.context.anonymous_id,
            "$device_id": self.context.device_id,
            "groups": self.context.groups,
            "person_properties": self.context.person_properties,
            "group_properties": self.context.group_properties,
            "geoip_disable": self.context.disable_geoip,
            // The backend would otherwise classify the `posthog-rs` user agent
            // as a server SDK and serve server-only flags, not client-only ones.
            "evaluation_runtime": "client",
        })
    }
}

impl FlagLoader {
    pub(super) fn new(
        options: ClientOptions,
        callbacks: Vec<super::flag_listeners::FlagsHook>,
    ) -> Self {
        Self {
            options: Arc::new(options),
            scheduling: Mutex::new(()),
            listeners: Arc::new(super::flag_listeners::Listeners::new(callbacks)),
        }
    }

    fn reload(&self, access: StateAccess, transport: &TransportHandle) {
        let scheduling = self.scheduling.lock().unwrap_or_else(|p| p.into_inner());
        let request = access.mutate(|state| Request::reserve(state, self.options.disable_geoip));
        let Ok((_, Some(request))) = request else {
            return;
        };
        let task = task_for(
            access,
            self.options.clone(),
            self.listeners.clone(),
            request,
        );
        transport.schedule_flags(task);
        drop(scheduling);
    }
}

/// The request task. If targeting changed while it was in flight and no newer
/// request exists, its completion continues with a follow-up for the current
/// targeting, as iOS and Android do, so a load is never lost.
fn task_for(
    access: StateAccess,
    options: Arc<ClientOptions>,
    listeners: Arc<super::flag_listeners::Listeners>,
    request: Request,
) -> flags::Task {
    let payload = request.payload(&options.api_key);
    flags::Task::with_follow_up(&payload, move |result| {
        let follow_up = install(&access, &request, result, options.disable_geoip);
        listeners.notify_completion(&access, request.revision, request.sequence);
        follow_up.map(|next| task_for(access, options, listeners, next))
    })
}

/// Transport failures are diagnosed by the worker. An obsolete completion
/// changes no state or status. When only targeting properties made it obsolete
/// and no newer request exists, returns the follow-up request, reserved in the
/// same update so a concurrent reload supersedes it.
fn install(
    access: &StateAccess,
    request: &Request,
    result: Result<flags::LoadedFlags, flags::LoadError>,
    disable_geoip: bool,
) -> Option<Request> {
    let follow_up = access.mutate(|state| {
        if state.flag_load.request != request.sequence {
            return None;
        }
        if state.flag_load.revision != request.revision {
            // Identity changes (identify, reset, groups) start their own request.
            if state.distinct_id != request.context.distinct_id
                || state.anonymous_id != request.context.anonymous_id
                || state.groups != request.context.groups
            {
                return None;
            }
            return Request::reserve(state, disable_geoip);
        }
        let mut completion = FlagCompletion {
            context: request.context.clone(),
            revision: request.revision,
            request: request.sequence,
            error: None,
            partial: false,
            invalid_payloads: false,
        };
        match result {
            Ok(loaded) => {
                // A valid response, including a partial one, ends bootstrap
                // use globally, not just for replaced keys.
                state.flag_load.remote_received = true;
                completion.partial = loaded.partial;
                completion.invalid_payloads = loaded.invalid_payloads;
                state.flags.cache = Some(Arc::new(FlagCache::updated(
                    state.flags.cache.as_deref(),
                    request.context.clone(),
                    loaded,
                )));
            }
            Err(error) => completion.error = Some(error),
        }
        state.flag_load.completed = Some(completion);
        None
    });
    follow_up.ok().and_then(|(_, next)| next)
}

impl Client {
    /// Request current-user flags without waiting for network I/O. Reload emits
    /// no analytics and remains available while opted out. Concurrent requests
    /// coalesce to one latest pending refresh; obsolete responses cannot install.
    /// Failures retain cached values and report through `on_error`.
    ///
    /// Uses the independent request worker. Delivery and in-memory access
    /// do not wait for requests, and flush does not wait for flag completion.
    pub fn reload_feature_flags(&self) {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return;
        }
        self.flag_loader.as_ref().unwrap().reload(
            self.state.as_ref().unwrap().access(),
            self.transport.as_ref().unwrap(),
        );
    }
}

#[cfg(test)]
#[path = "flags_tests.rs"]
mod tests;
