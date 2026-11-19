//! C bindings for the PostHog Rust SDK.
//!
//! The bindings wrap the blocking [`posthog::Client`]. Capture is
//! fire-and-forget: events are queued and a background thread batches and
//! sends them. Feature flag evaluation blocks the calling thread for the
//! duration of the `/flags` request.
//!
//! See `include/posthog.h` for the C API and its ownership rules.

#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::DateTime;
use posthog::{
    Client, ClientOptionsBuilder, EvaluateFlagsOptions, Event, FeatureFlagEvaluations, FlagValue,
};
use serde_json::Value;

const LIB_NAME: &str = "posthog-rs-ffi";
const LIB_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Result of a PostHog call.
#[repr(i32)]
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostHogStatus {
    /// The call succeeded.
    POSTHOG_STATUS_OK = 0,
    /// A required pointer argument was `NULL`.
    POSTHOG_STATUS_NULL_ARGUMENT = 1,
    /// A string argument was not valid UTF-8.
    POSTHOG_STATUS_INVALID_UTF8 = 2,
    /// A JSON argument could not be parsed or had the wrong shape.
    POSTHOG_STATUS_INVALID_JSON = 3,
    /// An argument was rejected by the SDK.
    POSTHOG_STATUS_INVALID_ARGUMENT = 4,
    /// A network request failed or PostHog returned an error.
    POSTHOG_STATUS_REQUEST = 5,
    /// The SDK panicked and the panic was caught. The call may have partly
    /// run, so don't assume it had no effect.
    POSTHOG_STATUS_PANIC = 6,
}

/// The state of a feature flag in a `PostHogFlags` snapshot.
#[repr(i32)]
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostHogFlagState {
    /// The flag is not in the snapshot.
    POSTHOG_FLAG_STATE_NOT_FOUND = 0,
    /// The flag is disabled.
    POSTHOG_FLAG_STATE_DISABLED = 1,
    /// The flag is enabled and has no variant.
    POSTHOG_FLAG_STATE_ENABLED = 2,
    /// The flag is enabled with a multivariate variant.
    POSTHOG_FLAG_STATE_VARIANT = 3,
}

/// Client configuration, created with `posthog_options_new`.
pub struct PostHogOptions {
    builder: ClientOptionsBuilder,
}

/// A PostHog client, created with `posthog_client_new`.
pub struct PostHogClient {
    client: Client,
    /// Set by `posthog_shutdown`. Flag calls on a shut-down client fail
    /// instead of making network requests.
    closed: AtomicBool,
}

impl PostHogClient {
    fn ensure_open(&self) -> Result<(), FfiError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(FfiError::new(
                PostHogStatus::POSTHOG_STATUS_INVALID_ARGUMENT,
                "the client is shut down",
            ));
        }
        Ok(())
    }
}

/// An event to capture, created with `posthog_event_new` or `posthog_event_new_anon`.
pub struct PostHogEvent {
    event: Event,
}

/// A snapshot of evaluated feature flags, created with `posthog_evaluate_flags`.
pub struct PostHogFlags {
    flags: FeatureFlagEvaluations,
}

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

struct FfiError {
    status: PostHogStatus,
    message: String,
}

