//! Runtime-agnostic retry primitives shared by the V0 and V1 capture paths.

use std::time::{Duration, SystemTime};

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use reqwest::header::HeaderMap;

use super::ClientOptions;
use crate::error::Error;

/// Outcome of one capture attempt, computed without any I/O so both the async
/// and blocking clients (and both V0 and V1) can share the decision logic and
/// keep only the transport-specific loop.
#[derive(Debug)]
pub(crate) enum Step {
    Done,
    Backoff(Duration),
    Fail(Error),
}

#[derive(Debug)]
pub(crate) enum FeatureFlagsResponseStep {
    Done,
    Backoff(Duration),
}

#[derive(Debug)]
pub(crate) enum FeatureFlagsTransportStep {
    Backoff(Duration),
    Fail(Error),
}

/// Statuses worth retrying. Everything else (including 429) is terminal so the
/// caller surfaces it without burning the attempt budget.
pub(crate) fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 500 | 502 | 503 | 504)
}

/// V0 retry decision. Beyond the shared retryable set, V0 retries 429 — but
/// only when the server sent a `Retry-After` (an explicit "try again in N"
/// instruction). A bare 429 stays terminal (`Error::RateLimit`), preserving
/// long-standing v0 behavior. The V1 endpoint never returns 429, so this is
/// intentionally scoped to V0.
#[cfg(not(feature = "capture-v1"))]
pub(crate) fn should_retry_v0(status: u16, has_retry_after: bool) -> bool {
    is_retryable_status(status) || (status == 429 && has_retry_after)
}

/// Parse `Retry-After` as either delay-seconds or an HTTP-date.
///
/// Non-positive delays and dates in the past are ignored, matching the HTTP
/// semantics that `Retry-After` is a minimum delay before the next attempt.
pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    parse_retry_after_at(headers, SystemTime::now())
}

fn parse_retry_after_at(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    let value = headers.get("retry-after")?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }

    if let Ok(secs) = value.parse::<u64>() {
        return (secs > 0).then(|| Duration::from_secs(secs));
    }

    parse_http_date(value)
        .and_then(|t| t.duration_since(now).ok())
        .filter(|d| !d.is_zero())
}

fn parse_http_date(value: &str) -> Option<SystemTime> {
    if let Ok(dt) = DateTime::parse_from_rfc2822(value) {
        return datetime_to_system_time(dt.with_timezone(&Utc));
    }

    // HTTP-date also permits the obsolete RFC 850 and ANSI C asctime forms.
    // Parse them as GMT/UTC without adding another dependency.
    for format in ["%A, %d-%b-%y %H:%M:%S GMT", "%a %b %e %H:%M:%S %Y"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(value, format) {
            return datetime_to_system_time(Utc.from_utc_datetime(&dt));
        }
    }

    None
}

fn datetime_to_system_time(dt: DateTime<Utc>) -> Option<SystemTime> {
    let secs = dt.timestamp();
    if secs < 0 {
        return None;
    }
    SystemTime::UNIX_EPOCH.checked_add(Duration::new(secs as u64, dt.timestamp_subsec_nanos()))
}

/// Backoff before `attempt`: exponential growth from
/// `retry_initial_backoff_ms`, clamped to `retry_max_backoff_ms`. When present,
/// `Retry-After` is treated as a server-provided minimum delay, so the client
/// waits for the longer of the configured backoff and `Retry-After` — but the
/// `Retry-After` is itself clamped to `retry_max_backoff_ms`, so a hostile/buggy
/// server header can't push a single wait past the ceiling the caller already
/// configured (default 30s). The configured backoff is never truncated.
pub(crate) fn backoff_duration(
    opts: &ClientOptions,
    attempt: u32,
    retry_after: Option<Duration>,
) -> Duration {
    let base_ms = opts.retry_initial_backoff_ms;
    let max_ms = opts.retry_max_backoff_ms;
    let backoff_ms = base_ms.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)));
    let configured_delay = Duration::from_millis(backoff_ms.min(max_ms));
    let max_backoff = Duration::from_millis(max_ms);
    let clamped_retry_after = retry_after.map(|d| d.min(max_backoff));
    clamped_retry_after.map_or(configured_delay, |d| configured_delay.max(d))
}

