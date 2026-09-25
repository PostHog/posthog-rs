//! Canonical "magic" property keys and their wire option-key counterparts.
//!
//! Capture v1 reads per-event controls from the event's `options` map, not
//! from `properties`. The legacy `$`-prefixed properties below still work: the
//! SDK moves each one into `options` when the caller has not set that option.
//! Crate-internal only.

/// Controls person-profile processing. Written by [`crate::Event::new_anon`]
/// (false) and [`crate::Event::add_group`] (true), and moved into the wire
/// options.
pub(crate) const PROCESS_PERSON_PROFILE_PROP: &str = "$process_person_profile";

// Property keys moved out of `Event.properties`.
pub(crate) const COOKIELESS_MODE_PROP: &str = "$cookieless_mode";
pub(crate) const IGNORE_SENT_AT_PROP: &str = "$ignore_sent_at";
pub(crate) const PRODUCT_TOUR_ID_PROP: &str = "$product_tour_id";
pub(crate) const SESSION_ID_PROP: &str = "$session_id";
pub(crate) const WINDOW_ID_PROP: &str = "$window_id";

// Wire option-object keys.
pub(crate) const COOKIELESS_MODE_OPT: &str = "cookieless_mode";
pub(crate) const DISABLE_SKEW_CORRECTION_OPT: &str = "disable_skew_correction";
pub(crate) const PRODUCT_TOUR_ID_OPT: &str = "product_tour_id";
pub(crate) const PROCESS_PERSON_PROFILE_OPT: &str = "process_person_profile";

/// (legacy property key, wire option key) pairs. Only these legacy keys fall
/// back into `options`; newer options have no `$` property.
pub(crate) const LEGACY_OPTION_PROPERTIES: &[(&str, &str)] = &[
    (COOKIELESS_MODE_PROP, COOKIELESS_MODE_OPT),
    (IGNORE_SENT_AT_PROP, DISABLE_SKEW_CORRECTION_OPT),
    (PRODUCT_TOUR_ID_PROP, PRODUCT_TOUR_ID_OPT),
    (PROCESS_PERSON_PROFILE_PROP, PROCESS_PERSON_PROFILE_OPT),
];