impl FfiError {
    fn new(status: PostHogStatus, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn null(name: &str) -> Self {
        Self::new(
            PostHogStatus::POSTHOG_STATUS_NULL_ARGUMENT,
            format!("`{name}` is NULL"),
        )
    }
}

impl From<posthog::Error> for FfiError {
    fn from(error: posthog::Error) -> Self {
        let status = match error {
            posthog::Error::InvalidArgument(_) | posthog::Error::InvalidTimestamp(_) => {
                PostHogStatus::POSTHOG_STATUS_INVALID_ARGUMENT
            }
            posthog::Error::Serialization(_) => PostHogStatus::POSTHOG_STATUS_INVALID_JSON,
            _ => PostHogStatus::POSTHOG_STATUS_REQUEST,
        };
        Self::new(status, error.to_string())
    }
}

fn set_last_error(message: Option<String>) {
    let message = message
        .map(|message| CString::new(message.replace('\0', " ")).expect("NUL bytes were replaced"));
    // `try_with` because C++ thread-local destructors can call in after this
    // thread's Rust TLS is gone, and `with` would panic outside `catch_unwind`.
    let _ = LAST_ERROR.try_with(|slot| *slot.borrow_mut() = message);
}

/// Runs `body`, records its error for `posthog_last_error`, and converts
/// panics into `PostHogStatus::POSTHOG_STATUS_PANIC` so they never unwind into C.
fn ffi_call(body: impl FnOnce() -> Result<(), FfiError>) -> PostHogStatus {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(())) => {
            set_last_error(None);
            PostHogStatus::POSTHOG_STATUS_OK
        }
        Ok(Err(error)) => {
            set_last_error(Some(error.message));
            error.status
        }
        Err(_) => {
            set_last_error(Some("PostHog SDK panicked".to_string()));
            PostHogStatus::POSTHOG_STATUS_PANIC
        }
    }
}

/// Like [`ffi_call`] for constructors, which report failure with `NULL`.
fn ffi_new<T>(body: impl FnOnce() -> Result<T, FfiError>) -> *mut T {
    let mut created = None;
    ffi_call(|| {
        created = Some(body()?);
        Ok(())
    });
    created.map_or(ptr::null_mut(), |value| Box::into_raw(Box::new(value)))
}

unsafe fn str_arg<'a>(value: *const c_char, name: &str) -> Result<&'a str, FfiError> {
    if value.is_null() {
        return Err(FfiError::null(name));
    }
    CStr::from_ptr(value).to_str().map_err(|_| {
        FfiError::new(
            PostHogStatus::POSTHOG_STATUS_INVALID_UTF8,
            format!("`{name}` is not valid UTF-8"),
        )
    })
}

unsafe fn ref_arg<'a, T>(value: *const T, name: &str) -> Result<&'a T, FfiError> {
    value.as_ref().ok_or_else(|| FfiError::null(name))
}

unsafe fn mut_arg<'a, T>(value: *mut T, name: &str) -> Result<&'a mut T, FfiError> {
    value.as_mut().ok_or_else(|| FfiError::null(name))
}

unsafe fn json_arg(value: *const c_char, name: &str) -> Result<Value, FfiError> {
    serde_json::from_str(str_arg(value, name)?).map_err(|error| {
        FfiError::new(
            PostHogStatus::POSTHOG_STATUS_INVALID_JSON,
            format!("`{name}` is not valid JSON: {error}"),
        )
    })
}

unsafe fn write_out<T>(out: *mut T, value: T) {
    if !out.is_null() {
        out.write(value);
    }
}

fn into_c_string(value: String) -> Result<*mut c_char, FfiError> {
    CString::new(value).map(CString::into_raw).map_err(|_| {
        FfiError::new(
            PostHogStatus::POSTHOG_STATUS_INVALID_ARGUMENT,
            "returned string contains a NUL byte",
        )
    })
}

/// Returns a description of the last error on the calling thread, or `NULL`
/// if the last call that returns a status or creates a handle succeeded.
/// `posthog_last_error` and the free functions leave it unchanged. The string belongs to the calling thread: copy it before the
/// next call that updates it, and before the thread exits.
#[no_mangle]
pub extern "C" fn posthog_last_error() -> *const c_char {
    LAST_ERROR
        .try_with(|slot| slot.borrow().as_ref().map_or(ptr::null(), |s| s.as_ptr()))
        .unwrap_or(ptr::null())
}

/// Frees a string returned by the SDK. `NULL` is ignored. Pass the pointer
/// unchanged, and don't write a NUL into the string first: the SDK uses the
/// string's length to free it.
#[no_mangle]
pub unsafe extern "C" fn posthog_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(CString::from_raw(value));
    }
}

