use std::sync::LazyLock;

use regex::Regex;

use crate::mcp::contract::REDACTED;

static POSTHOG_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)ph[a-z]_[A-Za-z0-9_-]{20,}(?-u:\b)").expect("posthog token pattern")
});

pub(super) fn redact_tokens(text: &str) -> String {
    POSTHOG_TOKEN.replace_all(text, REDACTED).into_owned()
}
