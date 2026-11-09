#![cfg(feature = "async-client")]

mod common;
mod email_support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::default_user_agent;
use email_support::{
    accepted, closed_url, options, unavailable, Answer, ScriptedServer, MESSAGE_ID, SEND_PATH,
    TEMPLATE_ID, UPLOADS_PATH,
};
use posthog_rs::{CodeEmail, EmailAttachment, EmailDeliveryStatus, EmailErrorKind, TemplateEmail};
use serde_json::json;

async fn client_for(server: &ScriptedServer) -> posthog_rs::Client {
    posthog_rs::client(options(&server.url).build().unwrap()).await
}

#[tokio::test]
async fn template_send_posts_the_flat_body_with_auth_and_a_uuid_key() {
    let server = ScriptedServer::start_async(vec![Answer::json(
        202,
        json!({"message_id": MESSAGE_ID, "warnings": [{"code": "w", "detail": "Heads up"}]}),
    )])
    .await;
    let client = posthog_rs::client(
        options(&server.url)
            .migrated_from("resend")
            .build()
            .unwrap(),
    )
    .await;

    let result = client
        .emails()
        .send(
            TemplateEmail::new("user@example.com", TEMPLATE_ID.to_uppercase())
                .template_data(json!({"name": "Ada"}))
                .distinct_id("user_123"),
        )
        .await
        .unwrap();

    assert_eq!(result.id, MESSAGE_ID);
    assert!(!result.replayed);
    assert_eq!(result.warnings.len(), 1);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, SEND_PATH);
    assert_eq!(
        request.json(),
        json!({
            "template_id": TEMPLATE_ID,
            "to": "user@example.com",
            "message_data": {"name": "Ada"},
            "distinct_id": "user_123",
        })
    );
    assert_eq!(request.header("authorization"), Some("Bearer phs_test"));
    assert_eq!(request.header("accept"), Some("application/json"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(
        request.header("user-agent"),
        Some(default_user_agent().as_str())
    );
    assert_eq!(request.header("x-posthog-migrated-from"), Some("resend"));
    let key = uuid::Uuid::parse_str(request.header("idempotency-key").unwrap()).unwrap();
    assert_eq!(key.get_version_num(), 4);
}

#[tokio::test]
async fn code_send_posts_the_rendered_content_and_its_options() {
    let server = ScriptedServer::start_async(vec![Answer::json(
        200,
        json!({"message_id": MESSAGE_ID, "replayed": true}),
    )])
    .await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .send(
            CodeEmail::new("user@example.com", "password-reset", "Reset your password")
                .html("<p>Reset</p>")
                .text("Reset")
                .from("Acme <security@example.com>")
                .reply_to("help@example.com")
                .capture_engagement(false)
                .distinct_id("user_123")
                .idempotency_key("password-reset:user_123"),
        )
        .await
        .unwrap();

    assert!(result.replayed);
    let request = &server.requests()[0];
    assert_eq!(
        request.json(),
        json!({
            "template_key": "password-reset",
            "subject": "Reset your password",
            "html": "<p>Reset</p>",
            "text": "Reset",
            "from": "Acme <security@example.com>",
            "reply_to": ["help@example.com"],
            "capture_engagement": false,
            "distinct_id": "user_123",
            "to": "user@example.com",
        })
    );
    assert_eq!(
        request.header("idempotency-key"),
        Some("password-reset:user_123")
    );
}

const PDF_SHA256: &str = "ccac55e87ec85819f85630feb88522f771cf4d4fc3a40b7f97fd124a128dfcb7";
const PNG_SHA256: &str = "0f4636c78f65d3639ece5a064b5ae753e3408614a14fb18ab4d7540d2c248543";
const PDF: [u8; 7] = [0x25, 0x50, 0x44, 0x46, 0x00, 0x9f, 0xff];
const PNG: [u8; 4] = [0x89, 0x50, 0x4e, 0x47];
/// The largest file one email can carry with empty bodies, from the spec.
const LARGEST_FILE: usize = 29_182_875;
const LARGEST_FILE_SHA256: &str =
    "28f197eae8bd57e7c2a94d2062d2cef0a04b34e7fd3e0c743d88cda2217f069c";

fn upload_slot(bucket: &ScriptedServer, sha256: &str, size: usize) -> serde_json::Value {
    json!({
        "sha256": sha256,
        "status": "upload",
        "url": format!("{}/bucket/team/2/{sha256}?X-Amz-Signature=signed", bucket.url),
        "method": "PUT",
        "headers": {
            "content-length": size.to_string(),
            "x-amz-checksum-sha256": "signed-checksum",
        },
        "url_expires_at": "2026-11-09T12:15:00Z",
    })
}

fn present_slot(sha256: &str) -> serde_json::Value {
    json!({"sha256": sha256, "status": "present", "expires_at": "2026-12-16T12:00:00Z"})
}

fn invoice_with_copies_and_files() -> CodeEmail {
    CodeEmail::new("user@example.com", "invoice", "Your invoice")
        .html(r#"<img src="cid:logo">"#)
        .cc("finance@example.com")
        .cc("boss@example.com")
        .bcc("audit@example.com")
        .attachment(EmailAttachment::new("invoice.pdf", PDF).content_type("application/pdf"))
        .attachment(EmailAttachment::new("logo.png", PNG).content_id("logo"))
}

#[tokio::test]
async fn code_send_uploads_the_files_posthog_lacks_then_sends_their_hashes() {
    let bucket = ScriptedServer::start_async(vec![Answer::xml(200, "")]).await;
    let server = ScriptedServer::start_async(vec![
        Answer::json(
            200,
            json!({"files": [upload_slot(&bucket, PDF_SHA256, PDF.len()), present_slot(PNG_SHA256)]}),
        ),
        accepted(),
    ])
    .await;
    let client = client_for(&server).await;

    client
        .emails()
        .send(invoice_with_copies_and_files().idempotency_key("invoice:42"))
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let (uploads, send) = (&requests[0], &requests[1]);
    assert_eq!(
        (uploads.method.as_str(), uploads.path.as_str()),
        ("POST", UPLOADS_PATH)
    );
    assert_eq!(
        uploads.json(),
        json!({"files": [{"sha256": PDF_SHA256, "size": 7}, {"sha256": PNG_SHA256, "size": 4}]})
    );
    assert_eq!(uploads.header("authorization"), Some("Bearer phs_test"));
    assert_eq!(uploads.header("content-type"), Some("application/json"));
    assert_eq!(uploads.header("idempotency-key"), None);

    let puts = bucket.requests();
    assert_eq!(puts.len(), 1, "only the file PostHog lacks is uploaded");
    let put = &puts[0];
    assert_eq!(put.method, "PUT");
    assert_eq!(put.path, format!("/bucket/team/2/{PDF_SHA256}"));
    assert_eq!(put.body, PDF);
    assert_eq!(put.header("content-length"), Some("7"));
    assert_eq!(put.header("x-amz-checksum-sha256"), Some("signed-checksum"));
    assert_eq!(put.header("authorization"), None);

    assert_eq!(
        (send.method.as_str(), send.path.as_str()),
        ("POST", SEND_PATH)
    );
    assert_eq!(send.header("idempotency-key"), Some("invoice:42"));
    assert_eq!(
        send.json(),
        json!({
            "template_key": "invoice",
            "subject": "Your invoice",
            "html": "<img src=\"cid:logo\">",
            "cc": ["finance@example.com", "boss@example.com"],
            "bcc": ["audit@example.com"],
            "attachments": [
                {"sha256": PDF_SHA256, "filename": "invoice.pdf", "content_type": "application/pdf"},
                {"sha256": PNG_SHA256, "filename": "logo.png", "content_id": "logo"},
            ],
            "capture_engagement": true,
            "to": "user@example.com",
        })
    );
}

#[tokio::test]
async fn a_refused_upload_sends_nothing_and_shows_the_buckets_answer() {
    let bucket = ScriptedServer::start_async(vec![Answer::xml(
        400,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>BadDigest</Code><Message>The SHA256 you specified did not match the calculated checksum.</Message><RequestId>abc</RequestId></Error>",
    )])
    .await;
    let server = ScriptedServer::start_async(vec![Answer::json(
        200,
        json!({"files": [upload_slot(&bucket, PDF_SHA256, PDF.len()), present_slot(PNG_SHA256)]}),
    )])
    .await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .send(invoice_with_copies_and_files())
        .await
        .unwrap_err();

    assert_eq!(error.code(), "attachment_upload_failed");
    assert_eq!(error.status(), Some(400));
    assert_eq!(error.kind(), EmailErrorKind::ApplicationError);
    assert_eq!(error.field(), Some("attachments__0"));
    assert_eq!(
        error.why(),
        Some("The file store answered BadDigest: The SHA256 you specified did not match the calculated checksum.")
    );
    assert!(error.message().contains("invoice.pdf"), "{}", error);
    assert_eq!(bucket.requests().len(), 1, "a refusal isn't retried");
    assert_eq!(server.requests().len(), 1, "no email is sent");
}

#[tokio::test]
async fn the_same_file_attached_twice_is_uploaded_once() {
    let bucket = ScriptedServer::start_async(vec![Answer::xml(200, "")]).await;
    let slot = upload_slot(&bucket, PDF_SHA256, PDF.len());
    let server = ScriptedServer::start_async(vec![
        Answer::json(200, json!({"files": [slot.clone(), slot]})),
        accepted(),
    ])
    .await;
    let client = client_for(&server).await;

    client
        .emails()
        .send(
            CodeEmail::new("user@example.com", "invoice", "Your invoices")
                .html("<p>Two copies</p>")
                .attachment(EmailAttachment::new("invoice.pdf", PDF))
                .attachment(EmailAttachment::new("copy.pdf", PDF)),
        )
        .await
        .unwrap();

    assert_eq!(bucket.requests().len(), 1);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].json()["attachments"],
        json!([
            {"sha256": PDF_SHA256, "filename": "invoice.pdf"},
            {"sha256": PDF_SHA256, "filename": "copy.pdf"},
        ])
    );
}

