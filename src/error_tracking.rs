use std::any::{type_name, type_name_of_val};
use std::collections::HashMap;
use std::error::Error as StdError;

use chrono::{DateTime, TimeZone, Utc};
use derive_builder::Builder;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::feature_flag_evaluations::FeatureFlagEvaluations;
use crate::{Error, Event};

const DEFAULT_MAX_FRAMES: usize = 64;
const DEFAULT_MAX_ERROR_SOURCES: usize = 50;

/// Error Tracking stacktrace and frame classification options.
#[derive(Builder, Clone, Debug)]
#[builder(default)]
pub struct ErrorTrackingOptions {
    capture_stacktrace: bool,
    max_frames: usize,
    max_error_sources: usize,
    in_app_include_paths: Vec<String>,
    in_app_exclude_paths: Vec<String>,
}

impl Default for ErrorTrackingOptions {
    fn default() -> Self {
        Self {
            capture_stacktrace: true,
            max_frames: DEFAULT_MAX_FRAMES,
            max_error_sources: DEFAULT_MAX_ERROR_SOURCES,
            in_app_include_paths: Vec::new(),
            in_app_exclude_paths: Vec::new(),
        }
    }
}

impl ErrorTrackingOptions {
    fn capture_stacktrace(&self) -> bool {
        self.capture_stacktrace
    }

    fn max_frames(&self) -> usize {
        self.max_frames
    }

    fn max_error_sources(&self) -> usize {
        self.max_error_sources
    }

    fn is_in_app_path(&self, filename: &str) -> bool {
        if self
            .in_app_exclude_paths
            .iter()
            .any(|path| filename.contains(path))
        {
            return false;
        }

        if !self.in_app_include_paths.is_empty() {
            return self
                .in_app_include_paths
                .iter()
                .any(|path| filename.contains(path));
        }

        default_in_app_path(filename)
    }

    fn is_in_app_frame(&self, filename: Option<&str>, function: Option<&str>) -> bool {
        if self.in_app_exclude_paths.iter().any(|path| {
            filename.is_some_and(|filename| filename.contains(path))
                || function.is_some_and(|function| function.contains(path))
        }) {
            return false;
        }

        if !self.in_app_include_paths.is_empty() {
            return self.in_app_include_paths.iter().any(|path| {
                filename.is_some_and(|filename| filename.contains(path))
                    || function.is_some_and(|function| function.contains(path))
            });
        }

        if filename.is_some_and(|filename| !self.is_in_app_path(filename)) {
            return false;
        }

        if let Some(function) = function {
            return default_in_app_function(function);
        }

        filename.is_some()
    }
}

/// A PostHog Error Tracking event builder.
#[derive(Clone, Debug)]
pub struct ExceptionCapture {
    distinct_id: Option<String>,
    exception_list: Vec<ExceptionItem>,
    properties: HashMap<String, Value>,
    groups: HashMap<String, String>,
    timestamp: Option<DateTime<Utc>>,
    uuid: Option<Uuid>,
    fingerprint: Option<String>,
    level: String,
}

impl ExceptionCapture {
    /// Build an exception capture from a Rust error.
    pub fn from_error<E>(error: &E) -> Self
    where
        E: StdError + ?Sized,
    {
        Self::from_error_with_options(error, &ErrorTrackingOptions::default())
    }

    /// Build an exception capture from a Rust error with custom options.
    pub fn from_error_with_options<E>(error: &E, options: &ErrorTrackingOptions) -> Self
    where
        E: StdError + ?Sized,
    {
        let mut exception_list = Vec::new();
        let stacktrace = if options.capture_stacktrace() {
            Some(capture_application_stacktrace(options))
        } else {
            None
        };

        exception_list.push(ExceptionItem {
            exception_type: simple_type_name(type_name::<E>()),
            value: error_value(error),
            mechanism: ExceptionMechanism::default(),
            stacktrace,
        });

        let mut source = error.source();
        while let Some(err) = source {
            if exception_list.len() >= options.max_error_sources() {
                break;
            }
            exception_list.push(ExceptionItem {
                exception_type: source_type_name(err),
                value: error_value(err),
                mechanism: ExceptionMechanism::default(),
                stacktrace: None,
            });
            source = err.source();
        }

        link_exception_chain(&mut exception_list);

        Self::from_exception_list(exception_list)
    }

