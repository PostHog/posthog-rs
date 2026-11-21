//! Setup-only served values and transient bootstrap reporting context.

use std::collections::HashMap;
use std::sync::Arc;

use super::flag_state::{FlagCache, FlagContext};
use super::{Client, Config};
use crate::client::transport::flags::LoadedFlags;
use crate::feature_flag_evaluations::EvaluatedFlagRecord;
use crate::FlagValue;

impl Client {
    pub(super) fn apply_flag_bootstrap(&self, config: &Config) {
        let Some(values) = &config.bootstrap_feature_flags else {
            return;
        };
        let _ = self.state.as_ref().unwrap().mutate(|state| {
            let context = Arc::new(FlagContext::current(state, config.disable_geoip));
            let cache = Arc::new(bootstrap_cache(config, values, context));
            state.flag_load.bootstrap = Some(cache.clone());
            state.flags.cache = Some(cache);
        });
    }
}

fn bootstrap_cache(
    config: &Config,
    values: &HashMap<String, FlagValue>,
    context: Arc<FlagContext>,
) -> FlagCache {
    let mut records = HashMap::new();
    // Unserved/orphan payloads are ignored.
    for (key, value) in values {
        let variant = match value {
            FlagValue::Boolean(false) => continue,
            FlagValue::String(variant) if variant.is_empty() => continue,
            FlagValue::String(variant) => Some(variant.clone()),
            FlagValue::Boolean(true) => None,
        };
        let payload = config
            .bootstrap_feature_flag_payloads
            .as_ref()
            .and_then(|payloads| payloads.get(key));
        records.insert(
            key.clone(),
            EvaluatedFlagRecord {
                enabled: true,
                variant,
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
        LoadedFlags {
            records,
            request_id: None,
            evaluated_at: None,
            partial: false,
            invalid_payloads: false,
            failed: Default::default(),
        },
    )
}
