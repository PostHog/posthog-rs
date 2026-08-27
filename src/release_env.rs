//! The release id the SDK reports as `$release_id`, from an explicit `release_id` option or the
//! `POSTHOG_RELEASE_ID` environment variable.
//!
//! This is the native counterpart to injecting `$release_id` into a web bundle. A native build has
//! no bundle to inject, so the CLI's `release resolve` prints the created release's id and it
//! reaches the app one of two ways: baked in at build time (set the `release_id` option to
//! `option_env!("POSTHOG_RELEASE_ID")`, so a shipped binary self-identifies with nothing to set at
//! runtime), or read from `POSTHOG_RELEASE_ID` in the environment at runtime (so a deploy can
//! supply it without a rebuild). An explicit option wins over the environment. Either way the SDK
//! stamps it on `$exception` events, so the server resolves the release by a direct id lookup — no
//! release name or version has to match anything the app reports.

use std::sync::OnceLock;

/// The environment variable the release id is read from.
const RELEASE_ID_ENV: &str = "POSTHOG_RELEASE_ID";

/// The release id from `POSTHOG_RELEASE_ID`, read once. `None` when the variable is unset or blank.
pub(crate) fn release_id() -> Option<&'static str> {
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE
        .get_or_init(|| normalize(std::env::var(RELEASE_ID_ENV).ok()))
        .as_deref()
}

/// Trim the raw value and treat a blank string as unset, so `POSTHOG_RELEASE_ID=` (or whitespace)
/// does not send an empty `$release_id`.
fn normalize(raw: Option<String>) -> Option<String> {
    raw.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Resolve the release id from the two sources, explicit option first, environment fallback second.
/// A `config` value set in code (typically a build-time `option_env!("POSTHOG_RELEASE_ID")`) wins
/// over `env` (the runtime `POSTHOG_RELEASE_ID`), so a deploy-time override is opt-in, not implicit.
pub(crate) fn resolve_release_id(config: Option<&str>, env: Option<&str>) -> Option<String> {
    config.or(env).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{normalize, resolve_release_id};

    #[test]
    fn an_explicit_config_id_wins_over_the_environment() {
        assert_eq!(
            resolve_release_id(Some("from-config"), Some("from-env")).as_deref(),
            Some("from-config")
        );
    }

    #[test]
    fn the_environment_is_used_when_no_config_id_is_set() {
        assert_eq!(
            resolve_release_id(None, Some("from-env")).as_deref(),
            Some("from-env")
        );
    }

    #[test]
    fn no_config_and_no_environment_is_none() {
        assert_eq!(resolve_release_id(None, None), None);
    }

    #[test]
    fn an_unset_variable_is_none() {
        assert_eq!(normalize(None), None);
    }

    #[test]
    fn a_blank_value_is_none() {
        // `POSTHOG_RELEASE_ID=` or an all-whitespace value must not send an empty release id.
        assert_eq!(normalize(Some("   ".to_string())), None);
    }

    #[test]
    fn a_value_is_trimmed() {
        assert_eq!(
            normalize(Some("  01a03d94-7dd8-0000-e1cb-2a269e5ea0b5  ".to_string())).as_deref(),
            Some("01a03d94-7dd8-0000-e1cb-2a269e5ea0b5")
        );
    }
}
