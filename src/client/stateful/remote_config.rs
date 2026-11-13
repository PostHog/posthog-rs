//! Remote configuration outlives identity changes; flag evaluation remains user-scoped.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::flag_state::{FlagCache, FlagCompletion, FlagContext};
use super::Client;
use crate::client::transport::{flags::LoadedFlags, remote_config};

impl Client {
    pub(super) fn load_remote_config(&self, preload_flags: bool) {
        if self.is_disabled() || self.closed.load(Ordering::Acquire) {
            return;
        }
        let access = self.state.as_ref().unwrap().access();
        let requested = access.mutate(|state| {
            if state.remote_config_loading {
                return None;
            }
            state.remote_config_loading = true;
            Some(state.flag_load.request)
        });
        let Ok((_, Some(expected_request))) = requested else {
            return;
        };
        let loader = self.flag_loader.as_ref().unwrap().clone();
        let options = loader.options.clone();
        let Some(url) =
            remote_config::config_url(options.endpoints().base_host(), &options.api_key)
        else {
            let _ = access.mutate(|state| state.remote_config_loading = false);
            remote_config::report(&options, remote_config::LoadError::InvalidResponse);
            if preload_flags {
                self.reload_feature_flags();
            }
            return;
        };
        let completion_access = access.clone();
        let task = remote_config::Task::new(url, move |result| {
            let installed = completion_access.mutate(|state| {
                state.remote_config_loading = false;
                let mut notify = None;
                let preload = match result {
                    Ok(config) => {
                        let no_flags = config.has_feature_flags == Some(false);
                        state.remote_config = Some(Arc::new(config));
                        // Independent flag requests are not governed by the startup hint.
                        if no_flags && state.flag_load.request == expected_request {
                            let context =
                                Arc::new(FlagContext::current(state, options.disable_geoip));
                            state.flags.cache = state
                                .flag_load
                                .bootstrap
                                .as_ref()
                                .filter(|cache| {
                                    cache.context.distinct_id == context.distinct_id
                                        && cache.context.anonymous_id == context.anonymous_id
                                        && cache.context.groups == context.groups
                                })
                                .cloned()
                                .or_else(|| {
                                    FlagCache::updated(
                                        None,
                                        context.clone(),
                                        state.flag_load.revision,
                                        LoadedFlags {
                                            records: HashMap::new(),
                                            request_id: None,
                                            evaluated_at: None,
                                            partial: false,
                                            invalid_payloads: false,
                                        },
                                    )
                                    .ok()
                                    .map(Arc::new)
                                });
                            state.flag_load.reports.clear();
                            state.flag_load.completed = Some(FlagCompletion {
                                context,
                                revision: state.flag_load.revision,
                                request: state.flag_load.request,
                                error: None,
                                partial: false,
                                invalid_payloads: false,
                            });
                            notify = Some((state.flag_load.revision, state.flag_load.request));
                        }
                        !no_flags
                    }
                    // A stale cached false hint must not suppress the failure fallback.
                    Err(_) => true,
                };
                (preload, notify)
            });
            let Ok((_, (preload, notify))) = installed else {
                return None;
            };
            if let Some((revision, request)) = notify {
                loader
                    .listeners
                    .notify_completion(&completion_access, revision, request);
            }
            if preload_flags && preload {
                loader.startup_task(completion_access, expected_request)
            } else {
                None
            }
        });
        if !self
            .transport
            .as_ref()
            .unwrap()
            .schedule_remote_config(task)
        {
            let _ = access.mutate(|state| state.remote_config_loading = false);
        }
    }
}

#[cfg(test)]
#[path = "remote_config_tests.rs"]
mod tests;

pub(super) mod cache_serde {
    use crate::client::transport::remote_config::RemoteConfig;
    use serde::{Deserialize, Serialize};
    use std::sync::Arc;

    pub(crate) fn serialize<S: serde::Serializer>(
        value: &Option<Arc<RemoteConfig>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_deref().serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Arc<RemoteConfig>>, D::Error> {
        Option::<RemoteConfig>::deserialize(deserializer).map(|config| config.map(Arc::new))
    }
}
