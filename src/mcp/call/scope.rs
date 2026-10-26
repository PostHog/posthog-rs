//! What a request carries that every event describing it needs, whichever
//! framework received it.

use std::time::Instant;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::mcp::capture::{ClientIdentity, ServerIdentity};
use crate::mcp::request_info::RequestInfo;
use crate::mcp::session::{TokenClient, TransportSession};

const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
const USER_AGENT_HEADER: &str = "user-agent";
const VENDOR_CLIENT_HEADER: &str = "x-anthropic-client";

pub(crate) struct RequestScope<'a> {
    pub(crate) method: &'static str,
    pub(crate) meta: Option<&'a Map<String, Value>>,
    pub(crate) http: Option<&'a http::request::Parts>,
    pub(crate) transport: &'a TransportSession,
    /// The `initialize` handshake, only when it is bound to this request's
    /// transport session. A handshake on an instance serving many clients,
    /// or one the framework synthesized, identifies no client.
    pub(crate) handshake: Option<ClientIdentity>,
    pub(crate) server: ServerIdentity,
    pub(crate) timestamp: DateTime<Utc>,
    /// When the request arrived, which a call's duration runs from.
    pub(crate) started: Instant,
}

impl<'a> RequestScope<'a> {
    pub(crate) fn request_info(
        &self,
        tool_name: Option<&'a str>,
        arguments: Option<&'a Map<String, Value>>,
    ) -> RequestInfo<'a> {
        RequestInfo {
            method: self.method,
            tool_name,
            arguments,
            meta: self.meta,
            http: self.http,
        }
    }

    /// Per-request sources win: `_meta`, then the protocol header, then a
    /// replayed session token, then a bound handshake.
    pub(crate) fn client_identity(&self, token: Option<&TokenClient>) -> ClientIdentity {
        let meta_client = self.meta_text(META_CLIENT_INFO, "name");
        let meta_version = self.meta_text(META_CLIENT_INFO, "version");
        let meta_protocol = self
            .meta
            .and_then(|meta| meta.get(META_PROTOCOL_VERSION))
            .and_then(Value::as_str)
            .map(str::to_string);
        let handshake = self.handshake.clone().unwrap_or_default();
        let token = token.cloned().unwrap_or_default();
        ClientIdentity {
            name: meta_client.or(token.name).or(handshake.name),
            version: meta_version.or(token.version).or(handshake.version),
            protocol_version: meta_protocol
                .or_else(|| self.header(PROTOCOL_VERSION_HEADER))
                .or(token.protocol_version)
                .or(handshake.protocol_version),
            user_agent: self.header(USER_AGENT_HEADER),
            vendor: self.header(VENDOR_CLIENT_HEADER),
        }
    }

    pub(crate) fn meta_field(&self, key: &str, field: &str) -> Option<&'a Value> {
        self.meta?.get(key)?.get(field)
    }

    fn meta_text(&self, key: &str, field: &str) -> Option<String> {
        let value = self.meta_field(key, field)?.as_str()?;
        (!value.is_empty()).then(|| value.to_string())
    }

    fn header(&self, name: &str) -> Option<String> {
        let value = self.http?.headers.get(name)?.to_str().ok()?.trim();
        (!value.is_empty()).then(|| value.to_string())
    }
}
