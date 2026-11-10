//! The I/O-free half of transactional email, shared by the async and blocking
//! clients: argument checks, request bodies and headers, reply parsing, the
//! retry policy, and the in-flight count that `shutdown` waits on.

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};
use serde_json::{Map, Value};
use tracing::warn;
use uuid::Uuid;

use super::error::{EmailError, EmailErrorKind};
use super::types::{
    CodeEmail, EmailDeliveryStatus, EmailStatus, EmailWarning, SendEmail, SendEmailResult,
    SendOptions, TemplateEmail,
};
use crate::client::{get_default_user_agent, ClientOptions};

pub(crate) const SEND_PATH: &str = "/api/projects/@current/messaging/send/email/";
const STATUS_PATH: &str = "/api/projects/@current/messaging/emails/";
const US_APP_HOST: &str = "https://us.posthog.com";
const EU_APP_HOST: &str = "https://eu.posthog.com";
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
/// The most of a reply body the client reads, after decompression. PostHog's
/// email replies are a few hundred bytes, so a larger body is unreadable.
pub(crate) const MAX_REPLY_BYTES: usize = 1 << 20;
const MAX_IDEMPOTENCY_KEY_LEN: usize = 255;
const MAX_TEMPLATE_KEY_LEN: usize = 400;

pub(crate) const SEND_NOT_DONE: &str = "no email was sent";
pub(crate) const GET_NOT_DONE: &str = "no email status was read";
pub(crate) const SEND_KEY_NEEDED: &str =
    "Sending email needs secret_key, a project secret API key with the transactional_email:write scope.";
pub(crate) const GET_KEY_NEEDED: &str = "Reading an email status needs secret_key, a project secret API key with the transactional_email:read or transactional_email:write scope.";

/// A send whose arguments passed every check.
#[derive(Debug)]
pub(crate) struct PreparedSend {
    pub(crate) body: Vec<u8>,
    pub(crate) idempotency_key: String,
    pub(crate) max_retries: Option<u32>,
}

/// Where and how the client reaches the email API.
#[derive(Debug)]
pub(crate) struct Connection {
    pub(crate) app_host: String,
    pub(crate) timeout: Duration,
    pub(crate) max_retries: u32,
    headers: HeaderMap,
}

impl Connection {
    /// The headers of a request, with an idempotency key for a send.
    pub(crate) fn headers(&self, idempotency_key: Option<&str>) -> Result<HeaderMap, EmailError> {
        let mut headers = self.headers.clone();
        if let Some(key) = idempotency_key {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            let value = HeaderValue::from_str(key).map_err(|_| invalid_idempotency_key())?;
            headers.insert(HeaderName::from_static("idempotency-key"), value);
        }
        Ok(headers)
    }
}

/// A PostHog answer, read whole.
#[derive(Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) payload: Option<Value>,
    pub(crate) retry_after: Option<Duration>,
}

/// The reply for a read body. An accepted send may already be queued, so an
/// unreadable 2xx body stays a retryable network error. An unreadable refusal
/// keeps its status and `Retry-After`, which still decide the retry.
pub(crate) fn reply_from_body(
    status: u16,
    retry_after: Option<Duration>,
    body: Result<Vec<u8>, EmailError>,
) -> Result<Reply, EmailError> {
    match body {
        Ok(bytes) => Ok(Reply {
            status,
            payload: serde_json::from_slice(&bytes).ok(),
            retry_after,
        }),
        Err(unreadable_acceptance) if (200..300).contains(&status) => Err(unreadable_acceptance),
        Err(_unreadable_refusal) => Ok(Reply {
            status,
            payload: None,
            retry_after,
        }),
    }
}

