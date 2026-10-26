//! `$mcp_parameters` and `$mcp_response`: the entry, nesting and key limits and
//! the key-based redaction apply first, then each string gets its own
//! redaction and limit.

use serde_json::{Map, Value};

use super::text::{bounded_text, truncate};
use crate::mcp::contract::limits::{
    MAX_PAYLOAD_DEPTH, MAX_PAYLOAD_ENTRIES, MAX_PAYLOAD_KEY_LENGTH, MAX_PAYLOAD_STRING_LENGTH,
};
use crate::mcp::contract::REDACTED;
use crate::mcp::privacy::{is_credential_key, TextPass};

const OVERFLOW_MARKER: &str = "[MaxProperties ~]";
const OVERFLOW_KEY: &str = "...";
const NESTED_ARRAY_MARKER: &str = "[Array]";
const NESTED_OBJECT_MARKER: &str = "[Object]";

pub(crate) fn bounded_payload(value: &Value, pass: TextPass) -> Value {
    bound(value, MAX_PAYLOAD_DEPTH, pass)
}

fn bound(value: &Value, remaining_depth: usize, pass: TextPass) -> Value {
    match value {
        Value::String(text) => Value::String(bounded_text(text, MAX_PAYLOAD_STRING_LENGTH, pass)),
        Value::Array(_) if remaining_depth == 0 => Value::from(NESTED_ARRAY_MARKER),
        Value::Object(_) if remaining_depth == 0 => Value::from(NESTED_OBJECT_MARKER),
        Value::Array(items) => bound_array(items, remaining_depth - 1, pass),
        Value::Object(object) => bound_object(object, remaining_depth - 1, pass),
        other => other.clone(),
    }
}

fn bound_array(items: &[Value], remaining_depth: usize, pass: TextPass) -> Value {
    let overflows = items.len() > MAX_PAYLOAD_ENTRIES;
    let kept = kept_entries(items.len());
    let mut bounded: Vec<Value> = items[..kept]
        .iter()
        .map(|item| bound(item, remaining_depth, pass))
        .collect();
    if overflows {
        bounded.push(Value::from(OVERFLOW_MARKER));
    }
    Value::Array(bounded)
}

fn bound_object(object: &Map<String, Value>, remaining_depth: usize, pass: TextPass) -> Value {
    let overflows = object.len() > MAX_PAYLOAD_ENTRIES;
    let mut bounded: Map<String, Value> = object
        .iter()
        .take(kept_entries(object.len()))
        .map(|(key, nested)| {
            let value = match is_credential_key(key) {
                true => Value::from(REDACTED),
                false => bound(nested, remaining_depth, pass),
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