/// Sans-IO decision for a completed V0 capture response. `attempt` is the
/// just-finished attempt number (1-based); the backoff for the next attempt
/// uses `attempt` directly so the first retry waits exactly
/// `retry_initial_backoff_ms`.
#[cfg(not(feature = "capture-v1"))]
pub(crate) fn v0_after_response(
    opts: &ClientOptions,
    attempt: u32,
    status: u16,
    retry_after: Option<Duration>,
    body: &str,
) -> Step {
    if should_retry_v0(status, retry_after.is_some()) && attempt < opts.max_capture_attempts {
        return Step::Backoff(backoff_duration(opts, attempt, retry_after));
    }
    match Error::from_http_response(status, body.to_string()) {
        Some(err) => Step::Fail(err),
        None => Step::Done,
    }
}

/// Sans-IO decision for a V0 transport error (no HTTP response). Retries
/// transport failures until the attempt budget is exhausted.
#[cfg(not(feature = "capture-v1"))]
pub(crate) fn v0_after_transport_error(
    opts: &ClientOptions,
    attempt: u32,
    err_msg: String,
) -> Step {
    if attempt >= opts.max_capture_attempts {
        return Step::Fail(Error::Connection(err_msg));
    }
    Step::Backoff(backoff_duration(opts, attempt, None))
}

fn is_retryable_feature_flags_status(status: u16) -> bool {
    matches!(status, 502 | 504)
}

/// Sans-IO decision for a completed remote `/flags` response. Only 502 and 504
/// are retried here; other HTTP statuses remain terminal for the caller.
pub(crate) fn feature_flags_after_response(
    opts: &ClientOptions,
    attempt: u32,
    status: u16,
) -> FeatureFlagsResponseStep {
    if is_retryable_feature_flags_status(status)
        && attempt <= opts.feature_flags_request_max_retries
    {
        return FeatureFlagsResponseStep::Backoff(backoff_duration(opts, attempt, None));
    }
    FeatureFlagsResponseStep::Done
}

/// Sans-IO decision for a remote `/flags` transport error. The feature-flags
/// option counts retries after the initial attempt, so `attempt == 1` is still
/// allowed when `feature_flags_request_max_retries == 1`.
pub(crate) fn feature_flags_after_transport_error(
    opts: &ClientOptions,
    attempt: u32,
    retryable: bool,
    err_msg: String,
) -> FeatureFlagsTransportStep {
    if !retryable || attempt > opts.feature_flags_request_max_retries {
        return FeatureFlagsTransportStep::Fail(Error::Connection(err_msg));
    }
    FeatureFlagsTransportStep::Backoff(backoff_duration(opts, attempt, None))
}

#[cfg(test)]
mod tests {
    use reqwest::header::{HeaderMap, HeaderValue};

    use super::*;
    use crate::client::ClientOptionsBuilder;

    fn test_opts() -> ClientOptions {
        ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .max_capture_attempts(3u32)
            .retry_initial_backoff_ms(100u64)
            .retry_max_backoff_ms(5000u64)
            .build()
            .unwrap()
    }

    #[test]
    fn retryable_statuses() {
        for code in [408, 500, 502, 503, 504] {
            assert!(
                is_retryable_status(code),
                "expected {} to be retryable",
                code
            );
        }
    }

    #[test]
    fn non_retryable_statuses() {
        for code in [200, 201, 400, 401, 402, 403, 413, 415, 418, 429, 404] {
            assert!(
                !is_retryable_status(code),
                "expected {} to NOT be retryable",
                code
            );
        }
    }

    #[cfg(not(feature = "capture-v1"))]
    #[test]
    fn v0_retries_429_only_with_retry_after() {
        assert!(should_retry_v0(429, true));
        assert!(!should_retry_v0(429, false));
        // The shared retryable set applies regardless of Retry-After.
        for code in [408, 500, 502, 503, 504] {
            assert!(should_retry_v0(code, false));
            assert!(should_retry_v0(code, true));
        }
        // Genuinely terminal statuses stay terminal either way.
        for code in [400, 401, 402, 403, 413, 415] {
            assert!(!should_retry_v0(code, false));
            assert!(!should_retry_v0(code, true));
        }
    }

