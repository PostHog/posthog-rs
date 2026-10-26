//! Large base64 payloads are replaced whole. Only their edges are read, so the
//! check costs the same for any size.

const MIN_LENGTH: usize = 10_240;
const EDGE_LENGTH: usize = 1_024;
const DATA_URL_SCHEME: &str = "data:";
const DATA_URL_BASE64_SUFFIX: &str = ";base64";

pub(crate) const BINARY_PLACEHOLDER: &str =
    "[binary data redacted - not supported by PostHog MCP analytics]";

pub(crate) fn is_binary(text: &str) -> bool {
    let long_enough = text.chars().nth(MIN_LENGTH - 1).is_some();
    long_enough && (is_base64_data_url(text) || edges_are_base64(text))
}

fn edges_are_base64(text: &str) -> bool {
    let unpadded = text.trim_end_matches('=');
    let head = unpadded.chars().take(EDGE_LENGTH);
    let tail = unpadded.chars().rev().take(EDGE_LENGTH);
    head.chain(tail).all(is_base64_character)
}

fn is_base64_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '+' | '/' | '-' | '_' | '\r' | '\n')
}

fn is_base64_data_url(text: &str) -> bool {
    let head: String = text.chars().take(EDGE_LENGTH).collect();
    let lowered = head.to_ascii_lowercase();
    let Some(header) = lowered.strip_prefix(DATA_URL_SCHEME) else {
        return false;
    };
    header
        .split_once(',')
        .is_some_and(|(media_type, _)| media_type.ends_with(DATA_URL_BASE64_SUFFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_base64_is_binary() {
        let blob = format!("{}==", "QUJD".repeat(3_000));
        assert!(is_binary(&blob));
    }

    #[test]
    fn data_urls_with_a_base64_header_are_binary() {
        let url = format!("data:image/png;base64,{}", "iVBO".repeat(3_000));
        assert!(is_binary(&url));
    }

    #[test]
    fn short_or_prose_strings_are_not_binary() {
        assert!(!is_binary("QUJD"));
        let prose = "lorem ipsum ".repeat(1_000);
        assert!(!is_binary(&prose));
    }
}
