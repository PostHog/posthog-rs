use super::connection::Connection;
use super::ids::derive_session_id;
use super::token;

/// What the transport already uses to keep one client's requests together.
/// Credentials, cookies and client addresses are never transport sessions.
#[derive(Clone, Debug, Default)]
pub enum TransportSession {
    /// A stateless request, which groups with no other request.
    #[default]
    None,
    /// An id carried on every request, such as a legacy `Mcp-Session-Id`.
    /// An id the framework assigns afresh to every request is not one.
    Id(String),
    /// A connection that can only ever carry one client, such as stdio.
    Connection(Connection),
}

/// The client a replayed PostHog session token names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TokenClient {
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) protocol_version: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ResolvedSession {
    pub(crate) session_id: Option<String>,
    pub(crate) token_client: Option<TokenClient>,
}

impl TransportSession {
    pub(crate) fn carries_session(&self) -> bool {
        !matches!(self, TransportSession::None)
    }

    pub(crate) fn resolve_for_call(&self) -> ResolvedSession {
        match self {
            TransportSession::Connection(connection) => {
                Self::from_session_id(connection.session_for_call())
            }
            other => other.resolve(),
        }
    }

    pub(crate) fn resolve(&self) -> ResolvedSession {
        match self {
            TransportSession::None => ResolvedSession::default(),
            TransportSession::Id(id) => Self::from_id(id),
            TransportSession::Connection(connection) => {
                Self::from_session_id(connection.current_session())
            }
        }
    }

    pub(crate) fn call_ended(&self) {
        if let TransportSession::Connection(connection) = self {
            connection.call_ended();
        }
    }

    fn from_id(id: &str) -> ResolvedSession {
        let Some(token) = token::decode(id) else {
            return Self::from_session_id(derive_session_id(id));
        };
        let token_client = TokenClient {
            name: token.cn,
            version: token.cv,
            protocol_version: token.pv,
        };
        ResolvedSession {
            session_id: Some(token.sid),
            token_client: Some(token_client),
        }
    }

    fn from_session_id(session_id: String) -> ResolvedSession {
        ResolvedSession {
            session_id: Some(session_id),
            token_client: None,
        }
    }
}
