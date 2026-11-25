//! Which `$session_id` an event belongs to: the host's resolver, else the
//! client's conversation id, else the agent's conversation handle, else the
//! transport's session, else a fresh one.

mod client;
mod connection;
mod conversation;
mod ids;
mod source;
mod token;
mod transport;

pub(crate) use client::{recognize, ClientRow, ClientSignals};
pub(crate) use connection::CallOnConnection;
pub use connection::Connection;
pub(crate) use conversation::Conversation;
pub(crate) use source::{Session, SessionClaims, SessionSource};
pub(crate) use transport::TokenClient;
pub use transport::TransportSession;
