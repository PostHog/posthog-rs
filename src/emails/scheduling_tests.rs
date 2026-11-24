//! Sends on virtual time, through the public client against a
//! scripted PostHog Cloud: retries take no real time, and the clock moves
//! only by the waits the client makes.

#[path = "../../tests/email_support/mod.rs"]
mod email_support;

use chrono::{DateTime, Duration, Utc};
use email_support::{
    accepted, unavailable, Answer, CloudFront, FakePostHog, ScriptedServer, MESSAGE_ID, TEMPLATE_ID,
};
use serde_json::json;

use super::shared::clock::virtual_time;
use crate::{CodeEmail, EmailAttachment, EmailError, SendEmailStatus, TemplateEmail};

fn start() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-12-01T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn before_1970() -> DateTime<Utc> {
    DateTime::UNIX_EPOCH - Duration::seconds(1)
}

fn sent_now() -> Answer {
    Answer::json(
        202,
        json!({"message_id": MESSAGE_ID, "status": "queued", "scheduled_at": null}),
    )
}

fn scheduled_at(after_start: Duration, max_retries: u32) -> TemplateEmail {
    TemplateEmail::new("user@example.com", TEMPLATE_ID)
        .scheduled_at(start() + after_start)
        .max_retries(max_retries)
}

const INVOICE: [u8; 4] = [0x25, 0x50, 0x44, 0x46];

/// A PostHog that holds the file already and answers the upload route's first
/// request `503`, so the client waits at least 0.8 seconds for the retry
/// before the send's first attempt.
fn posthog_slow_to_take_the_file(posthog: FakePostHog) -> FakePostHog {
    let posthog = posthog.answering_sends_with(sent_now());
    posthog.store(&INVOICE);
    posthog.upload_route_answers_next(unavailable());
    posthog
}

/// A code email with a file, due 60.5 seconds after the start.
fn with_a_file_due_after_its_upload() -> CodeEmail {
    CodeEmail::new("user@example.com", "invoice", "Your invoice")
        .text("Attached.")
        .attachment(EmailAttachment::new("invoice.pdf", INVOICE))
        .scheduled_at(start() + Duration::milliseconds(60_500))
        .max_retries(1)
}

fn assert_not_confirmed(error: EmailError) {
    assert_eq!(error.code(), "schedule_not_confirmed");
    assert_eq!(error.message_id(), Some(MESSAGE_ID));
}

#[cfg(feature = "async-client")]
mod async_client {
    use super::*;

    async fn cloud_client(server: &ScriptedServer, app_host: &str) -> (CloudFront, crate::Client) {
        cloud_client_of(&server.url, app_host).await
    }

    async fn cloud_client_of(url: &str, app_host: &str) -> (CloudFront, crate::Client) {
        let cloud = CloudFront::start(url).await;
        let mut builder = crate::ClientOptionsBuilder::default();
        builder
            .api_key("phc_test".to_string())
            .host("https://us.i.posthog.com")
            .app_host(app_host)
            .secret_key("phs_test")
            .http_client(cloud.http());
        let client = crate::client(builder.build().unwrap()).await;
        (cloud, client)
    }

    #[tokio::test]
    async fn a_queued_answer_is_accepted_up_to_a_minute_after_the_first_attempt() {
        let server = ScriptedServer::start_async(vec![sent_now()]).await;
        let (_cloud, client) = cloud_client(&server, "https://us.posthog.com").await;
        let _time = virtual_time::start_at(start());

        let sent = client
            .emails()
            .send(scheduled_at(Duration::seconds(60), 0))
            .await
            .unwrap();
        assert_eq!(sent.status(), SendEmailStatus::Queued);
        assert_eq!(sent.scheduled_at, None);

        let error = client
            .emails()
            .send(scheduled_at(Duration::milliseconds(60_001), 0))
            .await
            .unwrap_err();
        assert_not_confirmed(error);
    }

    #[tokio::test]
    async fn a_future_time_that_posthog_sends_now_fails_with_the_message_id() {
        for ahead in [Duration::minutes(2), Duration::days(1)] {
            let server = ScriptedServer::start_async(vec![sent_now()]).await;
            let (_cloud, client) = cloud_client(&server, "https://us.posthog.com").await;
            let _time = virtual_time::start_at(start());

            let error = client
                .emails()
                .send(scheduled_at(ahead, 2))
                .await
                .unwrap_err();

            assert_not_confirmed(error);
            assert_eq!(server.requests().len(), 1, "{ahead}");
        }
    }

    #[tokio::test]
    async fn a_send_works_with_a_clock_before_1970() {
        let server = ScriptedServer::start_async(vec![accepted()]).await;
        let (_cloud, client) = cloud_client(&server, "https://us.posthog.com").await;
        let _time = virtual_time::start_at(before_1970());

        let sent = client
            .emails()
            .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
            .await
            .unwrap();

        assert_eq!(sent.status(), SendEmailStatus::Queued);
    }

