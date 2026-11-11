//! Bounded flag work on the existing delivery worker. Queue one wake token and
//! replace its pending request; an executing request is never interrupted.

use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use super::{ClientOptions, Control};
use crate::client::common::{extract_flag_details, remote_record_from_detail, report_flags_error};
use crate::client::event_store::RecordBuffer;
use crate::client::retry::{self, FeatureFlagsResponseStep, FeatureFlagsTransportStep};
use crate::endpoints::Endpoint;
use crate::feature_flag_evaluations::EvaluatedFlagRecord;
use crate::feature_flags::FeatureFlagsResponse;
use crate::Error;

pub(crate) const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_FLAGS: usize = 10_000;

/// Sanitized stateful flags-load failure; cached values may remain usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoadError {
    RequestTooLarge,
    Network,
    Http(u16),
    ResponseTooLarge,
    InvalidResponse,
    TooManyFlags,
    QuotaLimited,
    CacheTooLarge,
}

pub(crate) struct LoadedFlags {
    pub(crate) records: HashMap<String, EvaluatedFlagRecord>,
    pub(crate) request_id: Option<String>,
    pub(crate) evaluated_at: Option<i64>,
    pub(crate) partial: bool,
    pub(crate) invalid_payloads: bool,
}

pub(crate) struct Task {
    body: Vec<u8>,
    distinct_id: Option<String>,
    complete: Box<dyn FnOnce(Result<LoadedFlags, LoadError>) + Send>,
}

impl Task {
    pub(crate) fn new(
        payload: &Value,
        complete: impl FnOnce(Result<LoadedFlags, LoadError>) + Send + 'static,
    ) -> Result<Self, LoadError> {
        let mut buffer = RecordBuffer {
            bytes: Vec::new(),
            limit: MAX_REQUEST_BYTES as u64,
        };
        serde_json::to_writer(&mut buffer, payload).map_err(|_| LoadError::RequestTooLarge)?;
        Ok(Self {
            body: buffer.bytes,
            distinct_id: payload
                .get("distinct_id")
                .and_then(Value::as_str)
                .map(str::to_owned),
            complete: Box::new(complete),
        })
    }

    fn run(self, http: &reqwest::blocking::Client, options: &ClientOptions) {
        let endpoint = options.endpoints().build_url(Endpoint::Flags);
        let result = fetch(http, options, &endpoint, &self.body);
        if let Err(error) = &result {
            report(options, &endpoint, self.distinct_id.as_deref(), *error);
        } else if result.as_ref().is_ok_and(|loaded| loaded.invalid_payloads) {
            report(
                options,
                &endpoint,
                self.distinct_id.as_deref(),
                LoadError::InvalidResponse,
            );
        }
        // Completion invokes state integration and application notifications, never
        // under the slot lock. A callback panic cannot terminate event delivery.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.complete)(result)))
            .is_err()
        {
            tracing::warn!("posthog-rs: feature flag completion panicked");
        }
    }
}

#[derive(Default)]
struct Pending {
    task: Option<Task>,
    queued: bool,
    closed: bool,
}

#[derive(Default)]
pub(crate) struct Slot(Mutex<Pending>);

impl Slot {
    pub(crate) fn enqueue(
        self: &Arc<Self>,
        task: Task,
        tx: &mpsc::Sender<Control>,
        closed: &AtomicBool,
    ) -> bool {
        let (accepted, discarded) = {
            let mut pending = self.0.lock().unwrap_or_else(|p| p.into_inner());
            if pending.closed || closed.load(Ordering::Acquire) {
                return false;
            }
            let old = pending.task.replace(task);
            if pending.queued {
                (true, old)
            } else {
                pending.queued = true;
                if tx.send(Control::Flags(self.clone())).is_ok() {
                    (true, old)
                } else {
                    pending.closed = true;
                    pending.queued = false;
                    (false, pending.task.take())
                }
            }
        };
        // Dropping a callback's captured values can execute destructors.
        drop(discarded);
        accepted
    }

    pub(crate) fn close(&self) {
        let discarded = {
            let mut pending = self.0.lock().unwrap_or_else(|p| p.into_inner());
            pending.closed = true;
            pending.task.take()
        };
        drop(discarded);
    }

    pub(crate) fn run(&self, http: &reqwest::blocking::Client, options: &ClientOptions) {
        let task = {
            let mut pending = self.0.lock().unwrap_or_else(|p| p.into_inner());
            pending.queued = false;
            pending.task.take()
        };
        if let Some(task) = task {
            task.run(http, options);
        }
    }
}

pub(crate) fn report(options: &ClientOptions, endpoint: &str, id: Option<&str>, error: LoadError) {
    let message = match error {
        LoadError::RequestTooLarge => "Feature flags request exceeds limit",
        LoadError::Network => "Feature flags request failed",
        LoadError::Http(_) => "Feature flags HTTP request failed",
        LoadError::ResponseTooLarge => "Feature flags response exceeds limit",
        LoadError::InvalidResponse => "Invalid feature flags response or payload",
        LoadError::TooManyFlags => "Feature flags count exceeds limit",
        LoadError::QuotaLimited => "Feature flags quota exceeded",
        LoadError::CacheTooLarge => "Feature flags cache exceeds limit",
    };
    let status = match error {
        LoadError::Http(status) => Some(status),
        _ => None,
    };
    let error = Error::Connection(message.into());
    if options.on_error.is_empty() {
        tracing::warn!("posthog-rs: {message}");
    } else {
        report_flags_error(&options.on_error, endpoint, id, status, None, &error);
    }
}

