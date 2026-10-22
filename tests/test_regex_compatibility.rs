#![allow(deprecated)]

use posthog_rs::{FlagCache, FlagValue, LocalEvaluationResponse, LocalEvaluator};
use serde_json::{json, Value};
use std::collections::HashMap;

const NAMED_CAPTURE_PATTERNS: [&str; 8] = [
    "(?<1>a)",
    "(?P<1>a)",
    "(?<x>a)(?<x>b)",
    "(?P<x>a)(?P<x>b)",
    "(?<a.b>a)",
    "(?P<a.b>a)",
    "(?<name>a)",
    "(?P<name>a)",
];

fn definitions(pattern: &str, operator: &str) -> LocalEvaluationResponse {
    serde_json::from_value(json!({
        "flags": [{"key": "regex-flag", "active": true, "filters": {"groups": [{
            "properties": [{"key": "value", "value": pattern, "operator": operator}]
        }]}}]
    }))
    .unwrap()
}

fn evaluate(
    pattern: &str,
    operator: &str,
    input: Value,
) -> Result<Option<FlagValue>, posthog_rs::InconclusiveMatchError> {
    let cache = FlagCache::new();
    cache.update(definitions(pattern, operator));
    LocalEvaluator::new(cache).evaluate_flag(
        "regex-flag",
        "user",
        &HashMap::from([("value".into(), input)]),
        &HashMap::new(),
        &HashMap::new(),
    )
}

#[test]
fn regex_compatibility_unsupported_is_inconclusive_in_production_context() {
    for operator in ["regex", "not_regex"] {
        for (pattern, input) in [
            ("a(?=b)", "ab"),
            ("(?<=a+)b", "aab"),
            (r"(a)\1", "aa"),
            (r"[\b]", "\u{8}"),
            (r"[\B]", "B"),
            (r"\h+", "A1"),
            (r"\<foo\>", "foo"),
            ("a++", "aa"),
            ("a**", ""),
            ("a*??", ""),
            ("a{2}*", ""),
            ("(?:)*", "x"),
            ("(){2}", "x"),
            ("a{,2}", ""),
            ("{foo}", "{foo}"),
            ("a{2foo}", "a{2foo}"),
            ("a{2", "a{2"),
            ("a{2,foo}", "a{2,foo}"),
            ("a{2,3,4}", "a{2,3,4}"),
            (r"[a\<]", "<"),
            ("{2}", "x"),
            ("a|{2}", "x"),
            ("(?-u:\\w+)", "é"),
            ("(?x)a # comment", "a"),
            ("(?R)^a$", "a\r\n"),
            // A native successful search is not enough: the service can exhaust its budget.
            (r".*\bbot\b.*", &"a".repeat(140)),
            (r".*\Bbot\B.*", &"a".repeat(140)),
        ] {
            for _ in 0..2 {
                // Cached failure must not turn into a definitive boolean.
                assert!(
                    evaluate(pattern, operator, json!(input)).is_err(),
                    "{} {:?}",
                    operator,
                    pattern
                );
            }
        }
    }
}

#[test]
fn regex_compatibility_named_captures_are_inconclusive() {
    for pattern in NAMED_CAPTURE_PATTERNS
        .iter()
        .copied()
        .chain(["(?<bad", "(?P<bad"])
    {
        for operator in ["regex", "not_regex"] {
            for input in ["a", "ab", "z"] {
                for _ in 0..2 {
                    assert!(
                        evaluate(pattern, operator, json!(input)).is_err(),
                        "{} {:?} {:?}",
                        operator,
                        pattern,
                        input
                    );
                }
            }
        }
    }
}

#[test]
fn regex_compatibility_invalid_is_false_for_both_operators() {
    for operator in ["regex", "not_regex"] {
        for pattern in ["[", "(", "*", "?*", r"\q", "a{2,1}", "a{0,4294967296}"] {
            for input in [json!("value"), json!("é"), Value::Null] {
                assert_eq!(
                    evaluate(pattern, operator, input).unwrap(),
                    Some(FlagValue::Boolean(false)),
                    "{} {:?}",
                    operator,
                    pattern
                );
            }
        }
    }
}

