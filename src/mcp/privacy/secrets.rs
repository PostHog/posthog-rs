//! Credential-shaped words: known vendor key formats, PEM private keys, and
//! long high-entropy tokens. Ported from posthog-python's `_looks_like_secret`,
//! tuned so ids, digests, paths and prose stay readable.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::mcp::contract::REDACTED;

const MIN_LENGTH: usize = 16;
const MIN_ENTROPY_BITS: f64 = 3.8;
const MIN_CHARACTER_CLASSES: usize = 3;
const KNOWN_FORMAT_MAX_LENGTH: usize = 200;
const PEM_PRIVATE_KEY_MARKER: &str = "PRIVATE KEY-----";
const STRUCTURE_CHARACTERS: &str = "()[]{}<>'\"`,;";
const ENCODED_REDACTED: &str = "%5Bredacted%5D";

static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")
        .expect("uuid pattern")
});

static PATH_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z.]*$").expect("path word pattern"));

static KNOWN_FORMAT: LazyLock<Regex> = LazyLock::new(|| {
    let formats = [
        r"sk-ant-[A-Za-z0-9_-]{16,}",
        r"sk-(?:proj-)?[A-Za-z0-9_-]{20,}",
        r"hf_[A-Za-z0-9]{34}",
        r"AKIA[0-9A-Z]{16}",
        r"(?:ASIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ABIA|ACCA)[0-9A-Z]{16}",
        r"AIza[A-Za-z0-9_-]{35}",
        r"ya29\.[A-Za-z0-9_-]{20,}",
        r"do[opr]_v1_[a-f0-9]{64}",
        r"(?:sk|pk|rk)_(?:live|test)_[A-Za-z0-9]{16,}",
        r"sq0[a-z]{3}-[A-Za-z0-9_-]{22,43}",
        r"gh[pousr]_[A-Za-z0-9]{36}",
        r"github_pat_[A-Za-z0-9_]{20,}",
        r"gl(?:pat|ptt|rt|soat)-[A-Za-z0-9_-]{20}",
        r"glsa_[A-Za-z0-9]{32}_[A-Fa-f0-9]{8}",
        r"xox[abeoprs]-[A-Za-z0-9-]{10,}",
        r"xapp-[0-9]-[A-Za-z0-9-]{10,}",
        r"SK[0-9a-fA-F]{32}",
        r"SG\.[A-Za-z0-9_-]{22}\.[A-Za-z0-9_-]{43}",
        r"key-[0-9a-f]{32}",
        r"[0-9a-f]{32}-us[0-9]{1,2}",
        r"npm_[A-Za-z0-9]{36}",
        r"pypi-AgEI[A-Za-z0-9_-]{50,}",
        r"dapi[0-9a-f]{32}",
        r"dp\.pt\.[A-Za-z0-9]{40,}",
        r"PMAK-[a-f0-9]{24}-[a-f0-9]{34}",
        r"lin_api_[A-Za-z0-9]{40}",
        r"ntn_[A-Za-z0-9]{40,}",
        r"shp(?:at|ca|pa|ss)_[a-fA-F0-9]{32}",
        r"NR(?:AK|JS|II|MA|RA)-[A-Za-z0-9]{27}",
        r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{6,}",
    ];
    Regex::new(&formats.join("|")).expect("known secret pattern")
});

#[derive(Default)]
struct CharacterClasses {
    lower: bool,
    upper: bool,
    digit: bool,
    symbol: bool,
    hex_only: bool,
}

impl CharacterClasses {
    fn count(&self) -> usize {
        [self.lower, self.upper, self.digit, self.symbol]
            .iter()
            .filter(|present| **present)
            .count()
    }
}

/// Redacts each space-separated word that reads as a credential, so the
/// surrounding prose stays readable.
pub(super) fn redact_secret_words(text: &str) -> String {
    redact_words(text, |word| is_known_secret(word) || is_high_entropy(word))
}

/// Redacts only known key formats, for text whose ids
/// and names the entropy check would mistake for credentials.
pub(super) fn redact_known_secrets(text: &str) -> String {
    redact_words(text, is_known_secret)
}

fn redact_words(text: &str, is_secret: impl Fn(&str) -> bool) -> String {
    let mut changed = false;
    let words: Vec<&str> = text
        .split(' ')
        .map(|word| {
            if is_secret_word(word, &is_secret) {
                changed = true;
                REDACTED
            } else {
                word
            }
        })
        .collect();
    if changed {
        words.join(" ")
    } else {
        text.to_string()
    }
}