fn fetch(
    http: &reqwest::blocking::Client,
    options: &ClientOptions,
    endpoint: &str,
    body: &[u8],
) -> Result<LoadedFlags, LoadError> {
    let mut attempt = 1;
    loop {
        let request = http
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::USER_AGENT,
                crate::client::get_default_user_agent(),
            )
            .timeout(Duration::from_secs(
                options.feature_flags_request_timeout_seconds,
            ))
            .body(body.to_vec());
        match request.send() {
            Ok(response) => {
                let status = response.status().as_u16();
                match retry::feature_flags_after_response(options, attempt, status) {
                    FeatureFlagsResponseStep::Backoff(delay) => {
                        drop(response);
                        std::thread::sleep(delay);
                        attempt += 1;
                        continue;
                    }
                    FeatureFlagsResponseStep::Done => {}
                }
                if status != 200 {
                    return Err(LoadError::Http(status));
                }
                let mut bytes = Vec::new();
                response
                    .take(MAX_RESPONSE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| LoadError::Network)?;
                if bytes.len() > MAX_RESPONSE_BYTES {
                    return Err(LoadError::ResponseTooLarge);
                }
                return decode(&bytes);
            }
            Err(error) => match retry::feature_flags_after_transport_error(
                options,
                attempt,
                retry::is_retryable_feature_flags_error(&error),
                "Feature flags request failed".into(),
            ) {
                FeatureFlagsTransportStep::Backoff(delay) => {
                    std::thread::sleep(delay);
                    attempt += 1;
                }
                FeatureFlagsTransportStep::Fail(_) => return Err(LoadError::Network),
            },
        }
    }
}

/// Envelope metadata also occurs on legacy responses. Decode it separately
/// because the shared server response enum omits those fields.
#[derive(serde::Deserialize)]
struct Envelope {
    #[serde(default, rename = "errorsWhileComputingFlags")]
    partial: bool,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    #[serde(rename = "evaluatedAt")]
    evaluated_at: Option<i64>,
    #[serde(default, rename = "minimalFlagCalledEvents")]
    minimal: bool,
}

fn decode(bytes: &[u8]) -> Result<LoadedFlags, LoadError> {
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(LoadError::ResponseTooLarge);
    }
    let mut value: Value = serde_json::from_slice(bytes).map_err(|_| LoadError::InvalidResponse)?;
    // Client v2 responses use product names; retain compatibility with the
    // boolean form understood by the existing Rust decoder. A quota-only
    // response need not contain flag maps.
    match value.get("quotaLimited") {
        Some(Value::Array(products)) => {
            if products.iter().any(|product| !product.is_string()) {
                return Err(LoadError::InvalidResponse);
            }
            if products
                .iter()
                .any(|product| product.as_str() == Some("feature_flags"))
            {
                return Err(LoadError::QuotaLimited);
            }
            value["quotaLimited"] = Value::Bool(false);
        }
        Some(Value::Bool(true)) => return Err(LoadError::QuotaLimited),
        _ => {}
    }
    let envelope: Envelope =
        serde::Deserialize::deserialize(&value).map_err(|_| LoadError::InvalidResponse)?;
    let mut failed = false;
    if let Some(flags) = value.get_mut("flags") {
        let flags = flags.as_object_mut().ok_or(LoadError::InvalidResponse)?;
        // Count the original map, including failed evaluations, before filtering.
        if flags.len() > MAX_FLAGS {
            return Err(LoadError::TooManyFlags);
        }
        for detail in flags.values() {
            match detail.get("failed") {
                Some(Value::Bool(true)) => failed = true,
                Some(Value::Bool(false)) | None => {}
                _ => return Err(LoadError::InvalidResponse),
            }
        }
        flags.retain(|_, detail| detail.get("failed") != Some(&Value::Bool(true)));
    }
    let response: FeatureFlagsResponse =
        serde_json::from_value(value).map_err(|_| LoadError::InvalidResponse)?;
    let legacy_response = matches!(&response, FeatureFlagsResponse::Legacy { .. });
    let details = extract_flag_details(response);
    if details.quota_limited {
        return Err(LoadError::QuotaLimited);
    }
    if details.flags.len() > MAX_FLAGS {
        return Err(LoadError::TooManyFlags);
    }
    let mut invalid_payloads = false;
    let minimal = envelope.minimal;
    let records = details
        .flags
        .into_iter()
        .map(|(key, mut detail)| {
            // Detach before the shared record conversion: its existing server policy
            // normalizes strings with a raw-string fallback. Remote string payloads
            // in both response formats are serialized JSON; decode once and omit
            // malformed strings before storing the normalized cache.
            let payload = detail
                .metadata
                .as_mut()
                .and_then(|metadata| metadata.payload.take());
            let payload = match payload {
                Some(Value::String(raw)) => match serde_json::from_str(&raw) {
                    Ok(value) => Some(value),
                    Err(_) => {
                        invalid_payloads = true;
                        None
                    }
                },
                other => other,
            };
            let mut record = remote_record_from_detail(detail, minimal);
            if legacy_response {
                // Legacy normalization synthesizes metadata only to carry payloads.
                record.id = None;
                record.version = None;
            }
            record.payload = payload;
            (key, record)
        })
        .collect();
    Ok(LoadedFlags {
        records,
        request_id: envelope.request_id,
        evaluated_at: envelope.evaluated_at,
        partial: envelope.partial || details.errors_while_computing_flags || failed,
        invalid_payloads,
    })
}

#[cfg(test)]
#[path = "transport_flags_tests.rs"]
mod tests;
