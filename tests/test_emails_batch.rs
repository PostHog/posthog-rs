#![cfg(feature = "async-client")]

mod email_support;

use email_support::{
    all_queued, batch_requests, keys_of, ok, options, queries_read, queued, recipients, refused,
    status_found, whole_call, Answer, ScriptedServer, BATCH_PATH, STATUS_BY_KEY_PATH, TEMPLATE_ID,
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

fn statuses(result: &SendBatchResult) -> Vec<BatchEmailStatus> {
    result.results.iter().map(|result| result.status).collect()
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
        ("recipient_suppressed", Some(422), None)
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
    let cases: Vec<(posthog::SendBatch, &str, &str, &str)> = vec![
        (
            TemplateBatch::new(TEMPLATE_ID, Vec::new()).into(),
            "invalid_recipients",
            "recipients",
            "recipients must hold at least one recipient.",
        ),
        (
            TemplateBatch::new("welcome", recipients(1)).into(),
            "invalid_template_id",
            "template_id",
            "template_id must be the template's UUID.",
        ),
        (
            TemplateBatch::new(TEMPLATE_ID, recipients(1))
                .template_data(json!([1]))
                .into(),
            "invalid_template_data",
            "template_data",
            "template_data must be a JSON object.",
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
            "recipients[1]: to must be one email address as a nonempty string.",
        ),
        (
            TemplateBatch::new(
                TEMPLATE_ID,
                vec![BatchRecipient::new("a@example.com").distinct_id("")],
            )
            .into(),
            "invalid_distinct_id",
            "recipients__0__distinct_id",
            "recipients[0]: distinct_id must be a nonempty string.",
        ),
        (
            TemplateBatch::new(
                TEMPLATE_ID,
                vec![BatchRecipient::new("a@example.com").idempotency_key("has space")],
            )
            .into(),
            "invalid_idempotency_key",
            "recipients__0__idempotency_key",
            "recipients[0]: idempotency_key must be 1 to 255 visible ASCII characters without spaces.",
        ),
        (
            TemplateBatch::new(TEMPLATE_ID, repeated).into(),
            "invalid_idempotency_key",
            "recipients__2__idempotency_key",
            "recipients[2] has the same idempotency_key as recipients[0]. Give each recipient its own key.",
        ),
        (
            TemplateBatch::new(
                TEMPLATE_ID,
                vec![BatchRecipient::new("a@example.com").template_data(json!([1]))],
            )
            .into(),
            "invalid_template_data",
            "recipients__0__template_data",
            "recipients[0]: template_data must be a JSON object.",
        ),
        (
            CodeBatch::new("digest", "Hi", recipients(1)).into(),
            "invalid_form",
            "html",
            "An email with template_key needs html, text or both.",
        ),
        (
            CodeBatch::new("has space", "Hi", recipients(1))
                .text("x")
                .into(),
            "invalid_template_key",
            "template_key",
            "template_key can only have letters, digits, hyphens (-) and underscores (_), at most 400.",
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
            "recipients[0] has template_data, but a batch with template_key sends the same content to every recipient. Send personalized content with a template batch or with send().",
        ),
    ];
    for (batch, code, field, message) in cases {
        let error = client.emails().batch(batch).await.unwrap_err();
        assert_eq!(
            (error.code(), error.field(), error.message()),
            (code, Some(field), message)
        );
        assert_eq!(error.kind(), EmailErrorKind::ValidationError, "{}", code);
        assert!(error.batch_result().is_none(), "{}", code);
    }
    assert!(server.requests().is_empty());
}

fn template(people: Vec<BatchRecipient>) -> TemplateBatch {
    TemplateBatch::new(TEMPLATE_ID, people)
}

fn keys_sent(server: &ScriptedServer) -> Vec<Vec<String>> {
    batch_requests(server).iter().map(keys_of).collect()
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

/// Every object key and array element below `roots`, or `None` for an
/// object or array deeper than 64 levels, as PostHog's pipeline counts them.
fn nodes_of(roots: &[&Value]) -> Option<usize> {
    let mut nodes = 0;
    let mut pending: Vec<(&Value, usize)> = roots.iter().map(|root| (*root, 1)).collect();
    while let Some((value, level)) = pending.pop() {
        let children: Vec<&Value> = match value {
            Value::Object(fields) => fields.values().collect(),
            Value::Array(items) => items.iter().collect(),
            _ => continue,
        };
        if level > 64 {
            return None;
        }
        nodes += children.len();
        pending.extend(children.into_iter().map(|child| (child, level + 1)));
    }
    Some(nodes)
}

/// Whether PostHog's pipeline refuses the request for what it copies for
/// each recipient: the shared data and content, and the recipient's own data.
fn exceeds_copy_limits(body: &Value) -> bool {
    let empty = json!({});
    let recipients = body["recipients"].as_array().unwrap();
    let shared = body.get("message_data").unwrap_or(&empty);
    let send = body.get("template_key").map(|_| {
        let content = &body["text"];
        json!({"content": {"subject": body["subject"], "html": body["html"], "text": if content.is_null() { &body["html"] } else { content }}})
    });
    let own: Vec<&Value> = recipients
        .iter()
        .map(|recipient| recipient.get("message_data").unwrap_or(&empty))
        .collect();
    let keys = |value: &Value| value.as_object().map_or(0, |fields| fields.len());
    let copied_keys: usize = own.iter().map(|data| keys(shared) + keys(data)).sum();
    let mut roots = vec![shared];
    roots.extend(send.as_ref());
    let Some(shared_nodes) = nodes_of(&roots) else {
        return true;
    };
    let Some(own_nodes) = own
        .iter()
        .map(|data| nodes_of(&[data]))
        .sum::<Option<usize>>()
    else {
        return true;
    };
    let shared_bytes =
        shared.to_string().len() + send.as_ref().map_or(0, |send| send.to_string().len());
    let copied_bytes: usize = own
        .iter()
        .map(|data| shared_bytes + data.to_string().len())
        .sum();
    copied_keys > 50_000
        || shared_nodes * own.len() + own_nodes > 500_000
        || copied_bytes > 16 * 1024 * 1024
}

/// Queues every recipient, unless the request copies more than PostHog's
/// pipeline takes, which it refuses whole with `413`.
fn within_copy_limits() -> Answer {
    Answer::of_request(|request| {
        if exceeds_copy_limits(&request.json()) {
            return whole_call(413, "content_too_large");
        }
        ok(keys_of(request)
            .iter()
            .map(|key| queued(&format!("msg_{key}")))
            .collect())
    })
}

fn nested(levels: usize) -> Value {
    (1..levels).fold(json!({}), |inner, _| json!({ "a": inner }))
}

#[tokio::test]
async fn a_request_holds_as_many_recipients_as_posthog_copies_data_for() {
    let keys =
        |count: usize| Value::Object((0..count).map(|key| (key.to_string(), json!(0))).collect());
    let cases = [
        (json!({"note": "x".repeat(262_131)}), vec![64, 36]),
        (keys(500), vec![100]),
        (keys(501), vec![99, 1]),
        (json!({"a": vec![0; 4999]}), vec![100]),
        (json!({"a": vec![0; 5000]}), vec![99, 1]),
    ];
    for (shared, sizes) in cases {
        let server = ScriptedServer::start_async(vec![within_copy_limits()]).await;
        let client = client_for(&server).await;

        let result = client
            .emails()
            .batch(template(recipients(100)).template_data(shared))
            .await
            .unwrap();

        assert_eq!(
            keys_sent(&server).iter().map(Vec::len).collect::<Vec<_>>(),
            sizes
        );
        assert_eq!(statuses(&result), vec![Queued; 100]);
    }
}

#[tokio::test]
async fn a_recipient_whose_data_nests_too_deep_fails_alone() {
    let mut people = recipients(3);
    people[0] = people[0].clone().template_data(nested(64));
    people[1] = people[1].clone().template_data(nested(65));
    let server = ScriptedServer::start_async(vec![within_copy_limits()]).await;
    let client = client_for(&server).await;

    let result = client.emails().batch(template(people)).await.unwrap();

    assert_eq!(keys_sent(&server), vec![vec!["key-0", "key-2"]]);
    assert_eq!(statuses(&result), vec![Queued, Failed, Queued]);
    assert_eq!(
        error_codes(&result)[1].as_deref(),
        Some("content_too_large")
    );
}

#[tokio::test]
async fn a_code_batch_counts_html_without_text_twice() {
    let server = ScriptedServer::start_async(vec![within_copy_limits()]).await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .batch(CodeBatch::new("digest", "News", recipients(100)).html("x".repeat(200 * 1024)))
        .await
        .unwrap();

    assert!(batch_requests(&server).len() > 1);
    assert_eq!(statuses(&result), vec![Queued; 100]);
}

#[tokio::test]
async fn a_request_fills_up_to_exactly_4_mib() {
    let base = json!({"template_id": TEMPLATE_ID, "message_data": {}, "recipients": []})
        .to_string()
        .len();
    let item = json!({"to": "a@example.com", "idempotency_key": "k1", "message_data": {"n": ""}})
        .to_string()
        .len();
    let notes = FOUR_MIB - base - 3 * item - 2;
    let one = |key: &str, note: usize| {
        BatchRecipient::new("a@example.com")
            .idempotency_key(key)
            .template_data(json!({ "n": "a".repeat(note) }))
    };
    let server = ScriptedServer::start_async(vec![all_queued()]).await;
    let client = client_for(&server).await;

    client
        .emails()
        .batch(template(vec![
            one("k1", notes / 3),
            one("k2", notes / 3),
            one("k3", notes - 2 * (notes / 3)),
            one("k4", 0),
        ]))
        .await
        .unwrap();

    assert_eq!(batch_requests(&server)[0].body.len(), FOUR_MIB);
    assert_eq!(
        keys_sent(&server).iter().map(Vec::len).collect::<Vec<_>>(),
        vec![3, 1]
    );
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
    let logged = format!("{error:?}");
    assert!(!logged.contains("user0@example.com") && !logged.contains("key-0"));
    assert!(logged.contains("quota_limited"));
}

#[tokio::test]
async fn reading_by_key_stops_when_posthog_stays_unreachable() {
    let server = ScriptedServer::start_async(vec![
        whole_call(503, "delivery_unavailable"),
        Answer::lost(),
    ])
    .await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(template(recipients(3)).max_retries(0))
        .await
        .unwrap_err();

    assert_eq!(batch_requests(&server).len(), 1);
    assert_eq!(queries_read(&server), vec!["idempotency_key=key-0"]);
    assert_eq!(
        statuses(error.batch_result().unwrap()),
        vec![Unconfirmed; 3]
    );
}

#[tokio::test]
async fn a_key_with_reserved_characters_is_encoded_in_the_read() {
    let server = ScriptedServer::start_async(vec![
        whole_call(503, "delivery_unavailable"),
        whole_call(404, "not_found"),
    ])
    .await;
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
async fn one_ambiguous_answer_leaves_the_request_unconfirmed_without_a_later_error() {
    for (answer, code) in [
        (ok(vec![queued("m0")]), "api_error"),
        (
            whole_call(409, "idempotency_key_in_flight"),
            "idempotency_key_in_flight",
        ),
    ] {
        let server = ScriptedServer::start_async(vec![answer, whole_call(404, "not_found")]).await;
        let client = client_for(&server).await;

        let error = client
            .emails()
            .batch(template(recipients(2)).max_retries(0))
            .await
            .unwrap_err();

        assert_eq!(error.code(), code);
        let result = error.batch_result().unwrap();
        assert_eq!(statuses(result), vec![Unconfirmed, Unconfirmed]);
        assert!(
            result.results.iter().all(|r| r.later_error.is_none()),
            "{}",
            code
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

/// Shuts the client down while the batch's first request waits for its
/// answer: the batch is admitted before `shutdown` runs.
async fn batch_during_shutdown(
    answers: Vec<Answer>,
    batch: TemplateBatch,
) -> (ScriptedServer, posthog::EmailError) {
    let server = ScriptedServer::start_async(answers).await;
    let client = client_for(&server).await;

    let emails = client.emails();
    let (sent, ()) = tokio::join!(emails.batch(batch), client.shutdown());

    (server, sent.unwrap_err())
}

#[tokio::test]
async fn a_shutdown_during_a_batch_stops_it_before_the_next_request() {
    let (server, error) =
        batch_during_shutdown(vec![all_queued()], template(recipients(150))).await;

    assert_eq!(error.code(), "client_closed");
    assert_eq!(server.requests().len(), 1);
    let result = error.batch_result().unwrap();
    let mut expected = vec![Queued; 100];
    expected.extend(vec![NotAttempted; 50]);
    assert_eq!(statuses(result), expected);
    assert_eq!(error_codes(result)[100].as_deref(), Some("client_closed"));
}

#[tokio::test]
async fn a_shutdown_before_a_lost_answer_reads_no_email_by_its_key() {
    let (server, error) = batch_during_shutdown(
        vec![whole_call(503, "delivery_unavailable"), status_found("m0")],
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
