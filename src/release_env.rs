//! The release id the SDK reports as `$release_id`, read from the environment at runtime.
//!
//! This is the deploy-time counterpart to injecting `$release_id` into a web bundle. A native build
//! has no bundle to inject, so the CLI's `release resolve` prints the created release's id, and the
//! app is launched with that id in `POSTHOG_RELEASE_ID`. The SDK reads it here and stamps it on
//! every event, so the server resolves the exception's release by a direct id lookup — no release
//! name or version has to match anything the app reports.

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

#[cfg(test)]
mod tests {
    use super::normalize;

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
