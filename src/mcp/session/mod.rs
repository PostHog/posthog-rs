//! Which `$session_id` an event belongs to: the agent's conversation handle,
//! else the transport's session, else a fresh one.

mod connection;
mod conversation;
mod ids;
mod token;
mod transport;

pub use connection::Connection;
pub(crate) use conversation::Conversation;
pub(crate) use ids::{derive_session_id, new_session_id};
pub use transport::TransportSession;
pub(crate) use transport::{ResolvedSession, TokenClient};
