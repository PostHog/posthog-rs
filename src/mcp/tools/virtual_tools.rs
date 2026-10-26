//! The opt-in tools analytics answers itself, which never reach the server's
//! handlers: `get_more_tools`, where an agent reports a capability the server
//! lacks, and `send_feedback`, where it reports what helped or hindered it.

use serde_json::{json, Value};

/// A tool analytics advertises and answers on the server's behalf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VirtualTool {
    MissingCapability,
    Feedback,
}

impl VirtualTool {
    pub(crate) const ALL: [VirtualTool; 2] =
        [VirtualTool::MissingCapability, VirtualTool::Feedback];

    pub(crate) fn default_name(self) -> &'static str {
        match self {
            VirtualTool::MissingCapability => "get_more_tools",
            VirtualTool::Feedback => "send_feedback",
        }
    }

    pub(crate) fn descriptor(self, name: &str) -> Value {
        match self {
            VirtualTool::MissingCapability => missing_capability_descriptor(name),
            VirtualTool::Feedback => feedback_descriptor(name),
        }
    }
}

pub(crate) const FEEDBACK_TYPES: [&str; 4] = ["missing_capability", "issue", "praise", "other"];
pub(crate) const SENTIMENTS: [&str; 4] = ["positive", "neutral", "negative", "mixed"];

pub(crate) const FEEDBACK_REPLY: &str = "Your feedback was recorded for the server's developers. No additional tools are available - continue with the tools already listed.";

const FEEDBACK_DESCRIPTION: &str = "Send feedback about this server to its developers. Most important: report a missing capability whenever no available tool fits your task, even if you can work around it (feedback_type \"missing_capability\"). Also welcome: a tool that failed or confused you, an unhelpful error, or something that worked well. This records the feedback; it does not add or change tools. Do not include user PII or sensitive content.";

fn feedback_descriptor(name: &str) -> Value {
    json!({
        "name": name,
        "description": FEEDBACK_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "properties": {
                "feedback_type": {
                    "type": "string",
                    "enum": FEEDBACK_TYPES,
                    "description": "What kind of feedback this is. Use 'missing_capability' when the tool you needed does not exist in the tool list - nothing failed, the capability is absent (this is the most valuable report; send it even if you found a workaround). Use 'issue' when an existing tool behaved badly: it failed, returned a confusing error, or its description or schema misled you. Use 'praise' when something worked notably well. Use 'other' for anything else.",
                },
                "summary": {
                    "type": "string",
                    "description": "One self-contained sentence. For 'missing_capability': the capability you needed, e.g. 'No tool to delete multiple cohorts in one call.' For 'issue': the tool and the problem, e.g. 'query-trends rejects relative date ranges with an unclear error.'",
                },
                "details": {
                    "type": "string",
                    "description": "Optional longer context: what you tried, exact parameter values, the error text you saw, and any workaround you used. Omit when the summary says it all.",
                },
                "friction_points": {
                    "type": "string",
                    "description": "Optional: the specific moments that slowed you down, as short bullet-like sentences, quoting exact tool names, parameters, or error text.",
                },
                "suggested_improvement": {
                    "type": "string",
                    "description": "Optional: the concrete change that would have helped, e.g. the tool to add, the description to reword, or the error message to improve.",
                },
                "tool_name": {
                    "type": "string",
                    "description": "Optional: the existing tool this feedback is about (for 'issue' or 'praise'). Leave empty for 'missing_capability' - the point is that no tool fits.",
                },
                "sentiment": {
                    "type": "string",
                    "enum": SENTIMENTS,
                    "description": "Optional: how the experience felt overall.",
                },
                "task_completed": {
                    "type": "boolean",
                    "description": "Optional: whether you still completed the user's task despite the problem.",
                },
            },
            "required": ["feedback_type", "summary"],
        },
        "annotations": {
            "title": "Send feedback",
            "readOnlyHint": true,
            "openWorldHint": true,
            "idempotentHint": true,
            "destructiveHint": false,
        },
    })
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
