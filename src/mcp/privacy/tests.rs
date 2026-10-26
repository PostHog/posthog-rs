//! Replays outputs of `@posthog/core`, which the URL and PII passes are ported
//! from, so the port is checked against that implementation rather than
//! against a reading of it.

use serde::Deserialize;

use super::*;

/// Each case is `[input, captured, free_text]`.
#[derive(Deserialize)]
struct Golden {
    input: String,
    captured: String,
    free_text: String,
}

fn goldens() -> Vec<Golden> {
    let fixture = include_str!("testdata/core-redaction-goldens.json");
    serde_json::from_str(fixture).expect("golden fixture parses")
}

fn core_free_text(text: &str) -> String {
    urls::redact_urls(&pii::redact_pii(&tokens::redact_tokens(text)))
}

fn mismatches(expected: impl Fn(&Golden) -> &str, actual: impl Fn(&str) -> String) -> Vec<String> {
    goldens()
        .iter()
        .filter_map(|golden| {
            let produced = actual(&golden.input);
            let wanted = expected(golden);
            (produced != wanted).then(|| {
                format!(
                    "input:    {}\nexpected: {wanted}\nactual:   {produced}",
                    golden.input
                )
            })
        })
        .collect()
}

#[test]
fn name_pass_matches_core_captured_values() {
    let failures = mismatches(
        |golden| &golden.captured,
        |input| TextPass::Name.apply(input),
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn pii_and_url_passes_match_core_free_text() {
    let failures = mismatches(|golden| &golden.free_text, core_free_text);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn free_text_redacts_personal_data_and_credentials() {
    let text = "email alice@example.com about key sk-live-abcdefghijklmnopqrstu";
    assert_eq!(
        TextPass::FreeText.apply(text),
        "email [redacted] about key [redacted]"
    );
}

#[test]
fn captured_text_keeps_personal_data_but_not_credentials() {
    let text = "GET https://svc:hunter2@internal.test/doc?token=abc failed for alice@example.com";
    let redacted = TextPass::Captured.apply(text);
    assert!(!redacted.contains("hunter2"));
    assert!(!redacted.contains("token=abc"));
    assert!(redacted.contains("alice@example.com"));
}
