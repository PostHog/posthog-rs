//! Current-user loading uses the shared delivery worker and validated state owner.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::flag_state::{FlagCache, FlagCompletion, FlagContext};
use super::writer::StateAccess;
use super::Client;
use crate::client::transport::{flags, TransportHandle};
use crate::client::ClientOptions;
use crate::endpoints::Endpoint;

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
    fn payload(&self, api_key: &str) -> Value {
        json!({
            "api_key": api_key,
            "distinct_id": self.context.distinct_id,
            "$anon_distinct_id": self.context.anonymous_id,
            "$device_id": self.context.anonymous_id,
            "groups": self.context.groups,
            "person_properties": self.context.person_properties,
            "group_properties": self.context.group_properties,
            "geoip_disable": self.context.disable_geoip,
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
        let request = access.mutate(|state| {
            let sequence = state.flag_load.request.checked_add(1)?;
            state.flag_load.request = sequence;
            state.flag_load.reports.clear();
            Some(Request {
                context: Arc::new(FlagContext::current(state, self.options.disable_geoip)),
                revision: state.flag_load.revision,
                sequence,
            })
        });
        let Ok((_, Some(request))) = request else {
            return;
        };
        let completion_request = request.clone();
        let completion_access = access.clone();
        let options = self.options.clone();
        let listeners = self.listeners.clone();
        let task = flags::Task::new(&request.payload(&self.options.api_key), move |result| {
            if let Some(error) = install(&completion_access, &completion_request, result) {
                report(&options, &completion_request, error);
            }
            listeners.notify_completion(
                &completion_access,
                completion_request.revision,
                completion_request.sequence,
            );
        });
        let failure = match task {
            Ok(task) => {
                transport.schedule_flags(task);
                None
            }
            Err(error) => Some(error),
        };
        drop(scheduling);
        // Error hooks may reenter reload or shutdown; never invoke them while
        // holding either the scheduling mutex or the state owner's lock.
        if let Some(error) = failure {
            install(&access, &request, Err(error));
            report(&self.options, &request, error);
            self.listeners
                .notify_completion(&access, request.revision, request.sequence);
        }
    }
}

fn report(options: &ClientOptions, request: &Request, error: flags::LoadError) {
    flags::report(
        options,
        &options.endpoints().build_url(Endpoint::Flags),
        Some(&request.context.distinct_id),
        error,
    );
}

/// Return only a new cache-admission error; transport failures are already
/// diagnosed by the worker. An obsolete completion changes no state or status.
fn install(
    access: &StateAccess,
    request: &Request,
    result: Result<flags::LoadedFlags, flags::LoadError>,
) -> Option<flags::LoadError> {
    access
        .mutate(|state| {
            if state.flag_load.revision != request.revision
                || state.flag_load.request != request.sequence
            {
                return None;
            }
            let mut completion = FlagCompletion {
                context: request.context.clone(),
                revision: request.revision,
                request: request.sequence,
                error: None,
                partial: false,
                invalid_payloads: false,
            };
            let mut diagnostic = None;
            match result {
                Ok(loaded) => {
                    // A valid response, including partial or cache-too-large,
                    // ends bootstrap use globally, not just for replaced keys.
                    state.flag_load.remote_received = true;
                    completion.partial = loaded.partial;
                    completion.invalid_payloads = loaded.invalid_payloads;
                    match FlagCache::updated(
                        state.flags.cache.as_deref(),
                        request.context.clone(),
                        request.revision,
                        loaded,
                    ) {
                        Ok(cache) => state.flags.cache = Some(Arc::new(cache)),
                        Err(error) => {
                            completion.error = Some(error);
                            diagnostic = Some(error);
                        }
                    }
                }
                Err(error) => completion.error = Some(error),
            }
            state.flag_load.reports.clear();
            state.flag_load.completed = Some(completion);
            diagnostic
        })
        .ok()
        .and_then(|(_, diagnostic)| diagnostic)
}

impl Client {
    /// Request current-user flags without waiting for network I/O. Reload emits
    /// no analytics and remains available while opted out. Concurrent requests
    /// coalesce to one latest pending refresh; obsolete responses cannot install.
    /// Failures retain cached values and report through `on_error`.
    ///
    /// Uses the existing delivery worker: an active flags request may delay
    /// delivery or flush, but not capture admission or in-memory state access.
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
