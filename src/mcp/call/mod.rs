//! A request's way through analytics, shared by the `rmcp` integration and
//! the manual API so both send the same events for the same traffic.

mod finish;
mod handle_delivery;
mod intent;
mod listing;
mod model;
mod prepare;
mod scope;
mod virtual_call;

pub(crate) use finish::{complete_call, end_call, CallEnd};
pub(crate) use listing::{emit_initialized, emit_tools_listed};
pub(crate) use prepare::{prepare_call, CallRequest, PreparedCall, ToolExistence};
pub(crate) use scope::RequestScope;
pub(crate) use virtual_call::answer_virtual_call;