/// Creates client options for a project API key (`phc_...`). Returns `NULL`
/// on error. A blank API key creates options for a disabled client.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_new(api_key: *const c_char) -> *mut PostHogOptions {
    ffi_new(|| {
        let mut builder = ClientOptionsBuilder::default();
        builder.api_key(str_arg(api_key, "api_key")?.to_string());
        Ok(PostHogOptions { builder })
    })
}

/// Frees client options. `NULL` is ignored.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_free(options: *mut PostHogOptions) {
    if !options.is_null() {
        drop(Box::from_raw(options));
    }
}

unsafe fn set_option(
    options: *mut PostHogOptions,
    set: impl FnOnce(&mut ClientOptionsBuilder),
) -> PostHogStatus {
    ffi_call(|| {
        set(&mut mut_arg(options, "options")?.builder);
        Ok(())
    })
}

/// Enables local feature flag evaluation. Requires a secret key.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_enable_local_evaluation(
    options: *mut PostHogOptions,
    value: bool,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.enable_local_evaluation(value);
    })
}

/// Never falls back to remote `/flags` when local evaluation is inconclusive.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_local_evaluation_only(
    options: *mut PostHogOptions,
    value: bool,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.local_evaluation_only(value);
    })
}

/// Disables all capture and flag requests.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_disabled(
    options: *mut PostHogOptions,
    value: bool,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.disabled(value);
    })
}

/// Disables GeoIP enrichment of events and flag requests.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_disable_geoip(
    options: *mut PostHogOptions,
    value: bool,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.disable_geoip(value);
    })
}

/// Whether events come from a server. Defaults to `true`; set `false` for
/// desktop and CLI applications.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_is_server(
    options: *mut PostHogOptions,
    value: bool,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.is_server(value);
    })
}

/// Number of queued events that triggers a flush. Defaults to 100.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_flush_at(
    options: *mut PostHogOptions,
    value: usize,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.flush_at(value);
    })
}

/// Interval between automatic flushes in milliseconds. Defaults to 5000.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_flush_interval_ms(
    options: *mut PostHogOptions,
    value: u64,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.flush_interval_ms(value);
    })
}

/// Maximum number of events sent in one request. Defaults to 100.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_max_batch_size(
    options: *mut PostHogOptions,
    value: usize,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.max_batch_size(value);
    })
}

/// Maximum number of queued events before new ones are dropped. Defaults to 10000.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_max_queue_size(
    options: *mut PostHogOptions,
    value: usize,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.max_queue_size(value);
    })
}

/// Timeout for capture and flag definition requests in seconds. Defaults to 30.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_request_timeout_seconds(
    options: *mut PostHogOptions,
    value: u64,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.request_timeout_seconds(value);
    })
}

/// Timeout for remote `/flags` requests in seconds. Defaults to 3.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_feature_flags_request_timeout_seconds(
    options: *mut PostHogOptions,
    value: u64,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.feature_flags_request_timeout_seconds(value);
    })
}

/// Retries after a transient `/flags` failure. Defaults to 1.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_feature_flags_request_max_retries(
    options: *mut PostHogOptions,
    value: u32,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.feature_flags_request_max_retries(value);
    })
}

/// Attempts per capture batch, including the first. Defaults to 4.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_max_capture_attempts(
    options: *mut PostHogOptions,
    value: u32,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.max_capture_attempts(value);
    })
}

/// Interval for polling local evaluation definitions in seconds. Defaults to 30.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_poll_interval_seconds(
    options: *mut PostHogOptions,
    value: u64,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.poll_interval_seconds(value);
    })
}

/// Maximum time shutdown spends delivering queued events in milliseconds. Defaults to 30000.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_shutdown_timeout_ms(
    options: *mut PostHogOptions,
    value: u64,
) -> PostHogStatus {
    set_option(options, |builder| {
        builder.shutdown_timeout_ms(value);
    })
}

