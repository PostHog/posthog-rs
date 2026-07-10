use std::any::{type_name, type_name_of_val};
use std::error::Error as StdError;
use std::io::Write;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
// Only the `#[cfg(test)]` standalone-client `install_panic_hook` helper needs
// `Arc` at module scope; the tests module imports its own.
#[cfg(test)]
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

/// How long the panic hook blocks the panicking thread waiting for the
/// `$exception` to flush before letting the panic proceed. Deliberately short:
/// the hook runs on the dying thread, so a long wait would freeze the crash (and
/// delay the panic message, which prints only after the flush) when PostHog is
/// slow or unreachable. Fixed rather than configurable for now — easy to expose
/// later if a need arises.
#[cfg(not(test))]
const PANIC_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
/// Shortened under test: the bounded-flush tests deliberately deadlock
/// `before_send`, so they wait the full budget — only its boundedness matters
/// there, not the production duration.
#[cfg(test)]
const PANIC_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

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
    /// When `true`, [`crate::init_global`] installs a process-wide panic hook
    /// that captures panics as `$exception` events through the global client.
    /// Defaults to `false` — panic autocapture is opt-in.
    ///
    /// Only the global client installs a hook: a panic hook is process-global
    /// (`std::panic::set_hook`), so it pairs with the process-global client, and
    /// there is intentionally no per-`Client` panic API for now.
    capture_panics: bool,
}

impl Default for ErrorTrackingOptions {
    fn default() -> Self {
        Self {
            capture_stacktrace: true,
            in_app_include_paths: Vec::new(),
            in_app_exclude_paths: Vec::new(),
            capture_panics: false,
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
            if !default_in_app_function(function) {
                return false;
            }
            // Only a known-symbol list for fileless bootstrap glue: a broader
            // "no `::` path and no file" rule would also hide legitimate app
            // symbols (`#[no_mangle]`/`#[export_name]` functions, C code
            // linked into the binary) that resolve the same way.
            if filename.is_none() && is_bootstrap_symbol(function) {
                return false;
            }
            return true;
        }

        filename.is_some()
    }
}

/// Install the panic hook against a specific `client`. Internal/test-only: the
/// public entry point is the global client's `capture_panics` option (via
/// [`crate::init_global`]), since a panic hook is process-global. Kept to
/// exercise the shared hook path against a standalone client in tests. A
/// disabled client installs nothing and returns `Ok(())`.
#[cfg(test)]
fn install_panic_hook(client: Arc<Client>) -> Result<(), Error> {
    if client.is_disabled() {
        return Ok(());
    }
    install_hook(move |panic_info| capture_panic(&client, panic_info))
}

/// If the global client has `capture_panics` enabled (opt-in) and can actually
/// send, install the panic hook against it. Best-effort and idempotent: a hook
/// installed earlier is left in place. Called by `init_global` once the global
/// client is set.
pub(crate) fn maybe_install_global_panic_hook() {
    let Some(client) = crate::global::global_client() else {
        return;
    };
    if !should_capture_global_panics(client) {
        return;
    }
    // The hook reads the global client at panic time — it lives in a process
    // `static`, so the hook needs no owned handle. `AlreadyInstalled` is benign.
    let _ = install_hook(|panic_info| match crate::global::global_client() {
        Some(client) => capture_panic(client, panic_info),
        None => Ok(()),
    });
}

/// Whether `init_global` should auto-install the panic hook for this client.
/// A disabled client can't send, so it must not latch the single process-wide
/// hook.
fn should_capture_global_panics(client: &Client) -> bool {
    !client.is_disabled() && client.error_tracking_options().capture_panics()
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
    // The fixed bound (`PANIC_FLUSH_TIMEOUT`) keeps the dying process from
    // hanging — and the panic message, which prints only after this returns,
    // from being delayed — when PostHog is slow/unreachable or a `before_send`
    // hook needs a lock the panic site still holds.
    client.flush_blocking_timeout(PANIC_FLUSH_TIMEOUT);
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
    // Loaded modules referenced by captured_frames; becomes the event-level
    // $debug_images property after trimming. Empty when stacktrace capture is
    // disabled or no frame points into a module with an uploadable debug id.
    captured_images: Vec<DebugImage>,
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

        let (captured_frames, captured_images) = if capture_stacktrace {
            let (frames, images) = capture_raw_application_frames();
            (Some(frames), images)
        } else {
            (None, Vec::new())
        };

        Self {
            items,
            captured_frames,
            captured_images,
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
        let (captured_frames, captured_images) = if capture_stacktrace {
            let (frames, images) = capture_raw_application_frames();
            (Some(frames), images)
        } else {
            (None, Vec::new())
        };

        Self {
            items: vec![ExceptionItem {
                exception_type: exception_type.into(),
                value: value.into(),
                mechanism: ExceptionMechanism::default(),
                stacktrace: None,
            }],
            captured_frames,
            captured_images,
            fingerprint: None,
            level: "error".to_string(),
        }
    }

    /// Build an exception from a panic, capturing the current stacktrace when
    /// `capture_stacktrace` is set.
    #[allow(deprecated)]
    fn from_panic_info(panic_info: &panic::PanicInfo<'_>, capture_stacktrace: bool) -> Self {
        let (captured_frames, captured_images) = if capture_stacktrace {
            let (frames, images) = capture_raw_panic_frames();
            (Some(frames), images)
        } else {
            (None, Vec::new())
        };

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
            captured_frames,
            captured_images,
            fingerprint: None,
            // Panics are unrecoverable (the process is unwinding/aborting), so
            // they are reported at `fatal`, not `error`.
            level: "fatal".to_string(),
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
            captured_images,
            fingerprint,
            level,
        } = self;
        if items.is_empty() {
            return Ok(());
        }

        let mut debug_images = Vec::new();
        if let Some(mut frames) = captured_frames {
            for frame in frames.iter_mut() {
                let function = (!frame.function.is_empty()).then_some(frame.function.as_str());
                // Frames without any symbol information keep their capture-time
                // image-based classification; the path/function rules have
                // nothing to act on.
                if function.is_some() || frame.filename.is_some() {
                    frame.in_app = options.is_in_app_frame(frame.filename.as_deref(), function);
                }
            }
            trim_to_max_frames(&mut frames, MAX_FRAMES);
            // Only report modules still referenced after trimming.
            debug_images = captured_images
                .into_iter()
                .filter(|image| {
                    frames
                        .iter()
                        .any(|f| f.image_addr.as_deref() == Some(image.image_addr.as_str()))
                })
                .collect();
            items[0].stacktrace = Some(ExceptionStacktrace::raw(frames));
        }

        event.insert_prop("$exception_level", level)?;
        if let Some(fingerprint) = fingerprint {
            event.insert_prop("$exception_fingerprint", fingerprint)?;
        }
        if !debug_images.is_empty() {
            event.insert_prop("$debug_images", debug_images)?;
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
///
/// Frames carry the raw `instruction_addr` for server-side symbolication
/// against uploaded debug symbols (`posthog-cli debug-symbols upload`), plus
/// best-effort client-side enrichment (`function`/`filename`/`lineno`) used
/// for display when no debug symbols are available.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct StackFrame {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(rename = "lineno")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_no: Option<u32>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub function: String,
    pub lang: String,
    pub in_app: bool,
    pub synthetic: bool,
    pub platform: String,
    /// Absolute address of the instruction, as a hex string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instruction_addr: Option<String>,
    /// Start address of the enclosing symbol, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_addr: Option<String>,
    /// Load address of the module containing the instruction, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_addr: Option<String>,
    /// Whether the SDK already resolved this frame's symbol client-side. When
    /// `true`, the server must not re-symbolicate the `instruction_addr`: the
    /// client already expanded any inline frames and the server would duplicate
    /// them. When `false`, the server resolves the address against uploaded
    /// debug symbols. Not consumed by the backend yet — sent ahead of support.
    pub client_resolved: bool,
}

/// A loaded module (binary image) referenced by captured stack frames. Sent as
/// the event-level `$debug_images` property so the server can map instruction
/// addresses onto uploaded debug symbols.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub(crate) struct DebugImage {
    #[serde(rename = "type")]
    pub image_type: String,
    /// The debug identifier matching the uploaded symbol set (derived from
    /// the GNU build id on ELF, `LC_UUID` on Mach-O).
    pub debug_id: String,
    /// The full code identifier (e.g. complete GNU build id), when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_id: Option<String>,
    pub image_addr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_vmaddr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_file: Option<String>,
    pub arch: String,
}

