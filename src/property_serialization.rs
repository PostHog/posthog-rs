use serde::{Serialize, Serializer};
use serde_json::Value;

/// Only wire-event property fields use this serializer. Event inputs and typed
/// envelope metadata retain their existing serialization rules. Normalize a
/// private JSON tree after hooks/enrichment without changing the source event.
pub(crate) struct EventProperties<'a, T> {
    pub event: &'a str,
    pub properties: &'a T,
}

impl<T: Serialize> Serialize for EventProperties<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = serde_json::to_value(self.properties).map_err(serde::ser::Error::custom)?;
        // Missing/error flag evaluations deliberately emit these root nulls.
        // Determine the exact dynamic key from the final event properties, never
        // a prefix exemption, and never reinsert fields trimmed by privacy rules.
        if self.event == "$feature_flag_called" {
            if let Value::Object(properties) = &mut value {
                let feature_key = properties
                    .get("$feature_flag")
                    .and_then(Value::as_str)
                    .map(|key| format!("$feature/{key}"));
                properties.retain(|key, value| {
                    !value.is_null()
                        || key == "$feature_flag_response"
                        || feature_key.as_ref() == Some(key)
                });
                for value in properties.values_mut() {
                    drop_null_object_members(value);
                }
            } else {
                drop_null_object_members(&mut value);
            }
        } else {
            drop_null_object_members(&mut value);
        }
        value.serialize(serializer)
    }
}

fn drop_null_object_members(value: &mut Value) {
    match value {
        Value::Object(properties) => {
            properties.retain(|_, value| !value.is_null());
            for value in properties.values_mut() {
                drop_null_object_members(value);
            }
        }
        Value::Array(items) => {
            for value in items {
                drop_null_object_members(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::event::InnerEvent;
    use crate::Event;
    use serde_json::{json, Value};

    #[test]
    fn v0_properties_are_normalized_only_at_serialization() {
        let mut event = Event::new("test", "user");
        event.insert_prop("test", Value::Null).unwrap();
        event
            .insert_prop("items", json!([null, {"drop": null}]))
            .unwrap();
        let input = serde_json::to_value(&event).unwrap();
        assert_eq!(input["properties"]["test"], Value::Null);
        let wire = serde_json::to_value(InnerEvent::new(event.clone(), "test-key".into())).unwrap();
        assert!(wire["properties"].get("test").is_none());
        assert_eq!(wire["properties"]["items"], json!([null, {}]));
        // Optional typed envelope metadata is not recursively pruned.
        assert_eq!(wire.get("timestamp"), Some(&Value::Null));
        assert_eq!(serde_json::to_value(&event).unwrap(), input);
    }

    #[test]
    fn flag_null_preservation_is_event_and_exact_key_scoped() {
        for minimal in [false, true] {
            let mut event = Event::new("$feature_flag_called", "user");
            event.insert_prop("$feature_flag", "missing").unwrap();
            for key in [
                "$feature_flag_response",
                "$feature/missing",
                "$feature/other",
                "custom",
            ] {
                event.insert_prop(key, Value::Null).unwrap();
            }
            if minimal {
                event.mark_minimal_flag_called();
                event.apply_minimal_flag_called_allowlist();
            }
            let wire =
                serde_json::to_value(InnerEvent::new(event.clone(), "test-key".into())).unwrap();
            assert_eq!(
                wire["properties"].get("$feature_flag_response"),
                Some(&Value::Null)
            );
            assert_eq!(
                wire["properties"].get("$feature/missing").is_none(),
                minimal
            );
            assert!(wire["properties"].get("$feature/other").is_none());
            assert!(wire["properties"].get("custom").is_none());
            #[cfg(feature = "capture-v1")]
            {
                let wire =
                    serde_json::to_value(crate::event_v1::V1Event::from_event(&event)).unwrap();
                assert_eq!(
                    wire["properties"].get("$feature_flag_response"),
                    Some(&Value::Null)
                );
                assert_eq!(
                    wire["properties"].get("$feature/missing").is_none(),
                    minimal
                );
            }
        }
        let properties = json!({"$feature_flag":"key", "$feature_flag_response":{"drop":null},
            "$feature/key":{"items":[null,{"drop":null}]}});
        let wire = serde_json::to_value(super::EventProperties {
            event: "$feature_flag_called",
            properties: &properties,
        })
        .unwrap();
        assert_eq!(wire["$feature_flag_response"], json!({}));
        assert_eq!(wire["$feature/key"], json!({"items":[null,{}]}));
        // Even a non-object V1 property value retains the recursive array policy.
        let wire = serde_json::to_value(super::EventProperties {
            event: "$feature_flag_called",
            properties: &json!([null,{"drop":null}]),
        })
        .unwrap();
        assert_eq!(wire, json!([null, {}]));
    }

    #[cfg(feature = "capture-v1")]
    #[test]
    fn v1_properties_added_after_build_are_normalized_without_mutation() {
        let event = Event::new("test", "user");
        let mut wire = crate::event_v1::V1Event::from_event(&event);
        wire.properties["late"] = json!({"drop": null, "items": [null, {"drop": null}]});
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(json["properties"]["late"], json!({"items": [null, {}]}));
        assert_eq!(json["options"], json!({}));
        assert!(json.get("session_id").is_none());
        assert_eq!(wire.properties["late"]["drop"], Value::Null);
    }
}
