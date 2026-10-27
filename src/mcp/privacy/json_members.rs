//! A tool that returns `structuredContent` also returns it as JSON text, where
//! a credential is a member inside a string rather than an object key. Values
//! that nest objects, and JSON inside a JSON string, are out of scope.

use std::sync::LazyLock;

use regex::{Captures, Regex};

use super::keys::is_credential_key;
use crate::mcp::contract::REDACTED;

const JSON_STRING: &str = r#""(?:[^"\\\n]|\\.)*(?:"|\\?(?m:$))"#;
const JSON_SCALAR_OTHER: &str = r"-?\d[\d.eE+-]*|true|false";

static JSON_MEMBER: LazyLock<Regex> = LazyLock::new(|| {
    let scalar = format!("{JSON_STRING}|{JSON_SCALAR_OTHER}");
    let array = format!(r"\[(?:\s*(?:{scalar}|null)\s*,?)*\s*(?:\]|$)");
    let key = r#""((?:[^"\\\n]|\\.)*)""#;
    Regex::new(&format!(r"({key}\s*:\s*)(?:{scalar}|{array})")).expect("json member pattern")
});

pub(super) fn redact_json_members(text: &str) -> String {
    if !text.contains('"') {
        return text.to_string();
    }
    JSON_MEMBER
        .replace_all(text, |member: &Captures<'_>| redacted_member(member))
        .into_owned()
}

fn redacted_member(member: &Captures<'_>) -> String {
    let key = decoded_key(&member[2]);
    if is_credential_key(&key) {
        format!("{}\"{REDACTED}\"", &member[1])
    } else {
        member[0].to_string()
    }
}

fn decoded_key(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    let quoted = format!("\"{raw}\"");
    serde_json::from_str(&quoted).unwrap_or_else(|_| raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_members_are_redacted_and_json_stays_valid() {
        let text = r#"{"user":"ada","password":"hunter2","tokens":[1,2]}"#;
        assert_eq!(
            redact_json_members(text),
            r#"{"user":"ada","password":"[redacted]","tokens":[1,2]}"#
        );
    }

    #[test]
    fn escaped_keys_and_arrays_are_recognized() {
        let text = r#"{"password": ["a", "b"], "api_key": 42}"#;
        assert_eq!(
            redact_json_members(text),
            r#"{"password": "[redacted]", "api_key": "[redacted]"}"#
        );
    }

    #[test]
    fn a_value_cut_off_at_the_end_is_still_redacted() {
        let text = r#"{"secret": "abc\"#;
        assert_eq!(redact_json_members(text), r#"{"secret": "[redacted]""#);
    }
}
