//! Shared, runtime-agnostic helpers for the V0 capture pipeline.
//! Each client keeps only the I/O; this module owns event preparation and
//! payload construction.

// The transport worker is always blocking reqwest, even for the async client,
// so the v0 request helpers operate on the blocking RequestBuilder.
use chrono::{DateTime, Utc};
use reqwest::blocking::RequestBuilder;

use super::{
    common::{apply_before_send_hooks, apply_capture_defaults, apply_runtime_context},
    BeforeSendHook, CaptureDefaults, ClientOptions,
};
use crate::endpoints::Endpoint;
use crate::error::Error;
use crate::event::{BatchRequest, Event, InnerEvent};

// ---------------------------------------------------------------------------
// Event preparation
// ---------------------------------------------------------------------------

/// Apply client-level default properties (caller-wins) and V0 metadata stamping.
///
/// Uses `insert_prop_default` so a caller who explicitly set a property on the
/// event before calling `capture()` keeps their value.
pub(crate) fn prepare_event(event: &mut Event, defaults: &CaptureDefaults) {
    apply_capture_defaults(event, defaults);
    apply_runtime_context(event);
    event.prepare_for_v0();
}

// ---------------------------------------------------------------------------
// Payload building
// ---------------------------------------------------------------------------

/// Build the JSON body for a V0 batch capture request.
///
/// Returns the serialized body together with the number of events that survived
/// `before_send` filtering, so the caller can account for filtered-out events as
/// terminal and track only what is actually in flight. `Ok(None)` means every
/// event was dropped by `before_send`.
pub(crate) fn build_batch_payload(
    events: Vec<Event>,
    api_key: String,
    historical_migration: bool,
    sent_at: DateTime<Utc>,
    defaults: &CaptureDefaults,
    before_send: &[BeforeSendHook],
) -> Result<Option<(String, usize)>, Error> {
    let inner_events: Vec<InnerEvent> = events
        .into_iter()
        .filter_map(|mut event| {
            prepare_event(&mut event, defaults);
            apply_before_send_hooks(before_send, event).map(|mut event| {
                // Final step, after before_send: for minimized `$feature_flag_called`
                // events, drop everything outside the allowlist so a hook cannot
                // reintroduce properties it strips.
                event.apply_minimal_flag_called_allowlist();
                InnerEvent::new_for_batch(event)
            })
        })
        .collect();

    if inner_events.is_empty() {
        return Ok(None);
    }

    let kept = inner_events.len();
    let batch_request = BatchRequest {
        api_key,
        historical_migration,
        sent_at: sent_at.to_rfc3339(),
        batch: inner_events,
    };
    serde_json::to_string(&batch_request)
        .map(|json| Some((json, kept)))
        .map_err(|e| Error::Serialization(e.to_string()))
}

/// Encode the V0 JSON body, compressing when configured. Returns the bytes and
/// the `Content-Encoding` token to advertise (`Some(token)` when compressed,
/// `None` when sent uncompressed). Routes through the shared `compress()`, so a
/// V0 build is gzip-only by construction; a non-gzip algorithm or a compression
/// failure falls back to uncompressed.
pub(crate) fn encode_body(
    options: &ClientOptions,
    json: String,
) -> (Vec<u8>, Option<&'static str>) {
    match options.capture_compression {
        Some(algo) => match crate::compression::compress(algo, json.as_bytes()) {
            Some((bytes, encoding)) => (bytes, Some(encoding)),
            None => (json.into_bytes(), None),
        },
        None => (json.into_bytes(), None),
    }
}

// ---------------------------------------------------------------------------
// Inline (immediate) capture preparation
// ---------------------------------------------------------------------------

/// Everything an inline immediate V0 capture needs after payload building: the
/// target URL (already carrying the `?compression=` query param when compressed),
/// the encoded body, the `Content-Encoding` token to advertise, and the number
/// of events kept after `before_send`. I/O-free, so both clients share it.
pub(crate) struct PreparedV0 {
    pub(crate) url: String,
    pub(crate) body: Vec<u8>,
    pub(crate) encoding: Option<&'static str>,
    pub(crate) kept: usize,
}

/// Prepare an inline immediate V0 capture: build the batch payload (applying
/// defaults + `before_send`), encode/compress it, and resolve the target URL.
/// Returns `Ok(None)` when every event was dropped by `before_send`, so the
/// caller returns a default summary without sending.
pub(crate) fn prepare_immediate(
    options: &ClientOptions,
    events: Vec<Event>,
    historical_migration: bool,
) -> Result<Option<PreparedV0>, Error> {
    let defaults = options.capture_defaults();
    let Some((json, kept)) = build_batch_payload(
        events,
        options.api_key.clone(),
        historical_migration,
        Utc::now(),
        &defaults,
        &options.before_send,
    )?
    else {
        return Ok(None);
    };

    let base_url = options.endpoints().build_url(Endpoint::Batch);
    let (body, encoding) = encode_body(options, json);
    // v0 capture reads the compression hint from the query param, not the header.
    let url = match encoding {
        Some(token) => format!("{base_url}?compression={token}"),
        None => base_url,
    };
    Ok(Some(PreparedV0 {
        url,
        body,
        encoding,
        kept,
    }))
}

// ---------------------------------------------------------------------------
// Header helpers
// ---------------------------------------------------------------------------

