use std::any::{type_name, type_name_of_val};
use std::error::Error as StdError;
use std::io::Write;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use derive_builder::Builder;
use serde::Serialize;
use serde_json::Value;

use crate::{Client, Error, Event};

/// Hard cap on stack frames per exception; frames beyond it are trimmed from
/// the outermost end.
const MAX_FRAMES: usize = 64;
/// Hard cap on the `source()` chain walk, bounding pathological or cyclic
/// error chains.
const MAX_ERROR_SOURCES: usize = 50;

/// Latches the single process-wide panic hook so a second install is rejected.
static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Client-level Error Tracking configuration, applied to every exception the
/// client captures. Set it via [`ErrorTrackingOptionsBuilder`] on
/// `ClientOptions::error_tracking`.
///
/// # Examples
///
/// ```
/// use posthog_rs::ErrorTrackingOptionsBuilder;
///
/// let options = ErrorTrackingOptionsBuilder::default()
///     .capture_stacktrace(true)
///     // Substring patterns match file paths and function symbols, so a
///     // crate prefix marks that crate's frames as not in-app.
///     .in_app_exclude_paths(vec!["other_crate::".to_string()])
///     .build()
///     .unwrap();
/// ```
#[derive(Builder, Clone, Debug)]
#[builder(default)]
pub struct ErrorTrackingOptions {
    /// Capture a stack trace at the `capture_exception` call site and attach
    /// it to the first entry of `$exception_list` (default: `true`).
    ///
    /// The trace shows where the error was *captured*, not where it was
    /// created — a bubbled-up `Err` value carries no stack of its own. The
    /// error type/message chain in `$exception_list` is always sent regardless
    /// of this setting. Disabling it skips the stack walk and per-frame symbol
    /// resolution entirely, which can matter when capturing handled errors in
    /// high-volume paths.
    capture_stacktrace: bool,
    /// Treat only frames matching one of these patterns as in-app. Patterns
    /// are substring matches against a frame's file path *and* function
    /// symbol, so both path fragments (`"/service/"`) and crate prefixes
    /// (`"my_service::"`) work. When empty, built-in defaults apply: frames
    /// from the cargo registry, the standard library, and vendored/target
    /// paths are library frames, everything else is in-app.
    in_app_include_paths: Vec<String>,
    /// Always mark matching frames as not in-app, taking precedence over
    /// includes and defaults. Same matching rules as `in_app_include_paths`
    /// — e.g. `"other_crate::"` excludes every frame of that crate.
    in_app_exclude_paths: Vec<String>,
    /// When `true` (the default), [`crate::init_global`] installs a process-wide
    /// panic hook that captures panics as `$exception` events through the global
    /// client — crash reporting, on by default. Set `false` to opt out.
    ///
    /// This flag only drives the *global* client's automatic install. A
    /// standalone [`Client`] has no process-static home for a `'static` hook, so
    /// route its panics by calling [`install_panic_hook`] with an `Arc<Client>`
    /// yourself; the flag does not affect standalone clients.
    capture_panics: bool,
    /// How long the installed panic hook blocks the panicking thread waiting for
    /// the `$exception` to be sent before letting the panic proceed (default:
    /// 2000 ms). Deliberately short and separate from `shutdown_timeout_ms`: the
    /// hook runs on the dying thread, so a long wait would freeze the crash —
    /// and delay the panic message, which only prints after the flush — whenever
    /// PostHog is slow or unreachable. Only consulted when the panic hook is
    /// installed via [`install_panic_hook`].
    panic_flush_timeout_ms: u64,
}

impl Default for ErrorTrackingOptions {
    fn default() -> Self {
        Self {
            capture_stacktrace: true,
            in_app_include_paths: Vec::new(),
            in_app_exclude_paths: Vec::new(),
            capture_panics: true,
            panic_flush_timeout_ms: 2000,
        }
    }
}

impl ErrorTrackingOptions {
    fn capture_stacktrace(&self) -> bool {
        self.capture_stacktrace
    }

    fn capture_panics(&self) -> bool {
        self.capture_panics
    }

    /// The panic-flush budget as a `Duration` (see `panic_flush_timeout_ms`).
    fn panic_flush_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.panic_flush_timeout_ms)
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

/// Install process-wide panic autocapture, capturing through `client`.
///
/// Most callers don't need this: the global client installs the hook
/// automatically (see [`ErrorTrackingOptions`]'s `capture_panics`, on by
/// default, via [`crate::init_global`]). Use this to capture panics through a
/// *standalone* [`Client`] instead — it takes an `Arc<Client>` because the hook
/// is `'static` and must keep the client alive for the rest of the process.
///
/// On a panic, a personless `$exception` event is built — the panic payload as
/// the exception value, panic-site `$exception_panic_file`/`_line`/`_column`
/// from [`std::panic::Location`], and a call-site stacktrace honoring the
/// client's `capture_stacktrace` — enqueued on `client`'s transport and flushed
/// (time-bounded), then the previously installed hook runs.
///
/// On the panicking thread the hook only enqueues and flushes; the actual HTTP
/// send and any `before_send` hooks run on the transport's background worker,
/// where the panic count is zero, so a panic there is a joinable thread panic
/// rather than the process abort a panic on the hook thread would cause.
///
/// Installing the hook twice returns [`Error::PanicHookAlreadyInstalled`].
pub fn install_panic_hook(client: Arc<Client>) -> Result<(), Error> {
    install_hook(move |panic_info| capture_panic(&client, panic_info))
}