    #[tokio::test]
    async fn the_minute_counts_from_the_first_attempt() {
        let server =
            ScriptedServer::start_async(vec![unavailable(), unavailable(), sent_now()]).await;
        let (_cloud, client) = cloud_client(&server, "https://us.posthog.com").await;
        let _time = virtual_time::start_at(start());

        let error = client
            .emails()
            .send(scheduled_at(Duration::seconds(61), 2))
            .await
            .unwrap_err();

        assert_not_confirmed(error);
        assert_eq!(server.requests().len(), 3);
    }

    #[tokio::test]
    async fn the_minute_counts_from_the_send_after_its_uploads() {
        let posthog = posthog_slow_to_take_the_file(FakePostHog::start_async().await);
        let (_cloud, client) = cloud_client_of(&posthog.url, "https://us.posthog.com").await;
        let _time = virtual_time::start_at(start());

        let sent = client
            .emails()
            .send(with_a_file_due_after_its_upload())
            .await
            .unwrap();

        assert_eq!(sent.status(), SendEmailStatus::Queued);
        assert_eq!(posthog.sent().len(), 1);
    }

    #[tokio::test]
    async fn a_scheduled_send_retries_with_the_same_time_and_key() {
        let server = ScriptedServer::start_async(vec![
            unavailable(),
            Answer::json(
                202,
                json!({
                    "message_id": MESSAGE_ID,
                    "replayed": true,
                    "status": "scheduled",
                    "scheduled_at": "2026-12-01T09:00:00.124000Z",
                }),
            ),
        ])
        .await;
        let (_cloud, client) = cloud_client(&server, "https://eu.posthog.com").await;
        let _time = virtual_time::start_at(start());

        let sent = client
            .emails()
            .send(scheduled_at(Duration::nanoseconds(123_000_500), 1))
            .await
            .unwrap();

        assert!(sent.replayed);
        assert_eq!(sent.status(), SendEmailStatus::Scheduled);
        assert_eq!(
            sent.scheduled_at,
            Some(start() + Duration::milliseconds(124))
        );
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        let key = requests[0].header("idempotency-key");
        assert!(key.is_some());
        for request in &requests {
            assert_eq!(request.json()["scheduled_at"], "2026-12-01T09:00:00.124Z");
            assert_eq!(request.header("idempotency-key"), key);
            assert_eq!(request.header("host"), Some("eu.posthog.com"));
        }
    }
}

#[cfg(not(feature = "async-client"))]
mod blocking_client {
    use super::*;

    fn cloud_client(server: &ScriptedServer) -> (CloudFront, crate::Client) {
        cloud_client_of(&server.url)
    }

    fn cloud_client_of(url: &str) -> (CloudFront, crate::Client) {
        let cloud = CloudFront::start_blocking(url);
        let mut builder = crate::ClientOptionsBuilder::default();
        builder
            .api_key("phc_test".to_string())
            .host("https://us.i.posthog.com")
            .secret_key("phs_test")
            .blocking_http_client(cloud.blocking_http());
        let client = crate::client(builder.build().unwrap());
        (cloud, client)
    }

    #[test]
    fn a_queued_answer_is_accepted_up_to_a_minute_after_the_first_attempt() {
        let server = ScriptedServer::start(vec![sent_now()]);
        let (_cloud, client) = cloud_client(&server);
        let _time = virtual_time::start_at(start());

        let sent = client
            .emails()
            .send(scheduled_at(Duration::seconds(60), 0))
            .unwrap();
        assert_eq!(sent.status(), SendEmailStatus::Queued);
        assert_eq!(sent.scheduled_at, None);

        let error = client
            .emails()
            .send(scheduled_at(Duration::milliseconds(60_001), 0))
            .unwrap_err();
        assert_not_confirmed(error);
    }

    #[test]
    fn a_send_works_with_a_clock_before_1970() {
        let server = ScriptedServer::start(vec![accepted()]);
        let (_cloud, client) = cloud_client(&server);
        let _time = virtual_time::start_at(before_1970());

        let sent = client
            .emails()
            .send(TemplateEmail::new("user@example.com", TEMPLATE_ID))
            .unwrap();

        assert_eq!(sent.status(), SendEmailStatus::Queued);
    }

    #[test]
    fn the_minute_counts_from_the_first_attempt() {
        let server = ScriptedServer::start(vec![unavailable(), unavailable(), sent_now()]);
        let (_cloud, client) = cloud_client(&server);
        let _time = virtual_time::start_at(start());

        let error = client
            .emails()
            .send(scheduled_at(Duration::seconds(61), 2))
            .unwrap_err();

        assert_not_confirmed(error);
        assert_eq!(server.requests().len(), 3);
    }

    #[test]
    fn the_minute_counts_from_the_send_after_its_uploads() {
        let posthog = posthog_slow_to_take_the_file(FakePostHog::start());
        let (_cloud, client) = cloud_client_of(&posthog.url);
        let _time = virtual_time::start_at(start());

        let sent = client
            .emails()
            .send(with_a_file_due_after_its_upload())
            .unwrap();

        assert_eq!(sent.status(), SendEmailStatus::Queued);
        assert_eq!(posthog.sent().len(), 1);
    }
}
