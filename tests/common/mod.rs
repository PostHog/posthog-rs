type CaptureError = (Option<u16>, String);

#[allow(dead_code)]
pub fn capture_error_sink() -> (
    std::sync::mpsc::Receiver<CaptureError>,
    impl Fn(&posthog::PostHogError<'_>) + Send + Sync + 'static,
) {
    let (tx, rx) = std::sync::mpsc::channel();
    (rx, move |failure| {
        if let posthog::PostHogError::Capture(failure) = failure {
            let _ = tx.send((failure.status(), format!("{:?}", failure.error())));
        }
    })
}

#[allow(dead_code)]
pub fn default_user_agent() -> String {
    format!("posthog-rs/{}", env!("CARGO_PKG_VERSION"))
}

/// What a caller sets on `$lib` and `$lib_version` (`None` leaves the key
/// out) and what must reach the wire. `parts` is whether the SDK's
/// `$lib_version__major/minor/patch` are sent.
#[allow(dead_code)]
pub struct LibCase {
    pub name: &'static str,
    pub lib: Option<serde_json::Value>,
    pub version: Option<serde_json::Value>,
    pub wire_lib: Option<serde_json::Value>,
    pub wire_version: Option<serde_json::Value>,
    pub parts: bool,
}

#[allow(dead_code)]
impl LibCase {
    /// An event named `name` carrying this case's values and a `case` marker.
    pub fn event(&self, name: &str) -> posthog::Event {
        let mut event = posthog::Event::new(name, "user-1");
        event.insert_prop("case", self.name).unwrap();
        if let Some(lib) = &self.lib {
            event.insert_prop("$lib", lib).unwrap();
        }
        if let Some(version) = &self.version {
            event.insert_prop("$lib_version", version).unwrap();
        }
        event
    }

    pub fn assert_wire(&self, properties: &serde_json::Value, context: &str) {
        let name = self.name;
        assert_eq!(
            properties.get("$lib"),
            self.wire_lib.as_ref(),
            "$lib, {context}, {name}"
        );
        assert_eq!(
            properties.get("$lib_version"),
            self.wire_version.as_ref(),
            "$lib_version, {context}, {name}"
        );
        let parts = [
            ("$lib_version__major", env!("CARGO_PKG_VERSION_MAJOR")),
            ("$lib_version__minor", env!("CARGO_PKG_VERSION_MINOR")),
            ("$lib_version__patch", env!("CARGO_PKG_VERSION_PATCH")),
        ];
        for (key, part) in parts {
            let expected = self
                .parts
                .then(|| serde_json::json!(part.parse::<u64>().unwrap()));
            assert_eq!(
                properties.get(key),
                expected.as_ref(),
                "{key}, {context}, {name}"
            );
        }
    }
}

/// Every `$lib` / `$lib_version` combination the SDK's pair fill tells apart.
#[allow(dead_code)]
pub fn lib_cases() -> Vec<LibCase> {
    use serde_json::{json, Value};
    let sdk_lib = || Some(json!("posthog-rs"));
    let sdk_version = || Some(json!(env!("CARGO_PKG_VERSION")));
    let case = |name, lib: Option<Value>, version: Option<Value>| LibCase {
        name,
        lib,
        version,
        wire_lib: sdk_lib(),
        wire_version: sdk_version(),
        parts: true,
    };
    let kept = |name, lib: Option<Value>, version: Option<Value>| LibCase {
        name,
        wire_lib: lib.clone(),
        wire_version: version.clone(),
        lib,
        version,
        parts: false,
    };
    vec![
        // No layer names a library: both get the SDK's values.
        case("unset", None, None),
        case("null_lib", Some(Value::Null), None),
        case("empty_lib", Some(json!("")), None),
        case("number_lib", Some(json!(5)), Some(json!("1.0.0"))),
        case("bool_lib", Some(json!(true)), None),
        case("object_lib", Some(json!({ "name": "x" })), None),
        case("lone_version", None, Some(json!("9.9.9"))),
        // The SDK's own name without a version: only the version is filled.
        case("sdk_lib_alone", sdk_lib(), None),
        case("sdk_lib_null_version", sdk_lib(), Some(Value::Null)),
        case("sdk_lib_empty_version", sdk_lib(), Some(json!(""))),
        case("sdk_lib_number_version", sdk_lib(), Some(json!(5))),
        // Anything else is sent as set, with no SDK version parts.
        kept("sdk_lib_with_version", sdk_lib(), Some(json!("0.1.0"))),
        kept("sdk_lib_with_crate_version", sdk_lib(), sdk_version()),
        kept("custom_lib_alone", Some(json!("custom-lib")), None),
        kept(
            "custom_lib_null_version",
            Some(json!("custom-lib")),
            Some(Value::Null),
        ),
        kept(
            "custom_lib_with_version",
            Some(json!("custom-lib")),
            Some(json!("9.9.9")),
        ),
    ]
}

/// Events as they reached the wire, keyed by `uuid` so a matcher that runs
/// more than once for a request records each event once.
pub type WireEvents =
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, serde_json::Value>>>;

/// Answers 200 to every POST on `path` and records each event in its `batch`.
#[allow(dead_code)]
pub fn record_wire_events(server: &httpmock::MockServer, path: &str) -> WireEvents {
    let events = WireEvents::default();
    let recorded = events.clone();
    let path = path.to_string();
    server.mock(move |when, then| {
        when.method(httpmock::Method::POST)
            .path(path)
            .is_true(move |request| {
                let body: serde_json::Value =
                    serde_json::from_slice(request.body_ref()).unwrap_or_default();
                let batch = body["batch"].as_array().cloned().unwrap_or_default();
                let mut recorded = recorded.lock().unwrap();
                for event in batch {
                    let uuid = event["uuid"].as_str().unwrap_or_default().to_string();
                    recorded.insert(uuid, event);
                }
                true
            });
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "results": {} }));
    });
    events
}

/// The `properties` of the one recorded event whose `case` property is `case`,
/// or of the only recorded event when `case` is `None`.
#[allow(dead_code)]
pub fn wire_properties(events: &WireEvents, case: Option<&str>) -> serde_json::Value {
    let events = events.lock().unwrap();
    let matching: Vec<&serde_json::Value> = events
        .values()
        .filter(|event| match case {
            Some(case) => event["properties"]["case"] == case,
            None => true,
        })
        .collect();
    assert_eq!(matching.len(), 1, "events for case {case:?}: {matching:?}");
    matching[0]["properties"].clone()
}
