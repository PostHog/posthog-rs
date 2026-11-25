//! Client conversation ids: a value an MCP client puts in a `tools/call`
//! request's `_meta` to name its conversation. Each row of the spec's table
//! is read only from a request its recognition rule matches, so a generic key
//! such as Codex's `sessionId` is never read from a client that means
//! something else by it.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use super::ids::derive_session_id;
use super::source::{Session, SessionSource};

static CHATGPT_USER_AGENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^openai-mcp/[^\s(]*\s*\(\s*chatgpt\s*[;,)]").expect("chatgpt pattern")
});
static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
        .expect("uuid pattern")
});
const NIL_UUID: &str = "00000000-0000-0000-0000-000000000000";
const VSCODE: [&str; 2] = ["Visual Studio Code", "Visual Studio Code - Insiders"];
const VSCODE_USER_AGENTS: [&str; 2] = ["Visual Studio Code/", "Visual Studio Code - Insiders/"];

/// What recognizes a request's client.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ClientSignals<'a> {
    pub(crate) user_agent: Option<&'a str>,
    /// `$mcp_client_name` as resolved for the request.
    pub(crate) client_name: Option<&'a str>,
}

#[derive(Debug)]
pub(crate) struct ClientRow {
    key: &'static str,
    value: ValueShape,
    recognizes: fn(ClientSignals<'_>) -> bool,
    /// The client sends its id on every call, so it is advertised no
    /// `conversation_id` and minted no handle.
    pub(crate) handle_free: bool,
}

#[derive(Debug)]
enum ValueShape {
    PrintableAscii,
    Uuid,
}

/// Checked in order; the first row that recognizes a request is its client's.
static ROWS: [ClientRow; 3] = [
    ClientRow {
        key: "openai/session",
        value: ValueShape::PrintableAscii,
        recognizes: |client| {
            client
                .user_agent
                .is_some_and(|ua| CHATGPT_USER_AGENT.is_match(ua))
        },
        handle_free: true,
    },
    ClientRow {
        key: "sessionId",
        value: ValueShape::Uuid,
        recognizes: |client| {
            user_agent_starts_with(client, &["codex-mcp-client/"])
                || client_name_is(client, &["codex-mcp-client"])
        },
        handle_free: false,
    },
    ClientRow {
        key: "vscode.conversationId",
        value: ValueShape::Uuid,
        recognizes: |client| {
            user_agent_starts_with(client, &VSCODE_USER_AGENTS) || client_name_is(client, &VSCODE)
        },
        handle_free: false,
    },
];

/// A client conversation id, which only ever reaches PostHog hashed into
/// `$session_id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClientConversation {
    key: &'static str,
    id: String,
}

impl ClientConversation {
    /// Keyed, so two clients' ids, or a client id and a handle, never share
    /// a hash input.
    pub(crate) fn session(&self) -> Session {
        let derived = derive_session_id(&format!("{}:{}", self.key, self.id));
        Session::new(derived, SessionSource::ClientMetadata)
    }
}

pub(crate) fn recognize(client: ClientSignals<'_>) -> Option<&'static ClientRow> {
    ROWS.iter().find(|row| (row.recognizes)(client))
}

impl ClientRow {
    pub(crate) fn conversation(
        &self,
        meta: Option<&Map<String, Value>>,
    ) -> Option<ClientConversation> {
        let value = meta?.get(self.key)?.as_str()?;
        let id = match self.value {
            ValueShape::PrintableAscii => printable_ascii(value)?,
            ValueShape::Uuid => uuid(value)?,
        };
        Some(ClientConversation { key: self.key, id })
    }
}

fn printable_ascii(value: &str) -> Option<String> {
    let valid =
        (1..=128).contains(&value.len()) && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte));
    valid.then(|| value.to_string())
}

fn uuid(value: &str) -> Option<String> {
    let lowered = value.to_ascii_lowercase();
    (UUID.is_match(&lowered) && lowered != NIL_UUID).then_some(lowered)
}

fn user_agent_starts_with(client: ClientSignals<'_>, prefixes: &[&str]) -> bool {
    client.user_agent.is_some_and(|ua| {
        prefixes.iter().any(|prefix| {
            ua.get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
        })
    })
}

fn client_name_is(client: ClientSignals<'_>, names: &[&str]) -> bool {
    client
        .client_name
        .is_some_and(|name| names.iter().any(|known| known.eq_ignore_ascii_case(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clients_are_recognized_by_the_spec_rules() {
        let cases = [
            (
                Some("openai-mcp/1.0.0 (ChatGPT)"),
                None,
                Some("openai/session"),
            ),
            (
                Some("openai-mcp/1.0.0 ( chatgpt; web)"),
                None,
                Some("openai/session"),
            ),
            (
                Some("OPENAI-MCP/2 (CHATGPT,x)"),
                None,
                Some("openai/session"),
            ),
            (Some("openai-mcp/1.0.0 (Codex)"), None, None),
            (Some("openai-mcp/1.0.0 (ChatGPT-Agent)"), None, None),
            (Some("Mozilla/5.0 openai-mcp/1.0.0 (ChatGPT)"), None, None),
            (None, Some("ChatGPT"), None),
            (Some("Codex-MCP-Client/0.50.0"), None, Some("sessionId")),
            (None, Some("codex-mcp-client"), Some("sessionId")),
            (
                Some("Visual Studio Code/1.99.0"),
                None,
                Some("vscode.conversationId"),
            ),
            (
                Some("visual studio code - insiders/1.100"),
                None,
                Some("vscode.conversationId"),
            ),
            (
                None,
                Some("Visual Studio Code - Insiders"),
                Some("vscode.conversationId"),
            ),
            (Some("Visual Studio Codex/1"), Some("example"), None),
        ];
        for (user_agent, client_name, key) in cases {
            let signals = ClientSignals {
                user_agent,
                client_name,
            };
            let recognized = recognize(signals).map(|row| row.key);
            assert_eq!(recognized, key, "{:?} {:?}", user_agent, client_name);
        }
    }

    #[test]
    fn values_must_match_their_rows_shape() {
        let chatgpt = &ROWS[0];
        let codex = &ROWS[1];
        let cases = [
            (chatgpt, Value::from(" "), Some(" ")),
            (chatgpt, Value::from("a".repeat(129)), None),
            (chatgpt, Value::from("conv\n1"), None),
            (chatgpt, Value::from(7), None),
            (codex, Value::from("019a4f2e1c3d7b8a9e0f1a2b3c4d5e6f"), None),
        ];
        for (row, value, expected) in cases {
            let meta: Map<String, Value> =
                std::iter::once((row.key.to_string(), value.clone())).collect();
            let read = row.conversation(Some(&meta)).map(|found| found.id);
            assert_eq!(read.as_deref(), expected, "{} {}", row.key, value);
        }
    }
}
