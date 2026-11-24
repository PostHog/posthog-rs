//! Batches that wait, on virtual time, through the public client: retries
//! take no real time, and a shutdown during a retry wait is told apart from
//! the wait ending.

use super::email_support;

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use email_support::{
    all_queued, options, recipients, whole_call, Answer, ScriptedServer, TEMPLATE_ID,
};

use super::shared::clock::{self, virtual_time};
use crate::{BatchEmailStatus, SendBatchResult, TemplateBatch};

use BatchEmailStatus::{Failed, NotAttempted};

fn start() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-12-01T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn template(count: usize) -> TemplateBatch {
    TemplateBatch::new(TEMPLATE_ID, recipients(count))
}

fn statuses(result: &SendBatchResult) -> Vec<BatchEmailStatus> {
    result.results.iter().map(|result| result.status).collect()
}

fn throttled_for_30_seconds() -> Answer {
    whole_call(429, "throttled").header("retry-after", "30")
}

/// The first request refused for 30 seconds, then 50 recipients never sent,
/// and the batch returned before its held wait could end on its own.
fn assert_stopped_in_the_retry_wait(error: &crate::EmailError, took: Duration) {
    assert!(took < virtual_time::HELD_WAIT, "returned after {:?}", took);
    assert_eq!(error.code(), "client_closed");
    let result = error.batch_result().unwrap();
    let mut expected = vec![Failed; 100];
    expected.extend(vec![NotAttempted; 50]);
    assert_eq!(statuses(result), expected);
    assert_eq!(
        result.results[0].error.as_ref().unwrap().code(),
        "client_closed"
    );
}

#[cfg(feature = "async-client")]
mod async_client {
    use email_support::{batch_requests, found_only, ok, queries_read, refused};
    use serde_json::json;

    use super::*;
    use crate::EmailErrorKind;
    use BatchEmailStatus::{Queued, Unconfirmed};

    async fn client_for(server: &ScriptedServer) -> crate::Client {
        crate::client(options(&server.url).build().unwrap()).await
    }

    #[tokio::test]
    async fn a_retry_after_a_lost_answer_keeps_a_refused_recipient_unconfirmed() {
        let server = ScriptedServer::start(vec![
            Answer::lost(),
            ok(vec![
                json!({"message_id": "m0", "status": "queued", "replayed": true}),
                refused("m1", "idempotency_key_reused", 422),
            ]),
        ]);
        let client = client_for(&server).await;
        let _time = virtual_time::start_at(start());

        let result = client.emails().batch(template(2)).await.unwrap();

        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].body, requests[1].body);
        assert!(clock::now() > start());
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
        let later = second.later_error.as_ref().unwrap();
        assert_eq!(
            (later.code(), later.message_id()),
            ("idempotency_key_reused", None)
        );
    }

    #[tokio::test]
    async fn a_503_then_a_refusal_also_keeps_the_recipient_unconfirmed() {
        let server = ScriptedServer::start(vec![
            whole_call(503, "delivery_unavailable"),
            ok(vec![refused("m0", "sending_paused", 422)]),
        ]);
        let client = client_for(&server).await;
        let _time = virtual_time::start_at(start());

        let result = client.emails().batch(template(1)).await.unwrap();

        let only = &result.results[0];
        assert_eq!(only.status, Unconfirmed);
        assert_eq!(only.error.as_ref().unwrap().code(), "delivery_unavailable");
        assert_eq!(only.later_error.as_ref().unwrap().code(), "sending_paused");
    }

    #[tokio::test]
    async fn a_lost_answer_after_retries_reads_each_email_by_its_key() {
        let server = ScriptedServer::start(vec![
            Answer::lost(),
            Answer::lost(),
            Answer::lost(),
            found_only("key-0", "m0"),
        ]);
        let client = client_for(&server).await;
        let _time = virtual_time::start_at(start());

        let error = client.emails().batch(template(2)).await.unwrap_err();

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
        assert!(result.results.iter().all(|r| {
            r.error.as_ref().unwrap().code() == "delivery_unavailable" && r.later_error.is_none()
        }));
    }

    #[tokio::test]
    async fn a_refusal_after_an_in_flight_answer_keeps_the_request_unconfirmed() {
        let server = ScriptedServer::start(vec![
            whole_call(409, "idempotency_key_in_flight"),
            whole_call(400, "invalid_input"),
            whole_call(404, "not_found"),
        ]);
        let client = client_for(&server).await;
        let _time = virtual_time::start_at(start());

        let error = client.emails().batch(template(2)).await.unwrap_err();

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
    async fn the_first_unclear_answer_stays_the_error() {
        let server = ScriptedServer::start(vec![
            Answer::lost(),
            whole_call(409, "idempotency_key_in_flight"),
            whole_call(404, "not_found"),
        ]);
        let client = client_for(&server).await;
        let _time = virtual_time::start_at(start());

        let error = client
            .emails()
            .batch(template(1).max_retries(1))
            .await
            .unwrap_err();

        let only = &error.batch_result().unwrap().results[0];
        assert_eq!(only.status, Unconfirmed);
        assert_eq!(only.error.as_ref().unwrap().code(), "delivery_unavailable");
        assert_eq!(
            only.later_error.as_ref().unwrap().code(),
            "idempotency_key_in_flight"
        );
    }

    #[tokio::test]
    async fn a_shutdown_during_a_retry_wait_returns_at_once() {
        let server = ScriptedServer::start(vec![throttled_for_30_seconds(), all_queued()]);
        let client = client_for(&server).await;
        let (held, mut waits) = tokio::sync::mpsc::unbounded_channel();
        let _time = virtual_time::start_at(start()).holding_waits(held);

        let emails = client.emails();
        let started = Instant::now();
        let (sent, ()) = tokio::join!(emails.batch(template(150)), async {
            assert_eq!(waits.recv().await, Some(Duration::from_secs(30)));
            client.shutdown().await;
        });

        assert_stopped_in_the_retry_wait(&sent.unwrap_err(), started.elapsed());
        assert_eq!(server.requests().len(), 1);
        assert_eq!(clock::now(), start());
    }
}

#[cfg(not(feature = "async-client"))]
mod blocking {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn a_shutdown_during_a_retry_wait_returns_at_once() {
        let server = ScriptedServer::start(vec![throttled_for_30_seconds(), all_queued()]);
        let client = Arc::new(crate::client(options(&server.url).build().unwrap()));
        let (held, mut waits) = tokio::sync::mpsc::unbounded_channel();
        let sender = Arc::clone(&client);
        let sent = std::thread::spawn(move || {
            let _time = virtual_time::start_at(start()).holding_waits(held);
            let started = Instant::now();
            let sent = sender.emails().batch(template(150));
            (sent, started.elapsed(), clock::now())
        });

        assert_eq!(waits.blocking_recv(), Some(Duration::from_secs(30)));
        client.shutdown();

        let (sent, took, now) = sent.join().unwrap();
        assert_stopped_in_the_retry_wait(&sent.unwrap_err(), took);
        assert_eq!(server.requests().len(), 1);
        assert_eq!(now, start());
    }
}