#[tokio::test]
async fn a_file_store_server_error_is_retried_before_the_send() {
    for (status, code) in [
        (500, "InternalError"),
        (502, "BadGateway"),
        (503, "SlowDown"),
        (504, "GatewayTimeout"),
    ] {
        let bucket = ScriptedServer::start_async(vec![
            Answer::xml(
                status,
                &format!("<Error><Code>{code}</Code><Message>Try again.</Message></Error>"),
            ),
            Answer::xml(200, ""),
        ])
        .await;
        let server = ScriptedServer::start_async(vec![
            Answer::json(
                200,
                json!({"files": [upload_slot(&bucket, PDF_SHA256, PDF.len()), present_slot(PNG_SHA256)]}),
            ),
            accepted(),
        ])
        .await;
        let client = client_for(&server).await;

        client
            .emails()
            .send(invoice_with_copies_and_files().max_retries(1))
            .await
            .unwrap_or_else(|error| panic!("{}: {}", status, error));

        let puts = bucket.requests();
        assert_eq!(puts.len(), 2, "{status}");
        assert_eq!(puts[1].body, PDF);
        assert_eq!(server.requests().len(), 2, "one upload request, one send");
    }
}

#[tokio::test]
async fn a_failed_upload_never_shows_the_signed_url() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let unreachable = format!(
        "http://{}/bucket/{PDF_SHA256}?X-Amz-Signature=secret-signature",
        closed.local_addr().unwrap()
    );
    drop(closed);
    for (url, code) in [
        (unreachable.as_str(), "network_error"),
        (
            "no-scheme?X-Amz-Signature=secret-signature",
            "unexpected_error",
        ),
    ] {
        let slot = json!({
            "sha256": PDF_SHA256,
            "status": "upload",
            "url": url,
            "method": "PUT",
            "headers": {"content-length": "7"},
            "url_expires_at": "2026-11-09T12:15:00Z",
        });
        let server = ScriptedServer::start_async(vec![Answer::json(
            200,
            json!({"files": [slot, present_slot(PNG_SHA256)]}),
        )])
        .await;
        let client = client_for(&server).await;

        let error = client
            .emails()
            .send(invoice_with_copies_and_files().max_retries(0))
            .await
            .unwrap_err();

        assert_eq!(error.code(), code);
        assert_eq!(error.field(), Some("attachments__0"));
        let mut chain = format!("{error} {error:?}");
        let mut source = std::error::Error::source(&error);
        while let Some(cause) = source {
            chain.push_str(&format!(" {cause} {cause:?}"));
            source = cause.source();
        }
        assert!(!chain.contains("secret-signature"), "{}", chain);
        assert_eq!(server.requests().len(), 1, "no email is sent");
    }
}