/// Sets the PostHog host, such as `https://eu.i.posthog.com`. Defaults to US Cloud.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_host(
    options: *mut PostHogOptions,
    host: *const c_char,
) -> PostHogStatus {
    ffi_call(|| {
        let options = mut_arg(options, "options")?;
        options.builder.host(str_arg(host, "host")?);
        Ok(())
    })
}

/// Sets the secret key (`phs_...`) used for local feature flag evaluation.
#[no_mangle]
pub unsafe extern "C" fn posthog_options_set_secret_key(
    options: *mut PostHogOptions,
    secret_key: *const c_char,
) -> PostHogStatus {
    ffi_call(|| {
        let options = mut_arg(options, "options")?;
        options
            .builder
            .secret_key(str_arg(secret_key, "secret_key")?);
        Ok(())
    })
}

/// Creates a client. The options are copied and can be freed afterwards.
/// Returns `NULL` on error. Free the client with `posthog_client_free`.
#[no_mangle]
pub unsafe extern "C" fn posthog_client_new(options: *const PostHogOptions) -> *mut PostHogClient {
    ffi_new(|| {
        let mut builder = ref_arg(options, "options")?.builder.clone();
        // Reports these bindings, not the Rust SDK, in the request headers and
        // as the default `$lib` and `$lib_version` of every event.
        builder.sdk_identity(LIB_NAME, LIB_VERSION);
        let options = builder.build().map_err(|error| {
            FfiError::new(
                PostHogStatus::POSTHOG_STATUS_INVALID_ARGUMENT,
                error.to_string(),
            )
        })?;
        Ok(PostHogClient {
            client: posthog::client(options),
            closed: AtomicBool::new(false),
        })
    })
}

/// Shuts the client down, delivering queued events within the shutdown
/// timeout, and frees it. `NULL` is ignored.
#[no_mangle]
pub unsafe extern "C" fn posthog_client_free(client: *mut PostHogClient) {
    if !client.is_null() {
        let client = Box::from_raw(client);
        let _ = catch_unwind(AssertUnwindSafe(move || {
            client.client.shutdown();
            drop(client);
        }));
    }
}

/// Queues a copy of `event` for delivery. The caller still owns `event`.
#[no_mangle]
pub unsafe extern "C" fn posthog_capture(
    client: *const PostHogClient,
    event: *const PostHogEvent,
) -> PostHogStatus {
    ffi_call(|| {
        let client = ref_arg(client, "client")?;
        let event = ref_arg(event, "event")?;
        client.client.capture(event.event.clone());
        Ok(())
    })
}

/// Links `previous_id`, such as an anonymous ID, to `distinct_id`.
#[no_mangle]
pub unsafe extern "C" fn posthog_alias(
    client: *const PostHogClient,
    previous_id: *const c_char,
    distinct_id: *const c_char,
) -> PostHogStatus {
    ffi_call(|| {
        let client = ref_arg(client, "client")?;
        let event = Event::alias(
            str_arg(previous_id, "previous_id")?,
            str_arg(distinct_id, "distinct_id")?,
        )?;
        client.client.capture(event);
        Ok(())
    })
}

/// Sets properties on a group. `properties_json` must be a JSON object or `NULL`.
#[no_mangle]
pub unsafe extern "C" fn posthog_group_identify(
    client: *const PostHogClient,
    group_type: *const c_char,
    group_key: *const c_char,
    properties_json: *const c_char,
) -> PostHogStatus {
    ffi_call(|| {
        let client = ref_arg(client, "client")?;
        let properties = if properties_json.is_null() {
            Value::Object(Default::default())
        } else {
            json_arg(properties_json, "properties_json")?
        };
        let event = Event::group_identify(
            str_arg(group_type, "group_type")?,
            str_arg(group_key, "group_key")?,
            properties,
        )?;
        client.client.capture(event);
        Ok(())
    })
}

