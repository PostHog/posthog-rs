//! The I/O-free half of transactional email, shared by the async and blocking
//! clients: argument checks, request bodies and headers, reply parsing, the
//! retry policy, and the in-flight count that `shutdown` waits on.

use std::collections::{HashMap, HashSet};
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
    BatchEmailResult, BatchEmailStatus, BatchRecipient, CodeContent, CodeEmail, EmailAttachment,
    EmailDeliveryStatus, EmailStatus, EmailWarning, SendBatch, SendBatchResult, SendEmail,
    SendEmailResult, SendOptions,
};
use crate::client::ClientOptions;

pub(crate) const SEND_PATH: &str = "/api/messaging/send/email/";
const UPLOADS_PATH: &str = "/api/messaging/attachments/uploads/";
const STATUS_PATH: &str = "/api/messaging/emails/";
const BATCH_PATH: &str = "/api/messaging/send/email_batch/";
const BATCH_MAX_RECIPIENTS: usize = 100;
const BATCH_MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const BATCH_MAX_DEPTH: usize = 64;
/// Room for what PostHog adds to a code batch's content for each recipient,
/// such as the sender and the tracking settings.
const CODE_CONTENT_ALLOWANCE: usize = 1024;
const CODE_CONTENT_NODES: usize = 32;
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
            let mut body = code_body(&email.content)?;
            for (field, addresses) in [("cc", &email.cc), ("bcc", &email.bcc)] {
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
            if let Some(capture) = email.capture_engagement {
                body.insert("capture_engagement".into(), Value::Bool(capture));
            }
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
    check_address_and_ids(
        to,
        options.distinct_id.as_deref(),
        options.idempotency_key.as_deref(),
    )
}

