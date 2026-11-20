//! String limits. Redaction only scans what a limit can keep, plus a margin so
//! a match that crosses the limit is still recognized whole.

use crate::mcp::contract::limits::{REDACTION_SCAN_MARGIN, TRUNCATION_MARKER};
use crate::mcp::privacy::{is_binary, TextPass, BINARY_PLACEHOLDER};

/// Keeps the start of `text`, marker included, within `limit` characters.
pub(crate) fn truncate(text: &str, limit: usize) -> String {
    if !exceeds(text, limit) {
        return text.to_string();
    }
    mark_cut(text, limit)
}

fn mark_cut(text: &str, limit: usize) -> String {
    let marker_length = TRUNCATION_MARKER.len();
    if limit < marker_length {
        return TRUNCATION_MARKER[..limit].to_string();
    }
    format!("{}{TRUNCATION_MARKER}", prefix(text, limit - marker_length))
}

/// Redacts `text` with `pass` at a cost bounded by `limit`, then cuts it to
/// `limit`. When the scan cut removed text, only the common prefix with the
/// redacted first `limit` characters is kept, marked as cut: each result holds
/// raw the match its own cut split, and the other redacts or omits it.
pub(crate) fn bounded_text(text: &str, limit: usize, pass: TextPass) -> String {
    if is_binary(text) {
        return truncate(BINARY_PLACEHOLDER, limit);
    }
    let scanned = prefix(text, limit + REDACTION_SCAN_MARGIN);
    let redacted = pass.apply(scanned);
    if scanned.len() == text.len() {
        return truncate(&redacted, limit);
    }
    let redacted_head = pass.apply(prefix(text, limit));
    let kept = common_prefix(&redacted, &redacted_head);
    if kept.is_empty() {
        return kept;
    }
    mark_cut(&kept, limit)
}

fn common_prefix(left: &str, right: &str) -> String {
    left.chars()
        .zip(right.chars())
        .take_while(|(a, b)| a == b)
        .map(|(character, _)| character)
        .collect()
}

/// Like [`bounded_text`], for a value that must not be blank: `None` when
/// nothing survives redaction and the margin cut.
pub(crate) fn bounded_non_empty(text: &str, limit: usize, pass: TextPass) -> Option<String> {
    let bounded = bounded_text(text, limit, pass);
    (!bounded.trim().is_empty()).then_some(bounded)
}

fn exceeds(text: &str, limit: usize) -> bool {
    text.char_indices().nth(limit).is_some()
}

fn prefix(text: &str, characters: usize) -> &str {
    match text.char_indices().nth(characters) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_counts_characters_and_includes_the_marker() {
        assert_eq!(truncate("héllo wörld", 8), "héllo...");
        assert_eq!(truncate("short", 8), "short");
    }

    #[test]
    fn redaction_is_bounded_by_the_limit() {
        let text = format!("{} alice@example.com", "a ".repeat(5_000));
        let bounded = bounded_text(&text, 2_048, TextPass::FreeText);
        assert_eq!(bounded.chars().count(), 2_048);
        assert!(bounded.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn a_match_split_by_the_scan_cut_never_slides_under_the_limit() {
        let shrinking_url = format!("https://example.com/?token={} ", "t".repeat(1_500));
        let email_start = 2_048 + REDACTION_SCAN_MARGIN - 8;
        let filler = "y".repeat(email_start - shrinking_url.chars().count() - 1);
        let text = format!("{shrinking_url}{filler} alice@example.com");
        let bounded = bounded_text(&text, 2_048, TextPass::FreeText);
        assert!(!bounded.contains("alice"));
        assert!(!bounded.contains("ttt"));
    }

    #[test]
    fn a_redaction_inside_the_margin_keeps_the_start() {
        let link = format!(
            "https://acme.s3.amazonaws.com/q3.pdf?token={}&X-Amz-Signature=abc123",
            "A".repeat(1_300)
        );
        assert_eq!(
            bounded_text(&link, 256, TextPass::Name),
            "https://acme.s3.amazonaws.com/q3.pdf?token=%5Bredacted%5D..."
        );
    }

    #[test]
    fn binary_strings_become_a_placeholder() {
        let blob = "QUJD".repeat(3_000);
        assert_eq!(
            bounded_text(&blob, 32_768, TextPass::Captured),
            BINARY_PLACEHOLDER
        );
    }
}
