//! Building PostHog events from what a request carried, and sending them.

mod context;
mod delivery;
mod failure;
mod outgoing;
mod response;

pub(crate) use context::{host_properties, ClientIdentity, EventContext, ServerIdentity};
pub(crate) use delivery::Delivery;
pub(crate) use failure::Failure;
pub(crate) use outgoing::OutgoingEvent;
pub(crate) use response::captured_response;
