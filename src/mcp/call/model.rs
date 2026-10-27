//! Which model made the call: the client's own request metadata first, the
//! agent's self-reported `llm_model` argument second.

use serde_json::{Map, Value};

use super::scope::RequestScope;
use crate::mcp::bounds::bounded_non_empty;
use crate::mcp::contract::arguments::LLM_MODEL;
use crate::mcp::contract::limits::MAX_NAME_LENGTH;
use crate::mcp::privacy::TextPass;

const CLIENT_MODEL_METADATA: [&str; 2] = [
    "io.modelcontextprotocol/aiInvocation",
    "x-codex-turn-metadata",
];
const MODEL_FIELD: &str = "model";
const UNKNOWN_MODEL: &str = "unknown";

pub(crate) const FROM_CLIENT_METADATA: &str = "client_metadata";
pub(crate) const SELF_REPORTED: &str = "self_reported";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Model {
    pub(crate) name: String,
    pub(crate) source: &'static str,
}

pub(crate) fn resolve(
    scope: &RequestScope<'_>,
    arguments: Option<&Map<String, Value>>,
    can_read_argument: bool,
) -> Option<Model> {
    let from_client = CLIENT_MODEL_METADATA
        .iter()
        .find_map(|key| known_model(scope.meta_field(key, MODEL_FIELD)?));
    if let Some(name) = from_client {
        return Some(Model {
            name,
            source: FROM_CLIENT_METADATA,
        });
    }
    if !can_read_argument {
        return None;
    }
    let name = known_model(arguments?.get(LLM_MODEL)?)?;
    Some(Model {
        name,
        source: SELF_REPORTED,
    })
}

/// A blank or `"unknown"` model names no model.
fn known_model(value: &Value) -> Option<String> {
    let model = value.as_str()?.trim();
    let unknown = model.is_empty() || model.eq_ignore_ascii_case(UNKNOWN_MODEL);
    if unknown {
        return None;
    }
    bounded_non_empty(model, MAX_NAME_LENGTH, TextPass::Captured)
}
