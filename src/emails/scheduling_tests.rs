//! Scheduled sends on virtual time, through the public client against a
//! scripted PostHog Cloud: retries take no real time, and the clock moves
//! only by the waits the client makes.

use super::email_support;

use chrono::{DateTime, Duration, Utc};
use email_support::{unavailable, Answer, CloudFront, ScriptedServer, MESSAGE_ID, TEMPLATE_ID};
use serde_json::json;

use super::shared::clock::virtual_time;
use crate::{EmailError, SendEmailStatus, TemplateEmail};

fn start() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-12-01T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
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

fn assert_not_confirmed(error: EmailError) {
    assert_eq!(error.code(), "schedule_not_confirmed");
    assert_eq!(error.message_id(), Some(MESSAGE_ID));
}

#[cfg(feature = "async-client")]
mod async_client {
    use super::*;

    async fn cloud_client(server: &ScriptedServer, app_host: &str) -> (CloudFront, crate::Client) {
        let cloud = CloudFront::start(&server.url).await;
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
        let cloud = CloudFront::start_blocking(&server.url);
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
}
