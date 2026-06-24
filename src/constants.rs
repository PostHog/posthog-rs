//! Canonical "magic" property keys and their V1 wire option-key counterparts.
//!
//! Single source of truth for the property-to-options mapping the SDK derives
//! internally (mirrors the PostHog backend reverse-extraction in
//! `capture_v1.py` / `analytics/types.rs`). Crate-internal only.

/// Controls person-profile processing. Written by [`crate::Event::new_anon`]
/// (false) and [`crate::Event::add_group`] (true), and lifted into the V1 wire
/// options. Used on the v0 path too, so it is always compiled.
pub(crate) const PROCESS_PERSON_PROFILE_PROP: &str = "$process_person_profile";

#[cfg(feature = "capture-v1")]
pub(crate) use v1::*;

#[cfg(feature = "capture-v1")]
mod v1 {
    use super::PROCESS_PERSON_PROFILE_PROP;

    // Property keys lifted out of `Event.properties`.
    pub(crate) const COOKIELESS_MODE_PROP: &str = "$cookieless_mode";
    pub(crate) const IGNORE_SENT_AT_PROP: &str = "$ignore_sent_at";
    pub(crate) const PRODUCT_TOUR_ID_PROP: &str = "$product_tour_id";
    pub(crate) const SESSION_ID_PROP: &str = "$session_id";
    pub(crate) const WINDOW_ID_PROP: &str = "$window_id";

    // V1 wire option-object keys.
    pub(crate) const COOKIELESS_MODE_OPT: &str = "cookieless_mode";
    pub(crate) const DISABLE_SKEW_CORRECTION_OPT: &str = "disable_skew_correction";
    pub(crate) const PRODUCT_TOUR_ID_OPT: &str = "product_tour_id";
    pub(crate) const PROCESS_PERSON_PROFILE_OPT: &str = "process_person_profile";

    /// (property key, wire option key) pairs lifted into the V1 `options` map.
    pub(crate) const OPTIONS_EXTRACTION_TABLE: &[(&str, &str)] = &[
        (COOKIELESS_MODE_PROP, COOKIELESS_MODE_OPT),
        (IGNORE_SENT_AT_PROP, DISABLE_SKEW_CORRECTION_OPT),
        (PRODUCT_TOUR_ID_PROP, PRODUCT_TOUR_ID_OPT),
        (PROCESS_PERSON_PROFILE_PROP, PROCESS_PERSON_PROFILE_OPT),
    ];
}
