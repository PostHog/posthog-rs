use super::*;
use crate::FlagValue;

#[test]
fn cached_exports_finish_before_http_and_identity_invalidates_before_latest_response() {
    let server = Server::new(vec![
        (200, json!({"featureFlags":{"obsolete":true}})),
        (
            200,
            json!({"featureFlags":{"fresh":false},"featureFlagPayloads":{"fresh":null}}),
        ),
    ]);
    let mut config = config(&server.host);
    config.preload_feature_flags = true;
    config.bootstrap_feature_flags = Some(HashMap::from([(
        "seed".into(),
        FlagValue::String("blue".into()),
    )]));
    config.bootstrap_feature_flag_payloads = Some(HashMap::from([("seed".into(), json!("123"))]));
    let client = Arc::new(Client::init_blocking(config));
    server.request();
    let before = client.state.as_ref().unwrap().snapshot();
    let reader = client.clone();
    let (tx, rx) = mpsc::channel();
    let reading = thread::spawn(move || {
        tx.send((
            reader.get_feature_flags(),
            reader.get_feature_flag_payloads(),
            reader.get_feature_flag_payload("seed"),
        ))
        .unwrap();
    });
    let ready = rx.recv_timeout(WAIT);
    let after_reads = client.state.as_ref().unwrap().snapshot();
    client.identify("person");
    let after_identify = client.get_feature_flags();
    let after_identify_payloads = client.get_feature_flag_payloads();
    server.respond();
    reading.join().unwrap();
    let request = server.request();
    let while_latest_pending = client.get_feature_flags();
    server.respond();
    client.flush_blocking();
    server.finish();
    assert_eq!(
        ready.expect("memory getters must not wait for a flags response"),
        (
            Some(HashMap::from([(
                "seed".into(),
                FlagValue::String("blue".into())
            )])),
            Some(HashMap::from([("seed".into(), json!("123"))])),
            Some(json!("123"))
        )
    );
    assert_eq!(
        before, after_reads,
        "event-free exports must not mutate client state"
    );
    assert_eq!(after_identify, None);
    assert_eq!(after_identify_payloads, None);
    assert_eq!(while_latest_pending, None);
    assert_eq!(request["distinct_id"], "person");
    assert_eq!(
        client.get_feature_flags(),
        Some(HashMap::from([("fresh".into(), FlagValue::Boolean(false))]))
    );
    assert_eq!(client.get_feature_flag_payload("fresh"), Some(Value::Null));
}