#[tokio::test]
async fn an_upload_request_posthog_refuses_sends_nothing() {
    let server = ScriptedServer::start_async(vec![Answer::json(
        400,
        json!({
            "type": "invalid_request",
            "code": "field_not_accepted",
            "detail": "This project can't send files yet, so PostHog didn't prepare the uploads.",
            "attr": "files",
            "why": "Sending files isn't available to this project yet.",
            "fix": "Send the email without files, or try again once files are available to this project.",
        }),
    )])
    .await;
    let client = client_for(&server).await;

    let error = client
        .emails()
        .send(invoice_with_copies_and_files())
        .await
        .unwrap_err();

    assert_eq!(error.code(), "field_not_accepted");
    assert_eq!(error.kind(), EmailErrorKind::ValidationError);
    assert_eq!(
        error.why(),
        Some("Sending files isn't available to this project yet.")
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn more_than_ten_files_are_refused_before_any_request() {
    let server = ScriptedServer::start_async(vec![accepted()]).await;
    let client = client_for(&server).await;
    let email = (0..11).fold(
        CodeEmail::new("user@example.com", "report", "Your reports").text("Attached."),
        |email, index| email.attachment(EmailAttachment::new(format!("{index}.csv"), "a,b")),
    );

    let error = client.emails().send(email).await.unwrap_err();

    assert_eq!(error.code(), "too_many_attachments");
    assert_eq!(error.kind(), EmailErrorKind::ValidationError);
    assert_eq!(error.field(), Some("attachments"));
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn an_email_over_40_mb_after_base64_is_refused_before_any_request() {
    let server = ScriptedServer::start_async(vec![
        Answer::json(200, json!({"files": [present_slot(LARGEST_FILE_SHA256)]})),
        accepted(),
    ])
    .await;
    let client = client_for(&server).await;
    let largest = EmailAttachment::new("scan.pdf", vec![0u8; LARGEST_FILE]);
    let one_byte_more = EmailAttachment::new("scan.pdf", vec![0u8; LARGEST_FILE + 1]);
    let email = |body: &str, file: &EmailAttachment| {
        CodeEmail::new("user@example.com", "scan", "Your scan")
            .text(body)
            .attachment(file.clone())
            .max_retries(0)
    };

    for (body, file) in [("", &one_byte_more), ("x", &largest)] {
        let error = client.emails().send(email(body, file)).await.unwrap_err();
        assert_eq!(error.code(), "message_too_large");
        assert_eq!(error.kind(), EmailErrorKind::ValidationError);
        assert_eq!(error.field(), Some("attachments"));
    }
    assert!(server.requests().is_empty());

    client.emails().send(email("", &largest)).await.unwrap();
    assert_eq!(server.requests().len(), 2, "the largest file goes out");
}

#[test]
fn a_printed_send_never_shows_the_file() {
    let email = CodeEmail::new("user@example.com", "invoice", "Your invoice")
        .text("Attached.")
        .attachment(EmailAttachment::new("invoice.pdf", vec![b'x'; 10_000]));

    let printed = format!("{email:?}");

    assert!(printed.contains("invoice.pdf"), "{}", printed);
    assert!(printed.len() < 1_000, "{}", printed);
}

#[tokio::test]
async fn code_send_defaults_engagement_capture_on() {
    let server = ScriptedServer::start_async(vec![accepted()]).await;
    let client = client_for(&server).await;

    client
        .emails()
        .send(CodeEmail::new("user@example.com", "welcome", "Welcome").text("Hi"))
        .await
        .unwrap();

    assert_eq!(
        server.requests()[0].json(),
        json!({
            "template_key": "welcome",
            "subject": "Welcome",
            "text": "Hi",
            "capture_engagement": true,
            "to": "user@example.com",
        })
    );
}

#[tokio::test]
async fn retries_reuse_the_idempotency_key() {
    let server = ScriptedServer::start_async(vec![unavailable(), accepted()]).await;
    let client = client_for(&server).await;

    let result = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID).max_retries(1))
        .await
        .unwrap();

    assert_eq!(result.id, MESSAGE_ID);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].header("idempotency-key"),
        requests[1].header("idempotency-key")
    );
    assert_eq!(requests[0].body, requests[1].body);
}