/// Apply test-harness extra headers to a V0 request.
pub(crate) fn apply_extra_headers(
    #[allow(unused_variables)] options: &ClientOptions,
    #[allow(unused_mut)] mut request: RequestBuilder,
) -> RequestBuilder {
    #[cfg(feature = "test-harness")]
    if let Some(ref extra) = options.extra_capture_headers {
        for (k, v) in extra {
            request = request.header(k.as_str(), v.as_str());
        }
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::MINIMAL_FLAG_CALLED_EVENT_PROPERTIES;
    use serde_json::json;
    use std::collections::HashSet;

    /// Build a `$feature_flag_called` event carrying the full property set the
    /// snapshot path would produce, plus a non-allowlisted extra to prove
    /// nothing leaks past the allowlist.
    fn full_flag_called_event() -> Event {
        let mut event = Event::new("$feature_flag_called", "user-1");
        event.add_group("company", "acme");
        for (k, v) in [
            ("$feature_flag", json!("my-flag")),
            ("$feature_flag_response", json!(true)),
            ("$feature/my-flag", json!(true)),          // stripped
            ("$feature_flag_payload", json!({"a": 1})), // stripped
            ("$feature_flag_has_experiment", json!(false)),
            ("$feature_flag_id", json!(42)),
            ("$feature_flag_version", json!(7)),
            ("$feature_flag_reason", json!("condition match")),
            ("$feature_flag_request_id", json!("req-1")),
            ("locally_evaluated", json!(false)),
            ("custom_super_property", json!("leak")), // stripped
        ] {
            event.insert_prop(k, v).unwrap();
        }
        event
    }

    #[test]
    fn minimal_flag_called_event_keeps_only_allowlisted_properties() {
        let mut event = full_flag_called_event();
        event.mark_minimal_flag_called();
        prepare_event(
            &mut event,
            &CaptureDefaults {
                disable_geoip: true,
                is_server: true,
            },
        );
        // Mirrors the no-hooks case in `build_batch_payload`, where the trim runs
        // right after `prepare_event` since `apply_before_send_hooks` is a no-op.
        event.apply_minimal_flag_called_allowlist();

        let keys: HashSet<&str> = event.properties().keys().map(String::as_str).collect();
        let allow: HashSet<&str> = MINIMAL_FLAG_CALLED_EVENT_PROPERTIES
            .iter()
            .copied()
            .collect();

        // Every surviving key is on the allowlist — the enrichment step's
        // `$lib_version__major/minor/patch`, `$feature/<key>`,
        // `$feature_flag_payload`, and the custom super property are all gone.
        assert!(
            keys.is_subset(&allow),
            "unexpected keys leaked: {:?}",
            keys.difference(&allow).collect::<Vec<_>>()
        );
        // Allowlisted evaluation + system-context props are retained.
        for expected in [
            "$feature_flag",
            "$feature_flag_response",
            "$feature_flag_has_experiment",
            "locally_evaluated",
            "$groups",
            "$geoip_disable",
            "$is_server",
            "$lib",
            "$lib_version",
            "$os",
            "$os_version",
        ] {
            assert!(
                keys.contains(expected),
                "missing allowlisted key {}",
                expected
            );
        }
        assert!(!keys.contains("$feature/my-flag"));
        assert!(!keys.contains("$feature_flag_payload"));
        assert!(!keys.contains("custom_super_property"));
        assert!(!keys.contains("$lib_version__major"));
    }

    #[test]
    fn non_minimal_flag_called_event_keeps_everything() {
        let mut event = full_flag_called_event();
        prepare_event(
            &mut event,
            &CaptureDefaults {
                disable_geoip: true,
                is_server: true,
            },
        );
        // No minimization marker -> the full shape is preserved, including the
        // super property and `$feature/<key>`.
        assert_eq!(
            event.properties().get("custom_super_property"),
            Some(&json!("leak"))
        );
        assert_eq!(
            event.properties().get("$feature/my-flag"),
            Some(&json!(true))
        );
        assert!(event.properties().contains_key("$lib_version__major"));
    }

    #[test]
    fn before_send_hook_cannot_reintroduce_properties_after_minimal_trim() {
        let mut event = Event::new("$feature_flag_called", "user-1");
        event.mark_minimal_flag_called();
        event
            .insert_prop("$feature_flag", json!("my-flag"))
            .unwrap();

        // A hook that stamps a non-allowlisted property, the way a real
        // before_send hook (env tags, app version, user metadata) would.
        let hooks = vec![BeforeSendHook::new(|mut event| {
            event.insert_prop("from_hook", json!("leak")).unwrap();
            Some(event)
        })];

        let (json_body, kept) = build_batch_payload(
            vec![event],
            "phc_test".to_string(),
            false,
            Utc::now(),
            &CaptureDefaults {
                disable_geoip: true,
                is_server: true,
            },
            &hooks,
        )
        .unwrap()
        .expect("event should not be dropped");

        assert_eq!(kept, 1);
        let parsed: serde_json::Value = serde_json::from_str(&json_body).unwrap();
        let properties = &parsed["batch"][0]["properties"];
        assert!(
            properties.get("from_hook").is_none(),
            "before_send hook property survived the minimal-event allowlist trim: {:?}",
            properties
        );
    }
}
