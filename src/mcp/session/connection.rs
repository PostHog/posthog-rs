//! A one-client connection, such as stdio, carries no session id, so the SDK
//! generates one when the connection opens. A desktop app can keep one stdio
//! process alive across many separate chats, so the first tool call 30 minutes
//! or more after the previous one ended starts a new session.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use super::ids::new_session_id;
use crate::mcp::contract::limits::SESSION_INACTIVITY;

/// The session of one connection that only ever carries one client. Clones
/// share it; create one per connection.
#[derive(Clone, Debug)]
pub struct Connection(Arc<Mutex<ConnectionState>>);

#[derive(Debug)]
struct ConnectionState {
    session_id: String,
    last_call_ended: Option<Instant>,
}

impl Connection {
    pub fn new() -> Self {
        let state = ConnectionState {
            session_id: new_session_id(),
            last_call_ended: None,
        };
        Self(Arc::new(Mutex::new(state)))
    }

    pub(crate) fn current_session(&self) -> String {
        self.state().session_id.clone()
    }

    pub(crate) fn session_for_call(&self) -> String {
        self.session_for_call_at(Instant::now())
    }

    pub(crate) fn call_ended(&self) {
        self.state().last_call_ended = Some(Instant::now());
    }

    fn session_for_call_at(&self, now: Instant) -> String {
        let mut state = self.state();
        let idle = state
            .last_call_ended
            .is_some_and(|ended| now.saturating_duration_since(ended) >= SESSION_INACTIVITY);
        if idle {
            state.session_id = new_session_id();
            state.last_call_ended = None;
        }
        state.session_id.clone()
    }

    fn state(&self) -> MutexGuard<'_, ConnectionState> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for Connection {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_share_the_session_until_the_connection_idles() {
        let connection = Connection::new();
        let first = connection.session_for_call();
        connection.call_ended();
        assert_eq!(connection.session_for_call(), first);

        connection.call_ended();
        let later = Instant::now() + SESSION_INACTIVITY;
        assert_ne!(connection.session_for_call_at(later), first);
    }

    #[test]
    fn separate_connections_get_separate_sessions() {
        assert_ne!(
            Connection::new().current_session(),
            Connection::new().current_session()
        );
    }
}
