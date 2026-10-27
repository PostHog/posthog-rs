//! Why the agent made the call: a standardized intent in the request's
//! `_meta` first, then the `context` argument, then the host's fallback.

use serde_json::{Map, Value};

use super::scope::RequestScope;
use crate::mcp::bounds::bounded_non_empty;
use crate::mcp::contract::arguments::CONTEXT;
use crate::mcp::contract::limits::MAX_TEXT_LENGTH;
use crate::mcp::privacy::TextPass;

const META_AI_INVOCATION: &str = "io.modelcontextprotocol/aiInvocation";
const META_USER_INTENT: &str = "userIntent";
const EMPTY_JSON_OBJECT: &str = "{}";

pub(crate) const FROM_CLIENT_METADATA: &str = "client_metadata";
pub(crate) const FROM_CONTEXT_ARGUMENT: &str = "context_parameter";
pub(crate) const INFERRED: &str = "inferred";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Intent {
    pub(crate) text: String,
    pub(crate) source: &'static str,
}

impl Intent {
    /// `None` for a blank intent, or the `"{}"` clients send for an empty one.
    pub(crate) fn new(text: &str, source: &'static str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() || text == EMPTY_JSON_OBJECT {
            return None;
        }
        let text = bounded_non_empty(text, MAX_TEXT_LENGTH, TextPass::FreeText)?;
        Some(Self { text, source })
    }
}

pub(crate) fn from_client_metadata(scope: &RequestScope<'_>) -> Option<Intent> {
    let text = scope
        .meta_field(META_AI_INVOCATION, META_USER_INTENT)?
        .as_str()?;
    Intent::new(text, FROM_CLIENT_METADATA)
}

/// A value that is not a string is no intent.
pub(crate) fn from_context_argument(arguments: Option<&Map<String, Value>>) -> Option<Intent> {
    let text = arguments?.get(CONTEXT)?.as_str()?;
    Intent::new(text, FROM_CONTEXT_ARGUMENT)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn empty_intents_are_omitted() {
        assert_eq!(Intent::new("  ", FROM_CONTEXT_ARGUMENT), None);
        assert_eq!(Intent::new("{}", FROM_CONTEXT_ARGUMENT), None);
    }

    #[test]
    fn personal_data_is_redacted_from_the_intent() {
        let arguments = json!({ "context": "find orders for alice@example.com" });
        let intent = from_context_argument(arguments.as_object()).expect("intent");
        assert_eq!(intent.text, "find orders for [redacted]");
        assert_eq!(intent.source, FROM_CONTEXT_ARGUMENT);
    }

    #[test]
    fn an_intent_redacted_away_entirely_is_omitted() {
        let text = format!("see https://x.io/?token={}", "t".repeat(4_000));
        assert_eq!(Intent::new(&text, FROM_CONTEXT_ARGUMENT), None);
    }

    #[test]
    fn a_non_string_context_is_no_intent() {
        let arguments = json!({ "context": { "why": "x" } });
        assert_eq!(from_context_argument(arguments.as_object()), None);
    }
}