pub(crate) fn prepare_send(email: SendEmail) -> Result<PreparedSend, EmailError> {
    let (to, mut body, options) = match email {
        SendEmail::Template(email) => {
            check_recipient(&email.to, &email.options)?;
            let body = template_body(&email)?;
            (email.to, body, email.options)
        }
        SendEmail::Code(email) => {
            check_recipient(&email.to, &email.options)?;
            let body = code_body(&email)?;
            (email.to, body, email.options)
        }
    };
    if let Some(distinct_id) = options.distinct_id {
        body.insert("distinct_id".into(), Value::String(distinct_id));
    }
    body.insert("to".into(), Value::String(to));
    let idempotency_key = match options.idempotency_key {
        Some(key) => key,
        None => Uuid::new_v4().to_string(),
    };
    let body = serde_json::to_vec(&body).map_err(|error| {
        EmailError::new(
            "unexpected_error",
            "The email could not be encoded as JSON.",
        )
        .with_source(error)
    })?;
    Ok(PreparedSend {
        body,
        idempotency_key,
        max_retries: options.max_retries,
    })
}

fn check_recipient(to: &str, options: &SendOptions) -> Result<(), EmailError> {
    if to.trim().is_empty() {
        return Err(EmailError::invalid(
            "invalid_recipient",
            "to must be one email address as a nonempty string.",
            "to",
        ));
    }
    if options.distinct_id.as_deref() == Some("") {
        return Err(EmailError::invalid(
            "invalid_distinct_id",
            "distinct_id must be a nonempty string.",
            "distinct_id",
        ));
    }
    if let Some(key) = &options.idempotency_key {
        if key.is_empty()
            || key.len() > MAX_IDEMPOTENCY_KEY_LEN
            || !key.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(invalid_idempotency_key());
        }
    }
    Ok(())
}

fn invalid_idempotency_key() -> EmailError {
    EmailError::invalid(
        "invalid_idempotency_key",
        "idempotency_key must be 1 to 255 visible ASCII characters without spaces.",
        "idempotency_key",
    )
}

fn template_body(email: &TemplateEmail) -> Result<Map<String, Value>, EmailError> {
    let template_id = Uuid::parse_str(email.template_id.trim()).map_err(|_| {
        EmailError::invalid(
            "invalid_template_id",
            "template_id must be the template's UUID.",
            "template_id",
        )
    })?;
    let message_data = match &email.template_data {
        None => Value::Object(Map::new()),
        Some(data @ Value::Object(_)) => data.clone(),
        Some(_) => {
            return Err(EmailError::invalid(
                "invalid_template_data",
                "template_data must be a JSON object.",
                "template_data",
            ))
        }
    };
    let mut body = Map::new();
    body.insert(
        "template_id".into(),
        Value::String(template_id.hyphenated().to_string()),
    );
    body.insert("message_data".into(), message_data);
    Ok(body)
}

