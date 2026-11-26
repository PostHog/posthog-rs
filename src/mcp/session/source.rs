//! A request's session and what produced it, decided by the spec's ladder:
//! the host's resolver, else the client, else the agent's handle, else the
//! transport, else a session of the request's own.

use super::client::ClientConversation;
use super::ids::{derive_session_id, new_session_id};

/// What produced a `$session_id`, sent as `$mcp_session_source`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionSource {
    Host,
    ClientMetadata,
    Conversation,
    Transport,
    Generated,
}

impl SessionSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SessionSource::Host => "host",
            SessionSource::ClientMetadata => "client_metadata",
            SessionSource::Conversation => "conversation",
            SessionSource::Transport => "transport",
            SessionSource::Generated => "generated",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Session {
    pub(crate) id: String,
    pub(crate) source: SessionSource,
}

impl Session {
    pub(crate) fn new(id: String, source: SessionSource) -> Self {
        Self { id, source }
    }

    pub(crate) fn generated() -> Self {
        Self::new(new_session_id(), SessionSource::Generated)
    }
}

/// Everything below the host's resolver that can name a request's session.
#[derive(Clone, Debug, Default)]
pub(crate) struct SessionClaims {
    pub(crate) client: Option<ClientConversation>,
    pub(crate) handle: Option<String>,
    pub(crate) transport: Option<Session>,
}

impl SessionClaims {
    /// The first claim wins; `host` is the resolver's answer, if it gave one.
    pub(crate) fn decide(self, host: Option<String>) -> Session {
        let Self {
            client,
            handle,
            transport,
        } = self;
        let derived = |id: &str, source| Session::new(derive_session_id(id), source);
        host.map(|id| derived(&id, SessionSource::Host))
            .or_else(|| client.map(|client| client.session()))
            .or_else(|| handle.map(|handle| derived(&handle, SessionSource::Conversation)))
            .or(transport)
            .unwrap_or_else(Session::generated)
    }
}
