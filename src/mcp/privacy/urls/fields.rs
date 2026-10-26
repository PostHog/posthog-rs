//! `&`-separated field lists: a query string, or a fragment's fields.

use url::form_urlencoded;

use super::super::keys::is_credential_field;
use super::super::tokens::redact_tokens;
use super::{holds_url, sanitize_urls_in, Mode};
use crate::mcp::contract::REDACTED;

const MAX_FIELDS: usize = 128;
const LEGACY_SEPARATOR: char = ';';

pub(super) struct SanitizedFields {
    pub changed: bool,
    pub last_field_sensitive: bool,
    pub serialized: String,
}

pub(super) fn too_many_fields(fields: &str) -> bool {
    fields.split('&').take(MAX_FIELDS + 1).count() > MAX_FIELDS
}

pub(super) fn sanitize_fields(fields: &str, nested_urls: bool) -> SanitizedFields {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    let mut changed = false;
    let mut last_field_sensitive = false;
    for (decoded_name, decoded_value) in form_urlencoded::parse(fields.as_bytes()) {
        let name = redact_tokens(&decoded_name);
        let value = redact_tokens(&decoded_value);
        let sanitized = sanitize_value(&name, &value, nested_urls);
        changed |= name != decoded_name || value != decoded_value || sanitized != value;
        last_field_sensitive = is_credential_field(&name) || sanitized != value;
        serializer.append_pair(&name, &sanitized);
    }
    SanitizedFields {
        changed,
        last_field_sensitive,
        serialized: serializer.finish(),
    }
}

fn sanitize_value(name: &str, value: &str, nested_urls: bool) -> String {
    if is_credential_field(name) || holds_legacy_credential(value) {
        return REDACTED.to_string();
    }
    if !holds_url(value) {
        return value.to_string();
    }
    match nested_urls {
        true => sanitize_urls_in(value, Mode::NESTED),
        false => REDACTED.to_string(),
    }
}

fn holds_legacy_credential(value: &str) -> bool {
    value.contains(LEGACY_SEPARATOR)
        && value.split(LEGACY_SEPARATOR).any(|piece| {
            let name = piece.split('=').next().unwrap_or_default();
            is_credential_field(name)
        })
}