#[tokio::test]
async fn client_retry_limit_applies_and_a_send_can_turn_it_off() {
    let server = ScriptedServer::start_async(vec![unavailable()]).await;
    let client = posthog_rs::client(
        options(&server.url)
            .email_request_max_retries(1u32)
            .build()
            .unwrap(),
    )
    .await;

    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), EmailErrorKind::ServiceUnavailable);
    assert_eq!(error.status(), Some(503));
    assert_eq!(error.message(), "Try again.");
    assert_eq!(server.requests().len(), 2);

    client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID).max_retries(0))
        .await
        .unwrap_err();
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn quota_and_long_retry_after_fail_at_once() {
    let server = ScriptedServer::start_async(vec![Answer::json(
        429,
        json!({"code": "quota_limited", "detail": "Quota spent."}),
    )])
    .await;
    let client = client_for(&server).await;
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "quota_limited");
    assert_eq!(error.kind(), EmailErrorKind::MonthlyQuotaExceeded);
    assert_eq!(server.requests().len(), 1);

    let server = ScriptedServer::start_async(vec![
        Answer::json(429, json!({"code": "throttled"})).header("retry-after", "60")
    ])
    .await;
    let client = client_for(&server).await;
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), EmailErrorKind::RateLimitExceeded);
    assert_eq!(error.retry_after(), Some(Duration::from_secs(60)));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn refusals_keep_posthogs_code_and_are_not_retried() {
    let cases = [
        (400, "invalid_input", EmailErrorKind::ValidationError),
        (403, "permission_denied", EmailErrorKind::InvalidPermission),
        (404, "not_found", EmailErrorKind::NotFound),
        (409, "template_outdated", EmailErrorKind::ValidationError),
        (
            422,
            "idempotency_key_reused",
            EmailErrorKind::InvalidIdempotentRequest,
        ),
        (422, "recipient_suppressed", EmailErrorKind::ValidationError),
    ];
    for (status, code, kind) in cases {
        let server = ScriptedServer::start_async(vec![Answer::json(
            status,
            json!({"code": code, "detail": "Refused.", "attr": "to", "fix": "Fix it."}),
        )])
        .await;
        let client = client_for(&server).await;
        let error = client
            .emails()
            .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
            .await
            .unwrap_err();
        assert_eq!(error.code(), code);
        assert_eq!(error.kind(), kind, "{status} {code}");
        assert_eq!(error.status(), Some(status));
        assert_eq!(error.field(), Some("to"));
        assert_eq!(error.fix(), Some("Fix it."));
        assert_eq!(server.requests().len(), 1, "{status} {code}");
    }
}

