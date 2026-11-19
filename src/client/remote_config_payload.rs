use reqwest::header::HeaderValue;
use tracing::warn;

use super::common::normalize_payload;
use super::ClientOptions;
use crate::Error;

pub(super) fn remote_config_payload_url(
    options: &ClientOptions,
    flag_key: &str,
) -> Result<reqwest::Url, Error> {
    let mut url = reqwest::Url::parse(&options.endpoints().api_host())
        .map_err(|e| Error::Connection(format!("Invalid host: {e}")))?;
    url.path_segments_mut()
        .map_err(|_| Error::Connection("Invalid host: cannot be a base URL".to_string()))?
        .pop_if_empty()
        .extend([
            "api",
            "projects",
            "@current",
            "feature_flags",
            flag_key,
            "remote_config",
        ]);
    url.query_pairs_mut().append_pair("token", &options.api_key);
    Ok(url)
}

pub(super) fn authorization_header(secret_key: &str) -> Result<HeaderValue, Error> {
    let mut value = HeaderValue::from_str(&format!("Bearer {secret_key}")).map_err(|_| {
        warn!("secret_key holds characters that can't go in an HTTP header");
        Error::Unauthorized
    })?;
    value.set_sensitive(true);
    Ok(value)
}

/// `Url` drops `.` and `..` path segments, which would send the request to a
/// different endpoint, and an empty key would leave an empty segment.
pub(super) fn reject_unsendable_key(flag_key: &str) -> Result<(), Error> {
    if matches!(flag_key, "" | "." | "..") {
        return Err(Error::BadRequest(format!(
            "flag key {flag_key:?} can't be sent as a URL path segment"
        )));
    }
    Ok(())
}

/// PostHog answers a flag that has no payload with an empty 2xx body.
pub(super) fn decode_remote_config_payload(
    body: &[u8],
) -> Result<Option<serde_json::Value>, Error> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice(body)
        .map(|value| Some(normalize_payload(value)))
        .map_err(|e| Error::Serialization(format!("Failed to parse response: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClientOptionsBuilder;
    use serde_json::json;

    fn options(host: &str, api_key: &str) -> ClientOptions {
        ClientOptionsBuilder::default()
            .api_key(api_key.to_string())
            .host(host)
            .build()
            .unwrap()
    }

    fn url_for(host: &str, flag_key: &str) -> reqwest::Url {
        remote_config_payload_url(&options(host, "phc_test"), flag_key).unwrap()
    }

    #[test]
    fn url_percent_encodes_key_as_one_segment() {
        let cases = [
            ("plain-flag", "plain-flag"),
            ("a/b", "a%2Fb"),
            ("a?b=1", "a%3Fb=1"),
            ("a#b", "a%23b"),
            ("50%", "50%25"),
            ("sp ace", "sp%20ace"),
            ("é", "%C3%A9"),
            ("@x", "@x"),
            ("a%2Fb", "a%252Fb"),
        ];
        for (key, encoded) in cases {
            let url = url_for("https://h.example", key);
            assert_eq!(
                url.path(),
                format!("/api/projects/@current/feature_flags/{encoded}/remote_config"),
                "key {key:?}"
            );
            assert_eq!(url.query(), Some("token=phc_test"), "key {key:?}");
            assert_eq!(url.fragment(), None, "key {key:?}");
        }
    }

    #[test]
    fn url_keeps_host_path_prefix_and_trailing_slash() {
        for host in [
            "https://h.example:8080/proxy",
            "https://h.example:8080/proxy/",
        ] {
            assert_eq!(
                url_for(host, "flag").as_str(),
                "https://h.example:8080/proxy/api/projects/@current/feature_flags/flag/remote_config?token=phc_test"
            );
        }
    }

    #[test]
    fn url_token_is_form_encoded() {
        let url =
            remote_config_payload_url(&options("https://h.example", "phc_a+b&c"), "flag").unwrap();
        assert_eq!(url.query(), Some("token=phc_a%2Bb%26c"));
    }

    #[test]
    fn dot_segment_keys_would_change_the_endpoint() {
        let dropped = "/api/projects/@current/feature_flags/remote_config";
        assert_eq!(url_for("https://h.example", ".").path(), dropped);
        assert_eq!(url_for("https://h.example", "..").path(), dropped);
    }

    #[test]
    fn unsendable_keys_are_rejected() {
        for key in ["", ".", ".."] {
            assert!(matches!(
                reject_unsendable_key(key),
                Err(Error::BadRequest(_))
            ));
        }
        for key in ["flag", "...", ".a", "a."] {
            assert!(reject_unsendable_key(key).is_ok());
        }
    }

    #[test]
    fn decode_object() {
        let value = decode_remote_config_payload(br#"{"foo":["bar","baz"]}"#).unwrap();
        assert_eq!(value, Some(json!({"foo": ["bar", "baz"]})));
    }

    #[test]
    fn decode_parses_a_string_holding_json_a_second_time() {
        let value = decode_remote_config_payload(br#""{\"foo\":[\"bar\"]}""#).unwrap();
        assert_eq!(value, Some(json!({"foo": ["bar"]})));
    }

    #[test]
    fn decode_keeps_a_string_that_is_not_json() {
        let value = decode_remote_config_payload(br#""not json""#).unwrap();
        assert_eq!(value, Some(json!("not json")));
    }

    #[test]
    fn decode_turns_a_numeric_string_into_a_number() {
        let value = decode_remote_config_payload(br#""123""#).unwrap();
        assert_eq!(value, Some(json!(123)));
    }

    #[test]
    fn decode_null_is_null() {
        assert_eq!(
            decode_remote_config_payload(b"null").unwrap(),
            Some(serde_json::Value::Null)
        );
    }

    #[test]
    fn decode_invalid_body_is_a_serialization_error() {
        assert!(matches!(
            decode_remote_config_payload(b"not json at all"),
            Err(Error::Serialization(_))
        ));
    }

    #[test]
    fn decode_empty_or_whitespace_body_is_none() {
        for body in [&b""[..], b" \n\t\r "] {
            assert_eq!(decode_remote_config_payload(body).unwrap(), None);
        }
    }

    #[test]
    fn authorization_header_is_sensitive() {
        let header = authorization_header("phx_secret").unwrap();
        assert!(header.is_sensitive());
        assert_eq!(header.to_str().unwrap(), "Bearer phx_secret");
    }

    #[test]
    fn authorization_header_rejects_control_characters() {
        assert!(matches!(
            authorization_header("phx_\nsecret"),
            Err(Error::Unauthorized)
        ));
    }
}
