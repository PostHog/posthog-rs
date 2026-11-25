//! A one-client connection, such as stdio, carries no session id, so the SDK
//! generates one when the connection opens. A desktop app can keep one stdio
//! process alive across many separate chats, so the first tool call 30 minutes
//! or more after the previous one ended starts a new session. A call still
//! running has not ended, so the session holds while any call is in flight.

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
    calls_in_flight: usize,
}

/// A tool call on a connection, which ends when this is dropped, however the
/// call ended, cancelled included.
#[derive(Debug)]
pub(crate) struct CallOnConnection(Connection);

impl Drop for CallOnConnection {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.calls_in_flight = state.calls_in_flight.saturating_sub(1);
        state.last_call_ended = Some(Instant::now());
    }
}

impl Connection {
    pub fn new() -> Self {
        let state = ConnectionState {
            session_id: new_session_id(),
            last_call_ended: None,
            calls_in_flight: 0,
        };
        Self(Arc::new(Mutex::new(state)))
    }

    pub(crate) fn current_session(&self) -> String {
        self.state().session_id.clone()
    }

    /// The session of a call arriving now, and the call, which ends when
    /// dropped.
    pub(crate) fn begin_call(&self) -> (String, CallOnConnection) {
        self.begin_call_at(Instant::now())
    }

    fn begin_call_at(&self, now: Instant) -> (String, CallOnConnection) {
        let mut state = self.state();
        let idle = state.calls_in_flight == 0
            && state
                .last_call_ended
                .is_some_and(|ended| now.saturating_duration_since(ended) >= SESSION_INACTIVITY);
        if idle {
            state.session_id = new_session_id();
            state.last_call_ended = None;
        }
        state.calls_in_flight += 1;
        (state.session_id.clone(), CallOnConnection(self.clone()))
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
        let (first, call) = connection.begin_call();
        drop(call);
        let (second, call) = connection.begin_call();
        assert_eq!(second, first);

        drop(call);
        let later = Instant::now() + SESSION_INACTIVITY;
        assert_ne!(connection.begin_call_at(later).0, first);
    }

    #[test]
    fn a_call_in_flight_keeps_the_session_until_every_call_ends() {
        let connection = Connection::new();
        let (first, ended) = connection.begin_call_at(Instant::now());
        drop(ended);
        let (_, in_flight) = connection.begin_call_at(Instant::now());

        let later = Instant::now() + SESSION_INACTIVITY;
        let (during, overlapping) = connection.begin_call_at(later);
        assert_eq!(during, first, "the previous call has not ended");

        drop((in_flight, overlapping));
        let (after, _) = connection.begin_call_at(Instant::now() + 2 * SESSION_INACTIVITY);
        assert_ne!(after, first, "every call ended 30 minutes before");
    }

    #[test]
    fn separate_connections_get_separate_sessions() {
        assert_ne!(
            Connection::new().current_session(),
            Connection::new().current_session()
        );
    }
}
