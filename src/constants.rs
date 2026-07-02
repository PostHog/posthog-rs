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

    /// The type the backend's `Options` struct expects for a lifted key. Drives
    /// the coercion in `V1Event::from_event_at`: a caller value that can't be
    /// coerced to this type is dropped rather than shipped (a mistyped option
    /// otherwise 400s the whole batch, since the backend `Options` is strict
    /// serde — see `rust/capture/src/v1/analytics/types.rs`).
    #[derive(Clone, Copy)]
    pub(crate) enum OptionKind {
        Bool,
        Str,
    }

    /// (property key, wire option key, expected type) tuples lifted into the V1
    /// `options` map. The type tag selects the coercion applied before the value
    /// is placed on the wire.
    pub(crate) const OPTIONS_EXTRACTION_TABLE: &[(&str, &str, OptionKind)] = &[
        (COOKIELESS_MODE_PROP, COOKIELESS_MODE_OPT, OptionKind::Bool),
        (
            IGNORE_SENT_AT_PROP,
            DISABLE_SKEW_CORRECTION_OPT,
            OptionKind::Bool,
        ),
        (PRODUCT_TOUR_ID_PROP, PRODUCT_TOUR_ID_OPT, OptionKind::Str),
        (
            PROCESS_PERSON_PROFILE_PROP,
            PROCESS_PERSON_PROFILE_OPT,
            OptionKind::Bool,
        ),
    ];
}