    #[test]
    fn parse_retry_after_cases() {
        let now = std::time::UNIX_EPOCH;

        // (header value, expected): delay-seconds and all HTTP-date forms parse;
        // missing, invalid, zero, and past/equal-date values are ignored.
        let cases: Vec<(Option<&str>, Option<Duration>)> = vec![
            (Some("5"), Some(Duration::from_secs(5))),
            (
                Some("Thu, 01 Jan 1970 00:00:30 GMT"),
                Some(Duration::from_secs(30)),
            ),
            (
                Some("Thursday, 01-Jan-70 00:00:30 GMT"),
                Some(Duration::from_secs(30)),
            ),
            (
                Some("Thu Jan  1 00:00:30 1970"),
                Some(Duration::from_secs(30)),
            ),
            (Some("0"), None),
            (None, None),
            (Some("not-a-number"), None),
            (Some("Thu, 01 Jan 1970 00:00:00 GMT"), None),
        ];
        for (header_val, expected) in cases {
            let mut headers = HeaderMap::new();
            if let Some(v) = header_val {
                headers.insert("retry-after", HeaderValue::from_str(v).unwrap());
            }
            assert_eq!(
                parse_retry_after_at(&headers, now),
                expected,
                "header={:?}",
                header_val
            );
        }
    }

    #[test]
    fn backoff_retry_after_minimum_semantics() {
        let opts = test_opts(); // initial=100ms, max=5000ms
                                // (retry_after, attempt, expected): Retry-After is a minimum, so the
                                // client waits for the longer of the configured backoff and Retry-After,
                                // but Retry-After is clamped to retry_max_backoff_ms (5000ms here).
        let cases: &[(Option<Duration>, u32, Duration)] = &[
            // Above the ceiling -> clamped to retry_max_backoff_ms.
            (
                Some(Duration::from_secs(42)),
                1,
                Duration::from_millis(5000),
            ),
            (
                Some(Duration::from_millis(1)),
                1,
                Duration::from_millis(100),
            ),
            (
                Some(Duration::from_millis(100)),
                1,
                Duration::from_millis(100),
            ),
        ];
        for &(retry_after, attempt, expected) in cases {
            assert_eq!(
                backoff_duration(&opts, attempt, retry_after),
                expected,
                "retry_after={retry_after:?} attempt={attempt}"
            );
        }
    }

    #[test]
    fn backoff_exponential_growth() {
        let opts = test_opts();
        assert_eq!(backoff_duration(&opts, 1, None), Duration::from_millis(100));
        assert_eq!(backoff_duration(&opts, 2, None), Duration::from_millis(200));
        assert_eq!(backoff_duration(&opts, 3, None), Duration::from_millis(400));
    }

    #[test]
    fn backoff_clamped_to_max() {
        let opts = ClientOptionsBuilder::default()
            .api_key("k".to_string())
            .retry_initial_backoff_ms(100u64)
            .retry_max_backoff_ms(150u64)
            .build()
            .unwrap();
        assert_eq!(backoff_duration(&opts, 3, None), Duration::from_millis(150));
    }

    #[test]
    fn backoff_clamps_retry_after_to_max_backoff() {
        // A hostile `Retry-After` is clamped to the configured max backoff
        // (`retry_max_backoff_ms`, 5000ms in test_opts) rather than honored.
        let opts = test_opts();
        assert_eq!(
            backoff_duration(&opts, 1, Some(Duration::from_secs(u64::MAX))),
            Duration::from_millis(opts.retry_max_backoff_ms)
        );
        // A Retry-After above the ceiling is also clamped to it.
        assert_eq!(
            backoff_duration(&opts, 1, Some(Duration::from_secs(600))),
            Duration::from_millis(opts.retry_max_backoff_ms)
        );
    }

    // -- v0 sans-IO decisions ------------------------------------------------

    #[cfg(not(feature = "capture-v1"))]
    fn schedule_opts() -> ClientOptions {
        ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .max_capture_attempts(10u32)
            .retry_initial_backoff_ms(100u64)
            .retry_max_backoff_ms(1_000_000u64)
            .build()
            .unwrap()
    }

    #[cfg(not(feature = "capture-v1"))]
    fn backoff_ms(step: Step) -> u64 {
        match step {
            Step::Backoff(d) => d.as_millis() as u64,
            _ => panic!("expected Step::Backoff"),
        }
    }