fn code_body(email: &CodeEmail) -> Result<Map<String, Value>, EmailError> {
    let key = &email.template_key;
    if key.is_empty()
        || key.len() > MAX_TEMPLATE_KEY_LEN
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(EmailError::invalid(
            "invalid_template_key",
            "template_key can only have letters, digits, hyphens (-) and underscores (_), at most 400.",
            "template_key",
        ));
    }
    if email.html.is_none() && email.text.is_none() {
        return Err(EmailError::invalid(
            "invalid_form",
            "A send with template_key needs html, text or both.",
            "html",
        ));
    }
    let mut body = Map::new();
    body.insert("template_key".into(), Value::String(key.clone()));
    body.insert("subject".into(), Value::String(email.subject.clone()));
    if let Some(html) = &email.html {
        body.insert("html".into(), Value::String(html.clone()));
    }
    if let Some(text) = &email.text {
        body.insert("text".into(), Value::String(text.clone()));
    }
    if let Some(from) = &email.from {
        body.insert("from".into(), Value::String(from.clone()));
    }
    if !email.reply_to.is_empty() {
        body.insert(
            "reply_to".into(),
            Value::Array(email.reply_to.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(tracking) = &email.tracking {
        body.insert("tracking".into(), tracking.to_json());
    }
    if let Some(capture) = email.capture_engagement {
        body.insert("capture_engagement".into(), Value::Bool(capture));
    }
    Ok(body)
}

/// The status read's path for a message ID, or an argument error.
pub(crate) fn status_path(message_id: &str) -> Result<String, EmailError> {
    let id = Uuid::parse_str(message_id.trim()).map_err(|_| {
        EmailError::invalid(
            "invalid_message_id",
            "message_id must be the message ID that send() returned.",
            "message_id",
        )
    })?;
    Ok(format!("{STATUS_PATH}{}/", id.hyphenated()))
}

/// The app host for email requests, or `None` when the client can't tell it.
///
/// PostHog Cloud ingestion hosts don't serve the app API, so they map to their
/// region's app host. A caller-set app host wins over the client's `host`. A
/// custom `host` is often an ingestion proxy, which must not receive the
/// secret key, so it never becomes the app host.
pub(crate) fn resolve_app_host(host: Option<&str>, app_host: Option<&str>) -> Option<String> {
    fn cloud_app_host(host: &str) -> Option<&'static str> {
        match host {
            "https://us.i.posthog.com" | "https://us.posthog.com" | "https://app.posthog.com" => {
                Some(US_APP_HOST)
            }
            "https://eu.i.posthog.com" | "https://eu.posthog.com" => Some(EU_APP_HOST),
            _ => None,
        }
    }
    let normalize = |host: &str| host.trim().trim_end_matches('/').to_string();
    if let Some(explicit) = app_host.map(normalize).filter(|host| !host.is_empty()) {
        return Some(
            cloud_app_host(&explicit)
                .map(str::to_string)
                .unwrap_or(explicit),
        );
    }
    cloud_app_host(&normalize(host?)).map(str::to_string)
}

/// The app host, timeout, retry limit and base headers, or the error that
/// stops every email request of this client.
pub(crate) fn connect(options: &ClientOptions, key_needed: &str) -> Result<Connection, EmailError> {
    let app_host = resolve_app_host(options.host.as_deref(), options.app_host.as_deref())
        .ok_or_else(|| {
            EmailError::new(
                "missing_app_host",
                "The client's host is not PostHog Cloud, so the client can't tell where to send email. Set app_host to your PostHog app host on the client.",
            )
        })?;
    let secret_key = options
        .secret_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| EmailError::new("missing_secret_key", key_needed))?;

    let mut headers = HeaderMap::new();
    let mut authorization =
        HeaderValue::from_str(&format!("Bearer {secret_key}")).map_err(|_| {
            EmailError::new(
                "invalid_secret_key",
                "secret_key holds characters that can't go in an HTTP header.",
            )
        })?;
    authorization.set_sensitive(true);
    headers.insert(AUTHORIZATION, authorization);
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    if let Ok(user_agent) = HeaderValue::from_str(&get_default_user_agent()) {
        headers.insert(USER_AGENT, user_agent);
    }
    if let Some(migrated_from) = &options.migrated_from {
        let value = HeaderValue::from_str(migrated_from)
            .ok()
            .filter(|_| {
                migrated_from
                    .bytes()
                    .all(|byte| (0x21..=0x7e).contains(&byte))
            })
            .ok_or_else(|| {
                EmailError::new(
                    "invalid_migrated_from",
                    "migrated_from must be visible ASCII characters, such as \"resend\".",
                )
            })?;
        headers.insert(HeaderName::from_static("x-posthog-migrated-from"), value);
    }

    Ok(Connection {
        app_host,
        timeout: Duration::from_secs(options.email_request_timeout_seconds),
        max_retries: options.email_request_max_retries,
        headers,
    })
}

/// A failed request with no answer from PostHog.
pub(crate) fn request_error(error: reqwest::Error) -> EmailError {
    if error.is_builder() {
        return EmailError::new(
            "unexpected_error",
            "The request to PostHog could not be built.",
        )
        .with_source(error);
    }
    EmailError::new("network_error", "The request to PostHog failed.").with_source(error)
}

