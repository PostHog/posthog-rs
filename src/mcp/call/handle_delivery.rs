//! Hands the conversation handle to the agent: as a text block on the call
//! that minted it, and mirrored into `structuredContent` for tools whose
//! output schema declares `_mcp_instructions`.

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{json, Value};

use crate::mcp::contract::arguments::{CONVERSATION_ID, INSTRUCTIONS_KEY};
use crate::mcp::session::Conversation;

/// Whether the agent receives the handle in `result`.
pub(crate) fn deliver_handle(
    result: &mut CallToolResult,
    conversation: &Conversation,
    mirror: bool,
) -> bool {
    let Some(handle) = conversation.id.as_deref() else {
        return false;
    };
    let mirrored = mirror && mirror_into_structured_content(result, handle);
    if conversation.minted {
        let block = json!({ CONVERSATION_ID: handle }).to_string();
        result.content.push(ContentBlock::text(block));
        return true;
    }
    mirrored
}

fn mirror_into_structured_content(result: &mut CallToolResult, handle: &str) -> bool {
    let Some(Value::Object(structured)) = result.structured_content.as_mut() else {
        return false;
    };
    if structured.contains_key(INSTRUCTIONS_KEY) {
        return false;
    }
    let instructions = json!({ CONVERSATION_ID: handle });
    structured.insert(INSTRUCTIONS_KEY.to_string(), instructions);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_handle_rides_a_text_block() {
        let mut result = CallToolResult::success(vec![ContentBlock::text("ok")]);
        let conversation = Conversation {
            id: Some("abc".to_string()),
            minted: true,
        };
        assert!(deliver_handle(&mut result, &conversation, false));
        let last = serde_json::to_value(result.content.last()).expect("serializes");
        assert_eq!(last["text"], "{\"conversation_id\":\"abc\"}");
    }

    #[test]
    fn an_echoed_handle_is_only_mirrored() {
        let mut result = CallToolResult::structured(json!({ "count": 1 }));
        let blocks_before = result.content.len();
        let conversation = Conversation {
            id: Some("abc".to_string()),
            minted: false,
        };
        assert!(deliver_handle(&mut result, &conversation, true));
        assert_eq!(result.content.len(), blocks_before);
        let structured = result.structured_content.expect("structured");
        assert_eq!(structured[INSTRUCTIONS_KEY][CONVERSATION_ID], "abc");
    }
}
