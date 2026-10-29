//! What the host can add to the `send_feedback` tool: report fields of its
//! own, and a handler that sees each report and may answer the agent.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Map, Value};

use super::contract::arguments::{CONTEXT, CONVERSATION_ID, LLM_MODEL};
use super::tools::virtual_tools::{FEEDBACK_TYPES, SENTIMENTS};

pub type FeedbackHandler = Arc<
    dyn Fn(FeedbackReport) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync,
>;

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
        if name.trim().is_empty() {
            return Some("a feedback field needs a name".to_string());
        }
        let injected = [CONTEXT, LLM_MODEL, CONVERSATION_ID];
        if BUILT_IN_FIELDS.contains(&name) || injected.contains(&name) {
            return Some(format!(
                "feedback field {name:?} is a built-in or analytics name"
            ));
        }
        let typed = self.schema.get("type").is_some_and(Value::is_string);
        (!typed).then(|| {
            format!("feedback field {name:?} needs a schema object with a string \"type\"")
        })
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
            Some(allowed) => allowed.iter().any(|option| same_value(option, value)),
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

/// JSON Schema equality: numbers by value at any depth, so `[3]` matches
/// `[3.0]`, and integers exactly, since an `f64` cannot tell large ones
/// apart.
fn same_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => match (integer(a), integer(b)) {
            (Some(a), Some(b)) => a == b,
            (None, None) => a.as_f64() == b.as_f64(),
            _ => false,
        },
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_value(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| same_value(a, b)))
        }
        _ => left == right,
    }
}

/// The number's exact value when it is whole, wider than any JSON integer.
fn integer(number: &serde_json::Number) -> Option<i128> {
    if let Some(whole) = number.as_i64() {
        return Some(whole.into());
    }
    if let Some(whole) = number.as_u64() {
        return Some(whole.into());
    }
    // With serde_json's `arbitrary_precision`, a larger integer keeps its digits.
    if let Ok(whole) = number.to_string().parse::<i128>() {
        return Some(whole);
    }
    let float = number.as_f64()?;
    let in_range = float.abs() < i128::MAX as f64;
    (float.fract() == 0.0 && in_range).then_some(float as i128)
}

/// A string argument that is not blank.
fn text_argument<'a>(arguments: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    let text = arguments.get(name)?.as_str()?;
    (!text.trim().is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::mcp::tools::virtual_tools::VirtualTool;

    #[test]
    fn every_built_in_report_argument_is_reserved() {
        let descriptor = VirtualTool::Feedback.descriptor("send_feedback", &Default::default());
        let properties = descriptor["inputSchema"]["properties"].as_object().unwrap();
        for name in properties.keys() {
            assert!(BUILT_IN_FIELDS.contains(&name.as_str()), "{}", name);
        }
    }

    /// Runs only when a crate in the build turns on serde_json's
    /// `arbitrary_precision`, the one way an integer past `u64` keeps its digits.
    #[test]
    fn integers_past_u64_match_exactly_when_their_digits_survive() {
        let parse = |text: &str| serde_json::from_str::<Value>(text).unwrap();
        let beyond = parse("18446744073709551617");
        if serde_json::to_string(&beyond).unwrap() != "18446744073709551617" {
            return;
        }
        let field = FeedbackField {
            name: "f".to_string(),
            schema: json!({ "type": "integer", "enum": [parse("18446744073709551616")] }),
            required: false,
        };
        assert!(!field.admits(&beyond));
        assert!(field.admits(&parse("18446744073709551616")));
    }

    #[test]
    fn values_must_match_the_declared_type_and_enum() {
        let cases = [
            (json!({ "type": "integer" }), json!(3.0), true),
            (json!({ "type": "integer" }), json!(3.5), false),
            (
                json!({ "type": "integer", "enum": [1, 2, 3] }),
                json!(3.0),
                true,
            ),
            (json!({ "type": "number", "enum": [1.5] }), json!(1.5), true),
            (
                json!({ "type": "integer", "enum": [9_007_199_254_740_992u64] }),
                json!(9_007_199_254_740_993u64),
                false,
            ),
            (
                json!({ "type": "integer", "enum": [9_007_199_254_740_992.0f64] }),
                json!(9_007_199_254_740_993u64),
                false,
            ),
            (
                json!({ "type": "integer", "enum": [9_007_199_254_740_992.0f64] }),
                json!(9_007_199_254_740_992u64),
                true,
            ),
            (json!({ "type": "number", "enum": [3] }), json!(3.5), false),
            (
                json!({ "type": "array", "enum": [[1, 2]] }),
                json!([1.0, 2]),
                true,
            ),
            (
                json!({ "type": "array", "enum": [[1, 2]] }),
                json!([2, 1]),
                false,
            ),
            (
                json!({ "type": "object", "enum": [{ "a": [1] }] }),
                json!({ "a": [1.0] }),
                true,
            ),
            (
                json!({ "type": "object", "enum": [{ "a": 1 }] }),
                json!({ "a": 1, "b": 2 }),
                false,
            ),
            (
                json!({ "type": "string", "enum": ["a"] }),
                json!("b"),
                false,
            ),
            (json!({ "type": "string" }), json!(7), false),
            (json!({ "type": "object" }), json!({ "k": 1 }), true),
        ];
        for (schema, value, admitted) in cases {
            let field = FeedbackField {
                name: "f".to_string(),
                schema: schema.clone(),
                required: false,
            };
            assert_eq!(field.admits(&value), admitted, "{schema} {value}");
        }
    }
}
