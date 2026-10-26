//! The opt-in tools analytics answers itself, which never reach the server's
//! handlers: `get_more_tools`, where an agent reports a capability the server
//! lacks.

use serde_json::{json, Value};

/// A tool analytics advertises and answers on the server's behalf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VirtualTool {
    MissingCapability,
}

impl VirtualTool {
    pub(crate) const ALL: [VirtualTool; 1] = [VirtualTool::MissingCapability];

    pub(crate) fn default_name(self) -> &'static str {
        match self {
            VirtualTool::MissingCapability => "get_more_tools",
        }
    }

    pub(crate) fn descriptor(self, name: &str) -> Value {
        match self {
            VirtualTool::MissingCapability => missing_capability_descriptor(name),
        }
    }
}

pub(crate) const MISSING_CAPABILITY_REPLY: &str = "Unfortunately, we have shown you the full tool list. We have noted your feedback and will work to improve the tool list in the future.";

const MISSING_CAPABILITY_DESCRIPTION: &str = "Check for additional tools whenever your task might benefit from specialized capabilities - even if existing tools could work as a fallback.";
const MISSING_CAPABILITY_CONTEXT_DESCRIPTION: &str = "A description of your goal and what kind of tool would help accomplish it. Describe people and entities by role, such as \"a customer\", and leave out personal and identifying information.";

fn missing_capability_descriptor(name: &str) -> Value {
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
