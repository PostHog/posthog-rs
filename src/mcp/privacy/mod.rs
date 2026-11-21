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
    /// Tool and resource names, transport headers, and the host's request
    /// context: the entropy detector would read names like
    /// `Get_Organization_Memberships` as credentials, so only known key
    /// formats are redacted.
    Name,
    /// Payloads, error messages, and other text a tool or server produced.
    Captured,
    /// Text the agent narrated, such as the intent, which can also carry
    /// personal data.
    FreeText,
    /// Object keys inside free text: the name passes and personal data.
    FreeName,
}

impl TextPass {
    pub(crate) fn apply(self, text: &str) -> String {
        match self {
            TextPass::Name => urls::redact_urls(&redact_name_credentials(text)),
            TextPass::Captured => urls::redact_urls(&redact_credentials(text)),
            TextPass::FreeText => urls::redact_urls(&pii::redact_pii(&redact_credentials(text))),
            TextPass::FreeName => {
                urls::redact_urls(&pii::redact_pii(&redact_name_credentials(text)))
            }
        }
    }

    /// The pass for object keys in a payload this pass redacts: the entropy
    /// check would redact field names.
    pub(crate) fn for_keys(self) -> TextPass {
        match self {
            TextPass::FreeText | TextPass::FreeName => TextPass::FreeName,
            TextPass::Name | TextPass::Captured => TextPass::Name,
        }
    }

    /// Whether numbers are read as text, as free text sent as JSON text is:
    /// a number can hold a card or phone number.
    pub(crate) fn reads_numbers(self) -> bool {
        matches!(self, TextPass::FreeText | TextPass::FreeName)
    }
}

fn redact_name_credentials(text: &str) -> String {
    let text = json_members::redact_json_members(text);
    secrets::redact_known_secrets(&tokens::redact_tokens(&text))
}

fn redact_credentials(text: &str) -> String {
    let text = json_members::redact_json_members(text);
    let text = tokens::redact_tokens(&text);
    secrets::redact_secret_words(&text)
}

#[cfg(test)]
mod tests;
