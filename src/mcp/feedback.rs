//! What the host can add to the `send_feedback` tool: report fields of its
//! own, and a handler that sees each report and may answer the agent.

use std::sync::Arc;

use serde_json::{Map, Value};

use super::contract::arguments::{CONTEXT, CONVERSATION_ID, LLM_MODEL};
use super::tools::virtual_tools::{FEEDBACK_TYPES, SENTIMENTS};

pub type FeedbackHandler = Arc<dyn Fn(&FeedbackReport) -> Option<String> + Send + Sync>;

const OTHER_FEEDBACK: &str = "other";

/// The built-in report arguments, and the names whose `$mcp_feedback_<name>`
/// property a built-in field already uses.
const BUILT_IN_FIELDS: [&str; 10] = [
    "feedback_type",
    "summary",
    "details",
    "friction_points",
    "suggested_improvement",
    "tool_name",
    "sentiment",
    "task_completed",
    "type",
    "tool",
];

/// One report argument the host declares, as a JSON Schema fragment.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FeedbackField {
    pub(crate) name: String,
    pub(crate) schema: Value,
    pub(crate) required: bool,
}

impl FeedbackField {
    /// Why the host cannot declare this field, if it cannot.
    pub(crate) fn problem(&self) -> Option<String> {
        let name = self.name.as_str();
        let injected = [CONTEXT, LLM_MODEL, CONVERSATION_ID];
        if BUILT_IN_FIELDS.contains(&name) || injected.contains(&name) {
            return Some(format!(
                "feedback field {name:?} is a built-in or analytics name"
            ));
        }
        let typed = self.schema.get("type").is_some_and(Value::is_string);
        (!typed).then(|| format!("feedback field {name:?} needs a schema object with a \"type\""))
    }

    /// Whether `value` has the declared `type` and, when declared, is one of
    /// its `enum` values.
    fn admits(&self, value: &Value) -> bool {
        let declared = self.schema.get("type").and_then(Value::as_str);
        let typed = match declared {
            Some("string") => value.is_string(),
            Some("number") => value.is_number(),
            Some("integer") => value.as_f64().is_some_and(|number| number.fract() == 0.0),
            Some("boolean") => value.is_boolean(),
            Some("array") => value.is_array(),
            Some("object") => value.is_object(),
            Some("null") => value.is_null(),
            _ => false,
        };
        let listed = match self.schema.get("enum").and_then(Value::as_array) {
            Some(allowed) => allowed.contains(value),
            None => true,
        };
        typed && listed
    }
}

/// One `send_feedback` call, as the handler sees it. Text is as the agent
/// wrote it, before redaction: the event carries the redacted copy.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct FeedbackReport {
    /// One of `missing_capability`, `issue`, `praise` or `other`; anything
    /// else the agent sent becomes `other`.
    pub feedback_type: String,
    /// Empty when the agent left it out.
    pub summary: String,
    pub details: Option<String>,
    pub friction_points: Option<String>,
    pub suggested_improvement: Option<String>,
    /// The existing tool the report is about.
    pub tool_name: Option<String>,
    /// One of `positive`, `neutral`, `negative` or `mixed`.
    pub sentiment: Option<String>,
    pub task_completed: Option<bool>,
    /// The declared fields whose values match their schema.
    pub extras: Map<String, Value>,
    /// Every argument the agent sent, which is never captured.
    pub raw: Map<String, Value>,
}

impl FeedbackReport {
    pub(crate) fn parse(arguments: &Map<String, Value>, fields: &[FeedbackField]) -> Self {
        let text = |name: &str| text_argument(arguments, name).map(str::to_string);
        let feedback_type = text_argument(arguments, "feedback_type")
            .filter(|kind| FEEDBACK_TYPES.contains(kind))
            .unwrap_or(OTHER_FEEDBACK);
        let sentiment = text_argument(arguments, "sentiment")
            .filter(|sentiment| SENTIMENTS.contains(sentiment));
        let extras = fields
            .iter()
            .filter_map(|field| {
                let value = arguments.get(&field.name)?;
                field
                    .admits(value)
                    .then(|| (field.name.clone(), value.clone()))
            })
            .collect();
        Self {
            feedback_type: feedback_type.to_string(),
            summary: text("summary").unwrap_or_default(),
            details: text("details"),
            friction_points: text("friction_points"),
            suggested_improvement: text("suggested_improvement"),
            tool_name: text("tool_name"),
            sentiment: sentiment.map(str::to_string),
            task_completed: arguments.get("task_completed").and_then(Value::as_bool),
            extras,
            raw: arguments.clone(),
        }
    }

    /// The non-empty of `summary` and `details`, joined by a blank line.
    pub(crate) fn intent(&self) -> Option<String> {
        let parts: Vec<&str> = std::iter::once(self.summary.as_str())
            .chain(self.details.as_deref())
            .filter(|part| !part.is_empty())
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }
}

/// A string argument that is not blank.
fn text_argument<'a>(arguments: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    let text = arguments.get(name)?.as_str()?;
    (!text.trim().is_empty()).then_some(text)
}
