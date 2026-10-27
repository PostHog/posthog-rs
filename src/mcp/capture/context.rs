//! What every MCP event carries: its session, the person it acts for, and the
//! client and server it describes.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::mcp::bounds::{bounded_entries, bounded_payload, bounded_text, truncate};
use crate::mcp::contract::limits::MAX_NAME_LENGTH;
use crate::mcp::contract::properties as key;
use crate::mcp::identity::Identity;
use crate::mcp::privacy::TextPass;
use crate::RequestContext;

const MCP_PREFIX: &str = "$mcp_";
const PERSON_CONTROLS: [&str; 5] = [
    key::PROCESS_PERSON_PROFILE,
    key::SET,
    "$set_once",
    "$unset",
    key::GROUPS,
];

/// The host's properties an MCP event may carry: none MCP or identity sets,
/// redacted and bounded as a payload, in name order.
pub(crate) fn host_properties(host: &RequestContext) -> Map<String, Value> {
    let mut kept: Vec<(&String, &Value)> = host
        .properties
        .iter()
        .filter(|(name, _)| {
            !name.starts_with(MCP_PREFIX) && !PERSON_CONTROLS.contains(&name.as_str())
        })
        .collect();
    kept.sort_unstable_by_key(|(name, _)| *name);
    bounded_entries(kept.into_iter(), TextPass::Name)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ClientIdentity {
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) protocol_version: Option<String>,
    pub(crate) user_agent: Option<String>,
    pub(crate) vendor: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ServerIdentity {
    pub(crate) name: Option<String>,
    pub(crate) version: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct EventContext {
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) session_id: String,
    pub(crate) conversation_id: Option<String>,
    pub(crate) identity: Option<Identity>,
    pub(crate) client: ClientIdentity,
    pub(crate) server: ServerIdentity,
    pub(crate) server_build: Option<String>,
    /// The host's request context, which events carry where MCP sets nothing.
    /// Built by `host_properties`, so a call holds only what capture keeps.
    pub(crate) host_properties: Map<String, Value>,
}

impl EventContext {
    pub(crate) fn distinct_id(&self) -> &str {
        match &self.identity {
            Some(identity) => &identity.distinct_id,
            None => &self.session_id,
        }
    }

    pub(crate) fn properties(&self) -> Map<String, Value> {
        let mut properties = Map::new();
        properties.insert(
            key::SESSION_ID.to_string(),
            Value::from(self.session_id.as_str()),
        );
        insert_text(
            &mut properties,
            key::CONVERSATION_ID,
            self.conversation_id.as_deref(),
        );
        self.insert_client(&mut properties);
        self.insert_server(&mut properties);
        self.insert_person(&mut properties);
        properties
    }

    fn insert_client(&self, properties: &mut Map<String, Value>) {
        let client = &self.client;
        insert_name(properties, key::CLIENT_NAME, client.name.as_deref());
        insert_name(properties, key::CLIENT_VERSION, client.version.as_deref());
        insert_name(
            properties,
            key::PROTOCOL_VERSION,
            client.protocol_version.as_deref(),
        );
        insert_header(
            properties,
            key::CLIENT_USER_AGENT,
            client.user_agent.as_deref(),
        );
        insert_header(properties, key::VENDOR_CLIENT, client.vendor.as_deref());
    }

    fn insert_server(&self, properties: &mut Map<String, Value>) {
        let server = &self.server;
        insert_name(properties, key::SERVER_NAME, server.name.as_deref());
        insert_name(properties, key::SERVER_VERSION, server.version.as_deref());
        insert_text(properties, key::SERVER_BUILD, self.server_build.as_deref());
    }

    /// Anonymous calls create no person profiles.
    fn insert_person(&self, properties: &mut Map<String, Value>) {
        let Some(identity) = &self.identity else {
            properties.insert(key::PROCESS_PERSON_PROFILE.to_string(), Value::Bool(false));
            return;
        };
        if !identity.properties.is_empty() {
            let set = Value::Object(identity.properties.clone());
            properties.insert(key::SET.to_string(), bounded_payload(&set, TextPass::Name));
        }
        if !identity.groups.is_empty() {
            let groups = identity
                .groups
                .iter()
                .map(|(group_type, group_key)| {
                    (
                        truncate(group_type, MAX_NAME_LENGTH),
                        Value::from(truncate(group_key, MAX_NAME_LENGTH)),
                    )
                })
                .collect();
            properties.insert(key::GROUPS.to_string(), Value::Object(groups));
        }
    }
}

fn insert_text(properties: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(text) = value.filter(|text| !text.is_empty()) {
        properties.insert(key.to_string(), Value::from(text));
    }
}

fn insert_name(properties: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    let bounded = value.map(|text| truncate(text, MAX_NAME_LENGTH));
    insert_text(properties, key, bounded.as_deref());
}

fn insert_header(properties: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    let bounded = value.map(|text| bounded_text(text, MAX_NAME_LENGTH, TextPass::Name));
    insert_text(properties, key, bounded.as_deref());
}
