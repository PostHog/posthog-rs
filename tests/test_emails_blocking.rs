#![cfg(not(feature = "async-client"))]

mod common;
mod email_support;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use common::default_user_agent;
use email_support::{
    accepted, connection_counting_store, options, unavailable, Answer, FakePostHog, ScriptedServer,
    MESSAGE_ID, SEND_PATH, TEMPLATE_ID,
};
use posthog_rs::{CodeEmail, EmailAttachment, EmailDeliveryStatus, EmailErrorKind, TemplateEmail};
use serde_json::json;

fn client_for(server: &ScriptedServer) -> posthog_rs::Client {
    posthog_rs::client(options(&server.url).build().unwrap())
}

#[test]
fn template_send_posts_the_flat_body_with_auth() {
    let server = ScriptedServer::start(vec![accepted()]);
    let client = client_for(&server);

    let result = client
        .emails()
        .send(
            TemplateEmail::new("user@example.com", TEMPLATE_ID)
                .template_data(json!({"name": "Ada"}))
                .distinct_id("user_123")
                .idempotency_key("welcome:user_123"),
        )
        .unwrap();

    assert_eq!(result.id, MESSAGE_ID);
    let request = &server.requests()[0];
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
    assert_eq!(request.header("idempotency-key"), Some("welcome:user_123"));
    assert_eq!(
        request.header("user-agent"),
        Some(default_user_agent().as_str())
    );
}

#[test]
fn code_send_posts_the_rendered_content() {
    let server = ScriptedServer::start(vec![accepted()]);
    let client = client_for(&server);

    client
        .emails()
        .send(
            CodeEmail::new("user@example.com", "password-reset", "Reset your password")
                .html("<p>Reset</p>")
                .from("Acme <security@example.com>"),
        )
        .unwrap();

    assert_eq!(
        server.requests()[0].json(),
        json!({
            "template_key": "password-reset",
            "subject": "Reset your password",
            "html": "<p>Reset</p>",
            "from": "Acme <security@example.com>",
            "to": "user@example.com",
        })
    );
}

const PDF_SHA256: &str = "ccac55e87ec85819f85630feb88522f771cf4d4fc3a40b7f97fd124a128dfcb7";
const PDF: [u8; 7] = [0x25, 0x50, 0x44, 0x46, 0x00, 0x9f, 0xff];

fn client_of(posthog: &FakePostHog) -> posthog_rs::Client {
    posthog_rs::client(options(&posthog.url).build().unwrap())
}

fn invoice() -> CodeEmail {
    CodeEmail::new("user@example.com", "invoice", "Your invoice")
        .text("Attached.")
        .cc("finance@example.com")
        .bcc("audit@example.com")
        .attachment(EmailAttachment::new("invoice.pdf", PDF))
}

#[test]
fn code_send_uploads_the_file_then_sends_its_hash() {
    let posthog = FakePostHog::start();
    let client = client_of(&posthog);

    client.emails().send(invoice()).unwrap();

    assert_eq!(posthog.versions(PDF_SHA256), vec![PDF.to_vec()]);
    assert_eq!(
        posthog.sent(),
        vec![json!({
            "template_key": "invoice",
            "subject": "Your invoice",
            "text": "Attached.",
            "cc": ["finance@example.com"],
            "bcc": ["audit@example.com"],
            "attachments": [{"sha256": PDF_SHA256, "filename": "invoice.pdf"}],
            "to": "user@example.com",
        })]
    );
}

#[test]
fn a_refused_upload_sends_nothing_and_shows_the_buckets_answer() {
    let posthog = FakePostHog::start().with_expired_grants();
    let client = client_of(&posthog);

    let error = client.emails().send(invoice()).unwrap_err();

    assert_eq!(error.code(), "attachment_upload_failed");
    assert_eq!(error.status(), Some(403));
    assert_eq!(error.kind(), EmailErrorKind::ApplicationError);
    assert_eq!(
        error.why(),
        Some("The file store answered AccessDenied: Request has expired")
    );
    assert!(posthog.stored(PDF_SHA256).is_none());
    assert!(posthog.sent().is_empty(), "no email is sent");
}