/// Blocks until queued events have been sent or failed.
#[no_mangle]
pub unsafe extern "C" fn posthog_flush(client: *const PostHogClient) -> PostHogStatus {
    ffi_call(|| {
        ref_arg(client, "client")?.client.flush();
        Ok(())
    })
}

/// Delivers queued events and stops the client, including the local
/// evaluation poller. Later captures are dropped, and later flag calls return
/// `POSTHOG_STATUS_INVALID_ARGUMENT` without a network request.
/// `posthog_client_free` also shuts down.
///
/// `shutdown_timeout_ms` bounds delivering the queue. A capture request
/// already being sent when shutdown starts runs to completion first, which
/// can take up to `request_timeout_seconds`.
#[no_mangle]
pub unsafe extern "C" fn posthog_shutdown(client: *const PostHogClient) -> PostHogStatus {
    ffi_call(|| {
        let client = ref_arg(client, "client")?;
        client.closed.store(true, Ordering::Release);
        client.client.shutdown();
        Ok(())
    })
}

/// Creates an event for a known user. An empty `distinct_id` creates an
/// anonymous event. Returns `NULL` on error.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_new(
    event: *const c_char,
    distinct_id: *const c_char,
) -> *mut PostHogEvent {
    ffi_new(|| {
        Ok(PostHogEvent {
            event: Event::new(
                str_arg(event, "event")?.to_string(),
                str_arg(distinct_id, "distinct_id")?.to_string(),
            ),
        })
    })
}

/// Creates an anonymous event that does not create a person profile.
/// Returns `NULL` on error.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_new_anon(event: *const c_char) -> *mut PostHogEvent {
    ffi_new(|| {
        Ok(PostHogEvent {
            event: Event::new_anon(str_arg(event, "event")?),
        })
    })
}

/// Frees an event. `NULL` is ignored.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_free(event: *mut PostHogEvent) {
    if !event.is_null() {
        drop(Box::from_raw(event));
    }
}

unsafe fn set_property(
    event: *mut PostHogEvent,
    key: *const c_char,
    value: impl FnOnce() -> Result<Value, FfiError>,
) -> PostHogStatus {
    ffi_call(|| {
        let event = mut_arg(event, "event")?;
        let key = str_arg(key, "key")?;
        event.event.insert_prop(key, value()?)?;
        Ok(())
    })
}

/// Sets a property to any JSON value, such as `{"plan":"pro"}` or `[1,2]`.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_set_property_json(
    event: *mut PostHogEvent,
    key: *const c_char,
    value_json: *const c_char,
) -> PostHogStatus {
    set_property(event, key, || json_arg(value_json, "value_json"))
}

/// Sets a string property.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_set_property_string(
    event: *mut PostHogEvent,
    key: *const c_char,
    value: *const c_char,
) -> PostHogStatus {
    set_property(event, key, || str_arg(value, "value").map(Value::from))
}

/// Sets an integer property.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_set_property_int(
    event: *mut PostHogEvent,
    key: *const c_char,
    value: i64,
) -> PostHogStatus {
    set_property(event, key, || Ok(Value::from(value)))
}

/// Sets a floating-point property. NaN and infinity are sent as `null`.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_set_property_double(
    event: *mut PostHogEvent,
    key: *const c_char,
    value: f64,
) -> PostHogStatus {
    set_property(event, key, || Ok(Value::from(value)))
}

/// Sets a boolean property.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_set_property_bool(
    event: *mut PostHogEvent,
    key: *const c_char,
    value: bool,
) -> PostHogStatus {
    set_property(event, key, || Ok(Value::from(value)))
}

/// Sets a capture option, such as `process_person_profile`, to a JSON value.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_set_option_json(
    event: *mut PostHogEvent,
    key: *const c_char,
    value_json: *const c_char,
) -> PostHogStatus {
    ffi_call(|| {
        let event = mut_arg(event, "event")?;
        let value = json_arg(value_json, "value_json")?;
        event.event.insert_option(str_arg(key, "key")?, value)?;
        Ok(())
    })
}

