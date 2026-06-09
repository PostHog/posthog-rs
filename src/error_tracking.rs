use std::any::{type_name, type_name_of_val};
use std::error::Error as StdError;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use derive_builder::Builder;
use reqwest::blocking::Client as BlockingHttpClient;
use reqwest::header::CONTENT_TYPE;
use serde::Serialize;
use serde_json::Value;
use tracing::debug;

use crate::client::ClientOptions;
use crate::endpoints::Endpoint;
use crate::event::InnerEvent;
use crate::{Error, Event};

const DEFAULT_MAX_FRAMES: usize = 64;
const DEFAULT_MAX_ERROR_SOURCES: usize = 50;
static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

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

/// Install process-wide panic autocapture.
///
/// Captured panics are sent personlessly using a synchronous best-effort request,
/// then the previously installed panic hook is called.
pub fn install_panic_hook<C: Into<ClientOptions>>(options: C) -> Result<(), Error> {
    if PANIC_HOOK_INSTALLED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(Error::PanicHookAlreadyInstalled);
    }

    let options = options.into();
    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        match panic::catch_unwind(AssertUnwindSafe(|| capture_panic(&options, panic_info))) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => debug!(error = %error, "failed to capture panic"),
            Err(_) => debug!("panic autocapture failed unexpectedly"),
        }

        previous_hook(panic_info);
    }));

    Ok(())
}

/// Optional context for `capture_exception_with`: person identity, custom
/// properties, groups, and exception fingerprint/level.
///
/// All fields are optional. An empty options set (`new()` / `Default`)
/// captures the exception personlessly with no extra context.
#[derive(Clone, Debug, Default)]
pub struct CaptureExceptionOptions {
    distinct_id: Option<String>,
    properties: Vec<(String, Value)>,
    groups: Vec<(String, String)>,
    fingerprint: Option<String>,
    level: Option<String>,
}

impl CaptureExceptionOptions {
    /// Create an empty options set: personless, no extra context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Associate the exception with a person.
    pub fn distinct_id<S: Into<String>>(mut self, distinct_id: S) -> Self {
        self.distinct_id = Some(distinct_id.into());
        self
    }

    /// Add a custom property to the exception event.
    pub fn property<K: Into<String>, V: Serialize>(
        mut self,
        key: K,
        value: V,
    ) -> Result<Self, Error> {
        let value = serde_json::to_value(value).map_err(|e| Error::Serialization(e.to_string()))?;
        self.properties.push((key.into(), value));
        Ok(self)
    }

    /// Capture the exception as a group event.
    pub fn group<N: Into<String>, I: Into<String>>(mut self, group_name: N, group_id: I) -> Self {
        self.groups.push((group_name.into(), group_id.into()));
        self
    }

    /// Set a custom exception fingerprint.
    pub fn fingerprint<S: Into<String>>(mut self, fingerprint: S) -> Self {
        self.fingerprint = Some(fingerprint.into());
        self
    }

    /// Set the exception severity level. Defaults to `"error"`.
    pub fn level<S: Into<String>>(mut self, level: S) -> Self {
        self.level = Some(level.into());
        self
    }
}

/// Build a `$exception` [`Event`] from a Rust error and capture options.
pub(crate) fn build_exception_event<E>(
    error: &E,
    options: CaptureExceptionOptions,
) -> Result<Event, Error>
where
    E: StdError + ?Sized,
{
    let CaptureExceptionOptions {
        distinct_id,
        properties,
        groups,
        fingerprint,
        level,
    } = options;

    let mut exception = Exception::from_error(error);
    if let Some(fingerprint) = fingerprint {
        exception.set_fingerprint(fingerprint);
    }
    if let Some(level) = level {
        exception.set_level(level);
    }

    let mut event = match distinct_id {
        Some(distinct_id) => exception.into_event(distinct_id),
        None => exception.into_event_anon(),
    };
    for (key, value) in properties {
        event.insert_prop(key, value)?;
    }
    for (group_name, group_id) in groups {
        event.add_group(&group_name, &group_id);
    }
    Ok(event)
}

/// A PostHog Error Tracking exception payload.
///
/// Holds only exception-specific data. Identity, custom properties, groups,
/// feature flags, and timestamps are attached by converting into an [`Event`]
/// with [`Exception::into_event`] / [`Exception::into_event_anon`] and using
/// the standard Event API.
///
/// Client-level [`ErrorTrackingOptions`] (stacktrace capture, in-app
/// classification, frame limits) are applied by the capturing client, so an
/// exception built anywhere always honors the client configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exception {
    items: Vec<ExceptionItem>,
    // SDK-captured raw frames pending client policy (in-app classification,
    // trimming, stacktrace opt-out), applied in finalize_exception and attached
    // to items[0]. None when the caller supplied stacktraces directly via
    // from_exception_list.
    captured_frames: Option<Vec<StackFrame>>,
    fingerprint: Option<String>,
    level: String,
}