#[tokio::test]
async fn a_send_in_flight_with_the_same_key_is_retried() {
    let server = ScriptedServer::start_async(vec![
        Answer::json(409, json!({"code": "idempotency_key_in_flight"})),
        accepted(),
    ])
    .await;
    let client = client_for(&server).await;
    client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID).max_retries(1))
        .await
        .unwrap();
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn an_acceptance_without_a_message_id_is_an_api_error() {
    let server = ScriptedServer::start_async(vec![Answer::json(202, json!({}))]).await;
    let client = client_for(&server).await;
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "api_error");
    assert_eq!(error.status(), Some(202));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn sends_that_cannot_go_out_make_no_request() {
    let server = ScriptedServer::start_async(vec![accepted()]).await;

    let mut no_key = options(&server.url);
    no_key.secret_key("  ");
    let client = posthog_rs::client(no_key.build().unwrap()).await;
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "missing_secret_key");
    assert_eq!(error.kind(), EmailErrorKind::MissingApiKey);

    let client = posthog_rs::client(
        posthog_rs::ClientOptionsBuilder::default()
            .api_key("phc_test".to_string())
            .host(server.url.clone())
            .secret_key("phs_test")
            .build()
            .unwrap(),
    )
    .await;
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "missing_app_host");

    let client = posthog_rs::client(options(&server.url).disabled(true).build().unwrap()).await;
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "client_disabled");

    let client = client_for(&server).await;
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", "welcome"))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "invalid_template_id");
    assert_eq!(error.kind(), EmailErrorKind::ValidationError);

    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn a_network_error_is_retried_then_reported() {
    let client = posthog_rs::client(
        options(&closed_url())
            .email_request_max_retries(1u32)
            .build()
            .unwrap(),
    )
    .await;
    let started = Instant::now();
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .await
        .unwrap_err();
    assert!(
        started.elapsed() >= Duration::from_millis(800),
        "one retry waits at least 0.8 s, took {:?}",
        started.elapsed()
    );
    assert_eq!(error.code(), "network_error");
    assert_eq!(error.kind(), EmailErrorKind::ServiceUnavailable);
    assert_eq!(error.status(), None);
    assert!(std::error::Error::source(&error).is_some());
    assert!(!error.to_string().contains("phs_test"));
}