/// A reply body that could not be read. Like any network error it is
/// retried, because an accepted send may already be queued.
pub(crate) fn read_error(error: impl std::error::Error + Send + Sync + 'static) -> EmailError {
    EmailError::new("network_error", "The reply from PostHog could not be read.").with_source(error)
}

pub(crate) fn reply_too_large() -> EmailError {
    EmailError::new(
        "network_error",
        format!("PostHog's reply is larger than {MAX_REPLY_BYTES} bytes, so the client did not read it."),
    )
}

fn text(fields: &Map<String, Value>, name: &str) -> Option<String> {
    match fields.get(name) {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

fn reply_fields(payload: Option<Value>) -> Map<String, Value> {
    match payload {
        Some(Value::Object(fields)) => fields,
        _ => Map::new(),
    }
}

fn reply_error(
    fields: &Map<String, Value>,
    status: u16,
    retry_after: Option<Duration>,
) -> EmailError {
    EmailError::from_response(
        status,
        text(fields, "code"),
        text(fields, "detail"),
        retry_after,
    )
    .with_field(text(fields, "attr"))
    .with_details(
        text(fields, "why"),
        text(fields, "fix"),
        text(fields, "docs_url"),
    )
}

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// A `2xx` reply's status and JSON fields.
pub(crate) struct Accepted {
    status: u16,
    fields: Map<String, Value>,
}

/// The reply's fields when PostHog answered `2xx`, else PostHog's error. Only
/// this error can be retried: a malformed `2xx` is reported as it is.
pub(crate) fn accept(reply: Reply) -> Result<Accepted, EmailError> {
    let fields = reply_fields(reply.payload);
    if !is_success(reply.status) {
        return Err(reply_error(&fields, reply.status, reply.retry_after));
    }
    Ok(Accepted {
        status: reply.status,
        fields,
    })
}

pub(crate) fn parse_send_reply(reply: Accepted) -> Result<SendEmailResult, EmailError> {
    let Accepted { status, fields } = reply;
    let id = text(&fields, "message_id").ok_or_else(|| {
        EmailError::from_response(
            status,
            None,
            Some("PostHog answered without a message ID.".into()),
            None,
        )
    })?;
    let warnings = match fields.get("warnings") {
        Some(Value::Array(warnings)) => warnings
            .iter()
            .filter_map(Value::as_object)
            .map(|warning| EmailWarning {
                code: text(warning, "code").unwrap_or_else(|| "warning".into()),
                message: text(warning, "detail")
                    .or_else(|| text(warning, "message"))
                    .unwrap_or_default(),
                fix: text(warning, "fix"),
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(SendEmailResult {
        id,
        replayed: fields.get("replayed") == Some(&Value::Bool(true)),
        warnings,
    })
}

fn timestamp(fields: &Map<String, Value>, name: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&text(fields, name)?)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

pub(crate) fn parse_status_reply(reply: Accepted) -> Result<EmailStatus, EmailError> {
    let Accepted { status, fields } = reply;
    match (
        text(&fields, "id"),
        text(&fields, "status"),
        timestamp(&fields, "created_at"),
        timestamp(&fields, "status_at"),
    ) {
        (Some(id), Some(delivery), Some(created_at), Some(status_at)) => Ok(EmailStatus {
            id,
            status: EmailDeliveryStatus::parse(&delivery),
            created_at,
            status_at,
            reason: text(&fields, "reason"),
        }),
        _ => Err(EmailError::from_response(
            status,
            None,
            Some("PostHog answered without an email status.".into()),
            None,
        )),
    }
}

pub(crate) fn log_warnings(result: &SendEmailResult) {
    for warning in &result.warnings {
        warn!(
            "PostHog email warning {}: {}",
            warning.code, warning.message
        );
    }
}

/// A send carries an idempotency key, so a retry can't queue a second email.
/// A status read changes nothing.
fn is_retryable(error: &EmailError) -> bool {
    match error.status() {
        None => error.is_network_error(),
        Some(409) => error.code() == "idempotency_key_in_flight",
        Some(429) => error.kind() != EmailErrorKind::MonthlyQuotaExceeded,
        Some(status) => status == 503,
    }
}

/// How long to wait before retry `attempt` (counting from 0), or `None` to
/// report the error now. A `Retry-After` longer than the longest backoff
/// reports the error now, because retrying sooner would break the server's
/// rate limit.
pub(crate) fn retry_delay(error: &EmailError, attempt: u32, max_retries: u32) -> Option<Duration> {
    if attempt >= max_retries || !is_retryable(error) {
        return None;
    }
    let retry_after = error.retry_after().unwrap_or_default();
    if retry_after > MAX_RETRY_DELAY {
        return None;
    }
    let backoff =
        Duration::from_secs_f64(2f64.powi(attempt.min(16) as i32) * jitter()).min(MAX_RETRY_DELAY);
    Some(backoff.max(retry_after))
}

/// A factor from 0.8 to 1.2, so clients that failed together don't retry
/// together.
fn jitter() -> f64 {
    let random = (Uuid::new_v4().as_u128() & u128::from(u32::MAX)) as f64 / f64::from(u32::MAX);
    0.8 + 0.4 * random
}

#[derive(Debug, Default)]
struct TrackerState {
    closed: bool,
    in_flight: usize,
}

/// Counts the email requests in flight, retries included, so `shutdown` can
/// wait for them, and refuses new ones once the client shuts down.
#[derive(Debug, Default)]
pub(crate) struct EmailTracker {
    state: Mutex<TrackerState>,
    idle: std::sync::Condvar,
    #[cfg(feature = "async-client")]
    idle_on_runtime: tokio::sync::Notify,
}

/// One email request in flight. Dropping it, also when the caller drops the
/// request's future, ends it.
pub(crate) struct InFlight<'a>(&'a EmailTracker);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.in_flight -= 1;
        if state.in_flight == 0 {
            self.0.idle.notify_all();
            #[cfg(feature = "async-client")]
            self.0.idle_on_runtime.notify_waiters();
        }
    }
}

impl EmailTracker {
    fn lock(&self) -> MutexGuard<'_, TrackerState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Starts an email request, or refuses it when the client is disabled or
    /// shut down.
    pub(crate) fn admit(
        &self,
        disabled: bool,
        nothing_done: &str,
    ) -> Result<InFlight<'_>, EmailError> {
        if disabled {
            return Err(EmailError::new(
                "client_disabled",
                format!("The PostHog client is disabled, so {nothing_done}."),
            ));
        }
        let mut state = self.lock();
        if state.closed {
            return Err(EmailError::new(
                "client_closed",
                format!("The PostHog client is shut down, so {nothing_done}."),
            ));
        }
        state.in_flight += 1;
        Ok(InFlight(self))
    }

    /// Refuses every later email request.
    pub(crate) fn close(&self) {
        self.lock().closed = true;
    }

    /// Waits until no email request is in flight, or until `deadline`. Off a
    /// Tokio runtime, such as under `futures::executor::block_on`, it blocks
    /// the calling thread instead.
    ///
    /// `notify_waiters` wakes every `Notified` made before it, so each pass
    /// makes one before it reads the count.
    #[cfg(feature = "async-client")]
    pub(crate) async fn wait_idle(&self, deadline: std::time::Instant) {
        if tokio::runtime::Handle::try_current().is_err() {
            return self.block_until_idle(deadline);
        }
        loop {
            let idle = self.idle_on_runtime.notified();
            if self.lock().in_flight == 0 {
                return;
            }
            if tokio::time::timeout_at(deadline.into(), idle)
                .await
                .is_err()
            {
                return;
            }
        }
    }

    /// Waits until no email request is in flight, or until `deadline`.
    #[cfg(not(feature = "async-client"))]
    pub(crate) fn wait_idle(&self, deadline: std::time::Instant) {
        self.block_until_idle(deadline);
    }

    fn block_until_idle(&self, deadline: std::time::Instant) {
        let mut state = self.lock();
        while state.in_flight > 0 {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return;
            };
            state = self
                .idle
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(prepared: &PreparedSend) -> Value {
        serde_json::from_slice(&prepared.body).unwrap()
    }

    fn send_reply(reply: Reply) -> Result<SendEmailResult, EmailError> {
        accept(reply).and_then(parse_send_reply)
    }

    fn status_reply(reply: Reply) -> Result<EmailStatus, EmailError> {
        accept(reply).and_then(parse_status_reply)
    }

    fn reply(status: u16, payload: Value) -> Reply {
        Reply {
            status,
            payload: Some(payload),
            retry_after: None,
        }
    }

    #[test]
    fn template_send_lowercases_the_id_and_defaults_the_data() {
        let prepared = prepare_send(
            TemplateEmail::new("user@example.com", "0190F6D2-7C1E-7B3A-9F5E-1C2D3E4F5A6B").into(),
        )
        .unwrap();
        assert_eq!(
            body(&prepared),
            json!({
                "template_id": "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b",
                "to": "user@example.com",
                "message_data": {},
            })
        );
        assert_eq!(
            Uuid::parse_str(&prepared.idempotency_key)
                .unwrap()
                .get_version_num(),
            4
        );
        assert_eq!(prepared.max_retries, None);
    }

    #[test]
    fn argument_checks_name_the_field() {
        let cases: Vec<(SendEmail, &str, &str)> = vec![
            (
                TemplateEmail::new(" ", "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b").into(),
                "invalid_recipient",
                "to",
            ),
            (
                TemplateEmail::new("a@example.com", "welcome").into(),
                "invalid_template_id",
                "template_id",
            ),
            (
                TemplateEmail::new("a@example.com", "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b")
                    .template_data(json!(["not", "an", "object"]))
                    .into(),
                "invalid_template_data",
                "template_data",
            ),
            (
                TemplateEmail::new("a@example.com", "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b")
                    .distinct_id("")
                    .into(),
                "invalid_distinct_id",
                "distinct_id",
            ),
            (
                TemplateEmail::new("a@example.com", "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b")
                    .idempotency_key("has space")
                    .into(),
                "invalid_idempotency_key",
                "idempotency_key",
            ),
            (
                TemplateEmail::new("a@example.com", "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b")
                    .idempotency_key("k".repeat(256))
                    .into(),
                "invalid_idempotency_key",
                "idempotency_key",
            ),
            (
                CodeEmail::new("a@example.com", "has space", "Hi")
                    .text("x")
                    .into(),
                "invalid_template_key",
                "template_key",
            ),
            (
                CodeEmail::new("a@example.com", "k".repeat(401), "Hi")
                    .text("x")
                    .into(),
                "invalid_template_key",
                "template_key",
            ),
            (
                CodeEmail::new("a@example.com", "welcome", "Hi").into(),
                "invalid_form",
                "html",
            ),
        ];
        for (email, code, field) in cases {
            let error = prepare_send(email).unwrap_err();
            assert_eq!(error.code(), code);
            assert_eq!(error.field(), Some(field));
            assert_eq!(
                error.kind(),
                crate::EmailErrorKind::ValidationError,
                "{code}"
            );
            assert_eq!(error.status(), None);
        }
    }

    #[test]
    fn longest_keys_pass() {
        prepare_send(
            CodeEmail::new("a@example.com", "k".repeat(400), "Hi")
                .html("<p>x</p>")
                .idempotency_key("~".repeat(255))
                .into(),
        )
        .unwrap();
    }

    #[test]
    fn app_host_follows_cloud_hosts_and_refuses_custom_ones() {
        let cases = [
            (Some("https://us.i.posthog.com"), None, Some(US_APP_HOST)),
            (Some("https://app.posthog.com/"), None, Some(US_APP_HOST)),
            (Some("https://eu.i.posthog.com"), None, Some(EU_APP_HOST)),
            (Some("https://eu.posthog.com"), None, Some(EU_APP_HOST)),
            (Some("https://proxy.example.com"), None, None),
            (
                Some("https://proxy.example.com"),
                Some(" https://posthog.example.com/ "),
                Some("https://posthog.example.com"),
            ),
            (
                Some("https://proxy.example.com"),
                Some("https://eu.i.posthog.com"),
                Some(EU_APP_HOST),
            ),
            (
                Some("https://eu.i.posthog.com"),
                Some("  "),
                Some(EU_APP_HOST),
            ),
            (None, None, None),
        ];
        for (host, app_host, expected) in cases {
            assert_eq!(
                resolve_app_host(host, app_host).as_deref(),
                expected,
                "{host:?} {app_host:?}"
            );
        }
    }

    #[test]
    fn send_reply_reads_the_id_warnings_and_replay() {
        let result = send_reply(reply(
            202,
            json!({
                "message_id": "msg_1",
                "replayed": true,
                "warnings": [
                    {"code": "w1", "detail": "First", "fix": "Do it"},
                    {"message": "Second"},
                    "not an object",
                ],
            }),
        ))
        .unwrap();
        assert_eq!(result.id, "msg_1");
        assert!(result.replayed);
        assert_eq!(
            result.warnings,
            vec![
                EmailWarning {
                    code: "w1".into(),
                    message: "First".into(),
                    fix: Some("Do it".into()),
                },
                EmailWarning {
                    code: "warning".into(),
                    message: "Second".into(),
                    fix: None,
                },
            ]
        );
    }

    #[test]
    fn send_reply_without_a_message_id_is_an_api_error() {
        for payload in [
            json!({}),
            json!({"message_id": ""}),
            json!({"message_id": 7}),
        ] {
            let error = send_reply(reply(202, payload)).unwrap_err();
            assert_eq!(error.code(), "api_error");
            assert_eq!(error.status(), Some(202));
            assert_eq!(retry_delay(&error, 0, 2), None);
        }
        let error = send_reply(Reply {
            status: 200,
            payload: None,
            retry_after: None,
        })
        .unwrap_err();
        assert_eq!(error.code(), "api_error");
    }

    #[test]
    fn error_reply_keeps_every_field() {
        let error = send_reply(Reply {
            status: 422,
            payload: Some(json!({
                "code": "recipient_suppressed",
                "detail": "Nope.",
                "attr": "to",
                "why": "Bounced before.",
                "fix": "Send to another address.",
                "docs_url": "https://posthog.com/docs",
            })),
            retry_after: Some(Duration::from_secs(3)),
        })
        .unwrap_err();
        assert_eq!(error.code(), "recipient_suppressed");
        assert_eq!(error.message(), "Nope.");
        assert_eq!(error.status(), Some(422));
        assert_eq!(error.field(), Some("to"));
        assert_eq!(error.why(), Some("Bounced before."));
        assert_eq!(error.fix(), Some("Send to another address."));
        assert_eq!(error.docs_url(), Some("https://posthog.com/docs"));
        assert_eq!(error.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(error.to_string(), "recipient_suppressed (HTTP 422): Nope.");

        let error = send_reply(reply(500, json!("oops"))).unwrap_err();
        assert_eq!(error.code(), "api_error");
        assert_eq!(error.message(), "PostHog answered HTTP 500.");
    }

    #[test]
    fn status_reply_parses_times_and_passes_unknown_statuses() {
        let status = status_reply(reply(
            200,
            json!({
                "id": "01a18356-c56c-0001-e494-7666bc915188",
                "status": "bounced",
                "created_at": "2026-10-28T16:27:44.364000Z",
                "status_at": "2026-10-28T16:30:02.100000Z",
                "reason": "hard",
            }),
        ))
        .unwrap();
        assert_eq!(status.status, EmailDeliveryStatus::Bounced);
        assert_eq!(status.reason.as_deref(), Some("hard"));
        assert_eq!(
            status.created_at.to_rfc3339(),
            "2026-10-28T16:27:44.364+00:00"
        );

        let status = status_reply(reply(
            200,
            json!({
                "id": "x",
                "status": "archived",
                "created_at": "2026-10-28T16:27:44Z",
                "status_at": "2026-10-28T16:27:44Z",
                "reason": null,
            }),
        ))
        .unwrap();
        assert_eq!(status.status, EmailDeliveryStatus::Other("archived".into()));
        assert_eq!(status.status.as_str(), "archived");
        assert_eq!(status.reason, None);

        let error = status_reply(reply(
            200,
            json!({"id": "x", "status": "sent", "created_at": "yesterday", "status_at": "2026-10-28T16:27:44Z"}),
        ))
        .unwrap_err();
        assert_eq!(error.code(), "api_error");
    }

    #[test]
    fn status_path_checks_the_id() {
        assert_eq!(
            status_path("01A18356-C56C-0001-E494-7666BC915188").unwrap(),
            "/api/projects/@current/messaging/emails/01a18356-c56c-0001-e494-7666bc915188/"
        );
        let error = status_path("msg_1").unwrap_err();
        assert_eq!(error.code(), "invalid_message_id");
    }

    fn answered(status: u16, code: &str, retry_after: Option<u64>) -> EmailError {
        EmailError::from_response(
            status,
            Some(code.into()),
            None,
            retry_after.map(Duration::from_secs),
        )
    }

    #[test]
    fn retries_only_what_waiting_can_fix() {
        let network = EmailError::new("network_error", "failed");
        assert!(retry_delay(&network, 0, 2).is_some());
        assert!(retry_delay(&answered(503, "unavailable", None), 1, 2).is_some());
        assert!(retry_delay(&answered(429, "throttled", None), 0, 2).is_some());
        assert!(retry_delay(&answered(409, "idempotency_key_in_flight", None), 0, 2).is_some());

        assert_eq!(retry_delay(&answered(503, "unavailable", None), 2, 2), None);
        assert_eq!(
            retry_delay(&answered(429, "quota_limited", None), 0, 2),
            None
        );
        assert_eq!(
            retry_delay(&answered(409, "template_outdated", None), 0, 2),
            None
        );
        assert_eq!(retry_delay(&answered(500, "api_error", None), 0, 2), None);
        assert_eq!(
            retry_delay(&answered(400, "invalid_input", None), 0, 2),
            None
        );
        assert_eq!(
            retry_delay(&EmailError::new("unexpected_error", "x"), 0, 2),
            None
        );
    }

    #[test]
    fn retry_waits_for_backoff_or_a_longer_retry_after() {
        let first = retry_delay(&answered(503, "unavailable", None), 0, 2).unwrap();
        assert!(first >= Duration::from_millis(800) && first <= Duration::from_millis(1200));
        let second = retry_delay(&answered(503, "unavailable", None), 1, 2).unwrap();
        assert!(second >= Duration::from_millis(1600) && second <= Duration::from_millis(2400));
        assert_eq!(
            retry_delay(&answered(429, "throttled", Some(7)), 0, 2),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            retry_delay(&answered(429, "throttled", Some(60)), 0, 2),
            None
        );
        let capped = retry_delay(&answered(503, "unavailable", None), 10, 20).unwrap();
        assert_eq!(capped, MAX_RETRY_DELAY);
    }

    #[test]
    fn tracker_refuses_after_close_and_counts_in_flight() {
        let tracker = EmailTracker::default();
        assert_eq!(
            tracker.admit(true, SEND_NOT_DONE).err().unwrap().code(),
            "client_disabled"
        );
        let in_flight = tracker.admit(false, SEND_NOT_DONE).unwrap();
        tracker.close();
        assert_eq!(tracker.lock().in_flight, 1);
        let error = tracker.admit(false, SEND_NOT_DONE).err().unwrap();
        assert_eq!(error.code(), "client_closed");
        assert_eq!(
            error.message(),
            "The PostHog client is shut down, so no email was sent."
        );
        drop(in_flight);
        assert_eq!(tracker.lock().in_flight, 0);
    }
}
