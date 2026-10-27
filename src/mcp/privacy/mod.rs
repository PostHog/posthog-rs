//! Redaction of credentials and personal data from captured strings, ported
//! from `@posthog/core` and PostHog's other MCP SDKs. Each pass runs where it
//! protects the next: credentials before PII, which would read the middle of
//! a token as a phone number, and PII before URLs, whose rewrite
//! percent-encodes the `@` the email pattern needs.

mod binary;
mod json_members;
mod keys;
mod patterns;
mod pii;
mod secrets;
mod tokens;
mod urls;

pub(crate) use binary::{is_binary, BINARY_PLACEHOLDER};
pub(crate) use keys::is_credential_key;

/// Which redactions a captured string gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextPass {
    /// Tool and resource names, and transport headers: the entropy detector
    /// would read names like `Get_Organization_Memberships` as credentials.
    Name,
    /// Payloads, error messages, and other text a tool or server produced.
    Captured,
    /// Text the agent narrated, such as the intent, which can also carry
    /// personal data.
    FreeText,
}

impl TextPass {
    pub(crate) fn apply(self, text: &str) -> String {
        match self {
            TextPass::Name => urls::redact_urls(&tokens::redact_tokens(text)),
            TextPass::Captured => urls::redact_urls(&redact_credentials(text)),
            TextPass::FreeText => urls::redact_urls(&pii::redact_pii(&redact_credentials(text))),
        }
    }
}

fn redact_credentials(text: &str) -> String {
    let text = json_members::redact_json_members(text);
    let text = tokens::redact_tokens(&text);
    secrets::redact_secret_words(&text)
}

#[cfg(test)]
mod tests;
