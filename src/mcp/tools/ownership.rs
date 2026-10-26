//! A tool that declares `context`, `llm_model` or `conversation_id` itself owns
//! it, and the argument is the tool's data. Analytics owns only the arguments
//! it added, and strips only those.

use serde_json::{Map, Value};

use crate::mcp::contract::arguments::{CONTEXT, CONVERSATION_ID, LLM_MODEL};

const COMPOSITION_KEYWORDS: [&str; 4] = ["$ref", "oneOf", "allOf", "anyOf"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArgumentFlags {
    pub(crate) context: bool,
    pub(crate) llm_model: bool,
    pub(crate) conversation_id: bool,
}

impl ArgumentFlags {
    pub(crate) fn names(self) -> impl Iterator<Item = &'static str> {
        IntoIterator::into_iter([
            (self.context, CONTEXT),
            (self.llm_model, LLM_MODEL),
            (self.conversation_id, CONVERSATION_ID),
        ])
        .filter(|(owned, _)| *owned)
        .map(|(_, name)| name)
    }
}

/// The enabled arguments analytics can add to a tool with this input schema.
pub(crate) fn injectable_arguments(
    input_schema: Option<&Value>,
    enabled: ArgumentFlags,
) -> ArgumentFlags {
    let schema = input_schema.and_then(Value::as_object);
    ArgumentFlags {
        context: enabled.context && can_add_argument(schema, CONTEXT),
        llm_model: enabled.llm_model && can_add_argument(schema, LLM_MODEL),
        conversation_id: enabled.conversation_id && can_add_argument(schema, CONVERSATION_ID),
    }
}

pub(crate) fn can_add_argument(schema: Option<&Map<String, Value>>, name: &str) -> bool {
    !declares(schema, name) && !schema.is_some_and(is_composed)
}

pub(crate) fn declares(schema: Option<&Map<String, Value>>, name: &str) -> bool {
    let properties = schema
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object);
    properties.is_some_and(|properties| properties.contains_key(name))
}

pub(crate) fn is_composed(schema: &Map<String, Value>) -> bool {
    COMPOSITION_KEYWORDS
        .iter()
        .any(|keyword| is_truthy(schema.get(*keyword)))
}

fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(_) => true,
    }
}

pub(crate) fn strip_owned_arguments(arguments: &mut Map<String, Value>, owned: ArgumentFlags) {
    for name in owned.names() {
        arguments.remove(name);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ALL: ArgumentFlags = ArgumentFlags {
        context: true,
        llm_model: true,
        conversation_id: true,
    };

    #[test]
    fn tools_that_declare_an_argument_own_it() {
        let schema = json!({ "type": "object", "properties": { "context": {} } });
        let injectable = injectable_arguments(Some(&schema), ALL);
        assert!(!injectable.context);
        assert!(injectable.llm_model);
    }

    #[test]
    fn composed_schemas_are_left_alone() {
        let schema = json!({ "oneOf": [{ "type": "object" }] });
        assert_eq!(
            injectable_arguments(Some(&schema), ALL),
            ArgumentFlags::default()
        );
    }

    #[test]
    fn disabled_arguments_are_never_owned() {
        let enabled = ArgumentFlags {
            context: true,
            ..ArgumentFlags::default()
        };
        let injectable = injectable_arguments(None, enabled);
        assert_eq!(injectable.names().collect::<Vec<_>>(), vec![CONTEXT]);
    }
}