#[test]
fn a_file_store_redirect_is_never_followed() {
    let posthog = FakePostHog::start();
    let elsewhere = FakePostHog::start();
    posthog.store_redirects_next_to(&elsewhere.store_url());
    let client = client_of(&posthog);

    let error = client.emails().send(invoice()).unwrap_err();

    assert_eq!(error.code(), "attachment_upload_failed");
    assert_eq!(error.status(), Some(307));
    assert!(error.message().contains("invoice.pdf"), "{}", error);
    assert!(!format!("{error} {error:?}").contains(&elsewhere.store_url()));
    assert!(
        elsewhere.stored(PDF_SHA256).is_none(),
        "the other store keeps nothing"
    );
    assert!(posthog.stored(PDF_SHA256).is_none());
    assert!(posthog.sent().is_empty(), "no email is sent");
}

#[test]
fn a_custom_clients_default_credentials_never_reach_the_file_store() {
    let posthog = FakePostHog::start();
    let mut defaults = reqwest::header::HeaderMap::new();
    defaults.insert(
        "authorization",
        "Bearer phx_app_credential".parse().unwrap(),
    );
    let http = reqwest::blocking::Client::builder()
        .default_headers(defaults)
        .build()
        .unwrap();
    let client = posthog_rs::client(
        options(&posthog.url)
            .blocking_http_client(http)
            .build()
            .unwrap(),
    );

    client.emails().send(invoice()).unwrap();

    assert_eq!(posthog.stored(PDF_SHA256), Some(PDF.to_vec()));
    assert_eq!(posthog.sent().len(), 1);
}

#[test]
fn a_custom_clients_uploads_reuse_one_connection_across_sends() {
    let (store, connections) = connection_counting_store();
    let slot = Answer::json(
        200,
        json!({"files": [{
            "sha256": PDF_SHA256,
            "status": "upload",
            "url": format!("{}/bucket/team/2/{}?X-Amz-Signature=signed", store, PDF_SHA256),
            "method": "PUT",
            "headers": {"content-length": "7"},
            "url_expires_at": "2026-11-09T12:15:00Z",
        }]}),
    );
    let server = ScriptedServer::start(vec![slot.clone(), accepted(), slot, accepted()]);
    let http = reqwest::blocking::Client::builder().build().unwrap();
    let client = posthog_rs::client(
        options(&server.url)
            .blocking_http_client(http)
            .build()
            .unwrap(),
    );

    client.emails().send(invoice()).unwrap();
    client.emails().send(invoice()).unwrap();

    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

#[test]
fn a_posthog_without_the_upload_route_sends_nothing_with_files() {
    let posthog = FakePostHog::start().without_upload_route();
    let client = client_of(&posthog);

    let error = client.emails().send(invoice()).unwrap_err();

    assert_eq!(error.code(), "not_found");
    assert_eq!(error.field(), Some("attachments"));
    assert_eq!(error.message(), "Not found.");
    assert!(posthog.sent().is_empty(), "no email is sent");
}

#[test]
fn retries_reuse_the_idempotency_key() {
    let server = ScriptedServer::start(vec![unavailable(), accepted()]);
    let client = client_for(&server);

    client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID).max_retries(1))
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].header("idempotency-key"),
        requests[1].header("idempotency-key")
    );
}