#[test]
fn regex_compatibility_shared_patterns_stay_local() {
    for operator in ["regex", "not_regex"] {
        for (pattern, input, expected) in [
            ("pro", json!("a pro plan"), true),
            ("^pro$", json!("PRO"), false),
            ("(?i)^pro$", json!("PRO"), true),
            ("(?i:pro)", json!("PRO"), true),
            (r"^\d+$", json!("١"), true),
            (r"^\w+$", json!("é"), true),
            (r"^\s$", json!("\u{feff}"), false),
            (r"^\p{L}+$", json!("é"), true),
            ("^.$", json!("😀"), true),
            ("^.$", json!("\r"), true),
            ("^[[:alpha:]]+$", json!("foo"), true),
            ("^[a-z&&[^aeiou]]+$", json!("bcdf"), true),
            ("^[a-z--b]$", json!("b"), false),
            ("^[a~~b]$", json!("a"), true),
            ("^foo$", json!("foo\n"), false),
            ("(?m)^foo$", json!("a\nfoo\nb"), true),
            (r"\Afoo\z", json!("foo"), true),
            ("^(?:ab|cd){2,3}$", json!("abcdab"), true),
            ("^(ab|cd){2,3}$", json!("abcdab"), true),
            (r"^\(\?<1>a\)$", json!("(?<1>a)"), true),
            (r"^\(\?P<1>a\)$", json!("(?P<1>a)"), true),
            (r"^\++$", json!("++"), true),
            (r"^a\-b\@c\:d\_e\=f\ g$", json!("a-b@c:d_e=f g"), true),
            (r"^\(\?i\)$", json!("(?i)"), true),
            (r"^\\b$", json!(r"\b"), true),
            (r"^[\^$?]+$", json!("$?^"), true),
            (r"^\^{2}$", json!("^^"), true),
            (r"^\x{41}+$", json!("AA"), true),
            ("", json!("é"), true),
            ("", Value::Null, true),
            ("^null$", Value::Null, true),
        ] {
            assert_eq!(
                evaluate(pattern, operator, input).unwrap(),
                Some(FlagValue::Boolean(expected != (operator == "not_regex"))),
                "{} {:?}",
                operator,
                pattern
            );
        }
    }
}

#[test]
fn regex_compatibility_compilation_size_failure_is_inconclusive() {
    for operator in ["regex", "not_regex"] {
        for _ in 0..2 {
            assert!(evaluate("a{1000000}", operator, json!("a")).is_err());
        }
    }
}

#[test]
fn regex_compatibility_cohort_negation_preserves_inconclusive() {
    for pattern in std::iter::once("a(?=b)").chain(NAMED_CAPTURE_PATTERNS.iter().copied()) {
        for operator in ["regex", "not_regex"] {
            for negation in [false, true] {
                let mut response = serde_json::to_value(definitions(pattern, operator)).unwrap();
                response["flags"][0]["filters"]["groups"][0]["properties"] = json!([
                    {"key": "id", "value": "cohort", "type": "cohort"}
                ]);
                response["cohorts"] = json!({"cohort": {"type": "AND", "values": [
                    {"key": "value", "value": pattern, "operator": operator, "negation": negation, "type": "person"}
                ]}});
                let cache = FlagCache::new();
                cache.update(serde_json::from_value(response).unwrap());
                let evaluator = LocalEvaluator::new(cache);
                let properties = HashMap::from([("value".into(), json!("ab"))]);
                assert!(evaluator
                    .evaluate_flag(
                        "regex-flag",
                        "user",
                        &properties,
                        &HashMap::new(),
                        &HashMap::new()
                    )
                    .is_err());
                assert!(evaluator.evaluate_all_flags(
                    "user",
                    &properties,
                    &HashMap::new(),
                    &HashMap::new()
                )["regex-flag"]
                    .is_err());
            }
        }
    }
}