/// If the global client has `capture_panics` enabled (the default), install the
/// panic hook against it. Best-effort and idempotent: a hook installed earlier
/// (e.g. a manual [`install_panic_hook`]) is left in place. Called by
/// `init_global` once the global client is set.
pub(crate) fn maybe_install_global_panic_hook() {
    let Some(client) = crate::global::global_client() else {
        return;
    };
    if !client.error_tracking_options().capture_panics() {
        return;
    }
    // The hook reads the global client at panic time — it lives in a process
    // `static`, so the hook needs no owned handle. `AlreadyInstalled` is benign.
    let _ = install_hook(|panic_info| match crate::global::global_client() {
        Some(client) => capture_panic(client, panic_info),
        None => Ok(()),
    });
}

/// Latch the single process-wide panic hook, then install one that runs
/// `capture` (kept panic-free) and chains the previously installed hook. Returns
/// [`Error::PanicHookAlreadyInstalled`] if a hook is already installed.
#[allow(deprecated)]
fn install_hook<F>(capture: F) -> Result<(), Error>
where
    F: Fn(&panic::PanicInfo<'_>) -> Result<(), Error> + Send + Sync + 'static,
{
    if PANIC_HOOK_INSTALLED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(Error::PanicHookAlreadyInstalled);
    }

    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        // Report capture failures straight to stderr: dispatching through
        // tracing would run arbitrary subscriber code on the panicking thread.
        // catch_unwind cannot prevent a nested-panic abort here (the panic count
        // is already non-zero), so `capture` is kept panic-free — it only
        // enqueues and flushes; the send happens on the worker thread.
        if let Ok(Err(error)) = panic::catch_unwind(AssertUnwindSafe(|| capture(panic_info))) {
            let _ = writeln!(
                std::io::stderr(),
                "posthog-rs: failed to capture panic: {error}"
            );
        }

        previous_hook(panic_info);
    }));

    Ok(())
}

/// Build the panic `$exception` event and route it through `client`'s transport:
/// a non-blocking enqueue followed by a time-bounded synchronous flush, so the
/// event is attempted before the process potentially exits without ever hanging
/// the dying process. `before_send` and the HTTP send run on the worker thread.
#[allow(deprecated)]
fn capture_panic(client: &Client, panic_info: &panic::PanicInfo<'_>) -> Result<(), Error> {
    // A panic on this client's own transport worker thread is almost always a
    // panicking `before_send` (which the worker already catches and logs).
    // Capturing it there would deadlock — a synchronous flush can't be serviced
    // by the worker that's busy running this hook — and would recurse: the
    // captured `$exception` re-enters `before_send` on the worker and panics
    // again. Skip it.
    if client.is_disabled() || client.on_transport_worker() {
        return Ok(());
    }
    let et_options = client.error_tracking_options();
    let event = build_panic_event(panic_info, et_options)?;
    // Enqueue through the tracing-free path: `capture` is `#[instrument]` and
    // warns on a full queue, both of which run subscriber code that's unsafe on
    // the panicking thread (it could panic again -> abort, or wait on a lock the
    // panic site holds -> hang before the previous hook runs).
    client.enqueue_panic_event(event);
    // Time-bounded flush on the panicking thread (before unwinding frees locks).
    // The bound (default 2s, `panic_flush_timeout_ms`) keeps the dying process
    // from hanging — and the panic message, which prints only after this returns,
    // from being delayed — when PostHog is slow/unreachable or a `before_send`
    // hook needs a lock the panic site still holds.
    client.flush_blocking_timeout(et_options.panic_flush_timeout());
    Ok(())
}

/// Build a personless `$exception` event from a panic. The panic-site location
/// is stamped before the reserved `$exception_*` properties so it can't override
/// them.
#[allow(deprecated)]
fn build_panic_event(
    panic_info: &panic::PanicInfo<'_>,
    et_options: &ErrorTrackingOptions,
) -> Result<Event, Error> {
    let exception = Exception::from_panic_info(panic_info, et_options.capture_stacktrace());

    let mut event = Event::new_anon("$exception");
    if let Some(location) = panic_info.location() {
        event.insert_prop("$exception_panic_file", location.file())?;
        event.insert_prop("$exception_panic_line", location.line())?;
        event.insert_prop("$exception_panic_column", location.column())?;
    }
    exception.write_into(&mut event, et_options)?;
    Ok(event)
}

