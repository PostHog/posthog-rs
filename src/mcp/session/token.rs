//! PostHog session tokens: the unpadded base64url of `{"sid", "cn", "cv",
//! "pv"}` that a stateless server hands out as `Mcp-Session-Id` and the client
//! replays, minted by the TypeScript and Python SDKs.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;

const MAX_TOKEN_LENGTH: usize = 4_096;
const MAX_SESSION_ID_LENGTH: usize = 128;

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
pub(crate) struct SessionToken {
    pub(crate) sid: String,
    #[serde(default, deserialize_with = "optional_text")]
    pub(crate) cn: Option<String>,
    #[serde(default, deserialize_with = "optional_text")]
    pub(crate) cv: Option<String>,
    #[serde(default, deserialize_with = "optional_text")]
    pub(crate) pv: Option<String>,
}

/// `None` for anything that is not a token: transport UUIDs, JWTs, garbage.
pub(crate) fn decode(value: &str) -> Option<SessionToken> {
    if value.is_empty() || value.len() > MAX_TOKEN_LENGTH {
        return None;
    }
    let unpadded = value.trim_end_matches('=');
    let bytes = URL_SAFE_NO_PAD.decode(unpadded).ok()?;
    let decoded: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    if !decoded.is_object() {
        return None;
    }
    let token: SessionToken = serde_json::from_value(decoded).ok()?;
    let valid_sid = !token.sid.is_empty() && token.sid.chars().count() <= MAX_SESSION_ID_LENGTH;
    valid_sid.then_some(token)
}

fn optional_text<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(json: &str) -> String {
        URL_SAFE_NO_PAD.encode(json)
    }

    #[test]
    fn tokens_decode_with_or_without_padding() {
        let raw = r#"{"sid":"ses_abc","cn":"claude-code","cv":"2.1.0","pv":"2025-06-18"}"#;
        let token = decode(&encode(raw)).expect("token");
        assert_eq!(token.sid, "ses_abc");
        assert_eq!(token.cn.as_deref(), Some("claude-code"));
        let padded = format!("{}==", encode(r#"{"sid":"ses_abcd"}"#));
        assert_eq!(decode(&padded).expect("token").sid, "ses_abcd");
    }

    #[test]
    fn non_tokens_are_rejected() {
        let rejected = [
            "0190f0e8-7a6b-7c3d-9e4f-5a6b7c8d9e0f".to_string(),
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig".to_string(),
            encode(r#"{"sid":""}"#),
            encode(r#"["sid"]"#),
            encode(&format!(r#"{{"sid":"{}"}}"#, "s".repeat(129))),
        ];
        for value in rejected {
            assert_eq!(decode(&value), None, "value {value:?}");
        }
    }

    #[test]
    fn bad_client_fields_do_not_reject_the_token() {
        let token = decode(&encode(r#"{"sid":"ses_abc","cn":7}"#)).expect("token");
        assert_eq!(token.cn, None);
    }
}
