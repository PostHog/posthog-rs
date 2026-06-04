//! Runtime-agnostic retry primitives shared by the V0 and V1 capture paths.

use std::time::Duration;

use reqwest::header::HeaderMap;

use super::ClientOptions;

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

/// Backoff before `attempt`: honor `Retry-After` when present, otherwise
/// exponential growth from `retry_initial_backoff_ms`, clamped to
/// `retry_max_backoff_ms`.
pub(crate) fn backoff_duration(
    opts: &ClientOptions,
    attempt: u32,
    retry_after_secs: Option<u64>,
) -> Duration {
    if let Some(secs) = retry_after_secs {
        Duration::from_secs(secs)
    } else {
        let base_ms = opts.retry_initial_backoff_ms;
        let max_ms = opts.retry_max_backoff_ms;
        let backoff_ms = base_ms.saturating_mul(2u64.saturating_pow(attempt.saturating_sub(1)));
        Duration::from_millis(backoff_ms.min(max_ms))
    }
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
    fn parse_retry_after_valid() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("5"));
        assert_eq!(parse_retry_after(&headers), Some(5));
    }

    #[test]
    fn parse_retry_after_missing() {
        let headers = HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);
    }

    #[test]
    fn parse_retry_after_non_numeric() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("not-a-number"));
        assert_eq!(parse_retry_after(&headers), None);
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
}