/// Ignores this module's own markers, which a value sanitized twice carries.
fn is_secret_word(word: &str, is_secret: impl Fn(&str) -> bool) -> bool {
    let unmarked = word.replace(ENCODED_REDACTED, "").replace(REDACTED, "");
    !unmarked.is_empty() && is_secret(&unmarked)
}

fn is_known_secret(word: &str) -> bool {
    let length = word.chars().count();
    word.contains(PEM_PRIVATE_KEY_MARKER)
        || ((MIN_LENGTH..=KNOWN_FORMAT_MAX_LENGTH).contains(&length) && KNOWN_FORMAT.is_match(word))
}

fn is_high_entropy(word: &str) -> bool {
    let length = word.chars().count();
    if length < MIN_LENGTH
        || word.contains(' ')
        || looks_like_path_or_url(word)
        || UUID.is_match(word)
    {
        return false;
    }
    let Some(classes) = character_classes(word) else {
        return false;
    };
    if classes.hex_only || classes.count() < MIN_CHARACTER_CLASSES {
        return false;
    }
    shannon_entropy(word, length) >= MIN_ENTROPY_BITS
}

fn character_classes(word: &str) -> Option<CharacterClasses> {
    let mut classes = CharacterClasses {
        hex_only: true,
        ..CharacterClasses::default()
    };
    for character in word.chars() {
        if STRUCTURE_CHARACTERS.contains(character) || is_python_space(character) {
            return None;
        }
        if character.is_lowercase() {
            classes.lower = true;
            classes.hex_only &= ('a'..='f').contains(&character);
        } else if character.is_uppercase() {
            classes.upper = true;
            classes.hex_only &= ('A'..='F').contains(&character);
        } else if character.is_numeric() {
            classes.digit = true;
        } else {
            classes.symbol = true;
            classes.hex_only = false;
        }
    }
    Some(classes)
}

fn shannon_entropy(word: &str, length: usize) -> f64 {
    let mut counts: HashMap<char, usize> = HashMap::new();
    for character in word.chars() {
        *counts.entry(character).or_insert(0) += 1;
    }
    counts
        .values()
        .map(|count| {
            let probability = *count as f64 / length as f64;
            -probability * probability.log2()
        })
        .sum()
}

fn looks_like_path_or_url(word: &str) -> bool {
    if word.contains("://") || word.contains('\\') {
        return true;
    }
    let path_words = word
        .split('/')
        .filter(|segment| !segment.is_empty() && PATH_WORD.is_match(segment))
        .count();
    path_words >= 2
}

/// Python's `str.isspace`, which also counts the ASCII separators U+001C..U+001F.
fn is_python_space(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_formats_and_random_tokens_are_redacted() {
        let text = "key ghp_abcdefghijklmnopqrstuvwxyz0123456789 and Zx9-Qw7_Lm3.Rt8+Yp2!Kd5 here";
        assert_eq!(
            redact_secret_words(text),
            "key [redacted] and [redacted] here"
        );
    }

    #[test]
    fn known_secrets_alone_leave_random_tokens() {
        let cases = [
            (
                "github said 401 for ghp_abcdefghijklmnopqrstuvwxyz0123456789",
                "github said 401 for [redacted]",
            ),
            ("/v1/charges/sk_live_abcdefghijklmnop1234", "[redacted]"),
            ("key AKIAABCDEFGHIJKLMNOP", "key [redacted]"),
            (
                "tenant Zx9-Qw7_Lm3.Rt8+Yp2!Kd5",
                "tenant Zx9-Qw7_Lm3.Rt8+Yp2!Kd5",
            ),
        ];
        for (text, redacted) in cases {
            assert_eq!(redact_known_secrets(text), redacted, "{text}");
        }
    }

    #[test]
    fn identifiers_paths_and_prose_stay_readable() {
        let kept = [
            "0190f0e8-7a6b-7c3d-9e4f-5a6b7c8d9e0f",
            "d41d8cd98f00b204e9800998ecf8427e",
            "/usr/local/share/applications",
            "search_docs_by_title",
            "an ordinary sentence with words",
        ];
        for text in kept {
            assert_eq!(redact_secret_words(text), text);
        }
    }
}
