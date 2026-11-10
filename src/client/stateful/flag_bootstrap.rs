//! Setup-only served values and transient bootstrap reporting context.

use std::collections::HashMap;
use std::sync::Arc;

use super::flag_state::{FlagCache, FlagContext, MAX_CACHE_BYTES};
use super::{Client, Config};
use crate::client::event_store::RecordBuffer;
use crate::client::transport::flags::{self, LoadError, LoadedFlags, MAX_FLAGS};
use crate::endpoints::Endpoint;
use crate::feature_flag_evaluations::EvaluatedFlagRecord;
use crate::FlagValue;

impl Client {
    pub(super) fn apply_flag_bootstrap(&self, config: &Config) {
        let Some(values) = &config.bootstrap_feature_flags else {
            return;
        };
        let result = self.state.as_ref().unwrap().mutate(|state| {
            let context = Arc::new(FlagContext::current(state, config.disable_geoip));
            let cache = Arc::new(bootstrap_cache(
                config,
                values,
                context,
                state.flag_load.revision,
            )?);
            state.flag_load.bootstrap = Some(cache.clone());
            state.flags.cache = Some(cache);
            Ok::<_, LoadError>(())
        });
        if let Ok((_, Err(error))) = result {
            let options = config.transport_options();
            flags::report(
                &options,
                &options.endpoints().build_url(Endpoint::Flags),
                Some(&self.get_distinct_id()),
                error,
            );
        }
    }
}

fn bootstrap_cache(
    config: &Config,
    values: &HashMap<String, FlagValue>,
    context: Arc<FlagContext>,
    revision: u64,
) -> Result<FlagCache, LoadError> {
    let mut records = HashMap::new();
    // Bound selected input before cloning it. Unserved/orphan payloads are
    // ignored. The ordinary cache assembler also bounds normalized metadata.
    let mut buffer = RecordBuffer {
        bytes: Vec::new(),
        limit: MAX_CACHE_BYTES as u64,
    };
    for (key, value) in values {
        match value {
            FlagValue::Boolean(false) => continue,
            FlagValue::String(variant) if variant.is_empty() => continue,
            _ => {}
        }
        let payload = config
            .bootstrap_feature_flag_payloads
            .as_ref()
            .and_then(|payloads| payloads.get(key));
        if records.len() >= MAX_FLAGS
            || serde_json::to_writer(&mut buffer, &(key, value, payload)).is_err()
        {
            return Err(LoadError::CacheTooLarge);
        }
        records.insert(
            key.clone(),
            EvaluatedFlagRecord {
                enabled: true,
                variant: match value {
                    FlagValue::String(variant) => Some(variant.clone()),
                    FlagValue::Boolean(_) => None,
                },
                payload: payload.cloned(),
                id: None,
                version: None,
                reason: None,
                locally_evaluated: false,
                has_experiment: None,
                minimal_flag_called_events: false,
            },
        );
    }
    FlagCache::updated(
        None,
        context,
        revision,
        LoadedFlags {
            records,
            request_id: None,
            evaluated_at: None,
            partial: false,
            invalid_payloads: false,
        },
    )
}