/// A module mapped into the process, used to attach load addresses to frames
/// and build the `$debug_images` list.
struct LoadedModule {
    base: u64,
    end: u64,
    image: DebugImage,
}

const fn native_image_type() -> &'static str {
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        "macho"
    } else if cfg!(target_os = "windows") {
        "pe"
    } else {
        "elf"
    }
}

/// Normalize a CPU architecture name to the shared native vocabulary used by
/// the other PostHog SDKs (informational; image matching is by debug_id and
/// address). `std::env::consts::ARCH` already uses most of these names.
fn normalize_arch(arch: &str) -> String {
    match arch {
        "aarch64" => "arm64".to_string(),
        other => other.to_string(),
    }
}

/// Render 16 bytes laid out as a little-endian GUID (Microsoft convention:
/// the first three fields are stored byte-swapped) as a canonical UUID string.
///
/// Used for PDB signatures, whose GUID is always stored little-endian on disk;
/// the swap is therefore unconditional, matching `symbolic`'s PE/PDB path at
/// upload time. (The ELF GNU-build-id path is endianness-aware instead — see
/// `debug_id_from_gnu_build_id`.)
fn guid_le_to_uuid(mut data: [u8; 16]) -> String {
    data[0..4].reverse();
    data[4..6].reverse();
    data[6..8].reverse();
    uuid::Uuid::from_bytes(data).to_string()
}

/// Derive a debug id from a GNU build id: the first 16 bytes interpreted as a
/// GUID, zero-padded when the build id is shorter.
///
/// This must match `symbolic`'s `ElfObject::compute_debug_id` (used by the
/// server and `posthog-cli` at upload time), which byte-swaps the first three
/// GUID fields *only for little-endian ELF objects*. The SDK enumerates its own
/// process, so the object endianness is this target's endianness — hence the
/// swap is gated on `target_endian` rather than applied unconditionally (a
/// big-endian ELF binary on s390x/powerpc64 must not swap, or its debug id
/// won't match the uploaded symbol set).
fn debug_id_from_gnu_build_id(build_id: &[u8]) -> Option<String> {
    if build_id.is_empty() {
        return None;
    }
    let mut data = [0u8; 16];
    let len = build_id.len().min(16);
    data[..len].copy_from_slice(&build_id[..len]);
    if cfg!(target_endian = "little") {
        data[0..4].reverse();
        data[4..6].reverse();
        data[6..8].reverse();
    }
    Some(uuid::Uuid::from_bytes(data).to_string())
}

fn debug_id_for(id: &findshlibs::SharedLibraryId) -> Option<String> {
    use findshlibs::SharedLibraryId;

    match id {
        SharedLibraryId::GnuBuildId(bytes) => debug_id_from_gnu_build_id(bytes),
        // Uppercase to match the chunk_ids stored by `posthog-cli dsym
        // upload`, which takes them verbatim from dwarfdump output.
        SharedLibraryId::Uuid(bytes) => {
            Some(uuid::Uuid::from_bytes(*bytes).to_string().to_uppercase())
        }
        SharedLibraryId::PdbSignature(guid, age) => {
            let uuid = guid_le_to_uuid(*guid);
            Some(if *age > 0 {
                format!("{uuid}-{age:x}")
            } else {
                uuid
            })
        }
        // PE timestamp/size signatures carry no debug id we can match symbols to.
        _ => None,
    }
}

