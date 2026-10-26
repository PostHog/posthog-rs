//! URL patterns with JavaScript's whitespace spelled out, so URLs end where
//! they do in `@posthog/core`: Rust's `\s` is Unicode-aware, JavaScript's adds
//! U+FEFF and omits U+0085.

use std::sync::LazyLock;

use regex::Regex;

const JS_SPACE: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

const SCHEME: &str = r"[A-Za-z][A-Za-z0-9+.-]{0,63}";

pub(super) static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r#"{SCHEME}:[^{JS_SPACE}<>"]+"#)).expect("url pattern"));

pub(super) static AUTHORITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("{SCHEME}://")).expect("authority pattern"));

pub(super) static AUTHORITY_AT_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^{SCHEME}://")).expect("leading authority pattern"));
