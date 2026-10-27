//! Keeps an event's properties within the byte bound without dropping a
//! required property. Payloads shrink first, then the other optional
//! properties, largest first. The error message is cut, never dropped, and a
//! required list, such as a listing's tool names, loses entries from its end
//! only as a last resort. Each entry's size is measured once and updated as it
//! shrinks.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::text::truncate;
use crate::mcp::contract::limits::{HOST_PROPERTIES_HEADROOM, MAX_EVENT_BYTES};
use crate::mcp::contract::properties::{ERROR_MESSAGE, PARAMETERS, RESPONSE};

const PAYLOADS: [&str; 2] = [RESPONSE, PARAMETERS];
const SHRINK_CAPS: [usize; 4] = [4_096, 1_024, 256, 64];
const LAST_RESORT_CAP: usize = 256;
const BUDGET: usize = MAX_EVENT_BYTES - HOST_PROPERTIES_HEADROOM;

pub(crate) fn fit_event_size(properties: &mut Map<String, Value>, required: &[&str]) {
    let mut sized = SizedProperties::measure(properties);
    if sized.fits() {
        return;
    }
    for key in PAYLOADS {
        if sized.shrink_or_drop(key) {
            return;
        }
    }
    for key in sized.optional_keys_largest_first(required) {
        if sized.shrink_or_drop(&key) {
            return;
        }
    }
    sized.replace_strings(ERROR_MESSAGE, LAST_RESORT_CAP);
    for key in required {
        if sized.fits() {
            return;
        }
        sized.drop_trailing_entries(key);
    }
}

struct SizedProperties<'a> {
    properties: &'a mut Map<String, Value>,
    entry_sizes: HashMap<String, usize>,
    total: usize,
}

impl<'a> SizedProperties<'a> {
    fn measure(properties: &'a mut Map<String, Value>) -> Self {
        let entry_sizes: HashMap<String, usize> = properties
            .iter()
            .map(|(key, value)| (key.clone(), entry_size(key, value)))
            .collect();
        let total = 1 + entry_sizes.values().sum::<usize>();
        Self {
            properties,
            entry_sizes,
            total,
        }
    }

    fn fits(&self) -> bool {
        self.total <= BUDGET
    }

    /// Whether the event fits once `key` is shrunk or, failing that, dropped.
    fn shrink_or_drop(&mut self, key: &str) -> bool {
        if !self.properties.contains_key(key) {
            return false;
        }
        for cap in SHRINK_CAPS {
            self.replace_strings(key, cap);
            if self.fits() {
                return true;
            }
        }
        self.drop(key);
        self.fits()
    }

    fn optional_keys_largest_first(&self, required: &[&str]) -> Vec<String> {
        let mut optional: Vec<(&String, &usize)> = self
            .entry_sizes
            .iter()
            .filter(|(key, _)| !required.contains(&key.as_str()) && key.as_str() != ERROR_MESSAGE)
            .collect();
        optional.sort_by(|left, right| right.1.cmp(left.1).then_with(|| left.0.cmp(right.0)));
        optional.into_iter().map(|(key, _)| key.clone()).collect()
    }

    fn replace_strings(&mut self, key: &str, cap: usize) {
        if let Some(value) = self.properties.get_mut(key) {
            truncate_strings(value, cap);
            let size = entry_size(key, value);
            self.update(key, Some(size));
        }
    }

    fn drop_trailing_entries(&mut self, key: &str) {
        let Some(Value::Array(items)) = self.properties.get_mut(key) else {
            return;
        };
        let mut size = self.entry_sizes.get(key).copied().unwrap_or(0);
        let mut total = self.total;
        while total > BUDGET {
            let Some(item) = items.pop() else {
                break;
            };
            let item_size = json_length(&item) + 1;
            size -= item_size;
            total -= item_size;
        }
        self.entry_sizes.insert(key.to_string(), size);
        self.total = total;
    }

    fn drop(&mut self, key: &str) {
        self.properties.remove(key);
        self.update(key, None);
    }

    fn update(&mut self, key: &str, size: Option<usize>) {
        let previous = self.entry_sizes.remove(key).unwrap_or(0);
        self.total -= previous;
        if let Some(size) = size {
            self.entry_sizes.insert(key.to_string(), size);
            self.total += size;
        }
    }
}

fn truncate_strings(value: &mut Value, cap: usize) {
    match value {
        Value::String(text) => *text = truncate(text, cap),
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| truncate_strings(item, cap)),
        Value::Object(object) => object
            .values_mut()
            .for_each(|nested| truncate_strings(nested, cap)),
        _ => {}
    }
}

/// The bytes an entry adds to compact JSON, its separating comma included.
fn entry_size(key: &str, value: &Value) -> usize {
    json_length(&Value::from(key)) + 1 + json_length(value) + 1
}

fn json_length(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::mcp::contract::properties::{LISTED_TOOL_NAMES, TOOL_DESCRIPTION, TOOL_NAME};

    fn serialized_size(properties: &Map<String, Value>) -> usize {
        serde_json::to_vec(properties).expect("serializes").len()
    }

    fn properties(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn measured_size_matches_serialization() {
        let mut map = properties(json!({ "a": "b", "list": [1, 2], "nested": { "x": null } }));
        let sized = SizedProperties::measure(&mut map);
        let measured = sized.total;
        assert_eq!(measured, serialized_size(&map));
    }

    #[test]
    fn payloads_shrink_before_anything_else() {
        let huge = "x".repeat(200_000);
        let mut map = properties(json!({
            TOOL_NAME: "export",
            TOOL_DESCRIPTION: "exports",
            RESPONSE: { "content": [{ "type": "text", "text": huge }] },
        }));
        fit_event_size(&mut map, &[TOOL_NAME]);
        assert!(serialized_size(&map) <= BUDGET);
        assert_eq!(map[TOOL_DESCRIPTION], "exports");
        assert!(map[RESPONSE]["content"][0]["text"].as_str().is_some());
    }

    #[test]
    fn required_properties_survive_and_the_error_message_is_cut() {
        let mut map = properties(json!({
            TOOL_NAME: "t".repeat(60_000),
            ERROR_MESSAGE: "e".repeat(60_000),
            TOOL_DESCRIPTION: "d".repeat(60_000),
        }));
        fit_event_size(&mut map, &[TOOL_NAME]);
        assert!(serialized_size(&map) <= BUDGET);
        assert!(map.contains_key(TOOL_NAME));
        assert!(map.contains_key(ERROR_MESSAGE));
    }

    #[test]
    fn a_required_list_loses_entries_from_its_end_as_a_last_resort() {
        let names: Vec<Value> = (0..3_000)
            .map(|index| Value::from(format!("tool_number_{index:05}")))
            .collect();
        let mut map = properties(json!({ LISTED_TOOL_NAMES: names }));
        fit_event_size(&mut map, &[LISTED_TOOL_NAMES]);
        assert!(serialized_size(&map) <= BUDGET);
        let kept = map[LISTED_TOOL_NAMES].as_array().expect("names");
        assert!(kept.len() > 1_000);
        assert_eq!(kept[0], "tool_number_00000");
    }
}
