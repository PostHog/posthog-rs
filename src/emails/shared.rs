//! The I/O-free half of transactional email, shared by the async and blocking
//! clients: argument checks, request bodies and headers, reply parsing, the
//! retry policy, and the in-flight count that `shutdown` waits on.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::convert::TryFrom;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Duration as TimeDelta, SecondsFormat, Utc};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT,
};
use reqwest::Method;
use serde_json::{json, Map, Value};
use tracing::warn;
use uuid::Uuid;

use super::error::{EmailError, EmailErrorKind};
use super::types::{
    BatchEmailResult, BatchEmailStatus, BatchRecipient, CodeEmail, EmailAttachment,
    EmailDeliveryStatus, EmailStatus, EmailWarning, SendBatch, SendBatchResult, SendEmail,
    SendEmailResult, SendOptions,
};
use crate::client::ClientOptions;

pub(crate) const SEND_PATH: &str = "/api/messaging/send/email/";
const UPLOADS_PATH: &str = "/api/messaging/attachments/uploads/";
const STATUS_PATH: &str = "/api/messaging/emails/";
const BATCH_PATH: &str = "/api/messaging/send/email_batch/";
const STATUS_BY_KEY_PATH: &str = "/api/messaging/emails/status/";
const BATCH_MAX_RECIPIENTS: usize = 100;
const BATCH_MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// PostHog waits up to 30 seconds for its pipeline to take a batch request,
/// so an attempt waits 15 seconds more than that for the answer.
const BATCH_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(45);
/// A recipient's answer with one of these codes may still have queued the
/// email.
const IN_FLIGHT_CODE: &str = "idempotency_key_in_flight";
const UNCONFIRMED_SLOT_CODES: [&str; 3] = [IN_FLIGHT_CODE, "delivery_unavailable", "api_error"];
const US_APP_HOST: &str = "https://us.posthog.com";
const EU_APP_HOST: &str = "https://eu.posthog.com";
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);
/// The most of a reply body the client reads, after decompression. PostHog's
/// email replies are a few hundred bytes, so a larger body is unreadable.
pub(crate) const MAX_REPLY_BYTES: usize = 1 << 20;
const MAX_IDEMPOTENCY_KEY_LEN: usize = 255;
const MAX_TEMPLATE_KEY_LEN: usize = 400;
const MAX_ATTACHMENTS: usize = 10;
const MAX_UPLOAD_HEADERS: usize = 128;
const MAX_MESSAGE_BYTES: u64 = 40_000_000;
const HEADERS_AND_MIME_BOUNDARIES_BYTES: u64 = 64 * 1024;
/// A subject travels as encoded words of up to 42 bytes, each 75 bytes once
/// folded into the header.
const SUBJECT_BYTES_PER_ENCODED_WORD: u64 = 42;
const FOLDED_ENCODED_WORD_BYTES: u64 = 75;
/// The slowest upload the client waits for: 1 Mbit/s on top of the email
/// timeout, so the largest file gets about four minutes.
const MIN_UPLOAD_BYTES_PER_SECOND: u64 = 128 * 1024;
const UPLOAD_FAILED: &str = "attachment_upload_failed";

pub(crate) const SEND_NOT_DONE: &str = "no email was sent";
pub(crate) const GET_NOT_DONE: &str = "no email status was read";
pub(crate) const SEND_KEY_NEEDED: &str =
    "Sending email needs secret_key, a project secret API key with the transactional_email:write scope.";
pub(crate) const GET_KEY_NEEDED: &str = "Reading an email status needs secret_key, a project secret API key with the transactional_email:read or transactional_email:write scope.";

/// A send whose arguments passed every check.
#[derive(Debug)]
pub(crate) struct PreparedSend {
    pub(crate) body: Vec<u8>,
    pub(crate) attachments: Vec<EmailAttachment>,
    pub(crate) idempotency_key: String,
    pub(crate) max_retries: Option<u32>,
    /// The time the body carries, rounded up to the millisecond.
    pub(crate) scheduled_at: Option<DateTime<Utc>>,
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
    let (to, mut body, options, attachments) = match email {
        SendEmail::Template(email) => {
            check_recipient(&email.to, &email.options)?;
            let body = template_body(&email.template_id, email.template_data.as_ref())?;
            (email.to, body, email.options, Vec::new())
        }
        SendEmail::Code(email) => {
            check_recipient(&email.to, &email.options)?;
            check_attachments(&email)?;
            let body = code_body(&email, "A send")?;
            (email.to, body, email.options, email.attachments)
        }
    };
    if let Some(distinct_id) = options.distinct_id {
        body.insert("distinct_id".into(), Value::String(distinct_id));
    }
    body.insert("to".into(), Value::String(to));
    let scheduled_at = options.scheduled_at.map(ceil_to_millisecond).transpose()?;
    if let Some(at) = scheduled_at {
        body.insert(
            "scheduled_at".into(),
            Value::String(at.to_rfc3339_opts(SecondsFormat::Millis, true)),
        );
    }
    let idempotency_key = match options.idempotency_key {
        Some(key) => key,
        None => Uuid::new_v4().to_string(),
    };
    Ok(PreparedSend {
        body: encode(&body)?,
        attachments,
        idempotency_key,
        max_retries: options.max_retries,
        scheduled_at,
    })
}

