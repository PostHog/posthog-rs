//! Fragments: a head and an optional `?`-separated tail, each read as fields
//! when it holds an `=` and as text otherwise.

use url::Url;

use super::fields::{sanitize_fields, too_many_fields as too_many_list_fields};
use super::{holds_url, sanitize_urls_in, Mode};
use crate::mcp::contract::REDACTED;

pub(super) struct SanitizedPart {
    pub value: String,
    pub last_field_sensitive: bool,
}

pub(super) fn too_many_fields(fragment: &str) -> bool {
    let (head, tail) = split(fragment);
    too_many_list_fields(head) || too_many_list_fields(tail.unwrap_or_default())
}

pub(super) fn sanitize_fragment(fragment: &str, nested_urls: bool) -> SanitizedPart {
    let (head, tail) = split(fragment);
    let head = sanitize_part(head, nested_urls);
    let Some(tail) = tail else {
        return head;
    };
    let tail = sanitize_tail(tail, head.last_field_sensitive, nested_urls);
    SanitizedPart {
        value: format!("{}?{}", head.value, tail.value),
        last_field_sensitive: tail.last_field_sensitive,
    }
}

pub(super) fn set_fragment(url: &mut Url, value: &str) {
    let value = value.strip_prefix('#').unwrap_or(value);
    url.set_fragment((!value.is_empty()).then_some(value));
}

fn split(fragment: &str) -> (&str, Option<&str>) {
    match fragment.split_once('?') {
        Some((head, tail)) => (head, Some(tail)),
        None => (fragment, None),
    }
}

/// A `?` inside a credential looks like the head/tail separator, so after a
/// credential the whole tail may be the rest of its value.
fn sanitize_tail(tail: &str, after_credential: bool, nested_urls: bool) -> SanitizedPart {
    if !after_credential {
        return sanitize_part(tail, nested_urls);
    }
    let value = if tail.is_empty() { "" } else { REDACTED };
    SanitizedPart {
        value: value.to_string(),
        last_field_sensitive: true,
    }
}

fn sanitize_part(part: &str, nested_urls: bool) -> SanitizedPart {
    if !part.contains('=') {
        return SanitizedPart {
            value: sanitize_text(part, nested_urls),
            last_field_sensitive: false,
        };
    }
    let fields = sanitize_fields(part, nested_urls);
    let value = if fields.changed {
        fields.serialized
    } else {
        part.to_string()
    };
    SanitizedPart {
        value,
        last_field_sensitive: fields.last_field_sensitive,
    }
}

fn sanitize_text(text: &str, nested_urls: bool) -> String {
    match (nested_urls, holds_url(text)) {
        (true, _) => sanitize_urls_in(text, Mode::NESTED),
        (false, true) => REDACTED.to_string(),
        (false, false) => text.to_string(),
    }
}