#[tokio::test]
async fn get_reads_the_status_without_an_idempotency_key() {
    let server = ScriptedServer::start_async(vec![Answer::json(
        200,
        json!({
            "id": MESSAGE_ID,
            "status": "bounced",
            "created_at": "2026-10-28T16:27:44.364000Z",
            "status_at": "2026-10-28T16:30:02.100000Z",
            "reason": "hard",
        }),
    )])
    .await;
    let client = client_for(&server).await;

    let status = client
        .emails()
        .get(&MESSAGE_ID.to_uppercase())
        .await
        .unwrap();

    assert_eq!(status.id, MESSAGE_ID);
    assert_eq!(status.status, EmailDeliveryStatus::Bounced);
    assert_eq!(status.reason.as_deref(), Some("hard"));
    let request = &server.requests()[0];
    assert_eq!(request.method, "GET");
    assert_eq!(
        request.path,
        format!("/api/projects/@current/messaging/emails/{MESSAGE_ID}/")
    );
    assert_eq!(request.header("authorization"), Some("Bearer phs_test"));
    assert_eq!(request.header("idempotency-key"), None);
    assert_eq!(request.header("content-type"), None);
}

#[tokio::test]
async fn get_of_an_unknown_id_is_not_found_once() {
    let server =
        ScriptedServer::start_async(vec![Answer::json(404, json!({"code": "not_found"}))]).await;
    let client = client_for(&server).await;
    let error = client.emails().get(MESSAGE_ID).await.unwrap_err();
    assert_eq!(error.kind(), EmailErrorKind::NotFound);
    assert_eq!(server.requests().len(), 1);

    let error = client.emails().get("msg_1").await.unwrap_err();
    assert_eq!(error.code(), "invalid_message_id");
    assert_eq!(server.requests().len(), 1);
}