fn encode(body: &Map<String, Value>) -> Result<Vec<u8>, EmailError> {
    serde_json::to_vec(body).map_err(|error| {
        EmailError::new(
            "unexpected_error",
            "The email could not be encoded as JSON.",
        )
        .with_source(error)
    })
}

/// PostHog keeps a scheduled time to the millisecond, so a finer time rounds
/// up: rounding down would send the email early.
fn ceil_to_millisecond(at: DateTime<Utc>) -> Result<DateTime<Utc>, EmailError> {
    let nanos = at.timestamp_subsec_nanos();
    let rounded = nanos.next_multiple_of(1_000_000);
    at.checked_add_signed(TimeDelta::nanoseconds(i64::from(rounded - nanos)))
        .ok_or_else(|| {
            EmailError::invalid(
                "invalid_scheduled_at",
                "scheduled_at is out of range.",
                "scheduled_at",
            )
        })
}

/// Only PostHog Cloud schedules emails. A self-hosted PostHog from before
/// scheduling would drop `scheduled_at` and send the email at once, which no
/// check of its answer can undo, so the client refuses before any request.
pub(crate) fn check_schedulable(
    prepared: &PreparedSend,
    connection: &Connection,
) -> Result<(), EmailError> {
    if prepared.scheduled_at.is_none() {
        return Ok(());
    }
    // Check the URL the send goes to: the app host alone can parse to the root
    // and still put a path in front of the endpoint.
    let url = reqwest::Url::parse(&format!("{}{SEND_PATH}", connection.app_host)).ok();
    let is_cloud = url.as_ref().is_some_and(|url| {
        url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.path() == SEND_PATH
            && url.query().is_none()
            && url.fragment().is_none()
            && matches!(url.host_str(), Some("us.posthog.com" | "eu.posthog.com"))
    });
    if is_cloud {
        return Ok(());
    }
    // The app host can carry a password or a token, so the message names only its host.
    let target = url.as_ref().and_then(reqwest::Url::host_str).map_or_else(
        || "a host that isn't PostHog Cloud".to_string(),
        str::to_string,
    );
    Err(EmailError::invalid(
        "scheduling_requires_cloud",
        format!("Scheduled sends work only on PostHog Cloud (https://us.posthog.com or https://eu.posthog.com), and this client sends to {target}, so no email was sent."),
        "scheduled_at",
    )
    .with_details(
        Some("A PostHog that doesn't schedule emails would send this one at once.".into()),
        Some("Send without scheduled_at, or send through PostHog Cloud's app host. A reverse proxy or custom domain can't schedule emails yet.".into()),
        None,
    ))
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

fn check_attachments(email: &CodeEmail) -> Result<(), EmailError> {
    let count = email.attachments.len();
    if count > MAX_ATTACHMENTS {
        return Err(EmailError::invalid(
            "too_many_attachments",
            format!("An email carries at most {MAX_ATTACHMENTS} files, and this one has {count}, so no email was sent."),
            "attachments",
        )
        .with_details(
            None,
            Some(format!("Attach at most {MAX_ATTACHMENTS} files, and link to any others from the email body.")),
            None,
        ));
    }
    let message_bytes = estimated_message_bytes(email);
    if count > 0 && message_bytes > MAX_MESSAGE_BYTES {
        return Err(EmailError::invalid(
            "message_too_large",
            format!("The email would be about {message_bytes} bytes after Base64 encoding, over the limit of 40 MB, so no email was sent."),
            "attachments",
        )
        .with_details(
            None,
            Some("Attach fewer or smaller files, about 29 MB in total at most, or link to large files from the email body.".into()),
            None,
        ));
    }
    Ok(())
}

/// The size of the email as the email provider counts it, the way PostHog
/// estimates it: each file and body in Base64 with line breaks, the subject
/// as folded encoded words, plus room for the other headers and the MIME
/// boundaries. Without text, PostHog sends text it derives from the HTML and
/// counts that too. The derived text is never longer than the HTML, so the
/// HTML counts twice.
fn estimated_message_bytes(email: &CodeEmail) -> u64 {
    let files = email.attachments.iter().map(|file| file.content.len());
    let html = email.html.as_deref().unwrap_or_default();
    let text = email.text.as_deref().filter(|text| !text.is_empty());
    let bodies = [html, text.unwrap_or(html)].map(str::len);
    let subject = (email.subject.len() as u64).div_ceil(SUBJECT_BYTES_PER_ENCODED_WORD)
        * FOLDED_ENCODED_WORD_BYTES;
    files
        .chain(bodies)
        .map(|size| base64_transfer_bytes(size as u64))
        .sum::<u64>()
        + subject
        + HEADERS_AND_MIME_BOUNDARIES_BYTES
}

fn base64_transfer_bytes(size: u64) -> u64 {
    (size.div_ceil(3) * 4 * 78).div_ceil(76)
}

fn template_body(
    template_id: &str,
    template_data: Option<&Value>,
) -> Result<Map<String, Value>, EmailError> {
    let template_id = Uuid::parse_str(template_id.trim()).map_err(|_| {
        EmailError::invalid(
            "invalid_template_id",
            "template_id must be the template's UUID.",
            "template_id",
        )
    })?;
    let message_data = match template_data {
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

/// The content of a code email. `form`, such as `"A batch"`, starts the
/// message of a missing body.
fn code_body(email: &CodeEmail, form: &str) -> Result<Map<String, Value>, EmailError> {
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
            format!("{form} with template_key needs html, text or both."),
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
    for (field, addresses) in [
        ("reply_to", &email.reply_to),
        ("cc", &email.cc),
        ("bcc", &email.bcc),
    ] {
        if !addresses.is_empty() {
            body.insert(field.into(), json!(addresses));
        }
    }
    if !email.attachments.is_empty() {
        let references = email
            .attachments
            .iter()
            .map(EmailAttachment::to_reference_json);
        body.insert("attachments".into(), Value::Array(references.collect()));
    }
    if let Some(sensitive) = email.sensitive {
        body.insert("sensitive".into(), Value::Bool(sensitive));
    }
    if let Some(tracking) = &email.tracking {
        body.insert("tracking".into(), tracking.to_json());
    }
    if let Some(capture) = email.capture_engagement {
        body.insert("capture_engagement".into(), Value::Bool(capture));
    }
    Ok(body)
}

/// The request that asks PostHog which files to upload, and where.
pub(crate) struct UploadsRequest {
    pub(crate) url: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Vec<u8>,
}

/// PostHog's refusal of the upload request. A `404` means this PostHog has no
/// upload route, sending email from code is off for the project, or
/// `app_host` isn't PostHog's, and the client can't tell which. It keeps
/// PostHog's own answer and names `attachments` as the field.
pub(crate) fn upload_route_error(error: EmailError) -> EmailError {
    if error.status() != Some(404) || error.field().is_some() {
        return error;
    }
    error.with_field(Some("attachments".into()))
}

pub(crate) fn uploads_request(
    connection: &Connection,
    attachments: &[EmailAttachment],
) -> Result<UploadsRequest, EmailError> {
    let mut headers = connection.headers(None)?;
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let files: Vec<Value> = attachments
        .iter()
        .map(EmailAttachment::to_upload_json)
        .collect();
    Ok(UploadsRequest {
        url: format!("{}{UPLOADS_PATH}", connection.app_host),
        headers,
        body: json!({ "files": files }).to_string().into_bytes(),
    })
}

/// One file to PUT to the file store, as PostHog signed it.
#[derive(Debug)]
pub(crate) struct Upload {
    index: usize,
    filename: String,
    pub(crate) method: Method,
    pub(crate) url: String,
    pub(crate) headers: HeaderMap,
    pub(crate) content: Bytes,
    pub(crate) timeout: Duration,
}

impl Upload {
    /// The file store's answer, or the error that stops the send. A refusal
    /// keeps the store's status, so a `500`, `502`, `503` or `504` is retried.
    pub(crate) fn check_reply(
        &self,
        status: u16,
        retry_after: Option<Duration>,
        body: Option<Vec<u8>>,
    ) -> Result<(), EmailError> {
        if is_success(status) {
            return Ok(());
        }
        let body = body.map(|body| String::from_utf8_lossy(&body).into_owned());
        let answer = match body
            .as_deref()
            .map(|body| (xml_text(body, "Code"), xml_text(body, "Message")))
        {
            Some((Some(code), Some(message))) => format!("{code}: {message}"),
            Some((Some(code), None)) => code.to_string(),
            _ => format!("HTTP {status}"),
        };
        Err(EmailError::from_response(
            status,
            Some(UPLOAD_FAILED.into()),
            Some(format!(
                "The file store refused the upload of {}, so no email was sent.",
                self.filename
            )),
            retry_after,
        )
        .with_field(Some(self.field()))
        .with_details(
            Some(format!("The file store answered {answer}")),
            Some("Send the email again. Each send asks PostHog for new upload URLs.".into()),
            None,
        ))
    }

    /// A PUT that got no answer from the file store, or couldn't be built. It
    /// drops the URL from the source, because a presigned URL's query signs the
    /// upload.
    pub(crate) fn request_error(&self, error: reqwest::Error) -> EmailError {
        let (code, failed) = if error.is_builder() {
            ("unexpected_error", "could not be built")
        } else {
            ("network_error", "failed")
        };
        EmailError::new(
            code,
            format!(
                "The upload of {} to the file store {failed}, so no email was sent.",
                self.filename
            ),
        )
        .with_field(Some(self.field()))
        .with_source(error.without_url())
    }

    /// PostHog named an upload URL that isn't https while the app host is.
    /// The error leaves the URL out, because its query signs the upload.
    fn not_https_error(&self, status: u16) -> EmailError {
        EmailError::from_response(
            status,
            None,
            Some(format!(
                "PostHog named an upload URL that isn't https, so {} would travel unencrypted, and no email was sent.",
                self.filename
            )),
            None,
        )
        .with_field(Some(self.field()))
        .with_details(
            Some("Your app_host uses https, and PostHog's file store URL doesn't.".into()),
            Some("Fix the file store's URL so it uses https. Only an app_host that is plain http, such as a PostHog on your own machine, lets files upload over plain http.".into()),
            None,
        )
    }

    fn field(&self) -> String {
        format!("attachments__{}", self.index)
    }
}

fn has_scheme(url: &str, scheme: &str) -> bool {
    url.get(..scheme.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
}

fn xml_text<'a>(body: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = body.find(&open)? + open.len();
    let end = start + body[start..].find(&format!("</{tag}>"))?;
    Some(body[start..end].trim()).filter(|text| !text.is_empty())
}

/// The files PostHog answered `upload` for, each once, in the send's order.
/// Each upload URL must be https, so the file and its signed URL travel
/// encrypted, unless the app host is plain http itself, as a PostHog on the
/// developer's own machine is.
pub(crate) fn uploads_to_make(
    reply: Accepted,
    attachments: &[EmailAttachment],
    connection: &Connection,
) -> Result<Vec<Upload>, EmailError> {
    let plain_http_allowed = has_scheme(&connection.app_host, "http://");
    let Accepted { status, fields } = reply;
    let malformed = || {
        EmailError::from_response(
            status,
            None,
            Some("PostHog answered the upload request without an answer for each file, so no email was sent.".into()),
            None,
        )
    };
    let slots = match fields.get("files") {
        Some(Value::Array(slots)) if slots.len() == attachments.len() => slots,
        _ => return Err(malformed()),
    };
    let mut uploading = HashSet::new();
    let mut uploads = Vec::new();
    for (index, (slot, attachment)) in slots.iter().zip(attachments).enumerate() {
        let slot = slot.as_object().ok_or_else(malformed)?;
        if text(slot, "sha256").as_deref() != Some(attachment.sha256.as_str()) {
            return Err(malformed());
        }
        match text(slot, "status").as_deref() {
            Some("present") => {}
            Some("upload") if !uploading.insert(&attachment.sha256) => {}
            Some("upload") => {
                let upload =
                    upload(index, attachment, slot, connection.timeout).ok_or_else(malformed)?;
                if !plain_http_allowed && !has_scheme(&upload.url, "https://") {
                    return Err(upload.not_https_error(status));
                }
                uploads.push(upload);
            }
            _ => return Err(malformed()),
        }
    }
    Ok(uploads)
}

fn upload(
    index: usize,
    attachment: &EmailAttachment,
    slot: &Map<String, Value>,
    timeout: Duration,
) -> Option<Upload> {
    let method = Method::from_bytes(text(slot, "method")?.as_bytes()).ok()?;
    let signed = slot.get("headers")?.as_object()?;
    // The HTTP client adds headers of its own to the upload and panics when the
    // header map has no room for them, so a grant with more headers than any
    // upload needs reads as malformed.
    if signed.len() > MAX_UPLOAD_HEADERS {
        return None;
    }
    let mut headers = HeaderMap::new();
    for (name, value) in signed {
        // `insert` panics on a full map, so `try_insert` stays as a backstop
        // if the cap above ever changes.
        headers
            .try_insert(
                HeaderName::from_bytes(name.as_bytes()).ok()?,
                HeaderValue::from_str(value.as_str()?).ok()?,
            )
            .ok()?;
    }
    let size = attachment.content.len() as u64;
    Some(Upload {
        index,
        filename: attachment.filename.clone(),
        method,
        url: text(slot, "url")?,
        headers,
        content: attachment.content.clone(),
        timeout: timeout + Duration::from_secs(size / MIN_UPLOAD_BYTES_PER_SECOND),
    })
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
    if let Ok(user_agent) = HeaderValue::from_str(&options.sdk_identity.user_agent()) {
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

pub(crate) fn upload_client_error(error: reqwest::Error) -> EmailError {
    EmailError::new(
        "unexpected_error",
        "The client could not build the HTTP client that uploads files, so no email was sent.",
    )
    .with_source(error)
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

/// How far ahead of the client's clock a time may be and still read as
/// passed when PostHog sends the email now, for clocks that drift apart.
const CLOCK_SKEW_SECONDS: i64 = 60;

/// The send's result. For a send with `scheduled_at`, PostHog must confirm
/// the time it waits for, or answer `queued` with `null` when the time had
/// passed: any other acceptance may send the email at another time.
pub(crate) fn parse_send_reply(
    reply: Accepted,
    scheduled_at: Option<DateTime<Utc>>,
    first_attempt_at: DateTime<Utc>,
) -> Result<SendEmailResult, EmailError> {
    let Accepted { status, fields } = reply;
    let id = text(&fields, "message_id").ok_or_else(|| {
        EmailError::from_response(
            status,
            None,
            Some("PostHog answered without a message ID.".into()),
            None,
        )
    })?;
    let warnings = parse_warnings(&fields);
    let confirmed = match scheduled_at {
        None => None,
        Some(requested) => match (fields.get("status"), fields.get("scheduled_at")) {
            (Some(Value::String(answered)), Some(Value::Null))
                if answered == "queued"
                    && requested <= first_attempt_at + TimeDelta::seconds(CLOCK_SKEW_SECONDS) =>
            {
                None
            }
            (Some(Value::String(answered)), Some(Value::String(time)))
                if answered == "scheduled" && instant(time) == Some(requested) =>
            {
                Some(requested)
            }
            _ => return Err(schedule_not_confirmed(id, status)),
        },
    };
    Ok(SendEmailResult {
        id,
        replayed: fields.get("replayed") == Some(&Value::Bool(true)),
        warnings,
        scheduled_at: confirmed,
    })
}

fn parse_warnings(fields: &Map<String, Value>) -> Vec<EmailWarning> {
    match fields.get("warnings") {
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
    }
}

fn instant(time: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(time)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

fn schedule_not_confirmed(id: String, status: u16) -> EmailError {
    EmailError::from_response(
        status,
        Some("schedule_not_confirmed".into()),
        Some(format!("PostHog accepted email {id} without confirming its scheduled_at, so it may go out at another time.")),
        None,
    )
    .with_field(Some("scheduled_at".into()))
    .with_details(
        None,
        Some(format!("Read the email's status with emails().get(\"{id}\") before you send it again: a send with another idempotency key makes a second email.")),
        None,
    )
    .with_message_id(id)
}

fn timestamp(fields: &Map<String, Value>, name: &str) -> Option<DateTime<Utc>> {
    instant(&text(fields, name)?)
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

pub(crate) fn log_warnings(warnings: &[EmailWarning]) {
    for warning in warnings {
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
        Some(500 | 502 | 504) => is_upload_refusal(error),
        Some(status) => status == 503,
    }
}

/// A PUT of the same bytes is safe to repeat, so the file store's transient
/// server errors are retried too.
fn is_upload_refusal(error: &EmailError) -> bool {
    error.code() == UPLOAD_FAILED
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

/// One recipient of a batch, checked and encoded as the request carries it.
#[derive(Debug)]
struct BatchMember {
    wire: Vec<u8>,
}

/// One request of a batch: its body and the positions of its recipients.
#[derive(Debug)]
struct BatchChunk {
    body: Bytes,
    indices: Vec<usize>,
}

/// Where a batch's requests go, and how long each attempt waits.
pub(crate) struct BatchRequest {
    pub(crate) url: String,
    pub(crate) headers: HeaderMap,
    pub(crate) timeout: Duration,
}

/// Batch requests carry no idempotency key of their own: each recipient
/// carries theirs in the body. An attempt waits at least as long as PostHog's
/// pipeline takes to answer, whatever the client's email timeout.
pub(crate) fn batch_request(connection: &Connection) -> Result<BatchRequest, EmailError> {
    let mut headers = connection.headers(None)?;
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(BatchRequest {
        url: format!("{}{BATCH_PATH}", connection.app_host),
        headers,
        timeout: connection.timeout.max(BATCH_ATTEMPT_TIMEOUT),
    })
}

/// The URL that reads an email by its idempotency key, or `None` when the app
/// host doesn't parse as a URL.
pub(crate) fn status_by_key_url(app_host: &str, idempotency_key: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(&format!("{app_host}{STATUS_BY_KEY_PATH}")).ok()?;
    url.query_pairs_mut()
        .append_pair("idempotency_key", idempotency_key);
    Some(url.to_string())
}

/// The client shut down while a request of a batch waited to retry.
pub(crate) fn retry_refused() -> EmailError {
    EmailError::new(
        "client_closed",
        "The PostHog client is shut down, so it didn't retry this request of the batch.",
    )
}

pub(crate) fn prepare_batch(batch: SendBatch) -> Result<BatchRun, EmailError> {
    let (shared, recipients, max_retries, takes_template_data) = match batch {
        SendBatch::Template(batch) => (
            template_body(&batch.template_id, batch.template_data.as_ref())?,
            batch.recipients,
            batch.max_retries,
            true,
        ),
        SendBatch::Code(batch) => (
            code_body(&batch.email, "A batch")?,
            batch.recipients,
            batch.email.options.max_retries,
            false,
        ),
    };
    let (members, results) = batch_members(recipients, takes_template_data)?;
    BatchRun::new(&shared, members, results, max_retries)
}

fn batch_members(
    recipients: Vec<BatchRecipient>,
    takes_template_data: bool,
) -> Result<(Vec<BatchMember>, Vec<BatchEmailResult>), EmailError> {
    if recipients.is_empty() {
        return Err(EmailError::invalid(
            "invalid_recipients",
            "recipients must hold at least one recipient.",
            "recipients",
        ));
    }
    let mut first_with_key = HashMap::new();
    let mut members = Vec::with_capacity(recipients.len());
    let mut results = Vec::with_capacity(recipients.len());
    for (index, recipient) in recipients.into_iter().enumerate() {
        let BatchRecipient {
            to,
            distinct_id,
            idempotency_key,
            template_data,
        } = recipient;
        let options = SendOptions {
            distinct_id,
            idempotency_key,
            ..SendOptions::default()
        };
        check_recipient(&to, &options).map_err(|error| at_recipient(index, &error))?;
        let key = options
            .idempotency_key
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let mut wire = Map::new();
        wire.insert("to".into(), Value::String(to.clone()));
        wire.insert("idempotency_key".into(), Value::String(key.clone()));
        if let Some(distinct_id) = options.distinct_id {
            wire.insert("distinct_id".into(), Value::String(distinct_id));
        }
        if let Some(data) = template_data {
            if !takes_template_data {
                return Err(EmailError::invalid(
                    "invalid_form",
                    format!("recipients[{index}] has template_data, but a batch with template_key sends the same content to every recipient. Send personalized content with a template batch or with send()."),
                    &format!("recipients__{index}__template_data"),
                ));
            }
            if !data.is_object() {
                return Err(EmailError::invalid(
                    "invalid_template_data",
                    format!("recipients[{index}]: template_data must be a JSON object."),
                    &format!("recipients__{index}__template_data"),
                ));
            }
            wire.insert("message_data".into(), data);
        }
        if let Some(first) = first_with_key.insert(key.clone(), index) {
            return Err(EmailError::invalid(
                "invalid_idempotency_key",
                format!("recipients[{index}] has the same idempotency_key as recipients[{first}]. Give each recipient its own key."),
                &format!("recipients__{index}__idempotency_key"),
            ));
        }
        members.push(BatchMember {
            wire: encode(&wire)?,
        });
        results.push(BatchEmailResult {
            index,
            to,
            idempotency_key: key,
            status: BatchEmailStatus::NotAttempted,
            message_id: None,
            replayed: false,
            error: None,
            later_error: None,
        });
    }
    Ok((members, results))
}

/// A recipient's argument error, with the recipient's position in the message
/// and the field.
fn at_recipient(index: usize, error: &EmailError) -> EmailError {
    EmailError::invalid(
        error.code(),
        format!("recipients[{index}]: {}", error.message()),
        &format!("recipients__{index}__{}", error.field().unwrap_or("to")),
    )
}

/// The recipients of one batch call, split into requests, and what PostHog
/// answered for each.
///
/// A recipient whose email PostHog may have queued, because an attempt got no
/// usable answer or PostHog said it couldn't tell, stays
/// [`Unconfirmed`](BatchEmailStatus::Unconfirmed) until PostHog answers
/// `queued` for them: a later refusal doesn't prove the email wasn't sent.
#[derive(Debug)]
pub(crate) struct BatchRun {
    results: Vec<BatchEmailResult>,
    maybe_queued: BTreeSet<usize>,
    warnings: Vec<EmailWarning>,
    chunks: Vec<BatchChunk>,
    pub(crate) max_retries: Option<u32>,
}

impl BatchRun {
    fn new(
        shared: &Map<String, Value>,
        members: Vec<BatchMember>,
        results: Vec<BatchEmailResult>,
        max_retries: Option<u32>,
    ) -> Result<Self, EmailError> {
        let mut head = encode(shared)?;
        head.pop();
        head.extend_from_slice(b",\"recipients\":[");
        let mut run = Self {
            results,
            maybe_queued: BTreeSet::new(),
            warnings: Vec::new(),
            chunks: Vec::new(),
            max_retries,
        };
        run.split(&head, &members);
        Ok(run)
    }

    /// Splits the recipients into requests of at most 100 recipients and
    /// 4 MiB, in order. A recipient too large for any request fails here and
    /// joins no request.
    fn split(&mut self, head: &[u8], members: &[BatchMember]) {
        let empty = head.len() + b"]}".len();
        let mut current: Vec<usize> = Vec::new();
        let mut size = empty;
        for (index, member) in members.iter().enumerate() {
            if empty + member.wire.len() > BATCH_MAX_BODY_BYTES {
                self.set(
                    index,
                    BatchEmailStatus::Failed,
                    None,
                    Some(recipient_too_large()),
                    None,
                );
                continue;
            }
            let mut added = member.wire.len() + usize::from(!current.is_empty());
            if !current.is_empty()
                && (current.len() == BATCH_MAX_RECIPIENTS || size + added > BATCH_MAX_BODY_BYTES)
            {
                self.chunks
                    .push(chunk(head, members, std::mem::take(&mut current)));
                size = empty;
                added = member.wire.len();
            }
            current.push(index);
            size += added;
        }
        if !current.is_empty() {
            self.chunks.push(chunk(head, members, current));
        }
    }

    pub(crate) fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub(crate) fn chunk_body(&self, position: usize) -> Bytes {
        self.chunks[position].body.clone()
    }

    fn set(
        &mut self,
        index: usize,
        status: BatchEmailStatus,
        message_id: Option<String>,
        error: Option<EmailError>,
        later_error: Option<EmailError>,
    ) {
        let result = &mut self.results[index];
        result.status = status;
        result.message_id = message_id;
        result.replayed = false;
        result.error = error;
        result.later_error = later_error;
    }

    /// An attempt that may have queued some of the chunk got no usable
    /// answer, or PostHog answered that another request still holds their
    /// keys: `error` when it said so.
    fn lost(&mut self, position: usize, answered: Option<&EmailError>) {
        for index in self.chunks[position].indices.clone() {
            self.maybe_queued.insert(index);
            let message_id = self.results[index].message_id.clone();
            let error = match answered {
                Some(error) => with_message_id(error.clone(), message_id.clone()),
                None => unconfirmed(message_id.clone()),
            };
            self.set(
                index,
                BatchEmailStatus::Unconfirmed,
                message_id,
                Some(error),
                None,
            );
        }
    }

    /// Records the answer to one attempt of chunk `position`, or returns the
    /// error that fails the request as a whole.
    pub(crate) fn answer(
        &mut self,
        position: usize,
        reply: Result<Reply, EmailError>,
    ) -> Result<(), EmailError> {
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                if error.is_network_error() {
                    self.lost(position, None);
                }
                return Err(error);
            }
        };
        let fields = reply_fields(reply.payload);
        let indices = self.chunks[position].indices.clone();
        if !is_success(reply.status) {
            let error = batch_reply_error(fields, reply.status, reply.retry_after, &indices);
            if error.code() == IN_FLIGHT_CODE {
                self.lost(position, Some(&error));
            } else if reply.status >= 500 {
                self.lost(position, None);
            }
            return Err(error);
        }
        let slots = match fields.get("results") {
            Some(Value::Array(slots)) if slots.len() == indices.len() => slots,
            _ => {
                self.lost(position, None);
                return Err(EmailError::from_response(
                    reply.status,
                    None,
                    Some("PostHog answered the batch without a result for each recipient.".into()),
                    None,
                ));
            }
        };
        for (index, slot) in indices.into_iter().zip(slots) {
            self.settle(index, slot.as_object());
        }
        for warning in parse_warnings(&fields) {
            if !self.warnings.contains(&warning) {
                self.warnings.push(warning);
            }
        }
        Ok(())
    }

    fn settle(&mut self, index: usize, slot: Option<&Map<String, Value>>) {
        let empty = Map::new();
        let slot = slot.unwrap_or(&empty);
        let message_id = text(slot, "message_id");
        let error = slot.get("error").filter(|error| !error.is_null());
        if error.is_none() && slot.get("status") == Some(&json!("queued")) && message_id.is_some() {
            self.maybe_queued.remove(&index);
            self.set(index, BatchEmailStatus::Queued, message_id, None, None);
            self.results[index].replayed = slot.get("replayed") == Some(&Value::Bool(true));
            return;
        }
        let answer = slot_error(error, message_id.clone());
        if self.maybe_queued.contains(&index) {
            let earlier = &mut self.results[index];
            let message_id = message_id.or_else(|| earlier.message_id.take());
            let error = earlier.error.take();
            self.set(
                index,
                BatchEmailStatus::Unconfirmed,
                message_id,
                error,
                Some(answer),
            );
        } else if UNCONFIRMED_SLOT_CODES.contains(&answer.code()) {
            self.maybe_queued.insert(index);
            self.set(
                index,
                BatchEmailStatus::Unconfirmed,
                message_id,
                Some(answer),
                None,
            );
        } else {
            self.set(
                index,
                BatchEmailStatus::Failed,
                message_id,
                Some(answer),
                None,
            );
        }
    }

    /// The request of chunk `position` failed as a whole, so the batch stops
    /// there.
    pub(crate) fn stop(&mut self, position: usize, error: &EmailError) {
        let later = error.status().filter(|status| *status < 500).map(|_| error);
        for index in self.chunks[position].indices.clone() {
            if !self.maybe_queued.contains(&index) {
                self.set(
                    index,
                    BatchEmailStatus::Failed,
                    None,
                    Some(error.clone()),
                    None,
                );
            } else if let Some(later) = later {
                let earlier = &mut self.results[index];
                let (message_id, earlier_error) = (earlier.message_id.take(), earlier.error.take());
                self.set(
                    index,
                    BatchEmailStatus::Unconfirmed,
                    message_id,
                    earlier_error,
                    Some(later.clone()),
                );
            }
        }
        self.skip_from(
            position + 1,
            &EmailError::new(
                "not_attempted",
                "An earlier request of the batch failed, so the client sent nothing for this recipient.",
            ),
        );
    }

    /// The client shut down before chunk `position` went out.
    pub(crate) fn closed_at(&mut self, position: usize) -> EmailError {
        let error = EmailError::new(
            "client_closed",
            "The PostHog client is shut down, so the rest of the batch wasn't sent.",
        );
        self.skip_from(position, &error);
        error
    }

    fn skip_from(&mut self, position: usize, error: &EmailError) {
        let indices: Vec<usize> = self.chunks[position..]
            .iter()
            .flat_map(|chunk| chunk.indices.iter().copied())
            .collect();
        for index in indices {
            self.set(
                index,
                BatchEmailStatus::NotAttempted,
                None,
                Some(error.clone()),
                None,
            );
        }
    }

    /// Unconfirmed recipients without a message ID, with their keys.
    pub(crate) fn lost_keys(&self) -> Vec<(usize, String)> {
        self.results
            .iter()
            .filter(|result| {
                result.status == BatchEmailStatus::Unconfirmed && result.message_id.is_none()
            })
            .map(|result| (result.index, result.idempotency_key.clone()))
            .collect()
    }

    /// PostHog has an email with the key of recipient `index`, as read by the
    /// key. The recipient stays unconfirmed: the read names the email, but
    /// can't tell whether this call queued it.
    pub(crate) fn found(&mut self, index: usize, status: EmailStatus) {
        self.results[index].message_id = Some(status.id);
    }

    /// The result of a batch whose every request PostHog answered.
    pub(crate) fn finish(self) -> SendBatchResult {
        let result = SendBatchResult {
            results: self.results,
            warnings: self.warnings,
        };
        log_warnings(&result.warnings);
        result
    }

    /// The error that stopped the batch, carrying every recipient's result.
    pub(crate) fn failure(self, error: EmailError) -> EmailError {
        let result = self.finish();
        error.with_batch(result)
    }
}

fn chunk(head: &[u8], members: &[BatchMember], indices: Vec<usize>) -> BatchChunk {
    let mut body = head.to_vec();
    for (position, index) in indices.iter().enumerate() {
        if position > 0 {
            body.push(b',');
        }
        body.extend_from_slice(&members[*index].wire);
    }
    body.extend_from_slice(b"]}");
    BatchChunk {
        body: Bytes::from(body),
        indices,
    }
}

fn recipient_too_large() -> EmailError {
    EmailError::invalid(
        "content_too_large",
        "This recipient's email is larger than one batch request takes, so it wasn't sent.",
        "recipients",
    )
    .with_details(
        None,
        Some("Send this recipient's email with send().".into()),
        None,
    )
}

fn with_message_id(error: EmailError, message_id: Option<String>) -> EmailError {
    match message_id {
        Some(id) => error.with_message_id(id),
        None => error,
    }
}

fn unconfirmed(message_id: Option<String>) -> EmailError {
    let error = EmailError::new(
        "delivery_unavailable",
        "PostHog didn't confirm this email, so it may still go out. Send it again with the same idempotency_key within 24 hours, so it goes out once.",
    )
    .with_kind(EmailErrorKind::ServiceUnavailable);
    with_message_id(error, message_id)
}

/// PostHog's answer about one recipient that it didn't queue.
fn slot_error(error: Option<&Value>, message_id: Option<String>) -> EmailError {
    let Some(Value::Object(error)) = error else {
        return with_message_id(
            EmailError::new(
                "api_error",
                "PostHog answered without a readable result for this email, so it may still go out.",
            ),
            message_id,
        );
    };
    let code = text(error, "code").unwrap_or_else(|| "api_error".into());
    let status = error
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok());
    let message = text(error, "detail")
        .unwrap_or_else(|| format!("PostHog didn't queue this email: {code}."));
    with_message_id(EmailError::answered(status, code, message), message_id)
}

/// PostHog's refusal of a whole batch request. A field of a recipient names
/// the recipient's position in the batch, not in the request.
fn batch_reply_error(
    mut fields: Map<String, Value>,
    status: u16,
    retry_after: Option<Duration>,
    indices: &[usize],
) -> EmailError {
    let mut set_unless_answered = |name: &str, value: &str| {
        if fields.get(name).map_or(true, Value::is_null) {
            fields.insert(name.into(), Value::String(value.into()));
        }
    };
    if status == 404 {
        set_unless_answered("code", "not_found");
        set_unless_answered(
            "detail",
            "PostHog found no batch send for this request, so no email was sent.",
        );
        set_unless_answered(
            "why",
            "The template doesn't exist, or batch sends are off for this project or this PostHog version.",
        );
        set_unless_answered(
            "fix",
            "Check template_id, or send each email the batch didn't queue with send() and its recipient's idempotency_key.",
        );
    }
    if text(&fields, "code").as_deref() == Some("batch_unavailable") {
        fields.insert(
            "fix".into(),
            Value::String("Until batch sends are on for the project, send each email the batch didn't queue with send() and its recipient's idempotency_key.".into()),
        );
    }
    let error = reply_error(&fields, status, retry_after);
    let field = error.field().map(|attr| recipients_field(attr, indices));
    error.with_field(field)
}

/// `recipients__<position in the request>__<field>` as the position in the
/// batch. A position the request doesn't have names `recipients`.
fn recipients_field(attr: &str, indices: &[usize]) -> String {
    let Some(rest) = attr.strip_prefix("recipients__") else {
        return attr.to_string();
    };
    let (position, tail) = rest.split_at(rest.find("__").unwrap_or(rest.len()));
    if position.is_empty() || !position.bytes().all(|byte| byte.is_ascii_digit()) {
        return attr.to_string();
    }
    match position
        .parse::<usize>()
        .ok()
        .and_then(|position| indices.get(position))
    {
        Some(index) => format!("recipients__{index}{tail}"),
        None => "recipients".to_string(),
    }
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

    /// Whether the client shut down, so a batch sends no more requests.
    pub(crate) fn is_closed(&self) -> bool {
        self.lock().closed
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
    use crate::emails::types::TemplateEmail;

    fn body(prepared: &PreparedSend) -> Value {
        serde_json::from_slice(&prepared.body).unwrap()
    }

    fn send_reply(reply: Reply) -> Result<SendEmailResult, EmailError> {
        accept(reply).and_then(|reply| parse_send_reply(reply, None, Utc::now()))
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
            "/api/messaging/emails/01a18356-c56c-0001-e494-7666bc915188/"
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