impl Exception {
    /// Build an exception from a Rust error, capturing the current stacktrace
    /// and walking the `source()` chain.
    pub fn from_error<E>(error: &E) -> Self
    where
        E: StdError + ?Sized,
    {
        let mut items = vec![ExceptionItem {
            exception_type: simple_type_name(type_name::<E>()),
            value: error_value(error),
            mechanism: ExceptionMechanism::default(),
            stacktrace: None,
        }];

        let mut source = error.source();
        while let Some(err) = source {
            // Hard safety bound for pathological/cyclic source chains; the
            // client's max_error_sources is applied at capture time.
            if items.len() >= DEFAULT_MAX_ERROR_SOURCES {
                break;
            }
            items.push(ExceptionItem {
                exception_type: source_type_name(err),
                value: error_value(err),
                mechanism: ExceptionMechanism::default(),
                stacktrace: None,
            });
            source = err.source();
        }

        link_exception_chain(&mut items);

        Self {
            items,
            captured_frames: Some(capture_raw_application_frames()),
            fingerprint: None,
            level: "error".to_string(),
        }
    }

    /// Build an exception from an arbitrary type/message pair, capturing the
    /// current stacktrace.
    // Only exercised by tests today; kept as the message-capture seam.
    #[allow(dead_code)]
    pub fn from_message<T: Into<String>, V: Into<String>>(exception_type: T, value: V) -> Self {
        Self {
            items: vec![ExceptionItem {
                exception_type: exception_type.into(),
                value: value.into(),
                mechanism: ExceptionMechanism::default(),
                stacktrace: None,
            }],
            captured_frames: Some(capture_raw_application_frames()),
            fingerprint: None,
            level: "error".to_string(),
        }
    }

    /// Build an exception from normalized exception items.
    ///
    /// Caller-provided stacktraces are sent as-is: no stacktrace is captured
    /// and client-side frame classification is not applied.
    // Unused: kept as the seam for caller-normalized exception payloads.
    #[allow(dead_code)]
    pub fn from_exception_list(items: Vec<ExceptionItem>) -> Self {
        Self {
            items,
            captured_frames: None,
            fingerprint: None,
            level: "error".to_string(),
        }
    }

    /// Build an exception from a panic, capturing the current stacktrace.
    #[allow(deprecated)]
    fn from_panic_info(panic_info: &panic::PanicInfo<'_>) -> Self {
        Self {
            items: vec![ExceptionItem {
                exception_type: "Panic".to_string(),
                value: panic_message(panic_info),
                mechanism: ExceptionMechanism {
                    mechanism_type: "panic".to_string(),
                    handled: false,
                    synthetic: false,
                    exception_id: None,
                    parent_id: None,
                },
                stacktrace: None,
            }],
            captured_frames: Some(capture_raw_panic_frames()),
            fingerprint: None,
            level: "error".to_string(),
        }
    }

    /// Set a custom exception fingerprint.
    pub fn set_fingerprint<S: Into<String>>(&mut self, fingerprint: S) {
        self.fingerprint = Some(fingerprint.into());
    }

    /// Set the exception severity level. Defaults to `"error"`.
    pub fn set_level<S: Into<String>>(&mut self, level: S) {
        self.level = level.into();
    }

    /// Convert into an identified `$exception` [`Event`].
    ///
    /// Use the standard [`Event`] API to attach custom properties, groups,
    /// feature flags, or a timestamp before capturing.
    pub fn into_event<S: Into<String>>(self, distinct_id: S) -> Event {
        let mut event = Event::new("$exception".to_string(), distinct_id.into());
        event.exception = Some(self);
        event
    }

    /// Convert into a personless `$exception` [`Event`].
    pub fn into_event_anon(self) -> Event {
        let mut event = Event::new_anon("$exception");
        event.exception = Some(self);
        event
    }
}