fn check_address_and_ids(
    to: &str,
    distinct_id: Option<&str>,
    idempotency_key: Option<&str>,
) -> Result<(), EmailError> {
    if to.trim().is_empty() {
        return Err(EmailError::invalid(
            "invalid_recipient",
            "to must be one email address as a nonempty string.",
            "to",
        ));
    }
    if distinct_id == Some("") {
        return Err(EmailError::invalid(
            "invalid_distinct_id",
            "distinct_id must be a nonempty string.",
            "distinct_id",
        ));
    }
    if let Some(key) = idempotency_key {
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
    let html = email.content.html.as_deref().unwrap_or_default();
    let text = email
        .content
        .text
        .as_deref()
        .filter(|text| !text.is_empty());
    let bodies = [html, text.unwrap_or(html)].map(str::len);
    let subject = (email.content.subject.len() as u64).div_ceil(SUBJECT_BYTES_PER_ENCODED_WORD)
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

fn code_body(content: &CodeContent) -> Result<Map<String, Value>, EmailError> {
    let key = &content.template_key;
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
    if content.html.is_none() && content.text.is_none() {
        return Err(EmailError::invalid(
            "invalid_form",
            "An email with template_key needs html, text or both.",
            "html",
        ));
    }
    let mut body = Map::new();
    body.insert("template_key".into(), Value::String(key.clone()));
    body.insert("subject".into(), Value::String(content.subject.clone()));
    if let Some(html) = &content.html {
        body.insert("html".into(), Value::String(html.clone()));
    }
    if let Some(text) = &content.text {
        body.insert("text".into(), Value::String(text.clone()));
    }
    if let Some(from) = &content.from {
        body.insert("from".into(), Value::String(from.clone()));
    }
    if !content.reply_to.is_empty() {
        body.insert("reply_to".into(), json!(content.reply_to));
    }
    if let Some(sensitive) = content.sensitive {
        body.insert("sensitive".into(), Value::Bool(sensitive));
    }
    if let Some(tracking) = &content.tracking {
        body.insert("tracking".into(), tracking.to_json());
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

/// The time the email clients read and the waits between their retries. On
/// a thread that a test put on virtual time, a wait passes at once and moves
/// the time on instead.
pub(crate) mod clock {
    use std::time::{Duration, SystemTime};

    use chrono::{DateTime, Utc};

    /// `Utc::now` panics on a clock set before 1970, so the time is converted
    /// from `SystemTime` instead.
    pub(crate) fn now() -> DateTime<Utc> {
        #[cfg(test)]
        let now = virtual_time::now().unwrap_or_else(SystemTime::now);
        #[cfg(not(test))]
        let now = SystemTime::now();
        DateTime::from(now)
    }

    /// Moves virtual time on by `delay` and returns `true`, or returns
    /// `false` when the caller must wait for real.
    pub(crate) fn skip_wait(delay: Duration) -> bool {
        #[cfg(test)]
        if virtual_time::advance(delay) {
            return true;
        }
        let _ = delay;
        false
    }

    /// The longest a batch waits for real before a retry, which a shutdown
    /// cuts short: `delay`, or nothing on virtual time.
    pub(crate) fn retry_wait(delay: Duration) -> Duration {
        if skip_wait(delay) {
            Duration::ZERO
        } else {
            delay
        }
    }

    #[cfg(test)]
    pub(crate) mod virtual_time {
        use std::cell::Cell;
        use std::time::{Duration, SystemTime};

        use chrono::{DateTime, Utc};

        thread_local! {
            static NOW: Cell<Option<SystemTime>> = const { Cell::new(None) };
        }

        /// Puts this thread's email clients on virtual time from `start`
        /// until the guard drops.
        #[cfg_attr(not(feature = "tls"), allow(dead_code))]
        pub(crate) fn start_at(start: DateTime<Utc>) -> Guard {
            NOW.with(|now| now.set(Some(start.into())));
            Guard
        }

        #[cfg_attr(not(feature = "tls"), allow(dead_code))]
        pub(crate) struct Guard;

        impl Drop for Guard {
            fn drop(&mut self) {
                NOW.with(|now| now.set(None));
            }
        }

        pub(super) fn now() -> Option<SystemTime> {
            NOW.with(Cell::get)
        }

        pub(super) fn advance(delay: Duration) -> bool {
            NOW.with(|now| match now.get() {
                Some(time) => {
                    now.set(Some(time + delay));
                    true
                }
                None => false,
            })
        }
    }
}

/// How far ahead of the client's clock a time may be and still read as
/// passed when PostHog sends the email now, for clocks that drift apart.
const CLOCK_SKEW_SECONDS: i64 = 60;

/// The send's result. For a send with `scheduled_at`, PostHog must confirm
/// the time it waits for, or answer `queued` with `null` when the time had
/// passed: any other acceptance may send the email at another time.
/// A scheduled send's time, and when the client first sent it.
pub(crate) struct Schedule {
    pub(crate) at: DateTime<Utc>,
    pub(crate) first_attempt_at: DateTime<Utc>,
}

impl Schedule {
    /// Reads the clock only for a send that has a time.
    pub(crate) fn of(scheduled_at: Option<DateTime<Utc>>) -> Option<Self> {
        scheduled_at.map(|at| Self {
            at,
            first_attempt_at: clock::now(),
        })
    }
}

pub(crate) fn parse_send_reply(
    reply: Accepted,
    schedule: Option<Schedule>,
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
    let confirmed = match schedule {
        None => None,
        Some(Schedule {
            at: requested,
            first_attempt_at,
        }) => match (fields.get("status"), fields.get("scheduled_at")) {
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

/// What PostHog's pipeline copies for each recipient of a request: the shared
/// data and content, and the recipient's own data. It refuses a request whose
/// copies add up to more than [`Copies::MAX`].
#[derive(Debug, Clone, Copy, Default)]
struct Copies {
    bytes: usize,
    keys: usize,
    nodes: usize,
}

impl Copies {
    const MAX: Copies = Copies {
        bytes: 16 * 1024 * 1024,
        keys: 50_000,
        nodes: 500_000,
    };
    const EMPTY_OBJECT: Copies = Copies {
        bytes: 2,
        keys: 0,
        nodes: 0,
    };

    /// The copy of `data`, or `None` when it nests deeper than PostHog takes.
    fn of(data: &Value) -> Option<Copies> {
        Some(Copies {
            bytes: json_len(data),
            keys: data.as_object().map_or(0, Map::len),
            nodes: nodes(data)?,
        })
    }

    fn plus(self, other: Copies) -> Copies {
        Copies {
            bytes: self.bytes + other.bytes,
            keys: self.keys + other.keys,
            nodes: self.nodes + other.nodes,
        }
    }

    fn fits(self) -> bool {
        self.bytes <= Self::MAX.bytes
            && self.keys <= Self::MAX.keys
            && self.nodes <= Self::MAX.nodes
    }
}

/// Every object key and array element below `root`, or `None` for an object
/// or array more than 64 levels deep, counting `root` as level 1.
fn nodes(root: &Value) -> Option<usize> {
    let mut count = 0;
    let mut pending = vec![(root, 1)];
    while let Some((value, level)) = pending.pop() {
        let children: Vec<&Value> = match value {
            Value::Object(fields) => fields.values().collect(),
            Value::Array(items) => items.iter().collect(),
            _ => continue,
        };
        if level > BATCH_MAX_DEPTH {
            return None;
        }
        count += children.len();
        pending.extend(children.into_iter().map(|child| (child, level + 1)));
    }
    Some(count)
}

fn json_len(value: &(impl serde::Serialize + ?Sized)) -> usize {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    let _ = serde_json::to_writer(&mut counter, value);
    counter.0
}

/// What PostHog copies of a code batch's content for each recipient. It
/// sends the content in its own shape, with a sender, tracking settings, and
/// text derived from the HTML when there is no text, which is never longer
/// than the HTML.
fn code_copies(content: &CodeContent, encoded: usize) -> Copies {
    let derived_text = match (&content.html, &content.text) {
        (Some(html), None) => json_len(html),
        _ => 0,
    };
    Copies {
        bytes: encoded + derived_text + CODE_CONTENT_ALLOWANCE + Copies::EMPTY_OBJECT.bytes,
        keys: 0,
        nodes: CODE_CONTENT_NODES + content.reply_to.len(),
    }
}

pub(crate) struct HttpCall {
    pub(crate) url: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
    pub(crate) timeout: Duration,
}

pub(crate) enum BatchStep {
    /// POST this request, then pass its reply to [`BatchRun::answered`].
    Call(HttpCall),
    /// Wait this long before a retry, or until the client shuts down.
    Wait(Duration),
    Done(Result<SendBatchResult, EmailError>),
}

/// A batch whose every recipient passed the checks and has a key.
pub(crate) struct PreparedBatch {
    head: Vec<u8>,
    members: Vec<Member>,
    max_retries: Option<u32>,
}

/// One recipient, as the request carries them, and what happened to them.
struct Member {
    wire: Vec<u8>,
    copies: Copies,
    result: BatchEmailResult,
}

pub(crate) fn prepare_batch(batch: SendBatch) -> Result<PreparedBatch, EmailError> {
    let (shared, recipients, max_retries, code) = match batch {
        SendBatch::Template(batch) => {
            let body = template_body(&batch.template_id, batch.template_data.as_ref())?;
            for (index, recipient) in batch.recipients.iter().enumerate() {
                if recipient
                    .template_data
                    .as_ref()
                    .is_some_and(|data| !data.is_object())
                {
                    return Err(EmailError::invalid(
                        "invalid_template_data",
                        format!("recipients[{index}]: template_data must be a JSON object."),
                        &format!("recipients__{index}__template_data"),
                    ));
                }
            }
            (body, batch.recipients, batch.max_retries, None)
        }
        SendBatch::Code(batch) => {
            if let Some(index) = batch
                .recipients
                .iter()
                .position(|recipient| recipient.template_data.is_some())
            {
                return Err(EmailError::invalid(
                    "invalid_form",
                    format!("recipients[{index}] has template_data, but a batch with template_key sends the same content to every recipient. Send personalized content with a template batch or with send()."),
                    &format!("recipients__{index}__template_data"),
                ));
            }
            let body = code_body(&batch.content)?;
            (
                body,
                batch.recipients,
                batch.max_retries,
                Some(batch.content),
            )
        }
    };
    let mut head = encode(&shared)?;
    let shared_copies = match &code {
        Some(content) => Some(code_copies(content, head.len())),
        None => Copies::of(&shared["message_data"]),
    };
    head.pop();
    head.extend_from_slice(b",\"recipients\":[");
    let members = batch_members(recipients, shared_copies, head.len() + b"]}".len())?;
    Ok(PreparedBatch {
        head,
        members,
        max_retries,
    })
}

/// Checks every recipient and gives each a key before any request. A
/// recipient too large for any request fails here, alone.
fn batch_members(
    recipients: Vec<BatchRecipient>,
    shared: Option<Copies>,
    empty_request: usize,
) -> Result<Vec<Member>, EmailError> {
    if recipients.is_empty() {
        return Err(EmailError::invalid(
            "invalid_recipients",
            "recipients must hold at least one recipient.",
            "recipients",
        ));
    }
    let mut first_with_key = HashMap::new();
    let mut members = Vec::with_capacity(recipients.len());
    for (index, recipient) in recipients.into_iter().enumerate() {
        let BatchRecipient {
            to,
            distinct_id,
            idempotency_key,
            template_data,
        } = recipient;
        check_address_and_ids(&to, distinct_id.as_deref(), idempotency_key.as_deref())
            .map_err(|error| at_recipient(index, &error))?;
        let key = idempotency_key.unwrap_or_else(|| Uuid::new_v4().to_string());
        if let Some(first) = first_with_key.insert(key.clone(), index) {
            return Err(EmailError::invalid(
                "invalid_idempotency_key",
                format!("recipients[{index}] has the same idempotency_key as recipients[{first}]. Give each recipient its own key."),
                &format!("recipients__{index}__idempotency_key"),
            ));
        }
        let own = template_data
            .as_ref()
            .map_or(Some(Copies::EMPTY_OBJECT), Copies::of);
        let mut wire = Map::new();
        wire.insert("to".into(), Value::String(to.clone()));
        wire.insert("idempotency_key".into(), Value::String(key.clone()));
        if let Some(distinct_id) = distinct_id {
            wire.insert("distinct_id".into(), Value::String(distinct_id));
        }
        if let Some(data) = template_data {
            wire.insert("message_data".into(), data);
        }
        let mut wire = encode(&wire)?;
        let copies = shared.zip(own).map(|(shared, own)| shared.plus(own));
        let fits =
            copies.is_some_and(Copies::fits) && empty_request + wire.len() <= BATCH_MAX_BODY_BYTES;
        if !fits {
            wire = Vec::new();
        }
        members.push(Member {
            wire,
            copies: copies.unwrap_or_default(),
            result: BatchEmailResult {
                index,
                to,
                idempotency_key: key,
                status: if fits {
                    BatchEmailStatus::NotAttempted
                } else {
                    BatchEmailStatus::Failed
                },
                message_id: None,
                replayed: false,
                error: (!fits).then(recipient_too_large),
                later_error: None,
            },
        });
    }
    Ok(members)
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

struct Chunk {
    indices: Vec<usize>,
    body: Bytes,
    retries: u32,
}

enum Phase {
    /// Send the next request from the recipient at this position.
    Send(usize),
    Sent(Chunk),
    Wait(Chunk, Duration),
    Retry(Chunk),
    Done,
}

/// One batch call: the client asks it for each [`BatchStep`] and passes it
/// every reply, so both clients run the same batch.
///
/// The batch sends its recipients in requests one after another, building
/// each body only when it goes out, and stops at a request that fails as a
/// whole or when the client shuts down.
///
/// A recipient whose email PostHog may have queued, because an attempt got no
/// usable answer or PostHog said it couldn't tell, stays
/// [`Unconfirmed`](BatchEmailStatus::Unconfirmed) with that first cause until
/// PostHog answers `queued` for them: a later refusal doesn't prove the email
/// wasn't sent, so it goes to `later_error`.
pub(crate) struct BatchRun<'a> {
    tracker: &'a EmailTracker,
    app_host: String,
    send_headers: HeaderMap,
    send_timeout: Duration,
    head: Vec<u8>,
    members: Vec<Member>,
    warnings: Vec<EmailWarning>,
    max_retries: u32,
    phase: Phase,
    stopped_by: Option<EmailError>,
}

impl<'a> BatchRun<'a> {
    /// An attempt waits at least as long as PostHog's pipeline takes to
    /// answer, whatever the client's email timeout.
    pub(crate) fn new(
        prepared: PreparedBatch,
        connection: &Connection,
        tracker: &'a EmailTracker,
    ) -> Result<Self, EmailError> {
        let mut send_headers = connection.headers(None)?;
        send_headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(Self {
            tracker,
            app_host: connection.app_host.clone(),
            send_headers,
            send_timeout: connection.timeout.max(BATCH_ATTEMPT_TIMEOUT),
            head: prepared.head,
            members: prepared.members,
            warnings: Vec::new(),
            max_retries: prepared.max_retries.unwrap_or(connection.max_retries),
            phase: Phase::Send(0),
            stopped_by: None,
        })
    }

    pub(crate) fn next(&mut self) -> BatchStep {
        loop {
            match std::mem::replace(&mut self.phase, Phase::Done) {
                Phase::Send(from) => match self.chunk_from(from) {
                    None => self.phase = Phase::Done,
                    Some(_) if self.tracker.is_closed() => {
                        let error = EmailError::new(
                            "client_closed",
                            "The PostHog client is shut down, so the rest of the batch wasn't sent.",
                        );
                        self.stop(&[], error.clone(), &error);
                    }
                    Some(chunk) => return self.post(chunk),
                },
                Phase::Wait(chunk, delay) => {
                    self.phase = Phase::Retry(chunk);
                    return BatchStep::Wait(delay);
                }
                Phase::Retry(chunk) if self.tracker.is_closed() => {
                    let error = EmailError::new(
                        "client_closed",
                        "The PostHog client is shut down, so it didn't retry this request of the batch.",
                    );
                    self.stop(&chunk.indices, error, &not_attempted());
                }
                Phase::Retry(mut chunk) => {
                    chunk.retries += 1;
                    return self.post(chunk);
                }
                Phase::Sent(_) => unreachable!("a call's reply comes first"),
                Phase::Done => return BatchStep::Done(self.finish()),
            }
        }
    }

    pub(crate) fn answered(&mut self, reply: Result<Reply, EmailError>) {
        match std::mem::replace(&mut self.phase, Phase::Done) {
            Phase::Sent(chunk) => match self.record(&chunk.indices, reply) {
                Ok(()) => self.phase = Phase::Send(chunk.indices[chunk.indices.len() - 1] + 1),
                Err(error) => match retry_delay(&error, chunk.retries, self.max_retries) {
                    Some(delay) => self.phase = Phase::Wait(chunk, delay),
                    None => self.stop(&chunk.indices, error, &not_attempted()),
                },
            },
            _ => unreachable!("a reply answers a call"),
        }
    }

    /// The next request: as many recipients from `from` on as fit, in order.
    fn chunk_from(&self, from: usize) -> Option<Chunk> {
        let mut indices: Vec<usize> = Vec::new();
        let mut size = self.head.len() + b"]}".len();
        let mut copies = Copies::default();
        for (index, member) in self.members.iter().enumerate().skip(from) {
            if member.result.status == BatchEmailStatus::Failed {
                continue;
            }
            let added = member.wire.len() + usize::from(!indices.is_empty());
            let with = copies.plus(member.copies);
            if !indices.is_empty()
                && (indices.len() == BATCH_MAX_RECIPIENTS
                    || size + added > BATCH_MAX_BODY_BYTES
                    || !with.fits())
            {
                break;
            }
            indices.push(index);
            size += added;
            copies = with;
        }
        if indices.is_empty() {
            return None;
        }
        let mut body = Vec::with_capacity(size);
        body.extend_from_slice(&self.head);
        for (position, index) in indices.iter().enumerate() {
            if position > 0 {
                body.push(b',');
            }
            body.extend_from_slice(&self.members[*index].wire);
        }
        body.extend_from_slice(b"]}");
        Some(Chunk {
            indices,
            body: Bytes::from(body),
            retries: 0,
        })
    }

    fn post(&mut self, chunk: Chunk) -> BatchStep {
        let call = HttpCall {
            url: format!("{}{BATCH_PATH}", self.app_host),
            headers: self.send_headers.clone(),
            body: chunk.body.clone(),
            timeout: self.send_timeout,
        };
        self.phase = Phase::Sent(chunk);
        BatchStep::Call(call)
    }

    /// Records the answer to one attempt of the request, or returns the error
    /// that fails the request as a whole.
    fn record(
        &mut self,
        indices: &[usize],
        reply: Result<Reply, EmailError>,
    ) -> Result<(), EmailError> {
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                if error.is_network_error() {
                    self.unclear(indices, Some(&unconfirmed()), None);
                }
                return Err(error);
            }
        };
        let fields = reply_fields(reply.payload);
        if !is_success(reply.status) {
            let error = batch_reply_error(fields, reply.status, reply.retry_after, indices);
            if error.code() == IN_FLIGHT_CODE {
                self.unclear(indices, Some(&error), Some(&error));
            } else if reply.status >= 500 {
                self.unclear(indices, Some(&unconfirmed()), Some(&error));
            } else {
                self.unclear(indices, None, Some(&error));
            }
            return Err(error);
        }
        let slots = match fields.get("results") {
            Some(Value::Array(slots)) if slots.len() == indices.len() => slots,
            _ => {
                let error = EmailError::from_response(
                    reply.status,
                    None,
                    Some("PostHog answered the batch without a result for each recipient.".into()),
                    None,
                );
                self.unclear(indices, Some(&unconfirmed()), Some(&error));
                return Err(error);
            }
        };
        for (index, slot) in indices.iter().zip(slots) {
            self.settle(*index, slot.as_object());
        }
        for warning in parse_warnings(&fields) {
            if !self.warnings.contains(&warning) {
                self.warnings.push(warning);
            }
        }
        Ok(())
    }

    /// An attempt PostHog may have queued: each recipient not yet unconfirmed
    /// becomes unconfirmed with `cause`, and each already unconfirmed keeps
    /// their first cause and gets PostHog's `answer` as their later error.
    fn unclear(
        &mut self,
        indices: &[usize],
        cause: Option<&EmailError>,
        answer: Option<&EmailError>,
    ) {
        for index in indices {
            let result = &mut self.members[*index].result;
            if result.status == BatchEmailStatus::Unconfirmed {
                if let Some(answer) = answer {
                    result.later_error = Some(answer.clone());
                }
            } else if let Some(cause) = cause {
                result.status = BatchEmailStatus::Unconfirmed;
                result.error = Some(resend_with_key(cause.clone()));
            }
        }
    }

    fn settle(&mut self, index: usize, slot: Option<&Map<String, Value>>) {
        let empty = Map::new();
        let slot = slot.unwrap_or(&empty);
        let result = &mut self.members[index].result;
        let message_id = text(slot, "message_id");
        let error = slot.get("error").filter(|error| !error.is_null());
        if error.is_none() && slot.get("status") == Some(&json!("queued")) && message_id.is_some() {
            result.status = BatchEmailStatus::Queued;
            result.message_id = message_id;
            result.replayed = slot.get("replayed") == Some(&Value::Bool(true));
            result.error = None;
            result.later_error = None;
            return;
        }
        let answer = slot_error(error);
        result.message_id = message_id.or(result.message_id.take());
        if result.status == BatchEmailStatus::Unconfirmed {
            result.later_error = Some(answer);
            return;
        }
        if UNCONFIRMED_SLOT_CODES.contains(&answer.code()) {
            result.status = BatchEmailStatus::Unconfirmed;
            result.error = Some(resend_with_key(answer));
        } else {
            result.status = BatchEmailStatus::Failed;
            result.error = Some(answer);
        }
    }

    /// The batch stops at the request of `indices` with `error`: the
    /// request's recipients that aren't unconfirmed fail with it, and each
    /// later recipient isn't attempted, with `skipped`.
    fn stop(&mut self, indices: &[usize], error: EmailError, skipped: &EmailError) {
        for index in indices {
            let result = &mut self.members[*index].result;
            if result.status != BatchEmailStatus::Unconfirmed {
                result.status = BatchEmailStatus::Failed;
                result.error = Some(error.clone());
            }
        }
        for member in &mut self.members {
            if member.result.status == BatchEmailStatus::NotAttempted {
                member.result.error = Some(skipped.clone());
            }
        }
        self.stopped_by = Some(error);
        self.phase = Phase::Done;
    }

    fn finish(&mut self) -> Result<SendBatchResult, EmailError> {
        let result = SendBatchResult {
            results: std::mem::take(&mut self.members)
                .into_iter()
                .map(|member| member.result)
                .collect(),
            warnings: std::mem::take(&mut self.warnings),
        };
        log_warnings(&result.warnings);
        match self.stopped_by.take() {
            None => Ok(result),
            Some(error) => Err(error.with_batch(result)),
        }
    }
}

fn not_attempted() -> EmailError {
    EmailError::new(
        "not_attempted",
        "An earlier request of the batch failed, so the client sent nothing for this recipient.",
    )
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

fn unconfirmed() -> EmailError {
    EmailError::new(
        "delivery_unavailable",
        "PostHog didn't confirm this email, so it may still go out.",
    )
    .with_kind(EmailErrorKind::ServiceUnavailable)
}

/// The client never reads an email by its key, so the only way to settle an
/// unconfirmed email is to send it again with that key: PostHog then queues it
/// at most once.
fn resend_with_key(error: EmailError) -> EmailError {
    let why = error.why().map(String::from);
    let docs_url = error.docs_url().map(String::from);
    error.with_details(
        why,
        Some("Send this email again with the same idempotency_key within 24 hours, so it goes out once.".into()),
        docs_url,
    )
}

/// PostHog's answer about one recipient that it didn't queue.
fn slot_error(error: Option<&Value>) -> EmailError {
    let Some(Value::Object(error)) = error else {
        return EmailError::new(
            "api_error",
            "PostHog answered without a readable result for this email, so it may still go out.",
        );
    };
    let code = text(error, "code").unwrap_or_else(|| "api_error".into());
    let status = error
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok());
    let message = text(error, "detail")
        .unwrap_or_else(|| format!("PostHog didn't queue this email: {code}."));
    EmailError::answered(status, code, message)
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
    changed: std::sync::Condvar,
    #[cfg(feature = "async-client")]
    changed_on_runtime: tokio::sync::Notify,
}

/// One email request in flight. Dropping it, also when the caller drops the
/// request's future, ends it.
pub(crate) struct InFlight<'a>(&'a EmailTracker);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.in_flight -= 1;
        if state.in_flight == 0 {
            self.0.notify();
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

    pub(crate) fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Refuses every later email request, and ends every retry wait.
    pub(crate) fn close(&self) {
        self.lock().closed = true;
        self.notify();
    }

    fn notify(&self) {
        self.changed.notify_all();
        #[cfg(feature = "async-client")]
        self.changed_on_runtime.notify_waiters();
    }

    /// Waits `delay` before a retry, or until the client shuts down.
    #[cfg(feature = "async-client")]
    pub(crate) async fn wait_to_retry(&self, delay: Duration) {
        let deadline = tokio::time::Instant::now() + clock::retry_wait(delay);
        loop {
            let changed = self.changed_on_runtime.notified();
            if self.is_closed() || tokio::time::Instant::now() >= deadline {
                return;
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                return;
            }
        }
    }

    /// Waits `delay` before a retry, or until the client shuts down.
    #[cfg(not(feature = "async-client"))]
    pub(crate) fn wait_to_retry(&self, delay: Duration) {
        let deadline = std::time::Instant::now() + clock::retry_wait(delay);
        let mut state = self.lock();
        while !state.closed {
            let Some(remaining) = deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|remaining| !remaining.is_zero())
            else {
                return;
            };
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
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
            let idle = self.changed_on_runtime.notified();
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
                .changed
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
        accept(reply).and_then(|reply| parse_send_reply(reply, None))
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
