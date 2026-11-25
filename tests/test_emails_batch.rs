#![cfg(feature = "async-client")]

mod email_support;

use email_support::{
    all_queued, keys_of, ok, options, queued, recipients, refused, says_to_resend_with_its_key,
    status_found, whole_call, Answer, ScriptedServer, BATCH_PATH, TEMPLATE_ID,
};
use posthog::{
    BatchEmailStatus, BatchRecipient, CodeBatch, EmailErrorKind, EmailTracking, SendBatchResult,
    TemplateBatch,
};
use serde_json::{json, Value};

use BatchEmailStatus::{Failed, Queued, Unconfirmed};

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
    let mut too_many_with_bad_data = recipients(101);
    too_many_with_bad_data[0] = BatchRecipient::new("a@example.com").template_data(json!([1]));
    let cases: Vec<(posthog::SendBatch, &str, &str, &str)> = vec![
        (
            TemplateBatch::new(TEMPLATE_ID, Vec::new()).into(),
            "invalid_recipients",
            "recipients",
            "recipients must hold at least one recipient.",
        ),
        (
            TemplateBatch::new(TEMPLATE_ID, recipients(101)).into(),
            "invalid_recipients",
            "recipients",
            "recipients holds 101 recipients, and a batch takes at most 100.",
        ),
        (
            TemplateBatch::new(TEMPLATE_ID, too_many_with_bad_data).into(),
            "invalid_recipients",
            "recipients",
            "recipients holds 101 recipients, and a batch takes at most 100.",
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

#[tokio::test]
async fn more_than_100_recipients_say_to_split_the_batch() {
    let server = ScriptedServer::start_async(vec![all_queued()]).await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(CodeBatch::new("digest", "News", recipients(101)).text("x"))
        .await
        .unwrap_err();

    assert_eq!(
        error.fix(),
        Some("Split the recipients into batches of at most 100.")
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn a_batch_over_4_mib_goes_out_in_one_request() {
    let server = ScriptedServer::start_async(vec![all_queued()]).await;
    let client = client_for(&server).await;
    let people = recipients(5)
        .into_iter()
        .map(|person| person.template_data(json!({"note": "x".repeat(900 * 1024)})))
        .collect();

    let result = client.emails().batch(template(people)).await.unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].body.len() > 4 * 1024 * 1024);
    assert_eq!(statuses(&result), vec![Queued; 5]);
}

#[tokio::test]
async fn a_batch_posthog_finds_too_large_returns_its_413() {
    for answer in [
        Answer::xml(413, "<h1>Request Entity Too Large</h1>"),
        Answer::json(413, json!({"code": 413, "detail": " ", "fix": ""})),
        whole_call(413, "content_too_large"),
    ] {
        let server = ScriptedServer::start_async(vec![answer]).await;
        let client = client_for(&server).await;

        let error = client
            .emails()
            .batch(template(recipients(100)))
            .await
            .unwrap_err();

        assert_eq!(server.requests().len(), 1);
        assert_eq!(
            (error.code(), error.status()),
            ("content_too_large", Some(413))
        );
        assert_eq!(
            error.fix(),
            Some("Split the recipients into smaller batches.")
        );
        let result = error.batch_result().unwrap();
        assert_eq!(statuses(result), vec![Failed; 100]);
    }
}

#[tokio::test]
async fn shared_data_posthog_finds_too_large_returns_its_400_and_own_fix() {
    let detail = "The content or `message_data` that a batch sends to every recipient can be at most 256 KiB. Send larger emails one at a time.";
    let server = ScriptedServer::start_async(vec![Answer::of_request(move |request| {
        if request.json()["message_data"].to_string().len() > 256 * 1024 {
            Answer::json(
                400,
                json!({"type": "invalid_request", "code": "content_too_large", "detail": detail}),
            )
        } else {
            all_queued()
        }
    })])
    .await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(template(recipients(1)).template_data(json!({"note": "x".repeat(300 * 1024)})))
        .await
        .unwrap_err();

    assert_eq!(
        (error.code(), error.status(), error.message(), error.fix()),
        ("content_too_large", Some(400), detail, None)
    );
}

fn template(people: Vec<BatchRecipient>) -> TemplateBatch {
    TemplateBatch::new(TEMPLATE_ID, people)
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
    for (answered, code) in [
        ("delivery_unavailable", "delivery_unavailable"),
        ("idempotency_key_in_flight", "idempotency_key_in_flight"),
        (" ", "api_error"),
    ] {
        let server =
            ScriptedServer::start_async(vec![ok(vec![refused("m0", answered, 503)])]).await;
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
        assert!(says_to_resend_with_its_key(only.error.as_ref()), "{}", code);
        assert_eq!(server.requests().len(), 1, "{}", code);
    }
}

#[tokio::test]
async fn an_unconfirmed_recipient_is_not_read_by_key_and_says_to_resend_with_its_key() {
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

    assert_eq!(server.requests().len(), 1);
    let second = &result.results[1];
    assert_eq!(
        (
            second.status,
            second.message_id.as_deref(),
            second.idempotency_key.as_str()
        ),
        (Unconfirmed, None, "key-1")
    );
    assert_eq!(
        second.error.as_ref().unwrap().code(),
        "delivery_unavailable"
    );
    assert!(says_to_resend_with_its_key(second.error.as_ref()));
}

#[tokio::test]
async fn a_refused_request_returns_every_recipients_result() {
    let server = ScriptedServer::start_async(vec![whole_call(429, "quota_limited")]).await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(template(recipients(3)))
        .await
        .unwrap_err();

    assert_eq!((error.code(), error.status()), ("quota_limited", Some(429)));
    assert_eq!(server.requests().len(), 1);
    let result = error.batch_result().unwrap();
    assert_eq!(statuses(result), vec![Failed; 3]);
    let refused = result.results[1].error.as_ref().unwrap();
    assert_eq!(
        (refused.code(), refused.message()),
        ("quota_limited", "Nope.")
    );
    assert_eq!(result.results[2].idempotency_key, "key-2");
    let logged = format!("{error:?}");
    assert!(!logged.contains("user0@example.com") && !logged.contains("key-0"));
    assert!(logged.contains("quota_limited"));
}

#[tokio::test]
async fn a_batch_that_fails_reads_no_email_by_key() {
    let server = ScriptedServer::start_async(vec![
        whole_call(503, "delivery_unavailable"),
        status_found("m0"),
    ])
    .await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .batch(template(recipients(3)).max_retries(0))
        .await
        .unwrap_err();

    assert_eq!(server.requests().len(), 1);
    let result = error.batch_result().unwrap();
    assert_eq!(statuses(result), vec![Unconfirmed; 3]);
    assert!(result
        .results
        .iter()
        .all(|result| result.message_id.is_none()
            && says_to_resend_with_its_key(result.error.as_ref())));
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
async fn a_404_without_a_field_says_the_template_or_batch_sends_are_missing() {
    let template_fix = "Check template_id. If it is right, send each recipient on its own with send() and its idempotency_key until batch sends are available.";
    let code_fix = "If this project can send email from code, send each recipient on its own with send() and its idempotency_key until batch sends are available.";
    let forms: [(posthog::SendBatch, &str, &str); 2] = [
        (
            template(recipients(2)).into(),
            "PostHog found no template with this ID, or batch sends aren't available for this project, so it sent no email of this request.",
            template_fix,
        ),
        (
            CodeBatch::new("digest", "News", recipients(2)).text("x").into(),
            "Batch sends, or sending email from code, aren't available for this project, so PostHog sent no email of this request.",
            code_fix,
        ),
    ];
    for (batch, message, fix) in forms {
        for answer in [
            Answer::json(
                404,
                json!({"type": "invalid_request", "code": "not_found", "detail": "Not found.", "attr": null}),
            ),
            Answer::json(
                404,
                json!({"type": "invalid_request", "code": "not_found", "detail": "Endpoint not found."}),
            ),
            Answer::xml(404, "<h1>Not Found</h1>"),
        ] {
            let server = ScriptedServer::start_async(vec![answer]).await;
            let client = client_for(&server).await;

            let error = client.emails().batch(batch.clone()).await.unwrap_err();

            assert_eq!((error.code(), error.status()), ("not_found", Some(404)));
            assert_eq!(error.kind(), EmailErrorKind::NotFound);
            assert_eq!((error.message(), error.fix()), (message, Some(fix)));
            assert_eq!(error.why(), None);
            assert_eq!(
                statuses(error.batch_result().unwrap()),
                vec![Failed, Failed]
            );
            assert_eq!(server.requests().len(), 1);
        }
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
        (Answer::xml(409, "<h1>Conflict</h1>"), "api_error"),
        (Answer::json(409, json!({"code": " "})), "api_error"),
        (
            Answer::json(409, json!({"code": 409, "detail": " "})),
            "api_error",
        ),
        (Answer::json(409, json!({"code": "api_error"})), "api_error"),
        (Answer::json(409, json!({"code": "conflict"})), "conflict"),
    ] {
        let server = ScriptedServer::start_async(vec![answer, status_found("m0")]).await;
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
            result
                .results
                .iter()
                .all(|r| r.later_error.is_none() && says_to_resend_with_its_key(r.error.as_ref())),
            "{}",
            code
        );
        assert_eq!(
            server
                .requests()
                .iter()
                .map(|request| request.path.as_str())
                .collect::<Vec<_>>(),
            vec![BATCH_PATH]
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
async fn warnings_reach_the_caller() {
    let server = ScriptedServer::start_async(vec![queued_with_warning()]).await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .batch(template(recipients(2)))
        .await
        .unwrap();

    assert_eq!(server.requests().len(), 1);
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
