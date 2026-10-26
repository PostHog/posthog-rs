//! A failed call: the label it is counted under, and the `$exception` that
//! error tracking groups by the failure's own type.

use serde_json::{json, Map, Value};

use crate::mcp::bounds::{bounded_non_empty, truncate};
use crate::mcp::contract::limits::{MAX_NAME_LENGTH, MAX_TEXT_LENGTH};
use crate::mcp::contract::properties as key;
use crate::mcp::contract::{EXCEPTION_SOURCE, REDACTED};
use crate::mcp::privacy::TextPass;

const DEFAULT_ERROR_TYPE: &str = "Error";
const EMPTY_MESSAGE: &str = "Unknown error";
const POINTER_TYPES: [&str; 3] = ["Box", "Arc", "Rc"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    exception_type: String,
    message: String,
    explicit_type: Option<String>,
    synthetic: bool,
}

impl Failure {
    /// A failure the handler raised, named by its type's path, such as
    /// [`std::any::type_name`] gives.
    pub(crate) fn thrown(type_name: &str, message: String) -> Self {
        Self {
            exception_type: short_type_name(type_name),
            message,
            explicit_type: None,
            synthetic: false,
        }
    }

    /// A result that reported its own failure, with no error behind it.
    pub(crate) fn reported(message: String) -> Self {
        Self {
            exception_type: DEFAULT_ERROR_TYPE.to_string(),
            message,
            explicit_type: None,
            synthetic: true,
        }
    }

    pub(crate) fn with_explicit_type(mut self, explicit_type: Option<String>) -> Self {
        let explicit_type = explicit_type
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty());
        if explicit_type.is_some() {
            self.explicit_type = explicit_type;
        }
        self
    }

    /// The `$mcp_error_type` and `$mcp_error_message` of the failed call.
    pub(crate) fn call_properties(&self) -> Map<String, Value> {
        let label = self
            .explicit_type
            .as_deref()
            .unwrap_or(&self.exception_type);
        let mut properties = Map::new();
        properties.insert(
            key::ERROR_TYPE.to_string(),
            Value::from(truncate(label, MAX_NAME_LENGTH)),
        );
        properties.insert(
            key::ERROR_MESSAGE.to_string(),
            Value::from(self.bounded_message()),
        );
        properties
    }

    /// The `$exception` properties, which never carry the explicit label.
    pub(crate) fn exception_properties(&self) -> Map<String, Value> {
        let exception = json!({
            "type": truncate(&self.exception_type, MAX_NAME_LENGTH),
            "value": self.bounded_message(),
            "mechanism": { "type": "generic", "handled": true, "synthetic": self.synthetic },
        });
        let mut properties = Map::new();
        properties.insert(key::EXCEPTION_LIST.to_string(), json!([exception]));
        properties.insert(key::EXCEPTION_LEVEL.to_string(), Value::from("error"));
        properties.insert(
            key::EXCEPTION_SOURCE.to_string(),
            Value::from(EXCEPTION_SOURCE),
        );
        properties
    }

    fn bounded_message(&self) -> String {
        let message = self.message.trim();
        let message = if message.is_empty() {
            EMPTY_MESSAGE
        } else {
            message
        };
        bounded_non_empty(message, MAX_TEXT_LENGTH, TextPass::Captured)
            .unwrap_or_else(|| REDACTED.to_string())
    }
}

/// `my_crate::errors::UpstreamTimeout<T>` becomes `UpstreamTimeout`. A trait
/// object or a smart pointer hides the type behind it, so it is a plain
/// `Error`.
fn short_type_name(type_name: &str) -> String {
    let without_generics = type_name.split('<').next().unwrap_or(type_name);
    let short = without_generics
        .rsplit("::")
        .next()
        .unwrap_or(without_generics)
        .trim();
    let hides_the_type = type_name.starts_with("dyn ")
        || type_name.starts_with('&')
        || POINTER_TYPES.contains(&short);
    if hides_the_type || short.is_empty() {
        DEFAULT_ERROR_TYPE.to_string()
    } else {
        short.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UpstreamTimeout;

    fn upstream_timeout() -> Failure {
        let type_name = std::any::type_name::<UpstreamTimeout>();
        Failure::thrown(type_name, "upstream timed out".to_string())
    }

    #[test]
    fn thrown_errors_are_named_by_their_type() {
        let failure = upstream_timeout();
        assert_eq!(
            failure.call_properties()[key::ERROR_TYPE],
            "UpstreamTimeout"
        );
        assert_eq!(
            failure.call_properties()[key::ERROR_MESSAGE],
            "upstream timed out"
        );
    }

    #[test]
    fn an_explicit_label_counts_the_call_but_not_the_exception() {
        let failure = upstream_timeout().with_explicit_type(Some("validation".to_string()));
        assert_eq!(failure.call_properties()[key::ERROR_TYPE], "validation");
        let exception = failure.exception_properties();
        assert_eq!(exception[key::EXCEPTION_LIST][0]["type"], "UpstreamTimeout");
    }

    #[test]
    fn wrappers_hide_the_type() {
        let name = std::any::type_name::<Box<dyn std::error::Error + Send + Sync>>();
        assert_eq!(short_type_name(name), "Error");
    }

    #[test]
    fn generic_types_keep_their_name() {
        assert_eq!(short_type_name("my_crate::FetchError<u8>"), "FetchError");
    }

    #[test]
    fn reported_failures_are_synthetic() {
        let failure = Failure::reported("403 Forbidden".to_string());
        let exception = failure.exception_properties();
        assert_eq!(
            exception[key::EXCEPTION_LIST][0]["mechanism"]["synthetic"],
            true
        );
        assert_eq!(failure.call_properties()[key::ERROR_TYPE], "Error");
    }
}