/// Enumerate the modules currently mapped into the process, sorted by load
/// address. Modules without a usable debug id are kept for address matching
/// (frames still get an `image_addr`) but marked so they're never reported
/// in `$debug_images`.
fn collect_loaded_modules() -> Vec<LoadedModule> {
    use findshlibs::{IterationControl, SharedLibrary, TargetSharedLibrary};

    let mut modules = Vec::new();

    TargetSharedLibrary::each(|shlib| {
        let base = shlib.actual_load_addr().0 as u64;
        let size = shlib.len() as u64;

        // The main executable's name can be empty on Linux; the code_file
        // fallback below covers that.
        let name = shlib.name().to_string_lossy().into_owned();
        let code_file = if name.is_empty() {
            std::env::current_exe()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        } else {
            Some(name)
        };

        // debug_id() (PDB GUID+age on Windows, same as id() elsewhere) is the
        // identifier that matches uploaded symbols; id() supplies the full
        // code identifier (e.g. complete GNU build id).
        let debug_id = shlib
            .debug_id()
            .as_ref()
            .and_then(debug_id_for)
            .unwrap_or_default();
        let code_id = match shlib.id() {
            Some(findshlibs::SharedLibraryId::GnuBuildId(bytes)) => {
                Some(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
            }
            _ => None,
        };

        modules.push(LoadedModule {
            base,
            end: base.saturating_add(size),
            image: DebugImage {
                image_type: native_image_type().to_string(),
                debug_id,
                code_id,
                image_addr: format!("0x{base:x}"),
                image_size: Some(size),
                image_vmaddr: Some(format!("0x{:x}", shlib.stated_load_addr().0 as u64)),
                code_file,
                arch: normalize_arch(std::env::consts::ARCH),
            },
        });

        IterationControl::Continue
    });

    modules.sort_by_key(|m| m.base);
    modules
}

fn find_module(modules: &[LoadedModule], addr: u64) -> Option<&LoadedModule> {
    let idx = modules.partition_point(|m| m.base <= addr);
    let module = modules[..idx].last()?;
    (addr < module.end).then_some(module)
}

// Captures raw Rust stack traces for Error Tracking. Frames are unclassified
// at this point: in-app classification and trimming are client policy, applied
// when the exception event is built. Every frame carries its instruction
// address; function/file/line enrichment is best-effort and missing entirely
// in stripped release builds.
//
// inline(never): the entry address of this function identifies the SDK's own
// frames for address-based stripping, which must survive symbol-less builds.
#[inline(never)]
fn capture_frames_current_first(skip: usize, modules: &[LoadedModule]) -> Vec<StackFrame> {
    let mut frames = Vec::new();
    let mut skipped = 0usize;

    backtrace::trace(|frame| {
        if skipped < skip {
            skipped += 1;
            return true;
        }

        let instruction_addr = frame.ip() as u64;
        let frame_symbol_addr = frame.symbol_address() as u64;
        let module = find_module(modules, instruction_addr);
        // Only send addresses the server can actually resolve: without a
        // module carrying a debug id there is no `$debug_images` entry to
        // match, and the frame should pass through as purely client-resolved.
        let resolvable = module.is_some_and(|m| !m.image.debug_id.is_empty());

        // One physical frame resolves to multiple symbols when the compiler
        // inlined functions into it; `resolve_frame` yields those layers
        // innermost-first. Collect them so we can choose how to emit based on
        // whether the server can symbolicate this address.
        let mut layers: Vec<(Option<String>, Option<u32>, String)> = Vec::new();
        backtrace::resolve_frame(frame, |symbol| {
            let filename = symbol.filename().map(path_to_string);
            let function = symbol
                .name()
                .map(|name| normalize_function_name(&name.to_string()));

            if filename.is_none() && function.is_none() {
                return;
            }

            layers.push((filename, symbol.lineno(), function.unwrap_or_default()));
        });

        if resolvable {
            // The server can symbolicate this address, so emit ONE frame per
            // physical frame carrying the raw `instruction_addr` and let the
            // resolver expand the inline chain from the symcache. Expanding
            // inlines client-side as well would double them after server-side
            // resolution (the resolver re-expands every address-bearing native
            // frame). This matches posthog-ios, which sends one frame per
            // return address. The outermost (physical) layer's name is a
            // client-side placeholder until the server resolves the address;
            // frame.symbol_address() is the physical entry the pinned-frame
            // stripping matches against.
            let physical = layers.last();
            frames.push(StackFrame {
                filename: physical.and_then(|(file, _, _)| file.clone()),
                line_no: physical.and_then(|(_, line, _)| *line),
                function: physical
                    .map(|(_, _, function)| function.clone())
                    .unwrap_or_default(),
                lang: "rust".to_string(),
                in_app: false,
                synthetic: false,
                platform: "native".to_string(),
                instruction_addr: Some(format!("0x{instruction_addr:x}")),
                symbol_addr: (frame_symbol_addr != 0).then(|| format!("0x{frame_symbol_addr:x}")),
                image_addr: module.map(|m| m.image.image_addr.clone()),
                // We deliberately did not expand inlines here — the server
                // resolves this address and expands its inline chain.
                client_resolved: false,
            });
        } else if !layers.is_empty() {
            // We resolved symbols locally but there's no uploadable debug image,
            // so the server can't symbolicate this address. Keep the client-side
            // inline expansion (one frame per layer, no native addresses) — it's
            // the only way these inlined calls survive. Layers are pushed
            // innermost-first here; the reverse in `capture_raw_frames` /
            // `capture_raw_panic_frames` later flips them so the outermost
            // logical layer leads and the inlined leaf is last, matching the
            // canonical bottom-up wire order.
            for (filename, line_no, function) in layers {
                frames.push(StackFrame {
                    filename,
                    line_no,
                    function,
                    lang: "rust".to_string(),
                    // Placeholder: these frames carry a name, so `write_into`
                    // reclassifies in_app from the path/function before sending.
                    in_app: false,
                    synthetic: false,
                    platform: "native".to_string(),
                    instruction_addr: None,
                    symbol_addr: None,
                    image_addr: None,
                    // Resolved client-side (no debug image for the server to
                    // use), so the server must not re-expand these.
                    client_resolved: true,
                });
            }
        }
        // A non-resolvable frame with no local symbols is dropped: with no name
        // and no address, neither the client nor the server can resolve it, so a
        // bare entry would be pure noise.

        true
    });

    frames
}

// Frames are in canonical wire order (outermost first, crash-site frame last),
// so trimming drops the outermost frames from the front and keeps the ones
// nearest the crash site.
fn trim_to_max_frames(frames: &mut Vec<StackFrame>, max_frames: usize) {
    if frames.len() > max_frames {
        frames.drain(..frames.len() - max_frames);
    }
}

/// Drop the innermost (front, current-first) SDK prefix by matching each frame's
/// symbol entry address against `pinned_entries` (the SDK capture functions).
/// This works even in stripped builds where there are no names to match. The SDK
/// frames sit at the front, so dropping through the last match removes the whole
/// prefix, including the unwinder frames before our innermost one.
///
/// Platform caveat: this only works where the frame's symbol address is the
/// runtime function entry (Linux/glibc via `_Unwind_FindEnclosingFunction`). On
/// macOS the symbolization backend reports the queried address rather than the
/// function entry, so the address pass never matches there and the caller's
/// name-based pass does the stripping instead; fully stripped Apple/Windows
/// builds keep the SDK prefix as address-only frames, which regain names through
/// server-side symbolication.
fn strip_pinned_prefix(frames: &mut Vec<StackFrame>, pinned_entries: &[u64]) {
    let scan = frames.len().min(16);
    let matches_pinned = |frame: &StackFrame| {
        frame
            .symbol_addr
            .as_deref()
            .and_then(|addr| u64::from_str_radix(addr.trim_start_matches("0x"), 16).ok())
            .is_some_and(|addr| pinned_entries.contains(&addr))
    };
    if let Some(last_sdk) = frames[..scan].iter().rposition(matches_pinned) {
        frames.drain(..=last_sdk);
    }
}

/// Capture the current raw stacktrace, dropping the leading SDK frames, and
/// return the loaded modules those frames point into for the `$debug_images`
/// property. The result is in canonical wire order — outermost frame first,
/// crash/capture-site frame last — matching the other PostHog SDKs.
///
/// `capture_frames_current_first` yields innermost-first, so the SDK's own
/// frames lead. Two passes drop them: an address-based pass that matches each
/// frame's symbol entry against `pinned_entries` (the SDK capture functions),
/// which works even in stripped builds where there are no names; then the
/// name-based `is_internal` pass for everything the resolver could name. Only
/// after stripping do we reverse into wire order (see the trailing `reverse`),
/// so both passes keep operating on the front of the innermost-first vec.
///
/// inline(never): this generic function sits between the pinned non-generic
/// wrapper and `capture_frames_current_first`; keeping it a physical frame lets
/// the name-based pass match it (its monomorphized address can't be pinned),
/// and draining through the outermost pinned wrapper sweeps it out in stripped
/// builds.
#[inline(never)]
fn capture_raw_frames(
    is_internal: impl Fn(&str) -> bool,
    pinned_entries: &[u64],
) -> (Vec<StackFrame>, Vec<DebugImage>) {
    let modules = collect_loaded_modules();
    let mut frames = capture_frames_current_first(0, &modules);

    // Address-based stripping first (see `strip_pinned_prefix`): works even in
    // stripped builds where the name-based pass below has nothing to match.
    strip_pinned_prefix(&mut frames, pinned_entries);

    while frames
        .first()
        .map(|frame| is_internal(&frame.function))
        .unwrap_or(false)
    {
        frames.remove(0);
    }

    // Flip innermost-first into canonical wire order: outermost frame first,
    // crash-site frame last. A single reverse of the flattened vec is correct
    // because `capture_frames_current_first` pushes both the physical frames
    // and each frame's inline layers innermost-first, so one reverse flips both
    // levels at once — the outermost physical frame leads, and within a
    // client-expanded frame the outermost logical layer leads with the inlined
    // leaf last, exactly the bottom-up input the server-side native contract
    // expects.
    frames.reverse();

    let images = referenced_images(modules, &frames);
    (frames, images)
}

/// Only report modules that frames actually point into, and only those with a
/// usable debug id; the final filtering against the trimmed frame list happens
/// in `write_into`.
fn referenced_images(modules: Vec<LoadedModule>, frames: &[StackFrame]) -> Vec<DebugImage> {
    modules
        .into_iter()
        .filter(|m| !m.image.debug_id.is_empty())
        .map(|m| m.image)
        .filter(|image| {
            frames
                .iter()
                .any(|f| f.image_addr.as_deref() == Some(image.image_addr.as_str()))
        })
        .collect()
}

// inline(never): this non-generic wrapper sits on the stack directly below the
// constructor and its entry address anchors the address-based stripping in
// stripped builds (the generic `capture_raw_frames` between it and
// `capture_frames_current_first` is matched by name instead — its monomorphized
// address isn't nameable as a single fn pointer).
#[inline(never)]
fn capture_raw_application_frames() -> (Vec<StackFrame>, Vec<DebugImage>) {
    let pinned = [
        capture_frames_current_first as *const () as u64,
        capture_raw_application_frames as *const () as u64,
    ];
    capture_raw_frames(is_internal_capture_frame, &pinned)
}

// inline(never): anchors the address-based capture-helper strip below, exactly
// like `capture_raw_application_frames` does for the manual path.
#[inline(never)]
fn capture_raw_panic_frames() -> (Vec<StackFrame>, Vec<DebugImage>) {
    // We deliberately keep the panic and unwind *runtime* machinery
    // (`panic_with_hook`, `begin_panic_handler`, `rust_begin_unwind`, ...) — it is
    // classified out-of-app and the UI collapses it, which is more robust than
    // dropping runtime internals by an ever-drifting name list. Everything
    // *innermost of* the panic dispatcher is the SDK's own hook plumbing: the
    // dispatcher (`rust_panic_with_hook`) synchronously invoked our hook, so our
    // capture helpers, the `install_hook` closures, and the `catch_unwind` guard
    // they run under all sit below it. Under the canonical crash-last wire order
    // the innermost frame becomes the tail, which must be the crash-side runtime
    // frame — not our hook plumbing — so we strip that inner prefix in three
    // layers of decreasing robustness:
    //
    //   1. Address-based: drop our own `backtrace`/capture-helper frames by
    //      pinned symbol entry. Works even in stripped builds with no names,
    //      matching the manual path's first pass.
    //   2. Dispatcher anchor: if the panic dispatcher is visible by name, drop
    //      everything up to (but not including) it — a single stable anchor that
    //      sweeps the whole hook-wrapper chain (closures + `catch_unwind` guard)
    //      without enumerating its drifting frames.
    //   3. Name fallback: if the dispatcher isn't nameable, drop the SDK capture
    //      helpers by name.
    //
    // In a fully stripped build only (1) runs; the nameless hook-wrapper frames
    // then survive as address-only frames that regain names (and normalization)
    // through server-side symbolication, the same documented limitation the
    // manual path carries.
    let modules = collect_loaded_modules();
    let mut frames = capture_frames_current_first(0, &modules);

    let pinned = [
        capture_frames_current_first as *const () as u64,
        capture_raw_panic_frames as *const () as u64,
    ];
    strip_pinned_prefix(&mut frames, &pinned);

    let scan = frames.len().min(24);
    let dispatcher = frames[..scan]
        .iter()
        .position(|frame| is_panic_dispatcher_frame(&frame.function));
    match dispatcher {
        Some(index) => {
            frames.drain(..index);
        }
        None => {
            while frames
                .first()
                .map(|frame| is_internal_capture_frame(&frame.function))
                .unwrap_or(false)
            {
                frames.remove(0);
            }
        }
    }

    // Flip innermost-first into canonical wire order: outermost frame first,
    // panic site last (see `capture_raw_frames` for why one reverse suffices).
    frames.reverse();
    let images = referenced_images(modules, &frames);
    (frames, images)
}

// The std panic dispatcher that synchronously invokes the installed hook. Its
// name has been stable across recent toolchains; everything innermost of it on a
// panicking thread is our own hook plumbing.
fn is_panic_dispatcher_frame(function: &str) -> bool {
    function.contains("rust_panic_with_hook") || function.contains("panicking::panic_with_hook")
}

// Matches the SDK's own capture-plumbing frames — the ones our capture helpers
// push onto the innermost end simply by calling `backtrace` from inside
// themselves. These are always noise and are stripped from the innermost prefix
// so the canonical tail after the wire-order reverse is the crash site rather
// than an SDK helper. Only SDK-owned names appear here (stable, we control
// them); panic/unwind *runtime* frames (`begin_panic_handler`,
// `rust_begin_unwind`, `panic_with_hook`, ...) are deliberately NOT matched —
// the strip loop stops at them and they survive, classified out-of-app for the
// UI to collapse.
fn is_internal_capture_frame(function: &str) -> bool {
    // Demanglers differ on qualified-path rendering across toolchain versions:
    // older output is `Exception::from_error`, newer output wraps the type as
    // `<posthog_rs::error_tracking::Exception>::from_error::<T>`. Strip the
    // angle brackets before matching so both forms hit.
    let function: String = function.replace(['<', '>'], "");
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

/// Thread/process entry symbols from libc/libpthread (`__clone`,
/// `start_thread`) and the C `main` shim. They resolve from the symbol table
/// with no source file and no `crate::` path, so the crate denylist can't see
/// them; matched exactly so app symbols of the same bare shape stay in-app.
fn is_bootstrap_symbol(function: &str) -> bool {
    matches!(
        function,
        "main"
            | "_start"
            | "__libc_start_main"
            | "clone"
            | "clone3"
            | "__clone"
            | "__clone3"
            | "start_thread"
            | "_pthread_start"
            | "thread_start"
    )
}

/// A trailing `-<hex hash>` on a cargo registry/checkout directory name, e.g.
/// `index.crates.io-6f17d22bba15001f` or `somecrate-9a8b7c6d5e4f3a2b`.
fn has_cargo_hash_suffix(dir: &str) -> bool {
    dir.rsplit_once('-')
        .is_some_and(|(_, hash)| hash.len() >= 8 && hash.chars().all(|ch| ch.is_ascii_hexdigit()))
}

/// Matches cargo's registry source layout,
/// `$CARGO_HOME/registry/src/<registry>-<hex hash>/<crate>-<version>/...`,
/// regardless of where the cargo home lives. The hash suffix and a crate
/// directory beneath are required so app paths that merely contain
/// registry-like names don't match.
fn is_cargo_registry_src(normalized: &str) -> bool {
    let Some(idx) = normalized.find("/registry/src/") else {
        return false;
    };
    let rest = &normalized[idx + "/registry/src/".len()..];
    let Some((registry_dir, rest)) = rest.split_once('/') else {
        return false;
    };
    has_cargo_hash_suffix(registry_dir) && rest.contains('/')
}

/// Matches cargo's git dependency layout,
/// `$CARGO_HOME/git/checkouts/<repo>-<hex ident hash>/<short rev>/...`,
/// regardless of where the cargo home lives. Both the ident-hash suffix and
/// the short-rev component are required so an app that merely lives under a
/// `git/checkouts/` directory (even one named `*-deadbeef`) doesn't match.
fn is_cargo_git_checkout(normalized: &str) -> bool {
    let Some(idx) = normalized.find("/git/checkouts/") else {
        return false;
    };
    let rest = &normalized[idx + "/git/checkouts/".len()..];
    let Some((repo_dir, rest)) = rest.split_once('/') else {
        return false;
    };
    let ident_ok = has_cargo_hash_suffix(repo_dir);
    let Some((rev_dir, rest)) = rest.split_once('/') else {
        return false;
    };
    let rev_ok = (7..=40).contains(&rev_dir.len())
        && rev_dir.chars().all(|ch| ch.is_ascii_hexdigit())
        && !rest.is_empty();
    ident_ok && rev_ok
}

fn default_in_app_path(filename: &str) -> bool {
    let normalized = filename.replace('\\', "/");
    // CARGO_HOME isn't always `~/.cargo` — the official Rust Docker images use
    // `/usr/local/cargo`. Rather than guessing at cargo-home names, the
    // registry-src and git-checkout checks match cargo's own on-disk layouts
    // under any home; the `/.cargo/` rules stay as the original home-based
    // fallback.
    if normalized.contains("/.cargo/registry/")
        || normalized.contains("/.cargo/git/")
        || is_cargo_registry_src(&normalized)
        || is_cargo_git_checkout(&normalized)
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

/// Strip trailing generic arguments from a function name so the crate-segment
/// checks see the path, not the instantiation. DWARF-derived names carry the
/// bare method name with its generic arguments
/// (`poll_future<tokio::runtime::blocking::task::BlockingTask<...>>`), where a
/// naive `::` split would yield a garbage first segment (`poll_future<tokio`).
/// A *leading* `<` is a qualified-path rendering
/// (`<alloc::boxed::Box<F> as core::ops::function::FnOnce<Args>>::call_once`),
/// not generic arguments, so the name is kept whole for the existing
/// `trim_start_matches('<')` handling.
fn strip_generic_args(function: &str) -> &str {
    match function.find('<') {
        Some(idx) if idx > 0 => &function[..idx],
        _ => function,
    }
}

fn default_in_app_function(function: &str) -> bool {
    // Bare runtime/unwind symbols that carry no `crate::` prefix (so the segment
    // match below can't catch them). `rust_begin_unwind` is the panic entry; the
    // `__rust`/`___rust` shims are the unwind glue (the extra underscore is
    // Mach-O's).
    if function.is_empty()
        || function == "_main"
        || function == "rust_begin_unwind"
        || function.starts_with("__rust")
        || function.starts_with("___rust")
    {
        return false;
    }

    !matches!(
        strip_generic_args(function)
            .trim_start_matches('<')
            .split("::")
            .next()
            .unwrap_or_default(),
        "alloc"
            | "anyhow"
            | "backtrace"
            | "color_eyre"
            | "core"
            | "eyre"
            | "futures_core"
            | "futures_util"
            | "log"
            | "posthog_rs"
            | "reqwest"
            | "std"
            | "stable_eyre"
            | "tokio"
            | "tracing"
            | "tracing_core"
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
        let frames = exception["stacktrace"]["frames"].as_array();

        // The user's panic site is captured (no longer forced to frame 0 — we
        // keep the machinery frames now instead of stripping them).
        let has_panic_site = frames.is_some_and(|frames| {
            frames.iter().any(|frame| {
                frame["function"]
                    .as_str()
                    .is_some_and(|name| name.contains("panic_hook_test_panic_site"))
            })
        });
        // Panic/unwind machinery is kept and marked out of app rather than
        // dropped by name.
        let has_machinery_not_in_app = frames.is_some_and(|frames| {
            frames.iter().any(|frame| {
                frame["in_app"] == false
                    && frame["function"].as_str().is_some_and(|name| {
                        name.contains("panicking") || name == "rust_begin_unwind"
                    })
            })
        });

        event["event"] == "$exception"
            // V0 injects `$process_person_profile` into properties; V1 keeps it
            // in the typed `options` object.
            && (event["properties"]["$process_person_profile"] == false
                || event["options"]["process_person_profile"] == false)
            && event["properties"]["$exception_level"] == "fatal"
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
            && has_panic_site
            && has_machinery_not_in_app
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
        // time-bounded (`PANIC_FLUSH_TIMEOUT`), so the hook returns and the panic
        // proceeds; the watchdog turns a regression (an unbounded wait) into a
        // failure instead of a hang.
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
    fn global_capture_panics_defaults_off_and_is_configurable() {
        assert!(
            !ErrorTrackingOptions::default().capture_panics(),
            "panic autocapture is opt-in (off by default)"
        );
        let enabled = ErrorTrackingOptionsBuilder::default()
            .capture_panics(true)
            .build()
            .unwrap();
        assert!(enabled.capture_panics(), "capture_panics is configurable");
    }

    #[test]
    fn should_capture_global_panics_gates_on_enabled_and_flag() {
        let enabled = build_test_client(
            ClientOptionsBuilder::default()
                .api_key("test_api_key".to_string())
                .error_tracking(
                    ErrorTrackingOptionsBuilder::default()
                        .capture_panics(true)
                        .build()
                        .unwrap(),
                )
                .build()
                .unwrap(),
        );
        assert!(should_capture_global_panics(&enabled));

        // Disabled gates it even with capture_panics on.
        let disabled = build_test_client(
            ClientOptionsBuilder::default()
                .api_key("test_api_key".to_string())
                .disabled(true)
                .error_tracking(
                    ErrorTrackingOptionsBuilder::default()
                        .capture_panics(true)
                        .build()
                        .unwrap(),
                )
                .build()
                .unwrap(),
        );
        assert!(
            !should_capture_global_panics(&disabled),
            "a disabled client must not latch the process-wide hook"
        );

        // Default options leave capture_panics off, so nothing installs.
        let default_off = build_test_client(
            ClientOptionsBuilder::default()
                .api_key("test_api_key".to_string())
                .build()
                .unwrap(),
        );
        assert!(!should_capture_global_panics(&default_off));
    }

    #[test]
    fn install_panic_hook_on_disabled_client_does_not_latch() {
        let _guard = panic_hook_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original_hook = panic::take_hook();
        let mut reset = PanicHookReset::new(original_hook);

        let disabled = build_test_client(
            ClientOptionsBuilder::default()
                .api_key("test_api_key".to_string())
                .disabled(true)
                .build()
                .unwrap(),
        );
        let result = install_panic_hook(disabled);
        let latched = PANIC_HOOK_INSTALLED.load(Ordering::Acquire);

        // Restore before asserting so a regression (which would install) can't
        // leave a hook dangling for other tests.
        reset.restore();
        assert!(result.is_ok(), "installing on a disabled client returns Ok");
        assert!(
            !latched,
            "a disabled client must not latch the process-wide hook"
        );
    }

    #[test]
    fn panic_machinery_frames_classify_out_of_app() {
        // Panic/unwind/SDK machinery is kept in the stacktrace now (not stripped
        // by name) and classified out of app by the default in-app rules, so the
        // UI can collapse it while the user's frames stay in-app.
        let options = ErrorTrackingOptions::default();
        for not_in_app in [
            "std::panicking::begin_panic_handler",
            "core::panicking::panic_fmt",
            "std::panic::catch_unwind",
            "std::sys::backtrace::__rust_begin_short_backtrace",
            "rust_begin_unwind",
            "__rust_try",
            "backtrace::backtrace::trace",
            "posthog_rs::error_tracking::capture_panic",
            "posthog_rs::error_tracking::install_hook::{{closure}}",
            "core::ops::function::FnOnce::call_once",
            "tokio::runtime::task::raw::poll",
            "futures_util::future::FutureExt::poll",
            "anyhow::error::Error::msg",
            "eyre::Report::msg",
            "color_eyre::config::EyreHook::into_eyre_hook::{{closure}}",
            "tracing::span::Span::record",
            "tracing_core::dispatcher::get_default",
            "log::__private_api::log",
        ] {
            assert!(
                !options.is_in_app_frame(None, Some(not_in_app)),
                "{} should classify as not in-app",
                not_in_app
            );
        }

        for in_app in [
            "my_app::checkout::process_payment",
            "checkout_service::submit",
        ] {
            assert!(
                options.is_in_app_frame(None, Some(in_app)),
                "{} should classify as in-app",
                in_app
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
    fn internal_capture_frames_match_both_demangler_renderings() {
        // Older toolchains demangle as `Type::method`, newer ones as
        // `<path::Type>::method::<T>` — the strip must catch both, or an SDK
        // frame survives at the crash-site end of the canonical order.
        for name in [
            "posthog_rs::error_tracking::Exception::from_error",
            "<posthog_rs::error_tracking::Exception>::from_error::<posthog_rs::error_tracking::tests::OuterError>",
            "<posthog_rs::error_tracking::Exception>::from_message",
            "<posthog_rs::client::Client>::capture_exception::<E>",
        ] {
            assert!(is_internal_capture_frame(name), "should strip {name:?}");
        }
        assert!(!is_internal_capture_frame(
            "my_app::checkout::Exception_from_error_report"
        ));
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
        // Canonical wire order is outermost first, so the crash/capture-site
        // user frame is the last frame.
        let crash_frame = frames.last().expect("expected crash frame");
        assert_eq!(crash_frame["platform"], "native");
        assert_eq!(crash_frame["lang"], "rust");
        let instruction_addr = crash_frame["instruction_addr"].as_str().unwrap_or_default();
        assert!(
            instruction_addr.starts_with("0x"),
            "expected hex instruction_addr, got {:?}",
            instruction_addr
        );
        let crash_function = crash_frame["function"].as_str().unwrap_or_default();
        assert!(
            crash_function.contains("from_error_builds_exception_list_with_stacktrace"),
            "expected user frame last, got {:?}",
            crash_function
        );
        assert!(
            !crash_function.contains("Exception::"),
            "expected SDK frames to be skipped, got {:?}",
            crash_function
        );
    }

    #[test]
    fn gnu_build_ids_convert_to_debug_ids_like_the_server() {
        // Vector verified against symbolic's ElfObject::debug_id (which the
        // server and CLI use): the first 16 bytes as a little-endian GUID.
        let build_id: Vec<u8> = (0..20)
            .map(|i| {
                u8::from_str_radix(
                    &"555398ebd01c90285a3d85138a19cbf9bbcec352"[i * 2..i * 2 + 2],
                    16,
                )
                .unwrap()
            })
            .collect();
        // symbolic swaps the first three GUID fields on little-endian ELF and
        // leaves them as-is on big-endian; debug_id_from_gnu_build_id mirrors
        // that, so the expected ids differ by host endianness.
        let (full, short) = if cfg!(target_endian = "little") {
            (
                "eb985355-1cd0-2890-5a3d-85138a19cbf9",
                "0000cdab-0000-0000-0000-000000000000",
            )
        } else {
            (
                "555398eb-d01c-9028-5a3d-85138a19cbf9",
                "abcd0000-0000-0000-0000-000000000000",
            )
        };
        assert_eq!(debug_id_from_gnu_build_id(&build_id).as_deref(), Some(full));

        // Short build ids are zero-padded to 16 bytes
        assert_eq!(
            debug_id_from_gnu_build_id(&[0xab, 0xcd]).as_deref(),
            Some(short)
        );
        assert_eq!(debug_id_from_gnu_build_id(&[]), None);
    }

    #[test]
    fn arch_normalizes_to_the_shared_native_vocabulary() {
        // aarch64 is reported as arm64 to match the other native SDKs; every
        // other name passes through unchanged.
        assert_eq!(normalize_arch("aarch64"), "arm64");
        assert_eq!(normalize_arch("x86_64"), "x86_64");
        assert_eq!(normalize_arch("arm"), "arm");
    }

    #[test]
    fn find_module_matches_address_ranges() {
        let module_at = |base: u64, size: u64| LoadedModule {
            base,
            end: base + size,
            image: DebugImage {
                image_type: "elf".to_string(),
                debug_id: "test".to_string(),
                code_id: None,
                image_addr: format!("0x{base:x}"),
                image_size: Some(size),
                image_vmaddr: None,
                code_file: None,
                arch: "x86_64".to_string(),
            },
        };

        let modules = vec![module_at(0x1000, 0x1000), module_at(0x4000, 0x1000)];

        assert_eq!(find_module(&modules, 0x1500).map(|m| m.base), Some(0x1000));
        assert_eq!(find_module(&modules, 0x4000).map(|m| m.base), Some(0x4000));
        assert!(find_module(&modules, 0x2000).is_none()); // gap between modules
        assert!(find_module(&modules, 0x500).is_none()); // before first module
        assert!(find_module(&modules, 0x5000).is_none()); // past the last module
    }

    #[test]
    fn captured_stacks_reference_loaded_debug_images() {
        let json = event_json(Exception::from_message(
            "AddrCheck",
            "captures addresses",
            true,
        ));

        let frames = json["properties"]["$exception_list"][0]["stacktrace"]["frames"]
            .as_array()
            .expect("expected stack frames");

        // instruction_addr is set only for frames whose module has a debug id;
        // it's omitted otherwise (e.g. system libraries without a GNU build id,
        // common on Linux). Assert the format wherever present, and that at
        // least one frame carries it.
        let mut saw_instruction_addr = false;
        for frame in frames {
            let Some(addr) = frame["instruction_addr"].as_str() else {
                continue;
            };
            saw_instruction_addr = true;
            assert!(
                addr.starts_with("0x") && u64::from_str_radix(&addr[2..], 16).is_ok(),
                "expected hex instruction_addr, got {:?}",
                frame["instruction_addr"]
            );
        }
        assert!(
            saw_instruction_addr,
            "expected at least one frame to carry an instruction_addr"
        );

        // The test binary itself is a loaded module with a debug id on the
        // platforms we capture modules on, so $debug_images must be present
        // and every entry must be referenced by at least one frame.
        let images = json["properties"]["$debug_images"]
            .as_array()
            .expect("expected $debug_images");
        assert!(!images.is_empty());
        let expected_type = super::native_image_type();
        let expected_arch = super::normalize_arch(std::env::consts::ARCH);
        for image in images {
            assert_eq!(image["type"].as_str(), Some(expected_type));
            assert_eq!(
                image["arch"].as_str(),
                Some(expected_arch.as_str()),
                "arch should match the running process"
            );
            let debug_id = image["debug_id"].as_str().unwrap_or_default();
            assert!(
                debug_id.len() >= 36,
                "expected uuid-shaped debug_id, got {:?}",
                debug_id
            );
            let image_addr = image["image_addr"].as_str().unwrap_or_default();
            assert!(
                frames
                    .iter()
                    .any(|f| f["image_addr"].as_str() == Some(image_addr)),
                "image {} not referenced by any frame",
                image_addr
            );
        }
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
    fn in_app_defaults_cover_docker_cargo_home_and_dwarf_names() {
        let options = ErrorTrackingOptions::default();

        // CARGO_HOME isn't always `~/.cargo`: the official Rust Docker images
        // put the registry under /usr/local/cargo.
        assert!(!options.is_in_app_path(
            "/usr/local/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.52.1/src/runtime/task/harness.rs"
        ));
        assert!(!options.is_in_app_path(
            "/usr/local/cargo/git/checkouts/somecrate-9a8b7c6d5e4f3a2b/0f1e2d3/src/lib.rs"
        ));
        // A renamed CARGO_HOME is still caught by cargo's own layouts.
        assert!(!options.is_in_app_path(
            "/cache/rust-deps/registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.219/src/de/mod.rs"
        ));
        assert!(!options.is_in_app_path(
            "/cache/rust-deps/git/checkouts/somecrate-9a8b7c6d5e4f3a2b/0f1e2d3/src/lib.rs"
        ));
        // ...but only cargo's `<repo>-<ident hash>/<short rev>/` checkout
        // layout: an app that happens to live under a `git/checkouts/`
        // directory stays in-app, even with a hex-looking repo-dir suffix.
        assert!(options.is_in_app_path("/srv/git/checkouts/my-service/src/main.rs"));
        assert!(options.is_in_app_path("/srv/git/checkouts/my-service-deadbeef/src/main.rs"));
        // Only cargo's real layouts match: apps under directories that merely
        // look cargo-ish stay in-app.
        assert!(options.is_in_app_path("/srv/mycargo/registry/src/model.rs"));
        assert!(options.is_in_app_path("/srv/cargo/registry/my-service/src/main.rs"));
        assert!(options
            .is_in_app_path("/srv/registry/src/index.crates.io-local/my-service/src/main.rs"));

        // DWARF-derived names hide the crate behind generic arguments
        // (`poll_future<tokio::...>`); the frame is still classified out of
        // app by its registry path, and the garbage `poll_future<tokio`
        // segment must not make the name override that.
        assert!(!options.is_in_app_frame(
            Some("/usr/local/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.52.1/src/runtime/task/harness.rs"),
            Some("poll_future<tokio::runtime::blocking::task::BlockingTask<tokio::runtime::scheduler::multi_thread::worker::{impl#0}::launch::{closure_env#0}>, tokio::runtime::blocking::schedule::BlockingSchedule>"),
        ));
        // An app function generic over a vendor type stays in-app: only the
        // base name (path before `<`) feeds the crate check.
        assert!(
            options.is_in_app_frame(Some("/app/src/worker.rs"), Some("run<tokio::time::Sleep>"),)
        );
        // A *fileless* DWARF-style name deliberately fails open to in-app: the
        // generic arguments are instantiation types, not the defining crate,
        // so guessing the owner from them would mislabel app functions generic
        // over vendor types. In practice DWARF-derived names ship with file
        // info, which classifies them (as above).
        assert!(options.is_in_app_frame(
            None,
            Some("poll_future<tokio::runtime::blocking::task::BlockingTask<T>, S>"),
        ));
        // Qualified-path renderings keep their leading `<` and still resolve
        // the crate segment.
        assert!(!options.is_in_app_frame(
            None,
            Some("<alloc::boxed::Box<F,A> as core::ops::function::FnOnce<Args>>::call_once"),
        ));

        // Bare thread/process bootstrap symbols with no source file are not
        // app code, even though no crate denylist entry can match them.
        assert!(!options.is_in_app_frame(None, Some("__clone")));
        assert!(!options.is_in_app_frame(None, Some("start_thread")));
        assert!(!options.is_in_app_frame(None, Some("main")));
        // ...but a pathless name backed by a source file keeps the path verdict,
        // and other bare symbols (`#[no_mangle]` exports, linked-in C code)
        // stay in-app.
        assert!(options.is_in_app_frame(Some("/app/src/main.rs"), Some("main")));
        assert!(options.is_in_app_frame(None, Some("my_exported_callback")));
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
    fn frames_are_trimmed_to_max_frames_keeping_the_crash_site() {
        let synthetic_frame = |index: usize| StackFrame {
            filename: None,
            line_no: None,
            function: format!("frame_{index}"),
            lang: "rust".to_string(),
            in_app: true,
            synthetic: false,
            platform: "native".to_string(),
            instruction_addr: None,
            symbol_addr: None,
            image_addr: None,
            client_resolved: false,
        };
        let exception = Exception {
            items: vec![ExceptionItem {
                exception_type: "Error".to_string(),
                value: "trimmed".to_string(),
                mechanism: ExceptionMechanism::default(),
                stacktrace: None,
            }],
            captured_frames: Some((0..MAX_FRAMES + 5).map(synthetic_frame).collect()),
            captured_images: Vec::new(),
            fingerprint: None,
            level: "error".to_string(),
        };

        let json = event_json_with(exception, &ErrorTrackingOptions::default());
        let frames = json["properties"]["$exception_list"][0]["stacktrace"]["frames"]
            .as_array()
            .expect("expected stack frames");
        assert_eq!(frames.len(), MAX_FRAMES);
        // Frames arrive in wire order (outermost first, crash site last), so
        // trimming drops the outermost frames from the front and keeps the
        // crash-site tail: `frame_0` is gone and the last frame survives.
        assert_eq!(frames[0]["function"], "frame_5");
        assert_eq!(
            frames[MAX_FRAMES - 1]["function"],
            format!("frame_{}", MAX_FRAMES + 4)
        );
    }

    #[test]
    fn stacktrace_keeps_crash_frame_last() {
        fn capture() -> ExceptionStacktrace {
            // Go through the real capture path so we assert the wire order the
            // SDK actually emits (outermost first, crash/capture site last),
            // not the raw innermost-first order of `capture_frames_current_first`.
            let mut frames = capture_raw_application_frames().0;
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
            .position(|function| function.contains("stacktrace_keeps_crash_frame_last::capture"))
            .expect("expected capture frame");
        let test_index = functions
            .iter()
            .position(|function| function.ends_with("stacktrace_keeps_crash_frame_last"))
            .expect("expected test frame");

        assert!(
            test_index < capture_index,
            "expected the caller before the innermost (capture-site) frame, got {:?}",
            functions
        );
    }

    #[test]
    fn inlined_frames_collapse_for_server_side_expansion() {
        // inline(always) is honored in debug builds, so these helpers share
        // their caller's physical frame. When the server can symbolicate the
        // address we emit ONE frame for that physical frame and let the resolver
        // expand the inline chain from the symcache; emitting the inline layers
        // here as well would double them, since the resolver re-expands every
        // address-bearing native frame. Only a frame the server can't resolve
        // keeps the client-side inline expansion.
        #[inline(always)]
        fn inline_leaf() -> Vec<StackFrame> {
            capture_raw_application_frames().0
        }

        #[inline(always)]
        fn inline_mid() -> Vec<StackFrame> {
            inline_leaf()
        }

        let frames = inline_mid();
        let functions: Vec<&str> = frames.iter().map(|frame| frame.function.as_str()).collect();

        // No two frames may carry the same instruction_addr: that duplicate is
        // exactly the double-expansion the resolver would inflict if we
        // pre-expanded inlines onto a shared address.
        let mut addrs: Vec<&str> = frames
            .iter()
            .filter_map(|frame| frame.instruction_addr.as_deref())
            .collect();
        let emitted = addrs.len();
        addrs.sort_unstable();
        addrs.dedup();
        assert_eq!(
            addrs.len(),
            emitted,
            "instruction_addr duplicated across frames: {:?}",
            frames
        );

        // client_resolved is the inverse of carrying an address: an addressed
        // frame is left for the server to resolve (false); an address-less frame
        // was resolved client-side (true).
        assert!(
            frames
                .iter()
                .all(|f| f.client_resolved == f.instruction_addr.is_none()),
            "client_resolved must be the inverse of instruction_addr presence: {:?}",
            frames
        );

        let leaf = functions
            .iter()
            .filter(|f| f.contains("inline_leaf"))
            .count();
        let mid = functions
            .iter()
            .filter(|f| f.contains("inline_mid"))
            .count();

        if frames.iter().any(|frame| frame.instruction_addr.is_some()) {
            // Resolvable: the inlined layers collapse into their physical frame,
            // which carries the address for the server to expand.
            assert!(
                leaf == 0 && mid == 0,
                "expected inlined layers collapsed for server-side expansion, got {:?}",
                functions
            );
        } else {
            // Not resolvable: client-side expansion preserves the inline chain.
            // In canonical wire order the outermost logical layer leads and the
            // inlined leaf is last, so `inline_mid` (the caller) precedes
            // `inline_leaf` (the inlined callee).
            let leaf_index = functions.iter().position(|f| f.contains("inline_leaf"));
            let mid_index = functions.iter().position(|f| f.contains("inline_mid"));
            assert!(
                matches!((leaf_index, mid_index), (Some(l), Some(m)) if m < l),
                "expected client-side inline expansion outermost first, got {:?}",
                functions
            );
        }
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
