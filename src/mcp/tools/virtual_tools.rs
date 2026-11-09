//! The opt-in tools analytics answers itself, which never reach the server's
//! handlers: `get_more_tools`, where an agent reports a capability the server
//! lacks, and `send_feedback`, where it reports what helped or hindered it.

use serde_json::{json, Value};

use crate::mcp::options::McpOptions;

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

    /// The name it is advertised under when enabled with `configured`; a
    /// blank name means its default.
    pub(crate) fn name(self, configured: Option<&str>) -> Option<&str> {
        let name = configured?.trim();
        Some(if name.is_empty() {
            self.default_name()
        } else {
            name
        })
    }

    pub(crate) fn descriptor(self, name: &str, options: &McpOptions) -> Value {
        match self {
            VirtualTool::MissingCapability => missing_capability_descriptor(name),
            VirtualTool::Feedback => feedback_descriptor(name, options),
        }
    }
}

pub(crate) const FEEDBACK_TYPES: [&str; 4] = ["missing_capability", "issue", "praise", "other"];
pub(crate) const SENTIMENTS: [&str; 4] = ["positive", "neutral", "negative", "mixed"];

pub(crate) const FEEDBACK_REPLY: &str = "Your feedback was recorded for the server's developers. No additional tools are available - continue with the tools already listed.";

const FEEDBACK_DESCRIPTION: &str = "Send feedback about this server to its developers: a capability no listed tool provides, a tool that failed or confused you, an unhelpful error, or something that worked well. Records the feedback for the server's developers; it does not add or change tools. Leave out personal and sensitive details.";

/// The built-in report schema, with the host's description and fields.
fn feedback_descriptor(name: &str, options: &McpOptions) -> Value {
    let description = options
        .feedback_description
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(FEEDBACK_DESCRIPTION);
    let mut descriptor = built_in_feedback_descriptor(name, description);
    let schema = &mut descriptor["inputSchema"];
    for field in &options.feedback_fields {
        schema["properties"][&field.name] = field.schema.clone();
        if field.required {
            if let Some(required) = schema["required"].as_array_mut() {
                required.push(Value::from(field.name.as_str()));
            }
        }
    }
    descriptor
}

fn built_in_feedback_descriptor(name: &str, description: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "feedback_type": {
                    "type": "string",
                    "enum": FEEDBACK_TYPES,
                    "description": "What kind of feedback this is. Use 'missing_capability' when the tool you needed does not exist in the tool list - nothing failed, the capability is absent. Use 'issue' when an existing tool behaved badly: it failed, returned a confusing error, or its description or schema misled you. Use 'praise' when something worked notably well. Use 'other' for anything else.",
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
        "annotations": records_data_annotations(),
    })
}

fn records_data_annotations() -> Value {
    json!({
        "readOnlyHint": false,
        "openWorldHint": false,
        "idempotentHint": false,
        "destructiveHint": false,
    })
}

pub(crate) const MISSING_CAPABILITY_REPLY: &str = "Unfortunately, we have shown you the full tool list. We have noted your feedback and will work to improve the tool list in the future.";

const MISSING_CAPABILITY_DESCRIPTION: &str = "Report that the user's request needs a capability this server does not have. Records the report for the server's developers; it does not return or add tools.";
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
        "annotations": records_data_annotations(),
    })
}
