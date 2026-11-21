//! Structured PII in agent-written text. Each pass runs in `@posthog/core`'s
//! order, so an earlier pass never eats digits a later one needs.

mod cards;

use std::sync::LazyLock;

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

static IPV6: LazyLock<Regex> = LazyLock::new(|| {
    let group = "[0-9A-Fa-f]{1,4}";
    let full = format!("(?:{group}:){{7}}{group}");
    let trailing_gap = format!("(?:{group}:){{1,7}}:");
    let middle_gap = format!("(?:{group}:){{1,6}}:{group}(?::{group}){{0,5}}");
    let leading_gap = format!("::(?:{group}(?::{group}){{0,6}})");
    let address = format!("(?:{full}|{trailing_gap}|{middle_gap}|{leading_gap})");
    Regex::new(&format!("(^|[^{WORD}:])({address})(?:$|[^{WORD}:])")).expect("ipv6 pattern")
});

static US_SSN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)[0-9]{3}[ .-][0-9]{2}[ .-][0-9]{4}(?-u:\b)").expect("ssn pattern")
});

static PHONE_NORTH_AMERICA: LazyLock<Regex> = LazyLock::new(|| {
    let area = r"(?:\([0-9]{3}\)[ ./-]?|[0-9]{3}[ ./-])";
    let number = format!(r"(?:\+?1[ ./-]?)?{area}[0-9]{{3}}[ ./-][0-9]{{4}}");
    Regex::new(&format!(r"(^|[^{WORD}+])({number})(?:$|[^{WORD}])")).expect("phone pattern")
});

static PHONE_INTERNATIONAL: LazyLock<Regex> = LazyLock::new(|| {
    let number = r"\+[0-9]{1,3}(?:[ ./()-]{0,2}[0-9]){7,13}";
    Regex::new(&format!(r"(^|[^{WORD}])({number})(?:$|[^{WORD}])")).expect("phone pattern")
});

pub(super) fn redact_pii(text: &str) -> String {
    let text = UNICODE_SPACE.replace_all(text, " ");
    let text = EMAIL.replace_all(&text, REDACTED);
    let text = IPV4.replace_all(&text, REDACTED);
    let text = redact_bounded(&IPV6, &text);
    let text = cards::redact_cards(&text);
    let text = US_SSN.replace_all(&text, REDACTED);
    let text = redact_bounded(&PHONE_NORTH_AMERICA, &text);
    redact_bounded(&PHONE_INTERNATIONAL, &text)
}

/// Redacts each match's second group. The boundary after it is matched but
/// not redacted, and the next search starts on it, so it can be the boundary
/// before the next match too, as a lookahead would allow.
fn redact_bounded(pattern: &Regex, text: &str) -> String {
    let mut redacted = String::with_capacity(text.len());
    let mut copied = 0;
    while let Some(found) = pattern.captures_at(text, copied) {
        let matched = found.get(2).expect("pattern has a second group");
        redacted.push_str(&text[copied..matched.start()]);
        redacted.push_str(REDACTED);
        copied = matched.end();
    }
    redacted.push_str(&text[copied..]);
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_character_bounds_the_match_before_it_and_the_one_after() {
        let cases = [
            ("415-555-0142 415-555-0142", "[redacted] [redacted]"),
            ("+44 20 7946 0958,+44 20 7946 0958", "[redacted],[redacted]"),
            ("::1 ::2", "[redacted] [redacted]"),
            ("+12345678 901234567890123", "[redacted] 901234567890123"),
            ("415-555-0142x 415-555-0142", "415-555-0142x [redacted]"),
        ];
        for (text, expected) in cases {
            assert_eq!(redact_pii(text), expected, "{text}");
        }
    }
}
