//! `$mcp_response`: media never leaves the server, and the rest is redacted
//! and bounded like any payload.

use serde_json::{json, Value};

use crate::mcp::bounds::bounded_payload;
use crate::mcp::privacy::TextPass;

const NOT_SUPPORTED: &str = "not supported by PostHog MCP analytics";

pub(crate) fn captured_response(result: &Value) -> Value {
    let mut response = result.clone();
    if let Some(Value::Array(content)) = response.get_mut("content") {
        for block in content.iter_mut() {
            if let Some(placeholder) = placeholder_for(block) {
                *block = placeholder;
            }
        }
    }
    bounded_payload(&response, TextPass::Captured)
}

fn placeholder_for(block: &Value) -> Option<Value> {
    let block_type = block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let has_blob = block.pointer("/resource/blob").is_some();
    let description = match block_type {
        "text" | "resource_link" => return None,
        "resource" if !has_blob => return None,
        "image" => "image content".to_string(),
        "audio" => "audio content".to_string(),
        "resource" => "binary resource content".to_string(),
        other => format!("unsupported content type {other:?}"),
    };
    let text = format!("[{description} redacted - {NOT_SUPPORTED}]");
    Some(json!({ "type": "text", "text": text }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_is_replaced_and_text_kept() {
        let result = json!({ "content": [
            { "type": "text", "text": "done" },
            { "type": "image", "data": "iVBORw0KGgo", "mimeType": "image/png" },
            { "type": "resource", "resource": { "uri": "file:///a", "blob": "AAAA" } },
        ] });
        let captured = captured_response(&result);
        assert_eq!(captured["content"][0]["text"], "done");
        assert!(!captured.to_string().contains("iVBORw0KGgo"));
        assert!(!captured.to_string().contains("AAAA"));
    }
}