/// Waits until the server has a request. Fails when the send ends without
/// one, or when none comes within 10 seconds.
async fn wait_for_first_request<T>(server: &ScriptedServer, send: &tokio::task::JoinHandle<T>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while server.requests().is_empty() {
        assert!(
            !send.is_finished(),
            "the send ended before it reached the server"
        );
        assert!(Instant::now() < deadline, "the server got no request");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn shutdown_waits_for_a_send_in_its_retry_backoff() {
    let server = ScriptedServer::start_async(vec![unavailable(), accepted()]).await;
    let client = Arc::new(client_for(&server).await);
    let sender = Arc::clone(&client);
    let send = tokio::spawn(async move {
        sender
            .emails()
            .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
            .await
    });
    wait_for_first_request(&server, &send).await;

    client.shutdown().await;

    assert_eq!(server.requests().len(), 2);
    assert!(send.await.unwrap().is_ok());
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "client_closed");
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_off_the_runtime_waits_for_a_send_in_its_retry_backoff() {
    let server = ScriptedServer::start_async(vec![unavailable(), accepted()]).await;
    let client = Arc::new(client_for(&server).await);
    let sender = Arc::clone(&client);
    let send = tokio::spawn(async move {
        sender
            .emails()
            .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
            .await
    });
    wait_for_first_request(&server, &send).await;

    let closer = Arc::clone(&client);
    let thread = std::thread::spawn(move || futures::executor::block_on(closer.shutdown()));
    tokio::task::spawn_blocking(move || thread.join().unwrap())
        .await
        .unwrap();

    assert_eq!(server.requests().len(), 2);
    assert!(send.await.unwrap().is_ok());
}

#[tokio::test]
async fn shutdown_keeps_to_its_timeout_during_a_retry_backoff() {
    let server = ScriptedServer::start_async(vec![unavailable(), accepted()]).await;
    let client = Arc::new(
        posthog_rs::client(
            options(&server.url)
                .shutdown_timeout_ms(100u64)
                .build()
                .unwrap(),
        )
        .await,
    );
    let sender = Arc::clone(&client);
    let send = tokio::spawn(async move {
        sender
            .emails()
            .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
            .await
    });
    wait_for_first_request(&server, &send).await;

    let started = Instant::now();
    client.shutdown().await;

    assert!(started.elapsed() < Duration::from_millis(700));
    assert_eq!(server.requests().len(), 1);
    // The send already in flight still finishes.
    assert!(send.await.unwrap().is_ok());
}

#[tokio::test]
async fn an_email_error_converts_into_the_crate_error() {
    async fn send(client: &posthog_rs::Client) -> Result<String, posthog_rs::Error> {
        Ok(client
            .emails()
            .send(TemplateEmail::new("", TEMPLATE_ID))
            .await?
            .id)
    }
    let server = ScriptedServer::start_async(vec![accepted()]).await;
    let client = client_for(&server).await;
    match send(&client).await {
        Err(posthog_rs::Error::Email(error)) => assert_eq!(error.code(), "invalid_recipient"),
        other => panic!("expected an email error, got {:?}", other),
    }
}

#[tokio::test]
async fn the_email_timeout_replaces_a_custom_clients_timeout() {
    let server = httpmock::MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.path(SEND_PATH);
            then.status(202)
                .delay(Duration::from_secs(5))
                .json_body(json!({"message_id": MESSAGE_ID}));
        })
        .await;
    let patient = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let client = posthog_rs::client(
        options(&server.base_url())
            .http_client(patient)
            .email_request_timeout_seconds(1)
            .email_request_max_retries(0)
            .build()
            .unwrap(),
    )
    .await;

    let started = Instant::now();
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .await
        .unwrap_err();

    let elapsed = started.elapsed();
    assert_eq!(error.code(), "network_error");
    assert_eq!(error.kind(), EmailErrorKind::ServiceUnavailable);
    assert!(
        elapsed >= Duration::from_millis(900) && elapsed < Duration::from_secs(4),
        "the send took {:?}",
        elapsed
    );
}

#[tokio::test]
async fn a_reply_over_one_mebibyte_is_not_read() {
    let padding = "x".repeat(1 << 20);
    let server = ScriptedServer::start_async(vec![
        Answer::json(202, json!({"message_id": MESSAGE_ID, "padding": padding})),
        Answer::json(503, json!({"code": "big", "padding": padding})),
    ])
    .await;
    let client = client_for(&server).await;
    let email = || TemplateEmail::new("user@example.com", TEMPLATE_ID).max_retries(0);

    // An accepted send may be queued, so an unread acceptance is a network error.
    let error = client.emails().send(email()).await.unwrap_err();
    assert_eq!(error.code(), "network_error");
    assert_eq!(error.status(), None);

    // A refusal still counts by its status, without its unread code.
    let error = client.emails().send(email()).await.unwrap_err();
    assert_eq!(error.code(), "api_error");
    assert_eq!(error.status(), Some(503));
    assert_eq!(error.kind(), EmailErrorKind::ServiceUnavailable);
}

#[tokio::test]
async fn reading_stops_at_the_cap_on_an_endless_reply() {
    let client = posthog_rs::client(
        options(&email_support::endless_body_server())
            .email_request_timeout_seconds(10)
            .email_request_max_retries(0)
            .build()
            .unwrap(),
    )
    .await;

    let started = Instant::now();
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .await
        .unwrap_err();

    assert_eq!(error.code(), "network_error");
    assert!(error.message().contains("larger than"), "{}", error);
    assert!(started.elapsed() < Duration::from_secs(5));
}
