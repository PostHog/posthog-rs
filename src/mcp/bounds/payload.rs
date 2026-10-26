//! `$mcp_parameters` and `$mcp_response`: the entry, nesting and key limits and
//! the key-based redaction apply first, then each string gets its own
//! redaction and limit. Strings share a budget of what redaction may scan, the
//! size of a whole event, so once it is spent the rest are cut short before
//! redaction scans them: an event that large is shrunk afterwards anyway.

use serde_json::{Map, Value};

use super::text::{bounded_text, truncate};
use crate::mcp::contract::limits::{
    MAX_EVENT_BYTES, MAX_PAYLOAD_DEPTH, MAX_PAYLOAD_ENTRIES, MAX_PAYLOAD_KEY_LENGTH,
    MAX_PAYLOAD_STRING_LENGTH, REDACTION_SCAN_MARGIN,
};
use crate::mcp::contract::REDACTED;
use crate::mcp::privacy::{is_credential_key, TextPass};

const OVERFLOW_MARKER: &str = "[MaxProperties ~]";
const OVERFLOW_KEY: &str = "...";
const NESTED_ARRAY_MARKER: &str = "[Array]";
const NESTED_OBJECT_MARKER: &str = "[Object]";
const EXHAUSTED_STRING_LENGTH: usize = 64;

pub(crate) fn bounded_payload(value: &Value, pass: TextPass) -> Value {
    let mut budget = MAX_EVENT_BYTES;
    bound(value, MAX_PAYLOAD_DEPTH, pass, &mut budget)
}

fn bound(value: &Value, remaining_depth: usize, pass: TextPass, budget: &mut usize) -> Value {
    match value {
        Value::String(text) => Value::String(bound_string(text, pass, budget)),
        Value::Array(_) if remaining_depth == 0 => Value::from(NESTED_ARRAY_MARKER),
        Value::Object(_) if remaining_depth == 0 => Value::from(NESTED_OBJECT_MARKER),
        Value::Array(items) => bound_array(items, remaining_depth - 1, pass, budget),
        Value::Object(object) => bound_object(object, remaining_depth - 1, pass, budget),
        other => other.clone(),
    }
}

fn bound_string(text: &str, pass: TextPass, budget: &mut usize) -> String {
    let limit = MAX_PAYLOAD_STRING_LENGTH
        .min(*budget)
        .max(EXHAUSTED_STRING_LENGTH);
    let scanned = text.chars().take(limit + REDACTION_SCAN_MARGIN).count();
    *budget = budget.saturating_sub(scanned);
    bounded_text(text, limit, pass)
}

fn bound_array(
    items: &[Value],
    remaining_depth: usize,
    pass: TextPass,
    budget: &mut usize,
) -> Value {
    let overflows = items.len() > MAX_PAYLOAD_ENTRIES;
    let kept = kept_entries(items.len());
    let mut bounded: Vec<Value> = items[..kept]
        .iter()
        .map(|item| bound(item, remaining_depth, pass, budget))
        .collect();
    if overflows {
        bounded.push(Value::from(OVERFLOW_MARKER));
    }
    Value::Array(bounded)
}

fn bound_object(
    object: &Map<String, Value>,
    remaining_depth: usize,
    pass: TextPass,
    budget: &mut usize,
) -> Value {
    let overflows = object.len() > MAX_PAYLOAD_ENTRIES;
    let mut bounded: Map<String, Value> = object
        .iter()
        .take(kept_entries(object.len()))
        .map(|(key, nested)| {
            let value = match is_credential_key(key) {
                true => Value::from(REDACTED),
                false => bound(nested, remaining_depth, pass, budget),
            };
            (truncate(key, MAX_PAYLOAD_KEY_LENGTH), value)
        })
        .collect();
    if overflows {
        bounded.insert(OVERFLOW_KEY.to_string(), Value::from(OVERFLOW_MARKER));
    }
    Value::Object(bounded)
}

fn kept_entries(length: usize) -> usize {
    if length > MAX_PAYLOAD_ENTRIES {
        MAX_PAYLOAD_ENTRIES - 1
    } else {
        length
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn credential_keys_are_redacted_whole() {
        let value = json!({ "api_key": { "nested": 1 }, "Authorization": "Bearer x", "q": "eu" });
        assert_eq!(
            bounded_payload(&value, TextPass::Captured),
            json!({ "api_key": "[redacted]", "Authorization": "[redacted]", "q": "eu" })
        );
    }

    #[test]
    fn entries_are_capped_with_a_marker() {
        let value = Value::Array((0..150).map(Value::from).collect());
        let bounded = bounded_payload(&value, TextPass::Captured);
        let items = bounded.as_array().expect("array");
        assert_eq!(items.len(), MAX_PAYLOAD_ENTRIES);
        assert_eq!(items[MAX_PAYLOAD_ENTRIES - 1], OVERFLOW_MARKER);
    }

    #[test]
    fn strings_past_the_event_budget_are_cut_short() {
        let block = Value::from("word ".repeat(8_000));
        let value = Value::Array(vec![block; MAX_PAYLOAD_ENTRIES - 1]);
        let bounded = bounded_payload(&value, TextPass::Captured);
        let lengths: Vec<usize> = bounded
            .as_array()
            .expect("array")
            .iter()
            .map(|item| item.as_str().expect("string").chars().count())
            .collect();
        assert_eq!(lengths[0], MAX_PAYLOAD_STRING_LENGTH);
        assert_eq!(lengths[MAX_PAYLOAD_ENTRIES - 2], EXHAUSTED_STRING_LENGTH);
    }

    #[test]
    fn nesting_beyond_the_limit_collapses() {
        let mut value = json!("leaf");
        for _ in 0..12 {
            value = json!([value]);
        }
        let bounded = bounded_payload(&value, TextPass::Captured);
        let mut depth = 0;
        let mut cursor = &bounded;
        while let Some(items) = cursor.as_array() {
            depth += 1;
            cursor = &items[0];
        }
        assert_eq!(depth, MAX_PAYLOAD_DEPTH);
        assert_eq!(cursor, NESTED_ARRAY_MARKER);
    }
}
