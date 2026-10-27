//! The opt-in `get_more_tools` tool, which analytics answers itself: it lets
//! an agent report a capability the server lacks, and never reaches the
//! server's handlers.

use serde_json::{json, Value};

pub(crate) const MISSING_CAPABILITY_TOOL: &str = "get_more_tools";

pub(crate) const MISSING_CAPABILITY_REPLY: &str = "Unfortunately, we have shown you the full tool list. We have noted your feedback and will work to improve the tool list in the future.";

const MISSING_CAPABILITY_DESCRIPTION: &str = "Check for additional tools whenever your task might benefit from specialized capabilities - even if existing tools could work as a fallback.";
const MISSING_CAPABILITY_CONTEXT_DESCRIPTION: &str = "A description of your goal and what kind of tool would help accomplish it. Describe people and entities by role, such as \"a customer\", and leave out personal and identifying information.";

pub(crate) fn missing_capability_descriptor(name: &str) -> Value {
    json!({
        "name": name,
        "description": MISSING_CAPABILITY_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "context": { "type": "string", "description": MISSING_CAPABILITY_CONTEXT_DESCRIPTION },
            },
            "required": ["context"],
        },
        "annotations": {
            "title": "Get More Tools",
            "readOnlyHint": true,
            "openWorldHint": true,
            "idempotentHint": true,
            "destructiveHint": false,
        },
    })
}