/// Optional context for `capture_exception_with`: person identity, custom
/// properties, groups, and exception fingerprint/level.
///
/// All fields are optional. An empty options set (`new()` / `Default`)
/// captures the exception personlessly with no extra context.
///
/// # Examples
///
/// ```
/// use posthog_rs::CaptureExceptionOptions;
///
/// let options = CaptureExceptionOptions::new()
///     .distinct_id("user-123")
///     .property("route", "/checkout")?
///     .group("company", "acme")
///     .fingerprint("checkout-error")
///     .level("warning");
/// # Ok::<(), posthog_rs::Error>(())
/// ```
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

/// Build a finalized `$exception` [`Event`] from a Rust error, capture
/// options, and the capturing client's Error Tracking configuration.
///
/// All client policy is applied here, eagerly: the stack walk only runs when
/// `capture_stacktrace` is enabled, and in-app classification, frame and
/// source-chain limits, and the reserved `$exception_*` properties are written
/// before the event is returned. The returned event is an ordinary [`Event`].
pub(crate) fn build_exception_event<E>(
    error: &E,
    options: CaptureExceptionOptions,
    et_options: &ErrorTrackingOptions,
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

    let mut exception = Exception::from_error(error, et_options.capture_stacktrace());
    if let Some(fingerprint) = fingerprint {
        exception.set_fingerprint(fingerprint);
    }
    if let Some(level) = level {
        exception.set_level(level);
    }

    let mut event = match distinct_id {
        Some(distinct_id) => Event::new("$exception".to_string(), distinct_id),
        None => Event::new_anon("$exception"),
    };
    for (key, value) in properties {
        event.insert_prop(key, value)?;
    }
    for (group_name, group_id) in groups {
        event.add_group(&group_name, &group_id);
    }

    // Reserved $exception_* properties are written after user-set properties
    // so they can't be overridden.
    exception.write_into(&mut event, et_options)?;
    Ok(event)
}

/// A PostHog Error Tracking exception payload.
///
/// Internal staging type: every construction site lives in this module and is
/// reached through a client method that holds the client's
/// [`ErrorTrackingOptions`], so client policy is applied eagerly when the
/// `$exception` event is built ([`build_exception_event`]). Constructors take
/// only a `capture_stacktrace` cost hint — the stack walk must happen at the
/// capture site or not at all, and disabling it skips the walk entirely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Exception {
    items: Vec<ExceptionItem>,
    // SDK-captured raw frames pending client policy (in-app classification
    // and trimming), applied in write_into and attached to items[0]. None when
    // stacktrace capture is disabled.
    captured_frames: Option<Vec<StackFrame>>,
    fingerprint: Option<String>,
    level: String,
}

impl Exception {
    /// Build an exception from a Rust error, walking the `source()` chain and
    /// capturing the current stacktrace when `capture_stacktrace` is set.
    pub(crate) fn from_error<E>(error: &E, capture_stacktrace: bool) -> Self
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
            if items.len() >= MAX_ERROR_SOURCES {
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
            captured_frames: if capture_stacktrace {
                Some(capture_raw_application_frames())
            } else {
                None
            },
            fingerprint: None,
            level: "error".to_string(),
        }
    }

    /// Build an exception from an arbitrary type/message pair, capturing the
    /// current stacktrace when `capture_stacktrace` is set.
    // Only exercised by tests today; kept as the message-capture seam.
    #[allow(dead_code)]
    pub(crate) fn from_message<T: Into<String>, V: Into<String>>(
        exception_type: T,
        value: V,
        capture_stacktrace: bool,
    ) -> Self {
        Self {
            items: vec![ExceptionItem {
                exception_type: exception_type.into(),
                value: value.into(),
                mechanism: ExceptionMechanism::default(),
                stacktrace: None,
            }],
            captured_frames: if capture_stacktrace {
                Some(capture_raw_application_frames())
            } else {
                None
            },
            fingerprint: None,
            level: "error".to_string(),
        }
    }

    /// Build an exception from a panic, capturing the current stacktrace when
    /// `capture_stacktrace` is set.
    #[allow(deprecated)]
    fn from_panic_info(panic_info: &panic::PanicInfo<'_>, capture_stacktrace: bool) -> Self {
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
            captured_frames: if capture_stacktrace {
                Some(capture_raw_panic_frames())
            } else {
                None
            },
            fingerprint: None,
            level: "error".to_string(),
        }
    }

    /// Set a custom exception fingerprint.
    pub(crate) fn set_fingerprint<S: Into<String>>(&mut self, fingerprint: S) {
        self.fingerprint = Some(fingerprint.into());
    }

    /// Set the exception severity level. Defaults to `"error"`.
    pub(crate) fn set_level<S: Into<String>>(&mut self, level: S) {
        self.level = level.into();
    }

    /// Apply client-level Error Tracking options (in-app classification, frame
    /// and source-chain limits) and write the reserved `$exception_*`
    /// properties onto `event`.
    fn write_into(self, event: &mut Event, options: &ErrorTrackingOptions) -> Result<(), Error> {
        let Exception {
            mut items,
            captured_frames,
            fingerprint,
            level,
        } = self;
        if items.is_empty() {
            return Ok(());
        }

        if let Some(mut frames) = captured_frames {
            for frame in frames.iter_mut() {
                let function = (!frame.function.is_empty()).then_some(frame.function.as_str());
                frame.in_app = options.is_in_app_frame(frame.filename.as_deref(), function);
            }
            trim_to_max_frames(&mut frames, MAX_FRAMES);
            items[0].stacktrace = Some(ExceptionStacktrace::raw(frames));
        }

        event.insert_prop("$exception_level", level)?;
        if let Some(fingerprint) = fingerprint {
            event.insert_prop("$exception_fingerprint", fingerprint)?;
        }
        event.insert_prop("$exception_list", items)?;
        Ok(())
    }
}