    /// Build an exception capture from an arbitrary type/message pair.
    pub fn from_message<T: Into<String>, V: Into<String>>(exception_type: T, value: V) -> Self {
        Self::from_message_with_options(exception_type, value, &ErrorTrackingOptions::default())
    }

    /// Build an exception capture from an arbitrary type/message pair with custom options.
    pub fn from_message_with_options<T: Into<String>, V: Into<String>>(
        exception_type: T,
        value: V,
        options: &ErrorTrackingOptions,
    ) -> Self {
        let stacktrace = if options.capture_stacktrace() {
            Some(capture_application_stacktrace(options))
        } else {
            None
        };
        Self::from_exception_list(vec![ExceptionItem {
            exception_type: exception_type.into(),
            value: value.into(),
            mechanism: ExceptionMechanism::default(),
            stacktrace,
        }])
    }

    /// Build an exception capture from normalized exception items.
    pub fn from_exception_list(exception_list: Vec<ExceptionItem>) -> Self {
        Self {
            distinct_id: None,
            exception_list,
            properties: HashMap::new(),
            groups: HashMap::new(),
            timestamp: None,
            uuid: None,
            fingerprint: None,
            level: "error".to_string(),
        }
    }

    /// Attach a distinct ID. Without this, the exception is sent personlessly.
    pub fn with_distinct_id<S: Into<String>>(mut self, distinct_id: S) -> Self {
        self.distinct_id = Some(distinct_id.into());
        self
    }

    /// Add a custom property to the exception event.
    pub fn with_prop<K: Into<String>, P: Serialize>(
        mut self,
        key: K,
        prop: P,
    ) -> Result<Self, Error> {
        let as_json =
            serde_json::to_value(prop).map_err(|e| Error::Serialization(e.to_string()))?;
        self.properties.insert(key.into(), as_json);
        Ok(self)
    }

    /// Capture this exception as a group event.
    pub fn with_group<N: Into<String>, I: Into<String>>(
        mut self,
        group_name: N,
        group_id: I,
    ) -> Self {
        self.groups.insert(group_name.into(), group_id.into());
        self
    }

    /// Attach a feature flag snapshot to this exception event.
    pub fn with_flags(mut self, flags: &FeatureFlagEvaluations) -> Self {
        for (key, value) in flags.event_properties() {
            self.properties.insert(key, value);
        }
        self
    }

    /// Set the exception event timestamp.
    pub fn with_timestamp<Tz>(mut self, timestamp: DateTime<Tz>) -> Self
    where
        Tz: TimeZone,
    {
        self.timestamp = Some(timestamp.with_timezone(&Utc));
        self
    }

    /// Override the auto-generated UUID for this exception event.
    pub fn with_uuid(mut self, uuid: Uuid) -> Self {
        self.uuid = Some(uuid);
        self
    }

    /// Set a custom exception fingerprint.
    pub fn with_fingerprint<S: Into<String>>(mut self, fingerprint: S) -> Self {
        self.fingerprint = Some(fingerprint.into());
        self
    }

    /// Convert this builder into a PostHog capture event.
    pub fn into_event(self) -> Result<Event, Error> {
        let mut event = match self.distinct_id {
            Some(distinct_id) if !distinct_id.is_empty() => {
                Event::new("$exception".to_string(), distinct_id)
            }
            _ => Event::new_anon("$exception".to_string()),
        };

        for (key, value) in self.properties {
            event.insert_prop(key, value)?;
        }
        for (group_name, group_id) in self.groups {
            event.add_group(&group_name, &group_id);
        }

        event.insert_prop("$exception_level", self.level)?;
        if let Some(fingerprint) = self.fingerprint {
            event.insert_prop("$exception_fingerprint", fingerprint)?;
        }
        event.insert_prop("$exception_list", self.exception_list)?;

        if let Some(timestamp) = self.timestamp {
            event.set_timestamp(timestamp)?;
        }
        if let Some(uuid) = self.uuid {
            event.set_uuid(uuid);
        }

        Ok(event)
    }
}

/// A normalized exception entry in `$exception_list`.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ExceptionItem {
    #[serde(rename = "type")]
    pub exception_type: String,
    pub value: String,
    pub mechanism: ExceptionMechanism,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stacktrace: Option<ExceptionStacktrace>,
}