/// Apply client-level Error Tracking options to an event's pending exception
/// payload and write the reserved `$exception_*` properties.
///
/// Runs at capture time inside the client, so exception constructors never
/// need client options — and after user-set properties, so reserved keys
/// can't be overridden.
pub(crate) fn finalize_exception(
    event: &mut Event,
    options: &ErrorTrackingOptions,
) -> Result<(), Error> {
    let Some(exception) = event.exception.take() else {
        return Ok(());
    };
    let Exception {
        mut items,
        captured_frames,
        fingerprint,
        level,
    } = exception;
    if items.is_empty() {
        return Ok(());
    }

    items.truncate(options.max_error_sources().max(1));
    if options.capture_stacktrace() {
        if let Some(mut frames) = captured_frames {
            for frame in frames.iter_mut() {
                let function = (!frame.function.is_empty()).then_some(frame.function.as_str());
                frame.in_app = options.is_in_app_frame(frame.filename.as_deref(), function);
            }
            trim_to_max_frames(&mut frames, options.max_frames());
            items[0].stacktrace = Some(ExceptionStacktrace::raw(frames));
        }
    }

    event.insert_prop("$exception_level", level)?;
    if let Some(fingerprint) = fingerprint {
        event.insert_prop("$exception_fingerprint", fingerprint)?;
    }
    event.insert_prop("$exception_list", items)?;
    Ok(())
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

// Captures raw Rust stack traces for Error Tracking. Frames are unclassified
// at this point: in-app classification and trimming are client policy, applied
// at capture time by finalize_exception.
fn capture_frames_current_first(skip: usize) -> Vec<StackFrame> {
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

            frames.push(StackFrame {
                filename,
                line_no: symbol.lineno(),
                function: function.unwrap_or_default(),
                lang: "rust".to_string(),
                in_app: false,
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

/// Capture the current raw stacktrace, dropping the SDK's own capture frames
/// so the caller's frame comes first.
fn capture_raw_application_frames() -> Vec<StackFrame> {
    let mut frames = capture_frames_current_first(0);
    while frames
        .first()
        .map(|frame| is_internal_capture_frame(&frame.function))
        .unwrap_or(false)
    {
        frames.remove(0);
    }

    frames
}

fn is_internal_capture_frame(function: &str) -> bool {
    function.starts_with("backtrace::")
        || function.contains("capture_frames_current_first")
        || function.contains("capture_raw_application_frames")
        || function.contains("Exception::from_error")
        || function.contains("Exception::from_message")
        || function.contains("build_exception_event")
        || function.contains("Client::capture_exception")
        || function.contains("global::capture_exception")
}

fn capture_raw_panic_frames() -> Vec<StackFrame> {
    let mut frames = capture_frames_current_first(0);
    while frames
        .first()
        .map(|frame| is_internal_panic_frame(&frame.function))
        .unwrap_or(false)
    {
        frames.remove(0);
    }

    frames
}

fn is_internal_panic_frame(function: &str) -> bool {
    is_internal_capture_frame(function)
        || function.contains("capture_panic")
        || function.contains("capture_raw_panic_frames")
        || function.contains("install_panic_hook")
        || function.contains("Exception::from_panic_info")
        || function.contains("AssertUnwindSafe")
        || function.starts_with("core::ops::function::FnOnce::call_once")
        || function.starts_with("std::panicking::")
        || function.starts_with("core::panicking::")
        || function.starts_with("std::sys::backtrace::")
        || function == "___rust_try"
        || function.contains("rust_begin_unwind")
}

#[allow(deprecated)]
fn capture_panic(options: &ClientOptions, panic_info: &panic::PanicInfo<'_>) -> Result<(), Error> {
    if options.is_disabled() {
        return Ok(());
    }

    let mut event = Exception::from_panic_info(panic_info).into_event_anon();
    if let Some(location) = panic_info.location() {
        event.insert_prop("$exception_panic_file", location.file())?;
        event.insert_prop("$exception_panic_line", location.line())?;
        event.insert_prop("$exception_panic_column", location.column())?;
    }

    send_panic_exception(options, event)
}

fn send_panic_exception(options: &ClientOptions, mut event: Event) -> Result<(), Error> {
    // Panic autocapture uses a temporary synchronous V0 send for the MVP: the
    // hook runs during unwinding and cannot rely on the normal Capture V1
    // client path. Keep this prep local until panic capture gets a blocking V1
    // sender instead of making V0 prep a feature-agnostic client helper.
    prepare_panic_v0_event(&mut event, options)?;
    let payload = serde_json::to_string(&InnerEvent::new(event, options.api_key().to_string()))
        .map_err(|e| Error::Serialization(e.to_string()))?;
    let client = BlockingHttpClient::builder()
        .timeout(Duration::from_secs(options.panic_capture_timeout_seconds()))
        .build()
        .map_err(|e| Error::Connection(e.to_string()))?;
    let response = client
        .post(options.endpoints().build_url(Endpoint::Capture))
        .header(CONTENT_TYPE, "application/json")
        .body(payload)
        .send()
        .map_err(|e| Error::Connection(e.to_string()))?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .unwrap_or_else(|_| "Unknown error".to_string());

    match Error::from_http_response(status, body) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Finalize the panic exception payload, apply client-level default
/// properties, and stamp V0 metadata.
fn prepare_panic_v0_event(event: &mut Event, options: &ClientOptions) -> Result<(), Error> {
    finalize_exception(event, options.error_tracking())?;
    let defaults = options.capture_defaults();
    if defaults.disable_geoip {
        event.insert_prop_default("$geoip_disable", Value::Bool(true));
    }
    if defaults.is_server {
        event.insert_prop_default("$is_server", Value::Bool(true));
    }
    event.prepare_for_v0();
    Ok(())
}

#[allow(deprecated)]
fn panic_message(panic_info: &panic::PanicInfo<'_>) -> String {
    let value = panic_info
        .payload()
        .downcast_ref::<&str>()
        .map(|value| (*value).to_string())
        .or_else(|| panic_info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic occurred".to_string());

    if value.is_empty() {
        "panic occurred".to_string()
    } else {
        value
    }
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
    use std::sync::{Arc, Mutex, OnceLock};

    use httpmock::prelude::*;
    use serde_json::{json, Value};

    use super::*;
    use crate::client::ClientOptionsBuilder;
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

    fn finalized_json(mut event: Event) -> Value {
        finalized_json_with(&mut event, &ErrorTrackingOptions::default())
    }

    fn finalized_json_with(event: &mut Event, options: &ErrorTrackingOptions) -> Value {
        finalize_exception(event, options).unwrap();
        event.prepare_for_v0();
        serde_json::to_value(InnerEvent::new(event.clone(), "api-key".to_string())).unwrap()
    }

    fn event_json(exception: Exception) -> Value {
        finalized_json(exception.into_event_anon())
    }

    #[allow(deprecated)]
    type PanicHook = Box<dyn Fn(&panic::PanicInfo<'_>) + Sync + Send + 'static>;

    struct PanicHookReset {
        previous: Option<PanicHook>,
    }

    impl PanicHookReset {
        fn new(previous: PanicHook) -> Self {
            Self {
                previous: Some(previous),
            }
        }

        fn restore(&mut self) {
            if let Some(previous) = self.previous.take() {
                panic::set_hook(previous);
            }
            PANIC_HOOK_INSTALLED.store(false, Ordering::Release);
        }
    }

    impl Drop for PanicHookReset {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                self.restore();
            }
        }
    }

    fn panic_hook_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[inline(never)]
    fn panic_hook_test_panic_site() {
        panic!("panic hook boom");
    }

    #[inline(never)]
    fn panic_hook_disabled_test_panic_site() {
        panic!("disabled panic hook boom");
    }

    fn request_has_panic_payload(req: &HttpMockRequest) -> bool {
        let Some(body) = req.body.as_deref() else {
            return false;
        };
        let Ok(body) = serde_json::from_slice::<Value>(body) else {
            return false;
        };
        let exception = &body["properties"]["$exception_list"][0];
        let first_function = exception["stacktrace"]["frames"][0]["function"]
            .as_str()
            .unwrap_or_default();

        body["event"] == "$exception"
            && body["properties"]["$process_person_profile"] == false
            && exception["type"] == "Panic"
            && exception["value"] == "panic hook boom"
            && exception["mechanism"]["type"] == "panic"
            && exception["mechanism"]["handled"] == false
            && body["properties"]["$exception_panic_file"]
                .as_str()
                .is_some_and(|file| file.contains("error_tracking.rs"))
            && body["properties"]["$exception_panic_line"]
                .as_u64()
                .is_some_and(|line| line > 0)
            && body["properties"]["$exception_panic_column"]
                .as_u64()
                .is_some_and(|column| column > 0)
            && first_function.contains("panic_hook_test_panic_site")
            && !first_function.contains("std::panicking")
            && !first_function.contains("core::panicking")
            && !first_function.contains("install_panic_hook")
    }

    #[test]
    fn panic_hook_sends_personless_exception_and_calls_previous_hook() {
        let _guard = panic_hook_test_lock().lock().unwrap();
        let original_hook = panic::take_hook();
        let mut reset = PanicHookReset::new(original_hook);
        let previous_called = Arc::new(AtomicBool::new(false));
        let previous_called_for_hook = Arc::clone(&previous_called);
        panic::set_hook(Box::new(move |_| {
            previous_called_for_hook.store(true, Ordering::Release);
        }));

        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/i/v0/e/")
                .matches(request_has_panic_payload);
            then.status(200);
        });
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .build()
            .unwrap();

        install_panic_hook(options).unwrap();
        assert!(matches!(
            install_panic_hook("test_api_key"),
            Err(Error::PanicHookAlreadyInstalled)
        ));

        let result = panic::catch_unwind(panic_hook_test_panic_site);
        reset.restore();

        assert!(result.is_err());
        assert!(previous_called.load(Ordering::Acquire));
        capture_mock.assert_hits(1);
    }

    #[test]
    fn disabled_panic_hook_does_not_send() {
        let _guard = panic_hook_test_lock().lock().unwrap();
        let original_hook = panic::take_hook();
        let mut reset = PanicHookReset::new(original_hook);
        panic::set_hook(Box::new(|_| {}));

        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST).path("/i/v0/e/");
            then.status(200);
        });
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .disabled(true)
            .build()
            .unwrap();

        install_panic_hook(options).unwrap();
        let result = panic::catch_unwind(panic_hook_disabled_test_panic_site);
        reset.restore();

        assert!(result.is_err());
        capture_mock.assert_hits(0);
    }

    #[test]
    fn from_error_builds_exception_list_with_stacktrace() {
        let error = OuterError { source: InnerError };
        let json = finalized_json(Exception::from_error(&error).into_event("user-1"));

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
            !top_function.contains("Exception::"),
            "expected SDK frames to be skipped, got {:?}",
            top_function
        );
    }

    #[test]
    fn from_error_accepts_borrowed_error_types() {
        let message = String::from("borrowed parse failure");
        let error = BorrowedError(&message);
        let json = event_json(Exception::from_error(&error));

        assert_eq!(
            json["properties"]["$exception_list"][0]["value"],
            "borrowed parse failure"
        );
    }

    #[test]
    fn personless_capture_disables_person_profile() {
        let json = event_json(Exception::from_message("Error", "no user context"));

        assert_eq!(json["event"], "$exception");
        assert_eq!(json["properties"]["$process_person_profile"], false);
    }

    #[test]
    fn custom_properties_cannot_override_reserved_exception_payload() {
        let mut event = Exception::from_message("Error", "real message").into_event_anon();
        event
            .insert_prop("$exception_list", json!([{"value": "fake"}]))
            .unwrap();

        let json = finalized_json(event);
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
        let mut event = Exception::from_error(&error).into_event_anon();
        let json = finalized_json_with(&mut event, &options);

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
        assert!(
            !options.is_in_app_frame(None, Some("posthog_rs::client::Client::capture_exception"))
        );
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
        let mut event = Exception::from_message("SmallStack", "keeps user frame").into_event_anon();
        let json = finalized_json_with(&mut event, &options);

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
            !top_function.contains("Exception::"),
            "expected SDK frames to be skipped, got {:?}",
            top_function
        );
    }

    #[test]
    fn stacktrace_keeps_top_frame_first() {
        fn capture() -> ExceptionStacktrace {
            let mut frames = capture_frames_current_first(0);
            trim_to_max_frames(&mut frames, 8);
            ExceptionStacktrace::raw(frames)
        }

        let frames = capture().frames;
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

    #[test]
    fn build_exception_event_defaults_to_personless() {
        let error = OuterError { source: InnerError };
        let event = build_exception_event(&error, CaptureExceptionOptions::default()).unwrap();
        let json = finalized_json(event);

        assert_eq!(json["event"], "$exception");
        assert_eq!(json["properties"]["$process_person_profile"], false);
        assert_eq!(json["properties"]["$exception_level"], "error");
    }

    #[test]
    fn build_exception_event_applies_options() {
        let error = OuterError { source: InnerError };
        let options = CaptureExceptionOptions::new()
            .distinct_id("user-1")
            .property("route", "/checkout")
            .unwrap()
            .group("company", "acme")
            .fingerprint("checkout-error")
            .level("warning");
        let event = build_exception_event(&error, options).unwrap();
        let json = finalized_json(event);

        assert_eq!(json["distinct_id"], "user-1");
        assert_eq!(json["properties"]["route"], "/checkout");
        assert_eq!(json["properties"]["$groups"]["company"], "acme");
        assert_eq!(
            json["properties"]["$exception_fingerprint"],
            "checkout-error"
        );
        assert_eq!(json["properties"]["$exception_level"], "warning");
    }
}
