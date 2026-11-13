//! The type and size limits of the spec's property reference, and the event
//! size bound.

mod event_size;
mod payload;
mod text;

pub(crate) use event_size::fit_event_size;
pub(crate) use payload::{bounded_entries, bounded_payload};
pub(crate) use text::{bounded_non_empty, bounded_text, truncate};