#[cfg(feature = "async-client")]
#[tokio::test]
async fn regex_compatibility_public_client_fallback_and_local_only() {
    use httpmock::prelude::*;
    use posthog_rs::{ClientOptionsBuilder, EvaluateFlagsOptions};
    use std::time::Duration;
    for pattern in std::iter::once("a(?=b)").chain(NAMED_CAPTURE_PATTERNS.iter().copied()) {
        let server = MockServer::start();
        let mut body = serde_json::to_value(definitions(pattern, "regex")).unwrap();
        for (key, pattern, operator) in [
            ("negative", pattern, "not_regex"),
            ("portable", "^ab$", "regex"),
            ("invalid", "[", "not_regex"),
        ] {
            let mut flag = body["flags"][0].clone();
            flag["key"] = json!(key);
            flag["filters"]["groups"][0]["properties"][0]["value"] = json!(pattern);
            flag["filters"]["groups"][0]["properties"][0]["operator"] = json!(operator);
            body["flags"].as_array_mut().unwrap().push(flag);
        }
        let definitions_mock = server.mock(|when, then| {
            when.method(GET).path("/flags/definitions/");
            then.status(200).json_body(body);
        });
        let remote = server.mock(|when, then| {
            when.method(POST).path("/flags/");
            then.status(200).json_body(json!({"featureFlags": {
            "regex-flag": "remote", "negative": "remote", "portable": "remote", "invalid": "remote"
        }}));
        });
        for local_only in [false, true] {
            let options = ClientOptionsBuilder::default()
                .host(server.base_url())
                .api_key("project-key".to_string())
                .personal_api_key("personal-key".to_string())
                .enable_local_evaluation(true)
                .local_evaluation_only(local_only)
                .poll_interval_seconds(60)
                .build()
                .unwrap();
            let client = posthog_rs::client(options).await;
            let expected_calls = if local_only { 2 } else { 1 };
            tokio::time::timeout(Duration::from_secs(5), async {
                while definitions_mock.calls_async().await < expected_calls {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                // Wait for definitions to be installed, not just requested.
                loop {
                    let result = client
                        .get_feature_flag(
                            "portable",
                            "user",
                            None,
                            Some(HashMap::from([("value".into(), json!("ab"))])),
                            None,
                        )
                        .await
                        .unwrap();
                    if result == Some(FlagValue::Boolean(true)) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let initial_calls = remote.calls_async().await;
            let properties = HashMap::from([("value".into(), json!("ab"))]);
            for key in ["regex-flag", "negative"] {
                let result = client
                    .get_feature_flag(key, "user", None, Some(properties.clone()), None)
                    .await
                    .unwrap();
                assert_eq!(
                    result,
                    if local_only {
                        None
                    } else {
                        Some(FlagValue::String("remote".into()))
                    }
                );
            }
            // Missing-property control proves the same remote gate is being exercised.
            let missing = client
                .get_feature_flag("regex-flag", "user", None, None, None)
                .await
                .unwrap();
            assert_eq!(
                missing,
                if local_only {
                    None
                } else {
                    Some(FlagValue::String("remote".into()))
                }
            );
            assert_eq!(
                client
                    .get_feature_flag("invalid", "user", None, Some(properties.clone()), None)
                    .await
                    .unwrap(),
                Some(FlagValue::Boolean(false))
            );
            let snapshot = client
                .evaluate_flags(
                    "user",
                    EvaluateFlagsOptions {
                        person_properties: Some(properties),
                        only_evaluate_locally: local_only,
                        flag_keys: Some(vec!["regex-flag".into()]),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                snapshot.get_flag("regex-flag"),
                if local_only {
                    None
                } else {
                    Some(FlagValue::String("remote".into()))
                }
            );
            remote
                .assert_calls_async(initial_calls + if local_only { 0 } else { 4 })
                .await;
            client.shutdown().await;
        }
    }
}