#[test]
fn a_refusal_keeps_posthogs_code() {
    let server = ScriptedServer::start(vec![Answer::json(
        429,
        json!({"code": "quota_limited", "detail": "Quota spent."}),
    )]);
    let client = client_for(&server);

    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .unwrap_err();

    assert_eq!(error.code(), "quota_limited");
    assert_eq!(error.kind(), EmailErrorKind::MonthlyQuotaExceeded);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn get_reads_the_status() {
    let server = ScriptedServer::start(vec![Answer::json(
        200,
        json!({
            "id": MESSAGE_ID,
            "status": "delivered",
            "created_at": "2026-10-28T16:27:44Z",
            "status_at": "2026-10-28T16:27:50Z",
            "reason": null,
        }),
    )]);
    let client = client_for(&server);

    let status = client.emails().get(MESSAGE_ID).unwrap();

    assert_eq!(status.status, EmailDeliveryStatus::Delivered);
    assert_eq!(server.requests()[0].method, "GET");
}

#[test]
fn shutdown_waits_for_a_send_on_another_thread_then_refuses() {
    let server = ScriptedServer::start(vec![unavailable(), accepted()]);
    let client = Arc::new(client_for(&server));
    let sender = Arc::clone(&client);
    let send = std::thread::spawn(move || {
        sender
            .emails()
            .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
            .map(|result| result.id)
    });
    server.wait_for(1);

    client.shutdown();

    assert_eq!(server.requests().len(), 2);
    assert_eq!(send.join().unwrap().unwrap(), MESSAGE_ID);
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .unwrap_err();
    assert_eq!(error.code(), "client_closed");
}

#[test]
fn shutdown_keeps_to_its_timeout() {
    let server = ScriptedServer::start(vec![unavailable(), accepted()]);
    let client = Arc::new(posthog_rs::client(
        options(&server.url)
            .shutdown_timeout_ms(100u64)
            .build()
            .unwrap(),
    ));
    let sender = Arc::clone(&client);
    let send = std::thread::spawn(move || {
        sender
            .emails()
            .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
            .is_ok()
    });
    server.wait_for(1);

    let started = std::time::Instant::now();
    client.shutdown();

    assert!(started.elapsed() < Duration::from_millis(700));
    assert_eq!(server.requests().len(), 1);
    assert!(send.join().unwrap());
}

#[test]
fn the_email_timeout_replaces_a_custom_clients_timeout() {
    let server = httpmock::MockServer::start();
    server.mock(|when, then| {
        when.path(SEND_PATH);
        then.status(202)
            .delay(Duration::from_secs(5))
            .json_body(json!({"message_id": MESSAGE_ID}));
    });
    let patient = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let client = posthog_rs::client(
        options(&server.base_url())
            .blocking_http_client(patient)
            .email_request_timeout_seconds(1)
            .email_request_max_retries(0)
            .build()
            .unwrap(),
    );

    let started = std::time::Instant::now();
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
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

#[test]
fn a_reply_over_one_mebibyte_is_not_read() {
    let padding = "x".repeat(1 << 20);
    let server = ScriptedServer::start(vec![
        Answer::json(202, json!({"message_id": MESSAGE_ID, "padding": padding})),
        Answer::json(503, json!({"code": "big", "padding": padding})),
    ]);
    let client = client_for(&server);
    let send = || {
        client
            .emails()
            .send(TemplateEmail::new("user@example.com", TEMPLATE_ID).max_retries(0))
    };

    let error = send().unwrap_err();
    assert_eq!(error.code(), "network_error");
    assert_eq!(error.status(), None);

    let error = send().unwrap_err();
    assert_eq!(error.code(), "api_error");
    assert_eq!(error.status(), Some(503));
    assert_eq!(error.kind(), EmailErrorKind::ServiceUnavailable);
}

#[test]
fn reading_stops_at_the_cap_on_an_endless_reply() {
    let client = posthog_rs::client(
        options(&email_support::endless_body_server())
            .email_request_timeout_seconds(10)
            .email_request_max_retries(0)
            .build()
            .unwrap(),
    );

    let started = std::time::Instant::now();
    let error = client
        .emails()
        .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
        .unwrap_err();

    assert_eq!(error.code(), "network_error");
    assert!(error.message().contains("larger than"), "{}", error);
    assert!(started.elapsed() < Duration::from_secs(5));
}