/// How an exception was captured.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ExceptionMechanism {
    #[serde(rename = "type")]
    pub mechanism_type: String,
    pub handled: bool,
    pub synthetic: bool,
    /// Position in the cause chain, `0` being the outermost error. Only set when
    /// the exception is part of a multi-error chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_id: Option<usize>,
    /// `exception_id` of the error this one was a source of.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<usize>,
}

impl Default for ExceptionMechanism {
    fn default() -> Self {
        Self {
            mechanism_type: "generic".to_string(),
            handled: true,
            synthetic: false,
            exception_id: None,
            parent_id: None,
        }
    }
}

/// A normalized stacktrace.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct ExceptionStacktrace {
    #[serde(rename = "type")]
    pub stacktrace_type: String,
    pub frames: Vec<StackFrame>,
}

impl ExceptionStacktrace {
    fn raw(frames: Vec<StackFrame>) -> Self {
        Self {
            stacktrace_type: "raw".to_string(),
            frames,
        }
    }
}

/// A normalized stack frame.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct StackFrame {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(rename = "lineno")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_no: Option<u32>,
    pub function: String,
    pub lang: String,
    pub in_app: bool,
    pub synthetic: bool,
    pub resolved: bool,
    pub platform: String,
}

// Captures Rust stack traces for Error Tracking.
fn capture_frames_current_first(options: &ErrorTrackingOptions, skip: usize) -> Vec<StackFrame> {
    let mut frames = Vec::new();
    let mut skipped = 0usize;

    backtrace::trace(|frame| {
        if skipped < skip {
            skipped += 1;
            return true;
        }

        let mut pushed = false;
        backtrace::resolve_frame(frame, |symbol| {
            if pushed {
                return;
            }

            let filename = symbol.filename().map(path_to_string);
            let function = symbol
                .name()
                .map(|name| normalize_function_name(&name.to_string()));

            if filename.is_none() && function.is_none() {
                return;
            }

            let in_app = options.is_in_app_frame(filename.as_deref(), function.as_deref());

            frames.push(StackFrame {
                filename,
                line_no: symbol.lineno(),
                function: function.unwrap_or_default(),
                lang: "rust".to_string(),
                in_app,
                synthetic: false,
                resolved: true,
                platform: "rust".to_string(),
            });
            pushed = true;
        });

        true
    });

    frames
}

fn trim_to_max_frames(frames: &mut Vec<StackFrame>, max_frames: usize) {
    if frames.len() > max_frames {
        frames.truncate(max_frames);
    }
}

fn capture_application_stacktrace(options: &ErrorTrackingOptions) -> ExceptionStacktrace {
    let mut frames = capture_frames_current_first(options, 0);
    while frames
        .first()
        .map(|frame| is_internal_capture_frame(&frame.function))
        .unwrap_or(false)
    {
        frames.remove(0);
    }
    trim_to_max_frames(&mut frames, options.max_frames());

    ExceptionStacktrace::raw(frames)
}

fn is_internal_capture_frame(function: &str) -> bool {
    function.starts_with("backtrace::")
        || function.contains("DefaultStackTraceExtractor::capture")
        || function.contains("capture_frames_current_first")
        || function.contains("capture_application_stacktrace")
        || function.contains("ExceptionCapture::from_error")
        || function.contains("ExceptionCapture::from_message")
        || function.contains("Client::exception_from_error")
        || function.contains("Client::capture_error")
        || function.contains("global::capture_error")
}

fn path_to_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Best-effort, human-readable exception type from a Rust type name.
///
/// Keeps the full module path (minus generic arguments and `&`/`dyn` markers) so
/// types whose leaf name is the idiomatic `Error` — `std::io::Error`,
/// `serde_json::Error`, `mycrate::Error` — stay distinguishable rather than all
/// collapsing to a single `"Error"`.
fn simple_type_name(type_name: &str) -> String {
    let trimmed = type_name.trim().trim_start_matches('&').trim();
    let trimmed = trimmed.strip_prefix("dyn ").unwrap_or(trimmed).trim();
    let trimmed = trimmed
        .split_once('<')
        .map_or(trimmed, |(outer_type, _)| outer_type)
        .trim_end();
    // A type-erased `dyn Error` only reports the trait itself, which carries no
    // concrete type information, so collapse it to a bare "Error".
    if trimmed.is_empty() || trimmed == "core::error::Error" || trimmed == "std::error::Error" {
        return "Error".to_string();
    }
    trimmed.to_string()
}

