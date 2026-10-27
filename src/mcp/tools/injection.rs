//! Advertises the enabled analytics arguments on each listed tool. `context`
//! and `llm_model` are required, because models fill required arguments, and
//! analytics strips them before the server validates the call.
//! `conversation_id` is optional: the agent has no handle on its first call.

use serde_json::{json, Map, Value};

use super::output_schema::declare_instructions;
use super::ownership::{can_add_argument, declares, ArgumentFlags};
use crate::mcp::contract::arguments::{
    CONTEXT, CONVERSATION_ID, CONVERSATION_ID_DESCRIPTION, LLM_MODEL,
};

#[derive(Clone, Debug)]
pub(crate) struct ArgumentInjection {
    pub(crate) enabled: ArgumentFlags,
    pub(crate) context_description: String,
    pub(crate) llm_model_description: String,
}

impl ArgumentInjection {
    /// A virtual tool's own report arguments take the place of `context`.
    pub(crate) fn for_virtual_tool(&self) -> Self {
        let enabled = ArgumentFlags {
            context: false,
            ..self.enabled
        };
        Self {
            enabled,
            ..self.clone()
        }
    }

    pub(crate) fn inject(&self, tool: &Value) -> Value {
        let mut injected = tool.clone();
        if let Some(descriptor) = injected.as_object_mut() {
            self.inject_into(descriptor);
        }
        injected
    }

    fn inject_into(&self, tool: &mut Map<String, Value>) {
        if self.enabled.context {
            add_argument(tool, CONTEXT, &self.context_description, true);
        }
        if self.enabled.llm_model {
            add_argument(tool, LLM_MODEL, &self.llm_model_description, true);
        }
        if self.enabled.conversation_id {
            let added = add_argument(tool, CONVERSATION_ID, CONVERSATION_ID_DESCRIPTION, false);
            if added {
                declare_instructions(tool);
            }
        }
    }
}

fn add_argument(
    tool: &mut Map<String, Value>,
    name: &str,
    description: &str,
    required: bool,
) -> bool {
    let schema = tool.get("inputSchema").and_then(Value::as_object);
    if !can_add_argument(schema, name) {
        warn_skipped(tool, schema, name);
        return false;
    }
    let empty_schema = || json!({ "type": "object", "properties": {} });
    let Some(schema) = tool
        .entry("inputSchema")
        .or_insert_with(empty_schema)
        .as_object_mut()
    else {
        return false;
    };
    let property = json!({ "type": "string", "description": description });
    let properties = schema.entry("properties").or_insert_with(|| json!({}));
    if let Some(properties) = properties.as_object_mut() {
        properties.insert(name.to_string(), property);
    }
    if required {
        add_required(schema, name);
    }
    true
}

fn add_required(schema: &mut Map<String, Value>, name: &str) {
    match schema.get_mut("required") {
        Some(Value::Array(required)) => {
            if !required.iter().any(|entry| entry == name) {
                required.push(name.into());
            }
        }
        _ => {
            schema.insert("required".to_string(), json!([name]));
        }
    }
}

fn warn_skipped(tool: &Map<String, Value>, schema: Option<&Map<String, Value>>, name: &str) {
    let tool_name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
    if declares(schema, name) {
        tracing::debug!("MCP tool \"{tool_name}\" declares its own '{name}'; keeping it as data");
    } else {
        tracing::warn!(
            "MCP tool \"{tool_name}\" has a composed input schema (oneOf/anyOf/allOf/$ref); \
             skipping '{name}' injection"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::contract::arguments::CONTEXT_DESCRIPTION;

    fn injection() -> ArgumentInjection {
        ArgumentInjection {
            enabled: ArgumentFlags {
                context: true,
                llm_model: true,
                conversation_id: true,
            },
            context_description: CONTEXT_DESCRIPTION.to_string(),
            llm_model_description: "model".to_string(),
        }
    }

    #[test]
    fn analytics_arguments_are_advertised() {
        let tool = json!({
            "name": "search_docs",
            "inputSchema": { "type": "object", "properties": { "query": {} }, "required": ["query"] },
        });
        let schema = injection().inject(&tool)["inputSchema"].clone();
        assert_eq!(schema["required"], json!(["query", "context", "llm_model"]));
        assert_eq!(schema["properties"]["context"]["type"], "string");
        assert_eq!(schema["properties"]["conversation_id"]["type"], "string");
    }

    #[test]
    fn a_declared_argument_is_left_to_the_tool() {
        let tool = json!({
            "name": "summarize",
            "inputSchema": { "type": "object", "properties": { "context": { "type": "string" } } },
        });
        let schema = injection().inject(&tool)["inputSchema"].clone();
        assert_eq!(schema["properties"]["context"], json!({ "type": "string" }));
        assert_eq!(schema["required"], json!(["llm_model"]));
    }

    #[test]
    fn the_registered_tool_is_not_modified() {
        let tool = json!({ "name": "t", "inputSchema": { "type": "object" } });
        let _ = injection().inject(&tool);
        assert_eq!(
            tool,
            json!({ "name": "t", "inputSchema": { "type": "object" } })
        );
    }
}
