//! Credentials inside URLs: userinfo, and the values of credential-named query
//! and fragment fields. A URL inside a kept field value is checked one level deep.

mod embedded;
mod fields;
mod fragments;

use url::Url;

use super::patterns::{AUTHORITY_AT_START, URL};
use super::tokens::redact_tokens;
use crate::mcp::contract::REDACTED;

const TRAILING_PUNCTUATION: &str = ".,;:!?)]}'";
const MAX_URL_LENGTH: usize = 8192;

#[derive(Clone, Copy)]
pub(super) struct Mode {
    pub nested_urls: bool,
    pub strip_punctuation: bool,
}

impl Mode {
    pub(super) const NESTED: Mode = Mode {
        nested_urls: false,
        strip_punctuation: true,
    };
}

pub(super) fn redact_urls(text: &str) -> String {
    let top_level = Mode {
        nested_urls: true,
        strip_punctuation: true,
    };
    sanitize_urls_in(text, top_level)
}

pub(super) fn sanitize_urls_in(text: &str, mode: Mode) -> String {
    URL.replace_all(text, |captures: &regex::Captures<'_>| {
        let address = &captures[0];
        let is_whole_text = address.len() == text.len();
        let strip_punctuation = mode.strip_punctuation && !is_whole_text;
        sanitize_match(
            address,
            Mode {
                strip_punctuation,
                ..mode
            },
        )
    })
    .into_owned()
}

pub(super) fn holds_url(text: &str) -> bool {
    URL.is_match(text)
}

fn sanitize_match(text: &str, mode: Mode) -> String {
    let starts = embedded::address_starts(text);
    if starts.is_empty() {
        return sanitize_address(text, mode);
    }
    let inner_mode = Mode {
        strip_punctuation: false,
        ..mode
    };
    let mut result = String::new();
    let mut start = 0;
    for next in starts {
        result += &sanitize_address(&text[start..next], inner_mode);
        start = next;
    }
    result + &sanitize_address(&text[start..], mode)
}

fn sanitize_address(text: &str, mode: Mode) -> String {
    if exceeds_url_length(text) && AUTHORITY_AT_START.is_match(text) {
        return REDACTED.to_string();
    }
    let (address, suffix) = match mode.strip_punctuation {
        true => split_trailing_punctuation(text),
        false => (text, ""),
    };
    match Url::parse(address) {
        Ok(url) => rewrite_url(url, address, suffix, mode.nested_urls),
        Err(_) => format!("{REDACTED}{suffix}"),
    }
}

fn exceeds_url_length(text: &str) -> bool {
    text.chars().nth(MAX_URL_LENGTH).is_some()
}

fn split_trailing_punctuation(text: &str) -> (&str, &str) {
    let address = text.trim_end_matches(|c| TRAILING_PUNCTUATION.contains(c));
    (address, &text[address.len()..])
}

fn rewrite_url(mut url: Url, address: &str, suffix: &str, nested_urls: bool) -> String {
    let query = url.query().unwrap_or_default().to_string();
    let fragment = url.fragment().unwrap_or_default().to_string();
    if fields::too_many_fields(&query) || fragments::too_many_fields(&fragment) {
        return format!("{REDACTED}{suffix}");
    }

    let userinfo_changed = redact_userinfo(&mut url);
    let query_fields = fields::sanitize_fields(&query, nested_urls);
    if query_fields.changed {
        let serialized = query_fields.serialized.as_str();
        url.set_query((!serialized.is_empty()).then_some(serialized));
    }
    let new_fragment = fragments::sanitize_fragment(&fragment, nested_urls);
    if new_fragment.value != fragment {
        fragments::set_fragment(&mut url, &new_fragment.value);
    }

    let changed = userinfo_changed || query_fields.changed || new_fragment.value != fragment;
    let last_field_sensitive = match fragment.is_empty() {
        true => query_fields.last_field_sensitive,
        false => new_fragment.last_field_sensitive,
    };
    let body = if changed {
        redact_tokens(url.as_str())
    } else {
        address.to_string()
    };
    let kept_suffix = if last_field_sensitive { "" } else { suffix };
    body + kept_suffix
}

fn redact_userinfo(url: &mut Url) -> bool {
    let has_password = url.password().is_some_and(|password| !password.is_empty());
    if url.username().is_empty() && !has_password {
        return false;
    }
    let _ = url.set_username(REDACTED);
    let _ = url.set_password(None);
    true
}
