#[cfg(feature = "e2e-test")]
#[test]
fn get_client() {
    use dotenv::dotenv;
    dotenv().ok(); // Load the .env file
    println!("Loaded .env for tests");

    // see https://us.posthog.com/project/115809/ for the e2e project
    use posthog_rs::Event;
    use std::collections::HashMap;

    let api_key = std::env::var("POSTHOG_RS_E2E_TEST_API_KEY").unwrap();
    let client = posthog_rs::client(api_key.as_str());

    let mut child_map = HashMap::new();
    child_map.insert("child_key1", "child_value1");

    let mut event = Event::new("e2e test event", "1234");
    event.insert_prop("key1", "value1").unwrap();
    event.insert_prop("key2", vec!["a", "b"]).unwrap();
    event.insert_prop("key3", child_map).unwrap();

    client.capture(event).unwrap();
}

#[test]
fn generation_builder_builds_event() {
    use posthog_rs::{GenerationBuilder, PrivacyMode, TraceBuilder, SpanBuilder, EmbeddingBuilder};

    let gen = GenerationBuilder::new()
        .distinct_id("user_123")
        .model("gemini-2.0-pro")
        .provider("google")
        .temperature(0.2)
        .max_output_tokens(512)
        .input_tokens(100)
        .output_tokens(50)
        .total_tokens(150)
        .latency_ms(1234)
        .cost_usd(0.0025)
        .request_id("req_1")
        .trace_id("trace_1")
        .metadata(serde_json::json!({"foo":"bar"}))
        .unwrap();

    let event = gen.build_event().unwrap();

    // Serialize event to ensure it can be sent
    let _ = serde_json::to_string(&event).unwrap();

    // privacy redaction
    let redacted = GenerationBuilder::new()
        .distinct_id("u")
        .input(serde_json::json!({"secret":"s"})).unwrap()
        .input_privacy(PrivacyMode::Redacted)
        .build_event()
        .unwrap();
    let s = serde_json::to_string(&redacted).unwrap();
    assert!(s.contains("[REDACTED]"));

    // trace/span
    let trace = TraceBuilder::new().distinct_id("u").trace_id("t").name("overall").latency_ms(10).build_event().unwrap();
    let span = SpanBuilder::new().distinct_id("u").trace_id("t").span_id("s1").parent_span_id("p").name("call").latency_ms(5).build_event().unwrap();
    let _ = serde_json::to_string(&trace).unwrap();
    let _ = serde_json::to_string(&span).unwrap();

    // embedding
    let emb = EmbeddingBuilder::new().distinct_id("u").provider("google").model("text-embedding-004").vector_dims(768).vector_count(1).input_tokens(42).latency_ms(3).build_event().unwrap();
    let _ = serde_json::to_string(&emb).unwrap();
}
