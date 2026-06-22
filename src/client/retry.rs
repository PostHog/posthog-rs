//! Runtime-agnostic retry primitives shared by the V0 and V1 capture paths.

use std::time::Duration;

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

/// Parse a numeric `Retry-After` (seconds). Ignores HTTP-date form, which the
/// capture endpoints don't use.
pub(crate) fn parse_retry_after(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
}

/// Hard cap on any scheduled retry delay. Guards the worker's `Instant + delay`
/// from overflowing — which would panic the worker thread and silently drop all
/// later captures — on a hostile/buggy `Retry-After` (or an extreme
/// `retry_max_backoff_ms`). A day is far beyond any sane retry delay.
const RETRY_BACKOFF_CAP: Duration = Duration::from_secs(86_400);

/// Backoff before `attempt`: honor `Retry-After` when present, otherwise
/// exponential growth from `retry_initial_backoff_ms`, clamped to
/// `retry_max_backoff_ms`. The result is capped at [`RETRY_BACKOFF_CAP`] so an
/// absurd value can't overflow the worker's `next_at` computation.
pub(crate) fn backoff_duration(
    opts: &ClientOptions,
    attempt: u32,
    retry_after_secs: Option<u64>,
) -> Duration {
    let delay = if let Some(secs) = retry_after_secs {
        Duration::from_secs(secs)
    } else {
        let base_ms = opts.retry_initial_backoff_ms;
        let max_ms = opts.retry_max_backoff_ms;
        let backoff_ms = base_ms.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)));
        Duration::from_millis(backoff_ms.min(max_ms))
    };
    delay.min(RETRY_BACKOFF_CAP)
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
    retry_after: Option<u64>,
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
        // (header value, expected): numeric parses, missing/non-numeric are None.
        let cases: [(Option<&str>, Option<u64>); 3] = [
            (Some("5"), Some(5)),
            (None, None),
            (Some("not-a-number"), None),
        ];
        for (header_val, expected) in cases {
            let mut headers = HeaderMap::new();
            if let Some(v) = header_val {
                headers.insert("retry-after", HeaderValue::from_str(v).unwrap());
            }
            assert_eq!(
                parse_retry_after(&headers),
                expected,
                "header={:?}",
                header_val
            );
        }
    }

    #[test]
    fn backoff_explicit_retry_after_wins() {
        let opts = test_opts();
        assert_eq!(
            backoff_duration(&opts, 1, Some(42)),
            Duration::from_secs(42)
        );
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
    fn backoff_caps_absurd_retry_after() {
        // A hostile `Retry-After` must be capped, not overflow `Instant` later.
        let opts = test_opts();
        assert_eq!(
            backoff_duration(&opts, 1, Some(u64::MAX)),
            RETRY_BACKOFF_CAP
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
            v0_after_response(&opts, 1, 429, Some(1), ""),
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
}
