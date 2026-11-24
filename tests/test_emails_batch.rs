#![cfg(feature = "async-client")]

mod email_support;

use std::sync::Arc;
use std::time::Duration;

use email_support::{
    options, Answer, Recorded, ScriptedServer, BATCH_PATH, STATUS_BY_KEY_PATH, TEMPLATE_ID,
};
use posthog::{
    BatchEmailStatus, BatchRecipient, CodeBatch, EmailErrorKind, EmailTracking, SendBatchResult,
    TemplateBatch,
};
use serde_json::{json, Value};

use BatchEmailStatus::{Failed, NotAttempted, Queued, Unconfirmed};

const FOUR_MIB: usize = 4 * 1024 * 1024;

async fn client_for(server: &ScriptedServer) -> posthog::Client {
    posthog::client(options(&server.url).build().unwrap()).await
}

fn queued(message_id: &str) -> Value {
    json!({"message_id": message_id, "status": "queued", "replayed": false})
}

fn refused(message_id: &str, code: &str, status: u16) -> Value {
    json!({
        "message_id": message_id,
        "error": {"code": code, "detail": format!("Refused: {code}."), "status": status},
    })
}

fn ok(slots: Vec<Value>) -> Answer {
    Answer::json(200, json!({ "results": slots }))
}

fn keys_of(request: &Recorded) -> Vec<String> {
    request.json()["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|recipient| recipient["idempotency_key"].as_str().unwrap().to_string())
        .collect()
}

/// Queues every recipient of the request, as `msg_<key>`.
fn all_queued() -> Answer {
    Answer::of_request(|request| {
        ok(keys_of(request)
            .iter()
            .map(|key| queued(&format!("msg_{key}")))
            .collect())
    })
}

fn whole_call(status: u16, code: &str) -> Answer {
    Answer::json(
        status,
        json!({"type": "invalid_request", "code": code, "detail": "Nope."}),
    )
}

fn status_found(message_id: &str) -> Answer {
    Answer::json(
        200,
        json!({
            "id": message_id,
            "status": "sent",
            "created_at": "2026-11-22T10:00:00Z",
            "status_at": "2026-11-22T10:00:01Z",
        }),
    )
}

fn recipients(count: usize) -> Vec<BatchRecipient> {
    (0..count)
        .map(|index| {
            BatchRecipient::new(format!("user{index}@example.com"))
                .idempotency_key(format!("key-{index}"))
        })
        .collect()
}

fn statuses(result: &SendBatchResult) -> Vec<BatchEmailStatus> {
    result.results.iter().map(|result| result.status).collect()
}

fn batch_requests(server: &ScriptedServer) -> Vec<Recorded> {
    server
        .requests()
        .into_iter()
        .filter(|request| request.path == BATCH_PATH)
        .collect()
}

#[tokio::test]
async fn a_template_batch_posts_one_request_with_a_result_per_recipient() {
    let server = ScriptedServer::start_async(vec![ok(vec![
        queued("m0"),
        json!({"message_id": "m1", "status": "queued", "replayed": true}),
        refused("m2", "recipient_suppressed", 422),
    ])])
    .await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .batch(
            TemplateBatch::new(
                TEMPLATE_ID.to_uppercase(),
                vec![
                    BatchRecipient::new("a@example.com")
                        .idempotency_key("welcome:a")
                        .distinct_id("a"),
                    BatchRecipient::new("b@example.com").template_data(json!({"name": "Bea"})),
                    BatchRecipient::new("c@example.com").idempotency_key("welcome:c"),
                ],
            )
            .template_data(json!({"name": "friend", "plan": "pro"})),
        )
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        (request.method.as_str(), request.path.as_str()),
        ("POST", BATCH_PATH)
    );
    assert_eq!(request.header("authorization"), Some("Bearer phs_test"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(request.header("idempotency-key"), None);
    let body = request.json();
    let generated = body["recipients"][1]["idempotency_key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        body,
        json!({
            "template_id": TEMPLATE_ID,
            "message_data": {"name": "friend", "plan": "pro"},
            "recipients": [
                {"to": "a@example.com", "idempotency_key": "welcome:a", "distinct_id": "a"},
                {"to": "b@example.com", "idempotency_key": generated, "message_data": {"name": "Bea"}},
                {"to": "c@example.com", "idempotency_key": "welcome:c"},
            ],
        })
    );
    assert_eq!(
        uuid::Uuid::parse_str(&generated).unwrap().get_version_num(),
        4
    );

    let results = &result.results;
    assert_eq!(
        results.iter().map(|r| r.index).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(
        results
            .iter()
            .map(|r| (r.to.as_str(), r.idempotency_key.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("a@example.com", "welcome:a"),
            ("b@example.com", generated.as_str()),
            ("c@example.com", "welcome:c"),
        ]
    );
    assert_eq!(
        results
            .iter()
            .map(|r| (r.status, r.message_id.as_deref(), r.replayed))
            .collect::<Vec<_>>(),
        vec![
            (Queued, Some("m0"), false),
            (Queued, Some("m1"), true),
            (Failed, Some("m2"), false),
        ]
    );
    assert_eq!(results[2].status.as_str(), "failed");
    let error = results[2].error.as_ref().unwrap();
    assert_eq!(
        (error.code(), error.status(), error.message_id()),
        ("recipient_suppressed", Some(422), Some("m2"))
    );
    assert_eq!(error.message(), "Refused: recipient_suppressed.");
    assert!(results[0].error.is_none() && results[0].later_error.is_none());
}

#[tokio::test]
async fn a_code_batch_sends_the_same_content_to_every_recipient() {
    let server = ScriptedServer::start_async(vec![all_queued()]).await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .batch(
            CodeBatch::new("launch-news", "We launched", recipients(2))
                .html("<p>Hi</p>")
                .text("Hi")
                .from("Acme <news@example.com>")
                .reply_to("support@example.com")
                .sensitive(false)
                .tracking(EmailTracking::Off),
        )
        .await
        .unwrap();

    assert_eq!(
        server.requests()[0].json(),
        json!({
            "template_key": "launch-news",
            "subject": "We launched",
            "html": "<p>Hi</p>",
            "text": "Hi",
            "from": "Acme <news@example.com>",
            "reply_to": ["support@example.com"],
            "sensitive": false,
            "tracking": false,
            "recipients": [
                {"to": "user0@example.com", "idempotency_key": "key-0"},
                {"to": "user1@example.com", "idempotency_key": "key-1"},
            ],
        })
    );
    assert_eq!(statuses(&result), vec![Queued, Queued]);
}

#[tokio::test]
async fn refuses_what_a_batch_does_not_take_before_any_request() {
    let server = ScriptedServer::start_async(vec![all_queued()]).await;
    let client = client_for(&server).await;
    let mut repeated = recipients(3);
    repeated[2] = BatchRecipient::new("again@example.com").idempotency_key("key-0");
    let cases: Vec<(posthog::SendBatch, &str, &str)> = vec![
        (
            TemplateBatch::new(TEMPLATE_ID, Vec::new()).into(),
            "invalid_recipients",
            "recipients",
        ),
        (
            TemplateBatch::new("welcome", recipients(1)).into(),
            "invalid_template_id",
            "template_id",
        ),
        (
            TemplateBatch::new(TEMPLATE_ID, recipients(1))
                .template_data(json!([1]))
                .into(),
            "invalid_template_data",
            "template_data",
        ),
        (
            TemplateBatch::new(
                TEMPLATE_ID,
                vec![
                    BatchRecipient::new("a@example.com"),
                    BatchRecipient::new(" "),
                ],
            )
            .into(),
            "invalid_recipient",
            "recipients__1__to",
        ),
        (
            TemplateBatch::new(
                TEMPLATE_ID,
                vec![BatchRecipient::new("a@example.com").distinct_id("")],
            )
            .into(),
            "invalid_distinct_id",
            "recipients__0__distinct_id",
        ),
        (
            TemplateBatch::new(
                TEMPLATE_ID,
                vec![BatchRecipient::new("a@example.com").idempotency_key("has space")],
            )
            .into(),
            "invalid_idempotency_key",
            "recipients__0__idempotency_key",
        ),
        (
            TemplateBatch::new(TEMPLATE_ID, repeated).into(),
            "invalid_idempotency_key",
            "recipients__2__idempotency_key",
        ),
        (
            TemplateBatch::new(
                TEMPLATE_ID,
                vec![BatchRecipient::new("a@example.com").template_data(json!([1]))],
            )
            .into(),
            "invalid_template_data",
            "recipients__0__template_data",
        ),
        (
            CodeBatch::new("digest", "Hi", recipients(1)).into(),
            "invalid_form",
            "html",
        ),
        (
            CodeBatch::new("has space", "Hi", recipients(1))
                .text("x")
                .into(),
            "invalid_template_key",
            "template_key",
        ),
        (
            CodeBatch::new(
                "digest",
                "Hi",
                vec![BatchRecipient::new("a@example.com").template_data(json!({"a": 1}))],
            )
            .text("x")
            .into(),
            "invalid_form",
            "recipients__0__template_data",
        ),
    ];
    for (batch, code, field) in cases {
        let error = client.emails().batch(batch).await.unwrap_err();
        assert_eq!((error.code(), error.field()), (code, Some(field)));
        assert_eq!(error.kind(), EmailErrorKind::ValidationError, "{}", code);
        assert!(error.batch_result().is_none(), "{}", code);
    }
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn an_argument_error_names_the_recipients_position() {
    let server = ScriptedServer::start_async(vec![all_queued()]).await;
    let client = client_for(&server).await;
    let mut people = recipients(3);
    people[2] = BatchRecipient::new("again@example.com").idempotency_key("key-0");

    let error = client
        .emails()
        .batch(TemplateBatch::new(TEMPLATE_ID, people))
        .await
        .unwrap_err();
    assert_eq!(
        error.message(),
        "recipients[2] has the same idempotency_key as recipients[0]. Give each recipient its own key."
    );

    let error = client
        .emails()
        .batch(CodeBatch::new("digest", "Hi", recipients(1)))
        .await
        .unwrap_err();
    assert_eq!(
        error.message(),
        "A batch with template_key needs html, text or both."
    );

    let error = client
        .emails()
        .batch(TemplateBatch::new(
            TEMPLATE_ID,
            vec![
                BatchRecipient::new("a@example.com"),
                BatchRecipient::new(""),
            ],
        ))
        .await
        .unwrap_err();
    assert_eq!(
        error.message(),
        "recipients[1]: to must be one email address as a nonempty string."
    );
}

fn template(people: Vec<BatchRecipient>) -> TemplateBatch {
    TemplateBatch::new(TEMPLATE_ID, people)
}

fn keys_sent(server: &ScriptedServer) -> Vec<Vec<String>> {
    batch_requests(server).iter().map(keys_of).collect()
}

fn queries_read(server: &ScriptedServer) -> Vec<String> {
    server
        .requests()
        .into_iter()
        .filter(|request| request.path == STATUS_BY_KEY_PATH)
        .map(|request| request.query)
        .collect()
}

/// Answers a read by key with `message_id` for `key`, and `404` for any other.
fn found_only(key: &'static str, message_id: &'static str) -> Answer {
    Answer::of_request(move |request| {
        if request.query == format!("idempotency_key={key}") {
            status_found(message_id)
        } else {
            whole_call(404, "not_found")
        }
    })
}

fn error_codes(result: &SendBatchResult) -> Vec<Option<String>> {
    result
        .results
        .iter()
        .map(|result| result.error.as_ref().map(|error| error.code().to_string()))
        .collect()
}

#[tokio::test]
async fn splits_by_100_recipients_and_sends_the_requests_in_order() {
    let server = ScriptedServer::start_async(vec![all_queued()]).await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .batch(template(recipients(250)))
        .await
        .unwrap();

    assert_eq!(
        keys_sent(&server).iter().map(Vec::len).collect::<Vec<_>>(),
        vec![100, 100, 50]
    );
    assert_eq!(keys_sent(&server)[1][0], "key-100");
    assert_eq!(
        result.results.iter().map(|r| r.index).collect::<Vec<_>>(),
        (0..250).collect::<Vec<_>>()
    );
    assert_eq!(
        result.results[249].message_id.as_deref(),
        Some("msg_key-249")
    );
    assert_eq!(statuses(&result), vec![Queued; 250]);
}

#[tokio::test]
async fn splits_by_the_4_mib_body_and_fails_only_a_recipient_too_large_alone() {
    let big = "x".repeat(FOUR_MIB / 3);
    let mut people: Vec<BatchRecipient> = recipients(5)
        .into_iter()
        .map(|recipient| recipient.template_data(json!({ "note": big })))
        .collect();
    people[3] = BatchRecipient::new("user3@example.com")
        .idempotency_key("key-3")
        .template_data(json!({"note": "y".repeat(FOUR_MIB)}));
    let server = ScriptedServer::start_async(vec![all_queued()]).await;
    let client = client_for(&server).await;

    let result = client.emails().batch(template(people)).await.unwrap();

    assert!(batch_requests(&server)
        .iter()
        .all(|request| request.body.len() <= FOUR_MIB));
    assert_eq!(
        keys_sent(&server),
        vec![vec!["key-0", "key-1"], vec!["key-2", "key-4"]]
    );
    assert_eq!(
        statuses(&result),
        vec![Queued, Queued, Queued, Failed, Queued]
    );
    let too_large = &result.results[3];
    let error = too_large.error.as_ref().unwrap();
    assert_eq!(error.code(), "content_too_large");
    assert_eq!(error.kind(), EmailErrorKind::ValidationError);
    assert_eq!(too_large.message_id, None);
    assert_eq!(too_large.idempotency_key, "key-3");
}

#[tokio::test]
async fn a_request_fills_up_to_exactly_4_mib() {
    let base = json!({"template_id": TEMPLATE_ID, "message_data": {}, "recipients": []})
        .to_string()
        .len();
    let item = json!({"to": "a@example.com", "idempotency_key": "k1", "message_data": {"n": ""}})
        .to_string()
        .len();
    let filler = "a".repeat(FOUR_MIB - base - item - 1 - item);
    let one = |key: &str, note: &str| {
        BatchRecipient::new("a@example.com")
            .idempotency_key(key)
            .template_data(json!({ "n": note }))
    };
    let server = ScriptedServer::start_async(vec![all_queued()]).await;
    let client = client_for(&server).await;

    client
        .emails()
        .batch(template(vec![
            one("k1", &filler),
            one("k2", ""),
            one("k3", ""),
        ]))
        .await
        .unwrap();

    assert_eq!(batch_requests(&server)[0].body.len(), FOUR_MIB);
    assert_eq!(
        keys_sent(&server).iter().map(Vec::len).collect::<Vec<_>>(),
        vec![2, 1]
    );
}

#[tokio::test]
async fn a_batch_waits_for_a_slow_answer_whatever_the_email_timeout() {
    let server =
        ScriptedServer::start_async(vec![all_queued().after(Duration::from_secs(2))]).await;
    let client = posthog::client(
        options(&server.url)
            .email_request_timeout_seconds(1)
            .email_request_max_retries(0)
            .build()
            .unwrap(),
    )
    .await;

    let result = client
        .emails()
        .batch(template(recipients(1)))
        .await
        .unwrap();

    assert_eq!(statuses(&result), vec![Queued]);
}

#[tokio::test]
async fn a_retry_after_a_lost_answer_keeps_a_refused_recipient_unconfirmed() {
    let server = ScriptedServer::start_async(vec![
        Answer::lost(),
        ok(vec![
            json!({"message_id": "m0", "status": "queued", "replayed": true}),
            refused("m1", "idempotency_key_reused", 422),
        ]),
    ])
    .await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .batch(template(recipients(2)))
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, requests[1].body);
    let (first, second) = (&result.results[0], &result.results[1]);
    assert_eq!(
        (first.status, first.message_id.as_deref(), first.replayed),
        (Queued, Some("m0"), true)
    );
    assert_eq!(
        (second.status, second.message_id.as_deref()),
        (Unconfirmed, Some("m1"))
    );
    let error = second.error.as_ref().unwrap();
    assert_eq!(error.code(), "delivery_unavailable");
    assert_eq!(error.kind(), EmailErrorKind::ServiceUnavailable);
    assert_eq!(
        second.later_error.as_ref().unwrap().code(),
        "idempotency_key_reused"
    );
}

#[tokio::test]
async fn a_503_then_a_refusal_also_keeps_the_recipient_unconfirmed() {
    let server = ScriptedServer::start_async(vec![
        whole_call(503, "delivery_unavailable"),
        ok(vec![refused("m0", "sending_paused", 422)]),
    ])
    .await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .batch(template(recipients(1)))
        .await
        .unwrap();

    let only = &result.results[0];
    assert_eq!(only.status, Unconfirmed);
    assert_eq!(only.later_error.as_ref().unwrap().code(), "sending_paused");
}

#[tokio::test]
async fn a_refusal_without_an_earlier_lost_answer_is_failed() {
    let server =
        ScriptedServer::start_async(vec![ok(vec![refused("m0", "sending_paused", 422)])]).await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .batch(template(recipients(1)))
        .await
        .unwrap();

    assert_eq!(statuses(&result), vec![Failed]);
    assert!(result.results[0].later_error.is_none());
}

#[tokio::test]
async fn a_recipient_posthog_could_not_confirm_is_unconfirmed() {
    for code in ["delivery_unavailable", "idempotency_key_in_flight"] {
        let server = ScriptedServer::start_async(vec![ok(vec![refused("m0", code, 503)])]).await;
        let client = client_for(&server).await;

        let result = client
            .emails()
            .batch(template(recipients(1)))
            .await
            .unwrap();

        let only = &result.results[0];
        assert_eq!(
            (only.status, only.message_id.as_deref()),
            (Unconfirmed, Some("m0"))
        );
        assert_eq!(only.error.as_ref().unwrap().code(), code);
        assert_eq!(server.requests().len(), 1, "{}", code);
    }
}

#[tokio::test]
async fn an_unconfirmed_recipient_without_an_id_is_read_by_key_after_a_batch_that_succeeded() {
    let server = ScriptedServer::start_async(vec![
        ok(vec![
            queued("m0"),
            json!({"error": {"code": "delivery_unavailable", "status": 503}}),
        ]),
        status_found("m1"),
    ])
    .await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .batch(template(recipients(2)))
        .await
        .unwrap();

    assert_eq!(queries_read(&server), vec!["idempotency_key=key-1"]);
    let second = &result.results[1];
    assert_eq!(
        (second.status, second.message_id.as_deref()),
        (Unconfirmed, Some("m1"))
    );
    assert_eq!(
        second.error.as_ref().unwrap().code(),
        "delivery_unavailable"
    );
}

#[tokio::test]
async fn a_whole_request_error_returns_every_recipients_result_so_far() {
    let server =
        ScriptedServer::start_async(vec![all_queued(), whole_call(429, "quota_limited")]).await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(template(recipients(250)))
        .await
        .unwrap_err();

    assert_eq!((error.code(), error.status()), ("quota_limited", Some(429)));
    assert_eq!(server.requests().len(), 2);
    let result = error.batch_result().unwrap();
    let mut expected = vec![Queued; 100];
    expected.extend(vec![Failed; 100]);
    expected.extend(vec![NotAttempted; 50]);
    assert_eq!(statuses(result), expected);
    let refused = result.results[150].error.as_ref().unwrap();
    assert_eq!(
        (refused.code(), refused.message()),
        ("quota_limited", "Nope.")
    );
    assert_eq!(error_codes(result)[200].as_deref(), Some("not_attempted"));
    assert_eq!(result.results[249].idempotency_key, "key-249");
}

#[tokio::test]
async fn a_lost_answer_after_retries_reads_each_email_by_its_key() {
    let server = ScriptedServer::start_async(vec![
        Answer::lost(),
        Answer::lost(),
        Answer::lost(),
        found_only("key-0", "m0"),
    ])
    .await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(template(recipients(2)))
        .await
        .unwrap_err();

    assert_eq!(error.code(), "network_error");
    assert_eq!(batch_requests(&server).len(), 3);
    assert_eq!(
        queries_read(&server),
        vec!["idempotency_key=key-0", "idempotency_key=key-1"]
    );
    let result = error.batch_result().unwrap();
    assert_eq!(
        result
            .results
            .iter()
            .map(|r| (r.status, r.message_id.as_deref()))
            .collect::<Vec<_>>(),
        vec![(Unconfirmed, Some("m0")), (Unconfirmed, None)]
    );
    assert_eq!(
        error_codes(result),
        vec![
            Some("delivery_unavailable".to_string()),
            Some("delivery_unavailable".to_string())
        ]
    );
}

#[tokio::test]
async fn reading_by_key_stops_when_posthog_stays_unreachable() {
    let server = ScriptedServer::start_async(vec![
        whole_call(503, "delivery_unavailable"),
        whole_call(503, "delivery_unavailable"),
        whole_call(503, "delivery_unavailable"),
        Answer::lost(),
    ])
    .await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(template(recipients(3)))
        .await
        .unwrap_err();

    assert_eq!(server.requests().len(), 4);
    assert_eq!(
        statuses(error.batch_result().unwrap()),
        vec![Unconfirmed; 3]
    );
}

#[tokio::test]
async fn a_key_with_reserved_characters_is_encoded_in_the_read() {
    let server =
        ScriptedServer::start_async(vec![Answer::lost(), whole_call(404, "not_found")]).await;
    let client = client_for(&server).await;

    client
        .emails()
        .batch(
            template(vec![
                BatchRecipient::new("a@example.com").idempotency_key("order#1&x=2+y")
            ])
            .max_retries(0),
        )
        .await
        .unwrap_err();

    let read = &server.requests()[1];
    assert_eq!(read.path, STATUS_BY_KEY_PATH);
    let url = reqwest::Url::parse(&format!("http://example.com/?{}", read.query)).unwrap();
    assert_eq!(
        url.query_pairs().into_owned().collect::<Vec<_>>(),
        vec![("idempotency_key".to_string(), "order#1&x=2+y".to_string())]
    );
}

#[tokio::test]
async fn a_request_another_request_still_holds_stays_unconfirmed_after_a_refusal() {
    let server = ScriptedServer::start_async(vec![
        whole_call(409, "idempotency_key_in_flight"),
        whole_call(400, "invalid_input"),
        whole_call(404, "not_found"),
    ])
    .await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(template(recipients(2)))
        .await
        .unwrap_err();

    assert_eq!(error.code(), "invalid_input");
    assert_eq!(batch_requests(&server).len(), 2);
    assert_eq!(queries_read(&server).len(), 2);
    let result = error.batch_result().unwrap();
    assert_eq!(statuses(result), vec![Unconfirmed, Unconfirmed]);
    let only = &result.results[0];
    assert_eq!(
        only.error.as_ref().unwrap().code(),
        "idempotency_key_in_flight"
    );
    assert_eq!(only.later_error.as_ref().unwrap().code(), "invalid_input");
}

#[tokio::test]
async fn batch_unavailable_is_not_retried_and_says_to_send_singly() {
    let server = ScriptedServer::start_async(vec![whole_call(409, "batch_unavailable")]).await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(template(recipients(2)))
        .await
        .unwrap_err();

    assert_eq!(server.requests().len(), 1);
    assert_eq!(error.code(), "batch_unavailable");
    assert_eq!(error.kind(), EmailErrorKind::ApplicationError);
    assert_eq!(
        error.fix(),
        Some("Until batch sends are on for the project, send each email the batch didn't queue with send() and its recipient's idempotency_key.")
    );
    assert_eq!(
        statuses(error.batch_result().unwrap()),
        vec![Failed, Failed]
    );
}

#[tokio::test]
async fn a_404_says_batch_sends_are_unavailable() {
    for answer in [
        Answer::xml(404, "<h1>Not Found</h1>"),
        Answer::json(
            404,
            json!({"type": "invalid_request", "code": "not_found", "detail": "Not found."}),
        ),
    ] {
        let server = ScriptedServer::start_async(vec![answer]).await;
        let client = client_for(&server).await;

        let error = client
            .emails()
            .batch(template(recipients(1)))
            .await
            .unwrap_err();

        assert_eq!(error.code(), "not_found");
        assert_eq!(error.kind(), EmailErrorKind::NotFound);
        assert!(error.why().unwrap().contains("batch sends"));
        assert_eq!(
            error.fix(),
            Some("Check template_id, or send each email the batch didn't queue with send() and its recipient's idempotency_key.")
        );
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn an_answer_without_a_result_per_recipient_leaves_the_request_unconfirmed() {
    let server =
        ScriptedServer::start_async(vec![ok(vec![queued("m0")]), whole_call(404, "not_found")])
            .await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(template(recipients(2)).max_retries(0))
        .await
        .unwrap_err();

    assert_eq!(error.code(), "api_error");
    assert_eq!(
        statuses(error.batch_result().unwrap()),
        vec![Unconfirmed, Unconfirmed]
    );
    assert_eq!(
        server
            .requests()
            .iter()
            .map(|request| request.path.as_str())
            .collect::<Vec<_>>(),
        vec![BATCH_PATH, STATUS_BY_KEY_PATH, STATUS_BY_KEY_PATH]
    );
}

fn queued_with_warning() -> Answer {
    Answer::of_request(|request| {
        let slots: Vec<Value> = keys_of(request).iter().map(|key| queued(key)).collect();
        Answer::json(
            200,
            json!({
                "results": slots,
                "warnings": [{"code": "sandbox_sender", "detail": "Sent from the test sender."}],
            }),
        )
    })
}

#[tokio::test]
async fn warnings_reach_the_caller_once() {
    let server = ScriptedServer::start_async(vec![queued_with_warning()]).await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .batch(template(recipients(150)))
        .await
        .unwrap();

    assert_eq!(server.requests().len(), 2);
    assert_eq!(
        result
            .warnings
            .iter()
            .map(|warning| warning.code.as_str())
            .collect::<Vec<_>>(),
        vec!["sandbox_sender"]
    );
}

#[tokio::test]
async fn warnings_reach_the_caller_when_a_later_request_fails() {
    let server = ScriptedServer::start_async(vec![
        queued_with_warning(),
        whole_call(429, "quota_limited"),
    ])
    .await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(template(recipients(150)))
        .await
        .unwrap_err();

    let warnings = &error.batch_result().unwrap().warnings;
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, "sandbox_sender");
}

#[tokio::test]
async fn a_client_that_cant_send_sends_nothing() {
    let server = ScriptedServer::start_async(vec![all_queued()]).await;
    let disabled = posthog::client(options(&server.url).disabled(true).build().unwrap()).await;
    let error = disabled
        .emails()
        .batch(template(recipients(1)))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "client_disabled");
    assert!(error.batch_result().is_none());

    let off_cloud = posthog::client(
        posthog::ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .host(server.url.clone())
            .secret_key("phs_test")
            .build()
            .unwrap(),
    )
    .await;
    let error = off_cloud
        .emails()
        .batch(template(recipients(1)))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "missing_app_host");

    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn a_refused_recipient_field_names_its_position_in_the_batch() {
    let cases = [
        (
            "recipients__37__idempotency_key".to_string(),
            "recipients__137__idempotency_key".to_string(),
        ),
        (
            "recipients__\u{b2}__idempotency_key".to_string(),
            "recipients__\u{b2}__idempotency_key".to_string(),
        ),
        (
            format!("recipients__{}__idempotency_key", "9".repeat(5000)),
            "recipients".to_string(),
        ),
        ("recipients__50".to_string(), "recipients".to_string()),
        ("template_id".to_string(), "template_id".to_string()),
    ];
    for (attr, field) in cases {
        let server = ScriptedServer::start_async(vec![
            all_queued(),
            Answer::json(
                400,
                json!({
                    "type": "validation_error",
                    "code": "max_length",
                    "detail": "Ensure this field has no more than 255 characters.",
                    "attr": attr,
                }),
            ),
        ])
        .await;
        let client = client_for(&server).await;

        let error = client
            .emails()
            .batch(template(recipients(150)))
            .await
            .unwrap_err();

        assert_eq!(error.field(), Some(field.as_str()));
        let mut expected = vec![Queued; 100];
        expected.extend(vec![Failed; 50]);
        assert_eq!(statuses(error.batch_result().unwrap()), expected);
    }
}

/// Waits until the server has a request, or fails after 10 seconds.
async fn wait_for_first_request(server: &ScriptedServer) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while server.requests().is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the server got no request"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn batch_during_shutdown(
    answers: Vec<Answer>,
    batch: TemplateBatch,
) -> (ScriptedServer, posthog::EmailError) {
    let server = ScriptedServer::start_async(answers).await;
    let client = Arc::new(client_for(&server).await);
    let sender = Arc::clone(&client);
    let sent = tokio::spawn(async move { sender.emails().batch(batch).await });
    wait_for_first_request(&server).await;

    client.shutdown().await;

    let error = sent.await.unwrap().unwrap_err();
    (server, error)
}

#[tokio::test]
async fn a_shutdown_during_a_batch_stops_it_before_the_next_request() {
    let (server, error) = batch_during_shutdown(
        vec![all_queued().after(Duration::from_millis(500))],
        template(recipients(150)),
    )
    .await;

    assert_eq!(error.code(), "client_closed");
    assert_eq!(server.requests().len(), 1);
    let result = error.batch_result().unwrap();
    let mut expected = vec![Queued; 100];
    expected.extend(vec![NotAttempted; 50]);
    assert_eq!(statuses(result), expected);
    assert_eq!(error_codes(result)[100].as_deref(), Some("client_closed"));
}

#[tokio::test]
async fn a_shutdown_during_a_retry_wait_sends_no_retry() {
    let (server, error) = batch_during_shutdown(
        vec![whole_call(429, "throttled"), all_queued()],
        template(recipients(150)),
    )
    .await;

    assert_eq!(error.code(), "client_closed");
    assert_eq!(server.requests().len(), 1);
    let result = error.batch_result().unwrap();
    let mut expected = vec![Failed; 100];
    expected.extend(vec![NotAttempted; 50]);
    assert_eq!(statuses(result), expected);
    assert_eq!(error_codes(result)[0].as_deref(), Some("client_closed"));
}

#[tokio::test]
async fn a_shutdown_before_a_lost_answer_reads_no_email_by_its_key() {
    let (server, error) = batch_during_shutdown(
        vec![
            whole_call(503, "delivery_unavailable").after(Duration::from_millis(500)),
            status_found("m0"),
        ],
        template(recipients(2)).max_retries(0),
    )
    .await;

    assert_eq!(error.code(), "delivery_unavailable");
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        statuses(error.batch_result().unwrap()),
        vec![Unconfirmed, Unconfirmed]
    );
}