    /// The schedule the call sites actually produce. Guards the `attempt + 1`
    /// off-by-one: the first retry must wait exactly `retry_initial_backoff_ms`
    /// (100ms here), not double it.
    #[cfg(not(feature = "capture-v1"))]
    #[test]
    fn v0_backoff_schedule_starts_at_initial() {
        let opts = schedule_opts();
        // attempt = the just-finished attempt; first retry follows attempt 1.
        assert_eq!(
            backoff_ms(v0_after_response(&opts, 1, 503, None, "")),
            100,
            "first retry must honor retry_initial_backoff_ms exactly"
        );
        assert_eq!(backoff_ms(v0_after_response(&opts, 2, 503, None, "")), 200);
        assert_eq!(backoff_ms(v0_after_response(&opts, 3, 503, None, "")), 400);
        // Same schedule for transport errors.
        assert_eq!(
            backoff_ms(v0_after_transport_error(&opts, 1, "timeout".into())),
            100
        );
        assert_eq!(
            backoff_ms(v0_after_transport_error(&opts, 2, "timeout".into())),
            200
        );
    }

    #[cfg(not(feature = "capture-v1"))]
    #[test]
    fn v0_after_response_terminal_and_success() {
        let opts = schedule_opts();
        // 2xx -> Done.
        assert!(matches!(
            v0_after_response(&opts, 1, 200, None, ""),
            Step::Done
        ));
        // Bare 429 (no Retry-After) -> terminal RateLimit, no retry.
        assert!(matches!(
            v0_after_response(&opts, 1, 429, None, ""),
            Step::Fail(Error::RateLimit)
        ));
        // 429 + Retry-After -> retried.
        assert!(matches!(
            v0_after_response(&opts, 1, 429, Some(Duration::from_secs(1)), ""),
            Step::Backoff(_)
        ));
        // Non-retryable 4xx -> terminal.
        assert!(matches!(
            v0_after_response(&opts, 1, 400, None, "bad"),
            Step::Fail(Error::BadRequest(_))
        ));
    }

    #[cfg(not(feature = "capture-v1"))]
    #[test]
    fn v0_exhausts_attempt_budget() {
        let opts = ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .max_capture_attempts(3u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(5u64)
            .build()
            .unwrap();
        // Retryable status on the final attempt surfaces the error instead of backing off.
        assert!(matches!(
            v0_after_response(&opts, 3, 503, None, "boom"),
            Step::Fail(Error::ServerError { .. })
        ));
        assert!(matches!(
            v0_after_transport_error(&opts, 3, "timeout".into()),
            Step::Fail(Error::Connection(_))
        ));
    }

    #[test]
    fn feature_flags_http_status_uses_retry_budget_for_502_and_504_only() {
        let opts = ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .feature_flags_request_max_retries(1u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(5u64)
            .build()
            .unwrap();

        for status in [502, 504] {
            assert!(matches!(
                feature_flags_after_response(&opts, 1, status),
                FeatureFlagsResponseStep::Backoff(_)
            ));
            assert!(matches!(
                feature_flags_after_response(&opts, 2, status),
                FeatureFlagsResponseStep::Done
            ));
        }
        for status in [500, 503, 429] {
            assert!(matches!(
                feature_flags_after_response(&opts, 1, status),
                FeatureFlagsResponseStep::Done
            ));
        }
    }

    #[test]
    fn feature_flags_transport_error_uses_retry_budget() {
        let opts = ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .feature_flags_request_max_retries(1u32)
            .retry_initial_backoff_ms(1u64)
            .retry_max_backoff_ms(5u64)
            .build()
            .unwrap();

        assert!(matches!(
            feature_flags_after_transport_error(&opts, 1, true, "reset".into()),
            FeatureFlagsTransportStep::Backoff(_)
        ));
        assert!(matches!(
            feature_flags_after_transport_error(&opts, 2, true, "reset".into()),
            FeatureFlagsTransportStep::Fail(Error::Connection(_))
        ));
        assert!(matches!(
            feature_flags_after_transport_error(&opts, 1, false, "refused".into()),
            FeatureFlagsTransportStep::Fail(Error::Connection(_))
        ));
    }
}