/// Associates the event with a group, such as `company` / `acme`.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_add_group(
    event: *mut PostHogEvent,
    group_type: *const c_char,
    group_key: *const c_char,
) -> PostHogStatus {
    ffi_call(|| {
        let event = mut_arg(event, "event")?;
        event.event.add_group(
            str_arg(group_type, "group_type")?,
            str_arg(group_key, "group_key")?,
        );
        Ok(())
    })
}

/// Sets when the event happened, in milliseconds since the Unix epoch.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_set_timestamp_ms(
    event: *mut PostHogEvent,
    unix_ms: i64,
) -> PostHogStatus {
    ffi_call(|| {
        let event = mut_arg(event, "event")?;
        let timestamp = DateTime::from_timestamp_millis(unix_ms).ok_or_else(|| {
            FfiError::new(
                PostHogStatus::POSTHOG_STATUS_INVALID_ARGUMENT,
                "timestamp is out of range",
            )
        })?;
        event.event.set_timestamp(timestamp)?;
        Ok(())
    })
}

/// Sets the event UUID, used to deduplicate events.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_set_uuid(
    event: *mut PostHogEvent,
    uuid: *const c_char,
) -> PostHogStatus {
    ffi_call(|| {
        let event = mut_arg(event, "event")?;
        let uuid = uuid::Uuid::parse_str(str_arg(uuid, "uuid")?).map_err(|error| {
            FfiError::new(
                PostHogStatus::POSTHOG_STATUS_INVALID_ARGUMENT,
                error.to_string(),
            )
        })?;
        event.event.set_uuid(uuid);
        Ok(())
    })
}

/// Adds `$feature/<key>` and `$active_feature_flags` from a snapshot without
/// another `/flags` request.
#[no_mangle]
pub unsafe extern "C" fn posthog_event_set_flags(
    event: *mut PostHogEvent,
    flags: *const PostHogFlags,
) -> PostHogStatus {
    ffi_call(|| {
        let event = mut_arg(event, "event")?;
        event.event.with_flags(&ref_arg(flags, "flags")?.flags);
        Ok(())
    })
}

