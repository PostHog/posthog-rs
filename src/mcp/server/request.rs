//! What analytics reads from an `rmcp` request context. It is copied out, so
//! the context itself can move on to the wrapped server.

use std::time::Instant;

use chrono::Utc;
use rmcp::model::ProtocolVersion;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::ServerHandler;
use serde_json::{Map, Value};

use super::Instrumented;
use crate::mcp::call::RequestScope;
use crate::mcp::capture::{ClientIdentity, ServerIdentity};
use crate::mcp::session::{Connection, TransportSession};

const SESSION_ID_HEADER: &str = "mcp-session-id";

pub(super) struct RequestFacts {
    meta: Option<Map<String, Value>>,
    http: Option<http::request::Parts>,
    transport: TransportSession,
    handshake: Option<ClientIdentity>,
    server: ServerIdentity,
    timestamp: chrono::DateTime<Utc>,
    started: Instant,
    pub(super) modern_protocol: bool,
}

impl RequestFacts {
    pub(super) fn scope(&self, method: &'static str) -> RequestScope<'_> {
        RequestScope {
            method,
            meta: self.meta.as_ref(),
            http: self.http.as_ref(),
            transport: &self.transport,
            handshake: self.handshake.clone(),
            server: self.server.clone(),
            timestamp: self.timestamp,
            started: self.started,
        }
    }

    pub(super) fn with_handshake(mut self, handshake: ClientIdentity) -> Self {
        self.handshake = Some(handshake);
        self
    }
}

impl<H: ServerHandler> Instrumented<H> {
    pub(super) fn request_facts(&self, context: &RequestContext<RoleServer>) -> RequestFacts {
        let http = context.extensions.get::<http::request::Parts>().cloned();
        let transport = transport_session(http.as_ref(), &self.instance.connection);
        let bound = http.is_none() || self.instance.served_handshake();
        let handshake = bound.then(|| handshake(context)).flatten();
        let server_info = self.inner.get_info().server_info;
        RequestFacts {
            meta: meta_object(context),
            http,
            transport,
            handshake,
            server: ServerIdentity {
                name: Some(server_info.name),
                version: Some(server_info.version),
            },
            timestamp: Utc::now(),
            started: Instant::now(),
            modern_protocol: is_modern(context),
        }
    }
}

/// A request without HTTP rides a one-client connection. Over HTTP, only a
/// replayed `Mcp-Session-Id` groups requests.
fn transport_session(
    http: Option<&http::request::Parts>,
    connection: &Connection,
) -> TransportSession {
    let Some(parts) = http else {
        return TransportSession::Connection(connection.clone());
    };
    let session_id = parts
        .headers
        .get(SESSION_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match session_id {
        Some(id) => TransportSession::Id(id.to_string()),
        None => TransportSession::None,
    }
}

/// The handshake identifies this request's client only on a connection or
/// session it belongs to. A stateless HTTP request's handshake is one `rmcp`
/// synthesized, so it identifies nobody.
fn handshake(context: &RequestContext<RoleServer>) -> Option<ClientIdentity> {
    let peer = context.peer.peer_info()?;
    Some(ClientIdentity {
        name: Some(peer.client_info.name.clone()).filter(|name| !name.is_empty()),
        version: Some(peer.client_info.version.clone()).filter(|version| !version.is_empty()),
        protocol_version: Some(peer.protocol_version.to_string()),
        user_agent: None,
        vendor: None,
    })
}

fn meta_object(context: &RequestContext<RoleServer>) -> Option<Map<String, Value>> {
    match serde_json::to_value(&context.meta) {
        Ok(Value::Object(meta)) if !meta.is_empty() => Some(meta),
        _ => None,
    }
}

fn is_modern(context: &RequestContext<RoleServer>) -> bool {
    let modern = ProtocolVersion::V_2026_07_28;
    context
        .protocol_version()
        .is_some_and(|version| version.as_str() >= modern.as_str())
}