/// Type name for a chained source.
///
/// Sources are exposed as `&dyn Error`, which is type-erased: `type_name_of_val`
/// can only report the trait, not the original type. Chained sources therefore
/// carry the value/message but report a generic `"Error"` type — the concrete
/// type of a `dyn Error` cannot be recovered on stable Rust.
fn source_type_name(error: &(dyn StdError + 'static)) -> String {
    simple_type_name(type_name_of_val(error))
}

/// Link a multi-error chain so each source points at the error it came from,
/// mirroring the `$exception_list` chaining other PostHog SDKs emit. Single
/// exceptions are left unlinked.
fn link_exception_chain(exception_list: &mut [ExceptionItem]) {
    if exception_list.len() < 2 {
        return;
    }
    for (index, item) in exception_list.iter_mut().enumerate() {
        item.mechanism.exception_id = Some(index);
        if index > 0 {
            item.mechanism.parent_id = Some(index - 1);
            item.mechanism.mechanism_type = "chained".to_string();
        }
    }
}

fn error_value<E>(error: &E) -> String
where
    E: StdError + ?Sized,
{
    let value = error.to_string();
    if value.is_empty() {
        "Error".to_string()
    } else {
        value
    }
}

fn normalize_function_name(function: &str) -> String {
    match function.rsplit_once("::") {
        Some((prefix, suffix)) if is_rust_symbol_hash(suffix) => prefix.to_string(),
        _ => function.to_string(),
    }
}

fn is_rust_symbol_hash(segment: &str) -> bool {
    segment.len() >= 9
        && segment.starts_with('h')
        && segment[1..].chars().all(|ch| ch.is_ascii_hexdigit())
}

fn default_in_app_path(filename: &str) -> bool {
    let normalized = filename.replace('\\', "/");
    if normalized.contains("/.cargo/registry/")
        || normalized.contains("/.cargo/git/")
        || normalized.contains("/rustc/")
        || normalized.contains("/rustc-")
        || normalized.contains("/library/alloc/src/")
        || normalized.contains("/library/core/src/")
        || normalized.contains("/library/proc_macro/src/")
        || normalized.contains("/library/std/src/")
        || normalized.contains("/library/test/src/")
        || normalized.contains("/toolchains/")
        || normalized.contains("/target/")
        || normalized.contains("/vendor/")
    {
        return false;
    }

    true
}

fn default_in_app_function(function: &str) -> bool {
    if function.is_empty() || function == "_main" {
        return false;
    }

    !matches!(
        function
            .trim_start_matches('<')
            .split("::")
            .next()
            .unwrap_or_default(),
        "alloc" | "backtrace" | "core" | "posthog_rs" | "reqwest" | "std" | "tokio"
    )
}

#[cfg(test)]
mod tests {
    use std::error::Error as StdError;
    use std::fmt;

    use serde_json::json;

    use super::*;
    use crate::event::InnerEvent;

    #[derive(Debug)]
    struct OuterError {
        source: InnerError,
    }

    impl fmt::Display for OuterError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "checkout failed")
        }
    }

    impl StdError for OuterError {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(&self.source)
        }
    }

    #[derive(Debug)]
    struct InnerError;

    impl fmt::Display for InnerError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "database unavailable")
        }
    }

    impl StdError for InnerError {}

    #[derive(Debug)]
    struct BorrowedError<'a>(&'a str);

    impl fmt::Display for BorrowedError<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl StdError for BorrowedError<'_> {}

    fn event_json(capture: ExceptionCapture) -> Value {
        let mut event = capture.into_event().unwrap();
        event.prepare_for_v0();
        serde_json::to_value(InnerEvent::new(event, "api-key".to_string())).unwrap()
    }

    #[test]
    fn from_error_builds_exception_list_with_stacktrace() {
        let error = OuterError { source: InnerError };
        let json = event_json(ExceptionCapture::from_error(&error).with_distinct_id("user-1"));

        assert_eq!(json["event"], "$exception");
        assert_eq!(json["distinct_id"], "user-1");
        assert_eq!(json["properties"]["$exception_level"], "error");

        let exception_list = json["properties"]["$exception_list"].as_array().unwrap();
        assert!(exception_list[0]["type"]
            .as_str()
            .unwrap()
            .ends_with("OuterError"));
        assert_eq!(exception_list[0]["value"], "checkout failed");
        assert_eq!(exception_list[0]["mechanism"]["type"], "generic");
        assert_eq!(exception_list[0]["mechanism"]["handled"], true);
        assert_eq!(exception_list[0]["mechanism"]["synthetic"], false);
        assert_eq!(exception_list[0]["mechanism"]["exception_id"], 0);
        assert_eq!(exception_list[0]["stacktrace"]["type"], "raw");
        assert_eq!(exception_list[1]["value"], "database unavailable");
        assert_eq!(exception_list[1]["mechanism"]["type"], "chained");
        assert_eq!(exception_list[1]["mechanism"]["exception_id"], 1);
        assert_eq!(exception_list[1]["mechanism"]["parent_id"], 0);

        let frames = exception_list[0]["stacktrace"]["frames"]
            .as_array()
            .expect("expected stack frames");
        let top_frame = frames.first().expect("expected top frame");
        assert_eq!(top_frame["platform"], "rust");
        assert_eq!(top_frame["lang"], "rust");
        assert_eq!(top_frame["resolved"], true);
        let top_function = top_frame["function"].as_str().unwrap_or_default();
        assert!(
            top_function.contains("from_error_builds_exception_list_with_stacktrace"),
            "expected user frame last, got {:?}",
            top_function
        );
        assert!(
            !top_function.contains("ExceptionCapture"),
            "expected SDK frames to be skipped, got {:?}",
            top_function
        );
    }

    #[test]
    fn from_error_accepts_borrowed_error_types() {
        let message = String::from("borrowed parse failure");
        let error = BorrowedError(&message);
        let json = event_json(ExceptionCapture::from_error(&error));

        assert_eq!(
            json["properties"]["$exception_list"][0]["value"],
            "borrowed parse failure"
        );
    }

    #[test]
    fn personless_capture_disables_person_profile() {
        let json = event_json(ExceptionCapture::from_message("Error", "no user context"));

        assert_eq!(json["event"], "$exception");
        assert_eq!(json["properties"]["$process_person_profile"], false);
    }

    #[test]
    fn custom_properties_cannot_override_reserved_exception_payload() {
        let capture = ExceptionCapture::from_message("Error", "real message")
            .with_prop("$exception_list", json!([{"value": "fake"}]))
            .unwrap();

        let json = event_json(capture);
        assert_eq!(
            json["properties"]["$exception_list"][0]["value"],
            "real message"
        );
    }

    #[test]
    fn options_can_disable_stacktrace_and_limit_sources() {
        let options = ErrorTrackingOptionsBuilder::default()
            .capture_stacktrace(false)
            .max_error_sources(1usize)
            .build()
            .unwrap();
        let error = OuterError { source: InnerError };
        let json = event_json(ExceptionCapture::from_error_with_options(&error, &options));

        let exception_list = json["properties"]["$exception_list"].as_array().unwrap();
        assert_eq!(exception_list.len(), 1);
        assert!(exception_list[0].get("stacktrace").is_none());
    }

    #[test]
    fn in_app_path_defaults_and_overrides_are_applied() {
        let options = ErrorTrackingOptions::default();
        assert!(options.is_in_app_path("/app/src/main.rs"));
        assert!(!options.is_in_app_path("/home/user/.cargo/registry/src/lib.rs"));
        assert!(!options.is_in_app_path(
            "/private/tmp/nix-build-rustc-1.91.1/rustc-1.91.1-src/library/core/src/ops/function.rs"
        ));
        assert!(options.is_in_app_frame(None, Some("checkout_service::submit")));
        assert!(!options.is_in_app_frame(None, Some("std::rt::lang_start")));
        assert!(!options.is_in_app_frame(None, Some("core::ops::function::FnOnce::call_once")));
        assert!(!options.is_in_app_frame(None, Some("posthog_rs::client::Client::capture_error")));
        assert!(!options.is_in_app_frame(None, Some("_main")));

        let options = ErrorTrackingOptionsBuilder::default()
            .in_app_include_paths(vec!["/service/".to_string(), "my_service::".to_string()])
            .in_app_exclude_paths(vec!["/service/vendor/".to_string()])
            .build()
            .unwrap();

        assert!(options.is_in_app_path("/service/src/main.rs"));
        assert!(!options.is_in_app_path("/other/src/main.rs"));
        assert!(!options.is_in_app_path("/service/vendor/lib.rs"));
        assert!(options.is_in_app_frame(None, Some("my_service::checkout")));
        assert!(!options.is_in_app_frame(None, Some("other_service::checkout")));
    }

    #[test]
    fn function_names_strip_rust_symbol_hashes() {
        assert_eq!(
            normalize_function_name("checkout_service::submit::h9ae4817223dd0b22"),
            "checkout_service::submit"
        );
        assert_eq!(
            normalize_function_name("std::rt::lang_start::{{closure}}::ha1fd5c62e470a8cc"),
            "std::rt::lang_start::{{closure}}"
        );
        assert_eq!(
            normalize_function_name("checkout_service::submit"),
            "checkout_service::submit"
        );
    }

    #[test]
    fn type_names_keep_path_and_strip_generics() {
        // Full path is kept so idiomatic `Error`-named types stay distinguishable.
        assert_eq!(
            simple_type_name("std::io::error::Error"),
            "std::io::error::Error"
        );
        assert_eq!(
            simple_type_name("mycrate::CheckoutError"),
            "mycrate::CheckoutError"
        );
        assert_eq!(simple_type_name("mycrate::Error"), "mycrate::Error");

        // Generic arguments and `&`/`dyn` markers are stripped.
        assert_eq!(simple_type_name("foo::Bar<baz::Qux>"), "foo::Bar");
        assert_eq!(
            simple_type_name(type_name::<Box<dyn StdError>>()),
            "alloc::boxed::Box"
        );

        // A type-erased `dyn Error` carries no concrete type, so it degrades to "Error".
        assert_eq!(simple_type_name("dyn core::error::Error"), "Error");
        assert_eq!(simple_type_name(type_name::<&dyn StdError>()), "Error");
    }

    #[test]
    fn application_stacktrace_applies_max_frames_after_skipping_sdk_frames() {
        let options = ErrorTrackingOptionsBuilder::default()
            .max_frames(1usize)
            .build()
            .unwrap();
        let json = event_json(ExceptionCapture::from_message_with_options(
            "SmallStack",
            "keeps user frame",
            &options,
        ));

        let frames = json["properties"]["$exception_list"][0]["stacktrace"]["frames"]
            .as_array()
            .expect("expected stack frames");
        assert_eq!(frames.len(), 1);
        let top_function = frames[0]["function"].as_str().unwrap_or_default();
        assert!(
            top_function.contains("application_stacktrace_applies_max_frames"),
            "expected user frame after SDK frame filtering, got {:?}",
            top_function
        );
        assert!(
            !top_function.contains("ExceptionCapture"),
            "expected SDK frames to be skipped, got {:?}",
            top_function
        );
    }

    #[test]
    fn stacktrace_keeps_top_frame_first() {
        fn capture(options: &ErrorTrackingOptions) -> ExceptionStacktrace {
            let mut frames = capture_frames_current_first(options, 0);
            trim_to_max_frames(&mut frames, options.max_frames());
            ExceptionStacktrace::raw(frames)
        }

        let options = ErrorTrackingOptionsBuilder::default()
            .max_frames(8usize)
            .build()
            .unwrap();
        let frames = capture(&options).frames;
        let functions: Vec<&str> = frames
            .iter()
            .map(|frame| frame.function.as_str())
            .filter(|function| !function.is_empty())
            .collect();

        let capture_index = functions
            .iter()
            .position(|function| function.contains("stacktrace_keeps_top_frame_first::capture"))
            .expect("expected capture frame");
        let test_index = functions
            .iter()
            .position(|function| function.ends_with("stacktrace_keeps_top_frame_first"))
            .expect("expected test frame");

        assert!(
            capture_index < test_index,
            "expected top frame before caller, got {:?}",
            functions
        );
    }
}
