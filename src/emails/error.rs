use std::error::Error as StdError;
use std::fmt;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::time::Duration;

/// A coarse category of an [`EmailError`], for code that branches on the kind
/// of failure rather than on the exact [`code`](EmailError::code).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EmailErrorKind {
    /// The send was malformed: a local argument check failed, or PostHog
    /// answered `400`, a `409` with `template_outdated`, or a `422` other than
    /// a reused idempotency key.
    ValidationError,
    /// The client has no secret key, or PostHog answered `401`.
    MissingApiKey,
    /// PostHog answered `403`: the secret key lacks the scope.
    InvalidPermission,
    /// PostHog answered `404`: no such template or email in this project.
    NotFound,
    /// PostHog answered `409`: a send with the same idempotency key is still
    /// in flight.
    ConcurrentIdempotentRequests,
    /// PostHog answered `422` with `idempotency_key_reused`: the key was
    /// already used for a different email.
    InvalidIdempotentRequest,
    /// PostHog answered `429`: too many requests.
    RateLimitExceeded,
    /// PostHog answered `429` with `quota_limited`: the monthly quota is spent.
    MonthlyQuotaExceeded,
    /// PostHog answered `503`, or the request failed on the network.
    ServiceUnavailable,
    /// Anything else.
    ApplicationError,
}

impl EmailErrorKind {
    /// The kind's name, such as `"validation_error"`, as every PostHog SDK
    /// spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            EmailErrorKind::ValidationError => "validation_error",
            EmailErrorKind::MissingApiKey => "missing_api_key",
            EmailErrorKind::InvalidPermission => "invalid_permission",
            EmailErrorKind::NotFound => "not_found",
            EmailErrorKind::ConcurrentIdempotentRequests => "concurrent_idempotent_requests",
            EmailErrorKind::InvalidIdempotentRequest => "invalid_idempotent_request",
            EmailErrorKind::RateLimitExceeded => "rate_limit_exceeded",
            EmailErrorKind::MonthlyQuotaExceeded => "monthly_quota_exceeded",
            EmailErrorKind::ServiceUnavailable => "service_unavailable",
            EmailErrorKind::ApplicationError => "application_error",
        }
    }

    pub(crate) fn of(status: Option<u16>, code: &str) -> Self {
        if code == "missing_secret_key" || status == Some(401) {
            return EmailErrorKind::MissingApiKey;
        }
        match status {
            None if code == "network_error" => EmailErrorKind::ServiceUnavailable,
            None if code.starts_with("invalid_") => EmailErrorKind::ValidationError,
            None => EmailErrorKind::ApplicationError,
            Some(400) => EmailErrorKind::ValidationError,
            Some(403) => EmailErrorKind::InvalidPermission,
            Some(404) => EmailErrorKind::NotFound,
            Some(409) if code == "template_outdated" => EmailErrorKind::ValidationError,
            Some(409) => EmailErrorKind::ConcurrentIdempotentRequests,
            Some(422) if code == "idempotency_key_reused" => {
                EmailErrorKind::InvalidIdempotentRequest
            }
            Some(422) => EmailErrorKind::ValidationError,
            Some(429) if code == "quota_limited" => EmailErrorKind::MonthlyQuotaExceeded,
            Some(429) => EmailErrorKind::RateLimitExceeded,
            Some(503) => EmailErrorKind::ServiceUnavailable,
            Some(_) => EmailErrorKind::ApplicationError,
        }
    }
}

impl fmt::Display for EmailErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// PostHog did not queue the email or read its status, or the client could not
/// tell whether it did.
///
/// Branch on [`code`](Self::code), PostHog's stable error code such as
/// `quota_limited`, or on [`kind`](Self::kind), a coarse category. The message
/// never holds the secret key or the request body.
#[derive(Debug)]
pub struct EmailError(Box<Details>);

#[derive(Debug)]
struct Details {
    kind: EmailErrorKind,
    code: String,
    message: String,
    status: Option<u16>,
    why: Option<String>,
    fix: Option<String>,
    docs_url: Option<String>,
    field: Option<String>,
    retry_after: Option<Duration>,
    source: Option<Source>,
}

/// The error that caused an [`EmailError`], such as a `reqwest::Error`.
///
/// Nothing changes it after the error is built, so it keeps `EmailError`, and
/// the crate's `Error` that wraps it, unwind safe, as that `Error` was before
/// it had an email variant.
struct Source(Box<dyn StdError + Send + Sync>);

impl UnwindSafe for Source {}
impl RefUnwindSafe for Source {}

impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl EmailError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        let code = code.into();
        Self(Box::new(Details {
            kind: EmailErrorKind::of(None, &code),
            code,
            message: message.into(),
            status: None,
            why: None,
            fix: None,
            docs_url: None,
            field: None,
            retry_after: None,
            source: None,
        }))
    }

    /// A local argument error about one field of the send.
    pub(crate) fn invalid(code: &str, message: impl Into<String>, field: &str) -> Self {
        Self::new(code, message).with_field(Some(field.to_string()))
    }

    pub(crate) fn from_response(
        status: u16,
        code: Option<String>,
        message: Option<String>,
        retry_after: Option<Duration>,
    ) -> Self {
        let code = code.unwrap_or_else(|| "api_error".to_string());
        Self(Box::new(Details {
            kind: EmailErrorKind::of(Some(status), &code),
            code,
            message: message.unwrap_or_else(|| format!("PostHog answered HTTP {status}.")),
            status: Some(status),
            why: None,
            fix: None,
            docs_url: None,
            field: None,
            retry_after,
            source: None,
        }))
    }

    pub(crate) fn with_field(mut self, field: Option<String>) -> Self {
        self.0.field = field;
        self
    }

    pub(crate) fn with_details(
        mut self,
        why: Option<String>,
        fix: Option<String>,
        docs_url: Option<String>,
    ) -> Self {
        self.0.why = why;
        self.0.fix = fix;
        self.0.docs_url = docs_url;
        self
    }

    pub(crate) fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.0.source = Some(Source(Box::new(source)));
        self
    }

    /// The coarse category of the failure.
    pub fn kind(&self) -> EmailErrorKind {
        self.0.kind
    }

    /// PostHog's error code, such as `quota_limited`, `recipient_suppressed`,
    /// `recipient_opted_out` or `unknown_category`, or an SDK code, such as
    /// `network_error`, `missing_secret_key` or `invalid_template_id`.
    pub fn code(&self) -> &str {
        &self.0.code
    }

    /// What went wrong, in words.
    pub fn message(&self) -> &str {
        &self.0.message
    }

    /// The HTTP status, when PostHog answered.
    pub fn status(&self) -> Option<u16> {
        self.0.status
    }

    /// Why PostHog refused the request, when it said.
    pub fn why(&self) -> Option<&str> {
        self.0.why.as_deref()
    }

    /// How to fix the request, when PostHog or the SDK said.
    pub fn fix(&self) -> Option<&str> {
        self.0.fix.as_deref()
    }

    /// A link to the docs for this error, when PostHog gave one.
    pub fn docs_url(&self) -> Option<&str> {
        self.0.docs_url.as_deref()
    }

    /// The request field the error is about, such as `to`, when one is known.
    pub fn field(&self) -> Option<&str> {
        self.0.field.as_deref()
    }

    /// How long PostHog asked the client to wait before it sends again, from
    /// the `Retry-After` header of a `429` or `503`.
    pub fn retry_after(&self) -> Option<Duration> {
        self.0.retry_after
    }

    pub(crate) fn is_network_error(&self) -> bool {
        self.0.status.is_none() && self.0.code == "network_error"
    }
}

impl fmt::Display for EmailError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let details = &self.0;
        match details.status {
            Some(status) => write!(f, "{} (HTTP {status}): {}", details.code, details.message),
            None => write!(f, "{}: {}", details.code, details.message),
        }
    }
}

impl StdError for EmailError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.0
            .source
            .as_ref()
            .map(|source| source.0.as_ref() as &(dyn StdError + 'static))
    }
}

#[cfg(test)]
mod tests {
    use super::EmailErrorKind as Kind;

    #[test]
    fn errors_keep_their_auto_traits() {
        fn check<T: Send + Sync + std::panic::UnwindSafe + std::panic::RefUnwindSafe>() {}
        check::<super::EmailError>();
        check::<crate::Error>();
    }

    #[test]
    fn kinds_follow_the_status_and_code() {
        let cases = [
            (None, "missing_secret_key", Kind::MissingApiKey),
            (Some(401), "authentication_failed", Kind::MissingApiKey),
            (None, "network_error", Kind::ServiceUnavailable),
            (None, "invalid_template_id", Kind::ValidationError),
            (None, "client_disabled", Kind::ApplicationError),
            (Some(400), "invalid_input", Kind::ValidationError),
            (Some(403), "permission_denied", Kind::InvalidPermission),
            (Some(404), "not_found", Kind::NotFound),
            (Some(409), "template_outdated", Kind::ValidationError),
            (
                Some(409),
                "idempotency_key_in_flight",
                Kind::ConcurrentIdempotentRequests,
            ),
            (
                Some(422),
                "idempotency_key_reused",
                Kind::InvalidIdempotentRequest,
            ),
            (Some(422), "recipient_suppressed", Kind::ValidationError),
            (Some(429), "quota_limited", Kind::MonthlyQuotaExceeded),
            (Some(429), "throttled", Kind::RateLimitExceeded),
            (Some(503), "service_unavailable", Kind::ServiceUnavailable),
            (Some(500), "api_error", Kind::ApplicationError),
            (Some(202), "api_error", Kind::ApplicationError),
        ];
        for (status, code, kind) in cases {
            assert_eq!(Kind::of(status, code), kind, "{status:?} {code}");
        }
    }
}
