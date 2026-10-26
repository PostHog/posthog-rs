//! Declares `_mcp_instructions` on output schemas, so the conversation handle
//! can be mirrored into `structuredContent` without failing validation.

use serde_json::{json, Map, Value};

use super::ownership::is_composed;
use crate::mcp::contract::arguments;

pub(crate) fn can_declare_instructions(output_schema: Option<&Value>) -> bool {
    let Some(Value::Object(schema)) = output_schema else {
        return false;
    };
    if is_composed(schema) {
        return false;
    }
    match schema.get("properties") {
        None => true,
        Some(Value::Object(properties)) => !properties.contains_key(arguments::INSTRUCTIONS_KEY),
        Some(_) => false,
    }
}

pub(crate) fn declare_instructions(tool: &mut Map<String, Value>) {
    let Some(output_schema) = tool.get("outputSchema").filter(|schema| schema.is_object()) else {
        return;
    };
    if !can_declare_instructions(Some(output_schema)) {
        let tool_name = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        tracing::warn!(
            "Tool \"{tool_name}\" cannot declare {}; its handle stays in content",
            arguments::INSTRUCTIONS_KEY
        );
        return;
    }
    let Some(schema) = tool.get_mut("outputSchema").and_then(Value::as_object_mut) else {
        return;
    };
    let properties = schema.entry("properties").or_insert_with(|| json!({}));
    if let Some(properties) = properties.as_object_mut() {
        properties.insert(
            arguments::INSTRUCTIONS_KEY.to_string(),
            instructions_schema(),
        );
    }
}

fn instructions_schema() -> Value {
    json!({
        "type": "object",
        "description": arguments::INSTRUCTIONS_DESCRIPTION,
        "properties": {
            "conversation_id": {
                "type": "string",
                "description": arguments::INSTRUCTIONS_CONVERSATION_ID_DESCRIPTION,
            },
        },
    })
}
