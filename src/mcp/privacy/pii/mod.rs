//! Structured PII in agent-written text. Each pass runs in `@posthog/core`'s
//! order, so an earlier pass never eats digits a later one needs.

mod cards;

use std::sync::LazyLock;

use fancy_regex::Regex as LookaroundRegex;
use regex::Regex;

use crate::mcp::contract::REDACTED;

const WORD: &str = "A-Za-z0-9_";

static UNICODE_SPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\x{A0}\x{1680}\x{2000}-\x{200A}\x{202F}\x{205F}\x{3000}]").expect("space pattern")
});

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]{1,255}\.[A-Za-z]{2,24}")
        .expect("email pattern")
});

static IPV4: LazyLock<Regex> = LazyLock::new(|| {
    let octet = r"(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])";
    Regex::new(&format!(r"(?-u:\b)(?:{octet}\.){{3}}{octet}(?-u:\b)")).expect("ipv4 pattern")
});

static IPV6: LazyLock<LookaroundRegex> = LazyLock::new(|| {
    let group = "[0-9A-Fa-f]{1,4}";
    let full = format!("(?:{group}:){{7}}{group}");
    let trailing_gap = format!("(?:{group}:){{1,7}}:");
    let middle_gap = format!("(?:{group}:){{1,6}}:{group}(?::{group}){{0,5}}");
    let leading_gap = format!("::(?:{group}(?::{group}){{0,6}})");
    let address = format!("(?:{full}|{trailing_gap}|{middle_gap}|{leading_gap})");
    LookaroundRegex::new(&format!("(^|[^{WORD}:])({address})(?=$|[^{WORD}:])"))
        .expect("ipv6 pattern")
});

static US_SSN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)[0-9]{3}[ .-][0-9]{2}[ .-][0-9]{4}(?-u:\b)").expect("ssn pattern")
});

static PHONE_NORTH_AMERICA: LazyLock<LookaroundRegex> = LazyLock::new(|| {
    let area = r"(?:\([0-9]{3}\)[ ./-]?|[0-9]{3}[ ./-])";
    let number = format!(r"(?:\+?1[ ./-]?)?{area}[0-9]{{3}}[ ./-][0-9]{{4}}");
    LookaroundRegex::new(&format!(r"(^|[^{WORD}+])({number})(?![{WORD}])")).expect("phone pattern")
});

static PHONE_INTERNATIONAL: LazyLock<LookaroundRegex> = LazyLock::new(|| {
    let number = r"\+[0-9]{1,3}(?:[ ./()-]{0,2}[0-9]){7,13}";
    LookaroundRegex::new(&format!(r"(^|[^{WORD}])({number})(?![{WORD}])")).expect("phone pattern")
});

pub(super) fn redact_pii(text: &str) -> String {
    let text = UNICODE_SPACE.replace_all(text, " ");
    let text = EMAIL.replace_all(&text, REDACTED);
    let text = IPV4.replace_all(&text, REDACTED);
    let text = replace_after_prefix(&IPV6, &text);
    let text = cards::redact_cards(&text);
    let text = US_SSN.replace_all(&text, REDACTED);
    let text = replace_after_prefix(&PHONE_NORTH_AMERICA, &text);
    replace_after_prefix(&PHONE_INTERNATIONAL, &text)
}

/// A pattern that exhausts its backtracking budget redacts the whole text,
/// failing closed rather than panicking on the request path.
fn replace_after_prefix(pattern: &LookaroundRegex, text: &str) -> String {
    let replacement = format!("${{1}}{REDACTED}");
    match pattern.try_replacen(text, 0, replacement.as_str()) {
        Ok(redacted) => redacted.into_owned(),
        Err(_) => REDACTED.to_string(),
    }
}