fn object_field<T>(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<T>, FfiError>
where
    T: serde::de::DeserializeOwned,
{
    match object.remove(key) {
        None => Ok(None),
        Some(Value::Null) => Err(FfiError::new(
            PostHogStatus::POSTHOG_STATUS_INVALID_JSON,
            format!("`options_json.{key}` is null; omit the field instead"),
        )),
        Some(value) => serde_json::from_value(value).map(Some).map_err(|error| {
            FfiError::new(
                PostHogStatus::POSTHOG_STATUS_INVALID_JSON,
                format!("`options_json.{key}` has the wrong type: {error}"),
            )
        }),
    }
}

fn evaluate_flags_options(options_json: Option<Value>) -> Result<EvaluateFlagsOptions, FfiError> {
    let mut options = EvaluateFlagsOptions::default();
    let Some(json) = options_json else {
        return Ok(options);
    };
    let Value::Object(mut object) = json else {
        return Err(FfiError::new(
            PostHogStatus::POSTHOG_STATUS_INVALID_JSON,
            "`options_json` must be a JSON object",
        ));
    };
    options.groups = object_field::<HashMap<String, String>>(&mut object, "groups")?;
    options.person_properties = object_field(&mut object, "person_properties")?;
    options.group_properties = object_field(&mut object, "group_properties")?;
    options.only_evaluate_locally =
        object_field(&mut object, "only_evaluate_locally")?.unwrap_or(false);
    options.disable_geoip = object_field(&mut object, "disable_geoip")?;
    options.flag_keys = object_field(&mut object, "flag_keys")?;
    if let Some(key) = object.keys().next() {
        return Err(FfiError::new(
            PostHogStatus::POSTHOG_STATUS_INVALID_JSON,
            format!("`options_json` has unknown key `{key}`"),
        ));
    }
    Ok(options)
}

/// Evaluates feature flags for `distinct_id` and writes a snapshot to
/// `flags_out`, or `NULL` on error. Blocks while flags are fetched. Free the
/// snapshot with `posthog_flags_free`.
///
/// `options_json` may be `NULL` or a JSON object with any of these fields:
/// `groups` (`{"company": "acme"}`), `person_properties` (`{"plan": "pro"}`),
/// `group_properties` (`{"company": {"size": 10}}`), `only_evaluate_locally`
/// (bool), `disable_geoip` (bool) and `flag_keys` (`["beta"]`). Omit a field
/// to use its default. `null` and unknown fields are rejected.
#[no_mangle]
pub unsafe extern "C" fn posthog_evaluate_flags(
    client: *const PostHogClient,
    distinct_id: *const c_char,
    options_json: *const c_char,
    flags_out: *mut *mut PostHogFlags,
) -> PostHogStatus {
    write_out(flags_out, ptr::null_mut());
    ffi_call(|| {
        if flags_out.is_null() {
            return Err(FfiError::null("flags_out"));
        }
        let client = ref_arg(client, "client")?;
        client.ensure_open()?;
        let distinct_id = str_arg(distinct_id, "distinct_id")?;
        let options_json = if options_json.is_null() {
            None
        } else {
            Some(json_arg(options_json, "options_json")?)
        };
        let options = evaluate_flags_options(options_json)?;
        let flags = client.client.evaluate_flags(distinct_id, options)?;
        flags_out.write(Box::into_raw(Box::new(PostHogFlags { flags })));
        Ok(())
    })
}

/// Fetches local evaluation definitions now. Requires local evaluation.
#[no_mangle]
pub unsafe extern "C" fn posthog_reload_feature_flags(
    client: *const PostHogClient,
) -> PostHogStatus {
    ffi_call(|| {
        let client = ref_arg(client, "client")?;
        client.ensure_open()?;
        client.client.reload_feature_flags()?;
        Ok(())
    })
}

/// Frees a flags snapshot. `NULL` is ignored.
#[no_mangle]
pub unsafe extern "C" fn posthog_flags_free(flags: *mut PostHogFlags) {
    if !flags.is_null() {
        drop(Box::from_raw(flags));
    }
}

/// Writes whether `key` is enabled to `enabled_out` and records a
/// `$feature_flag_called` event.
#[no_mangle]
pub unsafe extern "C" fn posthog_flags_is_enabled(
    flags: *const PostHogFlags,
    key: *const c_char,
    enabled_out: *mut bool,
) -> PostHogStatus {
    write_out(enabled_out, false);
    ffi_call(|| {
        if enabled_out.is_null() {
            return Err(FfiError::null("enabled_out"));
        }
        let flags = ref_arg(flags, "flags")?;
        enabled_out.write(flags.flags.is_enabled(str_arg(key, "key")?));
        Ok(())
    })
}

/// Writes the state of `key` to `state_out` and records a
/// `$feature_flag_called` event. For `POSTHOG_FLAG_STATE_VARIANT`, writes the
/// variant to `variant_out` when it is not `NULL`; free it with
/// `posthog_string_free`. Otherwise `*variant_out` is set to `NULL`.
#[no_mangle]
pub unsafe extern "C" fn posthog_flags_get_flag(
    flags: *const PostHogFlags,
    key: *const c_char,
    state_out: *mut PostHogFlagState,
    variant_out: *mut *mut c_char,
) -> PostHogStatus {
    write_out(state_out, PostHogFlagState::POSTHOG_FLAG_STATE_NOT_FOUND);
    write_out(variant_out, ptr::null_mut());
    ffi_call(|| {
        if state_out.is_null() {
            return Err(FfiError::null("state_out"));
        }
        let flags = ref_arg(flags, "flags")?;
        let (state, variant) = match flags.flags.get_flag(str_arg(key, "key")?) {
            None => (PostHogFlagState::POSTHOG_FLAG_STATE_NOT_FOUND, None),
            Some(FlagValue::Boolean(false)) => {
                (PostHogFlagState::POSTHOG_FLAG_STATE_DISABLED, None)
            }
            Some(FlagValue::Boolean(true)) => (PostHogFlagState::POSTHOG_FLAG_STATE_ENABLED, None),
            Some(FlagValue::String(variant)) => {
                (PostHogFlagState::POSTHOG_FLAG_STATE_VARIANT, Some(variant))
            }
        };
        if let (Some(variant), false) = (variant, variant_out.is_null()) {
            variant_out.write(into_c_string(variant)?);
        }
        state_out.write(state);
        Ok(())
    })
}

/// Writes the JSON payload of `key` to `payload_json_out`, or `NULL` if it has
/// none. Free it with `posthog_string_free`. Does not record an event.
#[no_mangle]
pub unsafe extern "C" fn posthog_flags_get_payload(
    flags: *const PostHogFlags,
    key: *const c_char,
    payload_json_out: *mut *mut c_char,
) -> PostHogStatus {
    write_out(payload_json_out, ptr::null_mut());
    ffi_call(|| {
        if payload_json_out.is_null() {
            return Err(FfiError::null("payload_json_out"));
        }
        let flags = ref_arg(flags, "flags")?;
        if let Some(payload) = flags.flags.get_flag_payload(str_arg(key, "key")?) {
            payload_json_out.write(into_c_string(payload.to_string())?);
        }
        Ok(())
    })
}

/// Writes the snapshot's flag keys as a JSON array to `keys_json_out`. Free
/// it with `posthog_string_free`.
#[no_mangle]
pub unsafe extern "C" fn posthog_flags_keys(
    flags: *const PostHogFlags,
    keys_json_out: *mut *mut c_char,
) -> PostHogStatus {
    write_out(keys_json_out, ptr::null_mut());
    ffi_call(|| {
        if keys_json_out.is_null() {
            return Err(FfiError::null("keys_json_out"));
        }
        let mut keys = ref_arg(flags, "flags")?.flags.keys();
        keys.sort();
        keys_json_out.write(into_c_string(Value::from(keys).to_string())?);
        Ok(())
    })
}

/// Returns a new snapshot with only the flags read so far, for
/// `posthog_event_set_flags`. Returns `NULL` on error.
#[no_mangle]
pub unsafe extern "C" fn posthog_flags_only_accessed(
    flags: *const PostHogFlags,
) -> *mut PostHogFlags {
    ffi_new(|| {
        Ok(PostHogFlags {
            flags: ref_arg(flags, "flags")?.flags.only_accessed(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn handles_are_thread_safe() {
        assert_send_sync::<PostHogClient>();
        assert_send_sync::<PostHogFlags>();
    }

    #[test]
    fn enums_are_four_bytes() {
        assert_eq!(std::mem::size_of::<PostHogStatus>(), 4);
        assert_eq!(std::mem::size_of::<PostHogFlagState>(), 4);
    }

    #[test]
    fn rejects_null_flag_options() {
        for key in [
            "groups",
            "person_properties",
            "group_properties",
            "only_evaluate_locally",
            "disable_geoip",
            "flag_keys",
        ] {
            let error = evaluate_flags_options(Some(serde_json::json!({ key: null }))).unwrap_err();
            assert_eq!(
                error.status,
                PostHogStatus::POSTHOG_STATUS_INVALID_JSON,
                "{key}"
            );
            assert!(error.message.contains(key));
        }
    }

    #[test]
    fn rejects_unknown_flag_option() {
        let error = evaluate_flags_options(Some(serde_json::json!({"group": {}}))).unwrap_err();
        assert_eq!(error.status, PostHogStatus::POSTHOG_STATUS_INVALID_JSON);
        assert!(error.message.contains("group"));
    }
}