/// A normalized exception entry in `$exception_list`.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExceptionItem {
    #[serde(rename = "type")]
    pub exception_type: String,
    pub value: String,
    pub mechanism: ExceptionMechanism,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stacktrace: Option<ExceptionStacktrace>,
}

/// How an exception was captured.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExceptionMechanism {
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
pub(crate) struct ExceptionStacktrace {
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
pub(crate) struct StackFrame {
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
// when the exception event is built.
fn capture_frames_current_first(skip: usize) -> Vec<StackFrame> {
    let mut frames = Vec::new();
    let mut skipped = 0usize;

    backtrace::trace(|frame| {
        if skipped < skip {
            skipped += 1;
            return true;
        }

        // One physical frame resolves to multiple symbols when the compiler
        // inlined functions into it; emit each inlined layer as its own frame
        // so the logical call chain survives. The resolver yields layers
        // innermost-first, which matches this stack's current-first order.
        backtrace::resolve_frame(frame, |symbol| {
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

/// Capture the current raw stacktrace, dropping the leading SDK frames matched
/// by `is_internal` so the caller's frame comes first.
fn capture_raw_frames(is_internal: impl Fn(&str) -> bool) -> Vec<StackFrame> {
    let mut frames = capture_frames_current_first(0);
    while frames
        .first()
        .map(|frame| is_internal(&frame.function))
        .unwrap_or(false)
    {
        frames.remove(0);
    }

    frames
}

fn capture_raw_application_frames() -> Vec<StackFrame> {
    capture_raw_frames(is_internal_capture_frame)
}

fn capture_raw_panic_frames() -> Vec<StackFrame> {
    capture_raw_frames(is_internal_panic_frame)
}

fn is_internal_capture_frame(function: &str) -> bool {
    function.starts_with("backtrace::")
        || function.contains("capture_frames_current_first")
        || function.contains("capture_raw_frames")
        || function.contains("capture_raw_application_frames")
        || function.contains("Exception::from_error")
        || function.contains("Exception::from_message")
        || function.contains("build_exception_event")
        || function.contains("Client::capture_exception")
        || function.contains("global::capture_exception")
}

/// Internal frames to strip from a panic stacktrace: the shared capture frames
/// plus the panic-hook and unwinding machinery.
fn is_internal_panic_frame(function: &str) -> bool {
    is_internal_capture_frame(function)
        || function.contains("capture_panic")
        || function.contains("capture_raw_panic_frames")
        || function.contains("build_panic_event")
        // The shared installer's hook closure plus each entry point's capture
        // closure (standalone `install_panic_hook`, global
        // `maybe_install_global_panic_hook`) sit between the unwinder and
        // `capture_panic`; none of these names is a substring of another.
        || function.contains("install_hook")
        || function.contains("install_panic_hook")
        || function.contains("maybe_install_global_panic_hook")
        || function.contains("Exception::from_panic_info")
        || function.contains("AssertUnwindSafe")
        || function.starts_with("core::ops::function::FnOnce::call_once")
        // std::panic::catch_unwind is an #[inline] wrapper that surfaces as a
        // logical frame inside the hook closure's physical frame.
        || function.starts_with("std::panic::")
        || function.starts_with("std::panicking::")
        || function.starts_with("core::panicking::")
        || function.starts_with("std::sys::backtrace::")
        // The boxed hook dispatch frame names the hook argument type;
        // `PanicInfo` covers binaries built before the 1.82 `PanicHookInfo` rename.
        || function.contains("PanicHookInfo")
        || function.contains("PanicInfo")
        // The unwind shim symbol is `__rust_try` on ELF and `___rust_try`
        // under Mach-O's extra leading underscore.
        || function.ends_with("__rust_try")
        || function.contains("rust_begin_unwind")
}

/// The panic payload as a string, falling back to a generic message.
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

/// Demangled symbols carry compiler-internal hashes that vary per platform and
/// rustc release: legacy mangling appends a trailing `::h<16 hex>`, and v0
/// mangling tags crate names with `[<hex>]` disambiguators (std ships v0-mangled
/// on Linux, so std frames demangle as `std[b887e3750a86e3a0]::panicking::…`).
/// Strip both so internal-frame matching and server-side grouping see stable,
/// readable names.
fn normalize_function_name(function: &str) -> String {
    let function = strip_crate_disambiguators(function);
    match function.rsplit_once("::") {
        Some((prefix, suffix)) if is_rust_symbol_hash(suffix) => prefix.to_string(),
        _ => function,
    }
}

fn strip_crate_disambiguators(function: &str) -> String {
    let mut out = String::with_capacity(function.len());
    let mut rest = function;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let bracketed = &rest[open..];
        match bracketed.find(']') {
            Some(close) => {
                let content = &bracketed[1..close];
                if !is_crate_disambiguator(content) {
                    out.push_str(&bracketed[..=close]);
                }
                rest = &bracketed[close + 1..];
            }
            None => {
                out.push_str(bracketed);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Lowercase-hex bracket contents of disambiguator length; array/slice type
/// brackets (`[u8; 32]`) never qualify.
fn is_crate_disambiguator(content: &str) -> bool {
    content.len() >= 8
        && content
            .chars()
            .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
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
    use std::sync::atomic::{AtomicBool, Ordering};
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

    fn built_event_json(mut event: Event) -> Value {
        event.prepare_for_v0();
        serde_json::to_value(InnerEvent::new(event, "api-key".to_string())).unwrap()
    }

    fn event_json_with(exception: Exception, options: &ErrorTrackingOptions) -> Value {
        let mut event = Event::new_anon("$exception");
        exception.write_into(&mut event, options).unwrap();
        built_event_json(event)
    }

    fn event_json(exception: Exception) -> Value {
        event_json_with(exception, &ErrorTrackingOptions::default())
    }

    #[allow(deprecated)]
    type PanicHook = Box<dyn Fn(&panic::PanicInfo<'_>) + Sync + Send + 'static>;

    /// Restores the previous panic hook and clears the install latch so the
    /// panic tests don't leak global state into one another.
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

    /// Serializes the panic tests: they share the process-wide panic hook and
    /// the install latch.
    fn panic_hook_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    // The async constructor only awaits when local evaluation is enabled, so a
    // plain client builds synchronously under a minimal executor — no Tokio
    // runtime needed to set up the panic-hook tests.
    #[cfg(feature = "async-client")]
    fn build_test_client(options: crate::client::ClientOptions) -> Arc<Client> {
        Arc::new(futures::executor::block_on(crate::client::client(options)))
    }

    #[cfg(not(feature = "async-client"))]
    fn build_test_client(options: crate::client::ClientOptions) -> Arc<Client> {
        Arc::new(crate::client::client(options))
    }

    // Initialize the process-wide global client (set-once), mirroring
    // `build_test_client`'s runtime-free construction across feature configs.
    #[cfg(feature = "async-client")]
    fn init_global_test(options: crate::client::ClientOptions) -> Result<(), Error> {
        futures::executor::block_on(crate::global::init_global_client(options))
    }

    #[cfg(not(feature = "async-client"))]
    fn init_global_test(options: crate::client::ClientOptions) -> Result<(), Error> {
        crate::global::init_global_client(options)
    }

    #[inline(never)]
    fn panic_hook_test_panic_site() {
        panic!("panic hook boom");
    }

    #[inline(never)]
    fn panic_hook_disabled_test_panic_site() {
        panic!("disabled panic hook boom");
    }

    /// Match the panic `$exception` event inside the transport's batch envelope
    /// (`batch[0]`) — the same event shape for the V0 and V1 wire formats.
    fn request_has_panic_payload(req: &HttpMockRequest) -> bool {
        let Some(body) = req.body.as_deref() else {
            return false;
        };
        let Ok(body) = serde_json::from_slice::<Value>(body) else {
            return false;
        };
        let event = &body["batch"][0];
        let exception = &event["properties"]["$exception_list"][0];
        let first_function = exception["stacktrace"]["frames"][0]["function"]
            .as_str()
            .unwrap_or_default();

        event["event"] == "$exception"
            // V0 injects `$process_person_profile` into properties; V1 keeps it
            // in the typed `options` object.
            && (event["properties"]["$process_person_profile"] == false
                || event["options"]["process_person_profile"] == false)
            && exception["type"] == "Panic"
            && exception["value"] == "panic hook boom"
            && exception["mechanism"]["type"] == "panic"
            && exception["mechanism"]["handled"] == false
            && event["properties"]["$exception_panic_file"]
                .as_str()
                .is_some_and(|file| file.contains("error_tracking.rs"))
            && event["properties"]["$exception_panic_line"]
                .as_u64()
                .is_some_and(|line| line > 0)
            && event["properties"]["$exception_panic_column"]
                .as_u64()
                .is_some_and(|column| column > 0)
            && first_function.contains("panic_hook_test_panic_site")
            && !first_function.contains("std::panicking")
            && !first_function.contains("install_panic_hook")
    }

    #[test]
    fn panic_hook_sends_personless_exception_and_calls_previous_hook() {
        let _guard = panic_hook_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original_hook = panic::take_hook();
        let mut reset = PanicHookReset::new(original_hook);
        let previous_called = Arc::new(AtomicBool::new(false));
        let previous_called_for_hook = Arc::clone(&previous_called);
        panic::set_hook(Box::new(move |_| {
            previous_called_for_hook.store(true, Ordering::Release);
        }));

        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST).matches(request_has_panic_payload);
            then.status(200);
        });
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .build()
            .unwrap();
        let client = build_test_client(options);

        install_panic_hook(Arc::clone(&client)).unwrap();
        assert!(matches!(
            install_panic_hook(Arc::clone(&client)),
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
        let _guard = panic_hook_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original_hook = panic::take_hook();
        let mut reset = PanicHookReset::new(original_hook);
        panic::set_hook(Box::new(|_| {}));

        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST);
            then.status(200);
        });
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .disabled(true)
            .build()
            .unwrap();
        let client = build_test_client(options);

        install_panic_hook(client).unwrap();
        let result = panic::catch_unwind(panic_hook_disabled_test_panic_site);
        reset.restore();

        assert!(result.is_err());
        capture_mock.assert_hits(0);
    }

    /// Panics inside Tokio tasks run the hook on a runtime worker thread; the
    /// transport's own worker is a separate std::thread, so the enqueue + flush
    /// still deliver the event rather than re-panicking on a runtime thread.
    #[cfg(feature = "async-client")]
    #[test]
    fn panic_hook_captures_panics_on_tokio_runtime_threads() {
        let _guard = panic_hook_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original_hook = panic::take_hook();
        let mut reset = PanicHookReset::new(original_hook);
        panic::set_hook(Box::new(|_| {}));

        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST)
                .body_contains(r#""value":"tokio task boom""#);
            then.status(200);
        });
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .build()
            .unwrap();
        install_panic_hook(build_test_client(options)).unwrap();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let result = runtime.block_on(async {
            tokio::spawn(async {
                panic!("tokio task boom");
            })
            .await
        });
        drop(runtime);

        // Strictest flavor: the hook fires on the very thread driving block_on
        // of a current-thread runtime.
        let current_thread = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let current_result = current_thread.block_on(async {
            panic::catch_unwind(AssertUnwindSafe(|| panic!("tokio task boom")))
        });
        drop(current_thread);
        reset.restore();

        assert!(result.is_err());
        assert!(current_result.is_err());
        capture_mock.assert_hits(2);
    }

    #[test]
    fn panic_in_before_send_on_worker_neither_deadlocks_nor_recurses() {
        // A `before_send` hook that panics unconditionally fires the panic hook
        // ON the transport worker thread. Capturing there must be skipped: a
        // synchronous self-flush would deadlock the worker, and routing the
        // `$exception` back through `before_send` (which panics again) would
        // recurse forever. A watchdog turns either regression into a failure.
        let _guard = panic_hook_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original_hook = panic::take_hook();
        let mut reset = PanicHookReset::new(original_hook);
        panic::set_hook(Box::new(|_| {}));

        let server = MockServer::start();
        let _capture_mock = server.mock(|when, then| {
            when.method(POST);
            then.status(200);
        });
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .before_send(|_event| panic!("before_send boom"))
            .build()
            .unwrap();
        let client = build_test_client(options);
        install_panic_hook(Arc::clone(&client)).unwrap();

        let finished = Arc::new(AtomicBool::new(false));
        let finished_for_worker = Arc::clone(&finished);
        let work_client = Arc::clone(&client);
        let _worker = std::thread::spawn(move || {
            work_client.capture(Event::new("boom", "user-1"));
            // From this (non-worker) thread this is a real blocking flush; it
            // returns only if the worker neither deadlocked nor spun on recursion.
            work_client.flush_blocking();
            finished_for_worker.store(true, Ordering::Release);
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !finished.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        reset.restore();

        assert!(
            finished.load(Ordering::Acquire),
            "panic in before_send on the worker thread deadlocked or recursed"
        );
        // The spawned thread is intentionally not joined: on a regression it is
        // stuck in flush_blocking and a join would hang too; on success it has
        // already finished. Dropping the handle detaches it.
    }

    #[test]
    fn panic_hook_flush_is_bounded_when_before_send_needs_a_panic_held_lock() {
        // The panic hook flushes on the *panicking* thread, before unwinding
        // releases locks held at the panic site. If a `before_send` hook needs
        // such a lock, the worker blocks on it and the hook would block on the
        // worker forever — the process hangs instead of crashing. The flush is
        // bounded by `panic_flush_timeout_ms`, so the hook returns and the panic
        // proceeds. A short timeout keeps the test fast; the watchdog turns a
        // regression (unbounded wait) into a failure instead of a hang.
        let _guard = panic_hook_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original_hook = panic::take_hook();
        let mut reset = PanicHookReset::new(original_hook);
        panic::set_hook(Box::new(|_| {}));

        // A lock the application holds across its panic and that `before_send`
        // also wants — the classic shape that would deadlock an unbounded flush.
        static SHARED: Mutex<()> = Mutex::new(());

        let server = MockServer::start();
        let _capture_mock = server.mock(|when, then| {
            when.method(POST);
            then.status(200);
        });
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .error_tracking(
                ErrorTrackingOptionsBuilder::default()
                    .panic_flush_timeout_ms(200u64)
                    .build()
                    .unwrap(),
            )
            .before_send(|event| {
                let _held = SHARED.lock().unwrap_or_else(|e| e.into_inner());
                Some(event)
            })
            .build()
            .unwrap();
        let client = build_test_client(options);
        install_panic_hook(Arc::clone(&client)).unwrap();

        let finished = Arc::new(AtomicBool::new(false));
        let finished_for_panicker = Arc::clone(&finished);
        let _panicker = std::thread::spawn(move || {
            {
                // Hold SHARED across the panic so the hook fires while it is
                // locked. Release it (end of scope) *before* signalling, so test
                // teardown can drain the worker without blocking on the lock.
                let _held = SHARED.lock().unwrap_or_else(|e| e.into_inner());
                let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                    panic!("boom while holding a before_send lock")
                }));
            }
            finished_for_panicker.store(true, Ordering::Release);
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !finished.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        reset.restore();

        assert!(
            finished.load(Ordering::Acquire),
            "panic hook flush hung on a before_send that needed a panic-held lock"
        );
        // Not joined: on a regression the thread is stuck in the hook's flush and
        // a join would hang too; on success it has already finished.
    }

    #[test]
    fn global_capture_panics_defaults_on_and_is_configurable() {
        assert!(
            ErrorTrackingOptions::default().capture_panics(),
            "panic autocapture is on by default"
        );
        let opted_out = ErrorTrackingOptionsBuilder::default()
            .capture_panics(false)
            .build()
            .unwrap();
        assert!(
            !opted_out.capture_panics(),
            "capture_panics is configurable"
        );
    }

    #[test]
    fn init_global_installs_panic_capture_by_default() {
        // capture_panics defaults on, so init_global installs a process-wide hook
        // that routes panics through the global client. The global client lives
        // in a set-once OnceLock, so this is the ONLY test that initializes it.
        let _guard = panic_hook_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original_hook = panic::take_hook();
        let mut reset = PanicHookReset::new(original_hook);
        panic::set_hook(Box::new(|_| {}));

        let server = MockServer::start();
        let capture_mock = server.mock(|when, then| {
            when.method(POST).matches(request_has_panic_payload);
            then.status(200);
        });
        let options = ClientOptionsBuilder::default()
            .api_key("test_api_key".to_string())
            .host(server.base_url())
            .build()
            .unwrap();
        init_global_test(options).expect("init_global succeeds");

        // Panic through the shared site so the payload matches the same matcher
        // as the standalone-client test.
        let _ = panic::catch_unwind(AssertUnwindSafe(panic_hook_test_panic_site));

        // Restore before asserting so a failed assertion can't leave the global
        // hook dangling for other tests.
        reset.restore();
        // `>= 1`, not exactly 1: a panic in another (non-serialized) test during
        // the install window would also be captured; our own panic guarantees one.
        assert!(
            capture_mock.hits() >= 1,
            "global panic hook did not capture the panic"
        );
    }

    #[test]
    fn is_internal_panic_frame_strips_panic_and_capture_machinery() {
        for internal in [
            "std::panicking::begin_panic_handler",
            "core::panicking::panic_fmt",
            "std::panic::catch_unwind",
            "std::sys::backtrace::__rust_begin_short_backtrace",
            "rust_begin_unwind",
            "backtrace::backtrace::trace",
            "posthog_rs::error_tracking::install_hook::{{closure}}",
            "posthog_rs::error_tracking::install_panic_hook::{{closure}}",
            "posthog_rs::error_tracking::maybe_install_global_panic_hook::{{closure}}",
            "posthog_rs::error_tracking::capture_panic",
            "posthog_rs::error_tracking::build_panic_event",
            "core::ops::function::FnOnce::call_once",
        ] {
            assert!(
                is_internal_panic_frame(internal),
                "{} should be treated as internal",
                internal
            );
        }

        for app in [
            "my_app::checkout::process_payment",
            "core::array::<impl [u8; 32]>::map",
        ] {
            assert!(
                !is_internal_panic_frame(app),
                "{} should not be treated as internal",
                app
            );
        }
    }

    #[test]
    fn function_names_strip_v0_crate_disambiguators() {
        // std ships v0-mangled on Linux; crate names demangle with `[hex]`.
        assert_eq!(
            normalize_function_name("std[b887e3750a86e3a0]::panicking::panic_with_hook"),
            "std::panicking::panic_with_hook"
        );
        assert_eq!(
            normalize_function_name(
                "<alloc[8a71accd1b3711a1]::boxed::Box<dyn core[e000b89356eb4406]::ops::function::Fn<(&std[b887e3750a86e3a0]::panic::PanicHookInfo,)>> as core[e000b89356eb4406]::ops::function::Fn<(&std[b887e3750a86e3a0]::panic::PanicHookInfo,)>>::call"
            ),
            "<alloc::boxed::Box<dyn core::ops::function::Fn<(&std::panic::PanicHookInfo,)>> as core::ops::function::Fn<(&std::panic::PanicHookInfo,)>>::call"
        );
        // Array and slice type brackets are not disambiguators.
        assert_eq!(
            normalize_function_name("core::array::<impl [u8; 32]>::map"),
            "core::array::<impl [u8; 32]>::map"
        );
        assert_eq!(
            normalize_function_name("<[u8] as checkout_service::Digest>::digest"),
            "<[u8] as checkout_service::Digest>::digest"
        );
    }

    #[test]
    fn from_error_builds_exception_list_with_stacktrace() {
        let error = OuterError { source: InnerError };
        let event = build_exception_event(
            &error,
            CaptureExceptionOptions::new().distinct_id("user-1"),
            &ErrorTrackingOptions::default(),
        )
        .unwrap();
        let json = built_event_json(event);

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
        let json = event_json(Exception::from_error(&error, true));

        assert_eq!(
            json["properties"]["$exception_list"][0]["value"],
            "borrowed parse failure"
        );
    }

    #[test]
    fn personless_capture_disables_person_profile() {
        let json = event_json(Exception::from_message("Error", "no user context", true));

        assert_eq!(json["event"], "$exception");
        assert_eq!(json["properties"]["$process_person_profile"], false);
    }

    #[test]
    fn custom_properties_cannot_override_reserved_exception_payload() {
        let error = OuterError { source: InnerError };
        let event = build_exception_event(
            &error,
            CaptureExceptionOptions::new()
                .property("$exception_list", json!([{"value": "fake"}]))
                .unwrap(),
            &ErrorTrackingOptions::default(),
        )
        .unwrap();

        let json = built_event_json(event);
        assert_eq!(
            json["properties"]["$exception_list"][0]["value"],
            "checkout failed"
        );
    }

    #[test]
    fn options_can_disable_stacktrace() {
        let options = ErrorTrackingOptionsBuilder::default()
            .capture_stacktrace(false)
            .build()
            .unwrap();
        let error = OuterError { source: InnerError };
        let event =
            build_exception_event(&error, CaptureExceptionOptions::new(), &options).unwrap();
        let json = built_event_json(event);

        let exception_list = json["properties"]["$exception_list"].as_array().unwrap();
        assert_eq!(exception_list.len(), 2);
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
    fn frames_are_trimmed_to_max_frames_keeping_the_top() {
        let synthetic_frame = |index: usize| StackFrame {
            filename: None,
            line_no: None,
            function: format!("frame_{index}"),
            lang: "rust".to_string(),
            in_app: true,
            synthetic: false,
            resolved: true,
            platform: "rust".to_string(),
        };
        let exception = Exception {
            items: vec![ExceptionItem {
                exception_type: "Error".to_string(),
                value: "trimmed".to_string(),
                mechanism: ExceptionMechanism::default(),
                stacktrace: None,
            }],
            captured_frames: Some((0..MAX_FRAMES + 5).map(synthetic_frame).collect()),
            fingerprint: None,
            level: "error".to_string(),
        };

        let json = event_json_with(exception, &ErrorTrackingOptions::default());
        let frames = json["properties"]["$exception_list"][0]["stacktrace"]["frames"]
            .as_array()
            .expect("expected stack frames");
        assert_eq!(frames.len(), MAX_FRAMES);
        // Trimming drops the outermost tail; the crash-site top frame survives.
        assert_eq!(frames[0]["function"], "frame_0");
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
    fn inlined_functions_become_separate_frames() {
        // inline(always) is honored in debug builds, so the two helpers share
        // the test function's physical frame and must surface as their own
        // logical frames, innermost first.
        #[inline(always)]
        fn inline_leaf() -> Vec<StackFrame> {
            capture_raw_application_frames()
        }

        #[inline(always)]
        fn inline_mid() -> Vec<StackFrame> {
            inline_leaf()
        }

        let frames = inline_mid();
        let functions: Vec<&str> = frames.iter().map(|frame| frame.function.as_str()).collect();

        let leaf_index = functions
            .iter()
            .position(|function| function.contains("inline_leaf"))
            .unwrap_or_else(|| panic!("expected inline_leaf frame, got {:?}", functions));
        let mid_index = functions
            .iter()
            .position(|function| function.contains("inline_mid"))
            .unwrap_or_else(|| panic!("expected inline_mid frame, got {:?}", functions));
        assert!(
            leaf_index < mid_index,
            "expected innermost inlined layer first, got {:?}",
            functions
        );
    }

    #[test]
    fn build_exception_event_defaults_to_personless() {
        let error = OuterError { source: InnerError };
        let event = build_exception_event(
            &error,
            CaptureExceptionOptions::default(),
            &ErrorTrackingOptions::default(),
        )
        .unwrap();
        let json = built_event_json(event);

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
        let event =
            build_exception_event(&error, options, &ErrorTrackingOptions::default()).unwrap();
        let json = built_event_json(event);

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
