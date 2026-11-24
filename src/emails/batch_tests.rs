//! Batches that wait or race a shutdown, through the public client: retries
//! wait on virtual time, and a held answer lets the client shut down while
//! its request is in flight.

use super::email_support;

use std::sync::{mpsc, Mutex};

use chrono::{DateTime, Utc};
use email_support::{
    all_queued, options, recipients, whole_call, Answer, ScriptedServer, TEMPLATE_ID,
};
use tokio::sync::mpsc::UnboundedReceiver;

use super::shared::clock::virtual_time;
use crate::{BatchEmailStatus, ClientOptionsBuilder, SendBatchResult, TemplateBatch};

use BatchEmailStatus::{Failed, Queued};

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

/// `answer`, given only once the test sends on the returned sender, and a
/// receiver told when the request arrives.
fn held(answer: Answer) -> (Answer, UnboundedReceiver<()>, mpsc::Sender<()>) {
    let (arrived, arrival) = tokio::sync::mpsc::unbounded_channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let held = Answer::of_request(move |_| {
        let _ = arrived.send(());
        let _ = released.lock().unwrap().recv();
        answer.clone()
    });
    (held, arrival, release)
}

/// A shutdown that returns at once, so a test can let the held answer go
/// after it.
fn shutting_down_at_once(server: &ScriptedServer) -> ClientOptionsBuilder {
    let mut builder = options(&server.url);
    builder.shutdown_timeout_ms(0u64);
    builder
}

/// The request refused for 30 seconds after the shutdown began: no retry,
/// and every recipient failed with `client_closed`.
fn assert_stopped_before_the_retry(error: &crate::EmailError, server: &ScriptedServer) {
    assert_eq!(server.requests().len(), 1);
    assert_eq!(error.code(), "client_closed");
    let result = error.batch_result().unwrap();
    assert_eq!(statuses(result), vec![Failed, Failed]);
    assert_eq!(
        result.results[0].error.as_ref().unwrap().code(),
        "client_closed"
    );
}

/// PostHog queued both recipients of the only request.
fn assert_all_queued(result: &SendBatchResult, server: &ScriptedServer) {
    assert_eq!(server.requests().len(), 1);
    assert_eq!(statuses(result), vec![Queued, Queued]);
}

#[cfg(feature = "async-client")]
mod async_client {
    use email_support::{ok, refused, says_to_resend_with_its_key, status_found};
    use serde_json::json;

    use super::*;
    use crate::EmailErrorKind;
    use BatchEmailStatus::Unconfirmed;

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
    async fn a_lost_answer_after_retries_leaves_every_recipient_unconfirmed() {
        let server = ScriptedServer::start(vec![
            Answer::lost(),
            Answer::lost(),
            Answer::lost(),
            status_found("m0"),
        ]);
        let client = client_for(&server).await;
        let _time = virtual_time::start_at(start());

        let error = client.emails().batch(template(2)).await.unwrap_err();

        assert_eq!(error.code(), "network_error");
        assert_eq!(server.requests().len(), 3);
        let result = error.batch_result().unwrap();
        assert!(result.results.iter().all(|r| {
            (r.status, r.message_id.as_deref()) == (Unconfirmed, None)
                && r.error.as_ref().unwrap().code() == "delivery_unavailable"
                && says_to_resend_with_its_key(r.error.as_ref())
                && r.later_error.is_none()
        }));
    }

    #[tokio::test]
    async fn a_refusal_after_an_in_flight_answer_keeps_the_request_unconfirmed() {
        let server = ScriptedServer::start(vec![
            whole_call(409, "idempotency_key_in_flight"),
            whole_call(400, "invalid_input"),
            status_found("m0"),
        ]);
        let client = client_for(&server).await;
        let _time = virtual_time::start_at(start());

        let error = client.emails().batch(template(2)).await.unwrap_err();

        assert_eq!(error.code(), "invalid_input");
        assert_eq!(server.requests().len(), 2);
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

    /// Paused time moves only by the runtime's timers, so the request times
    /// out at its own timeout, at once and without a real wait.
    #[tokio::test(start_paused = true)]
    async fn an_attempt_waits_45_seconds_whatever_the_email_timeout() {
        let (answer, _arrival, release) = held(all_queued());
        let server = ScriptedServer::start(vec![answer]);
        let mut builder = options(&server.url);
        builder.email_request_timeout_seconds(1u64);
        let client = crate::client(builder.build().unwrap()).await;
        let started = tokio::time::Instant::now();

        let error = client
            .emails()
            .batch(template(2).max_retries(0))
            .await
            .unwrap_err();

        assert_eq!(started.elapsed().as_secs(), 45);
        assert_eq!(
            statuses(error.batch_result().unwrap()),
            vec![Unconfirmed; 2]
        );
        drop(release);
    }

    /// Sends `batch` while the client shuts down once its first request
    /// arrived, then lets the held answer go.
    async fn batch_racing_shutdown(
        answer: Answer,
        then: Vec<Answer>,
        batch: TemplateBatch,
    ) -> (Result<SendBatchResult, crate::EmailError>, ScriptedServer) {
        let (answer, mut arrival, release) = held(answer);
        let server = ScriptedServer::start([vec![answer], then].concat());
        let client = crate::client(shutting_down_at_once(&server).build().unwrap()).await;
        let _time = virtual_time::start_at(start());

        let emails = client.emails();
        let (sent, ()) = tokio::join!(emails.batch(batch), async {
            arrival.recv().await;
            client.shutdown().await;
            release.send(()).unwrap();
        });
        (sent, server)
    }

    #[tokio::test]
    async fn a_shutdown_while_the_last_request_is_answered_keeps_its_results() {
        let (sent, server) = batch_racing_shutdown(all_queued(), vec![], template(2)).await;

        assert_all_queued(&sent.unwrap(), &server);
    }

    #[tokio::test]
    async fn a_shutdown_before_a_retry_sends_no_retry() {
        let (sent, server) =
            batch_racing_shutdown(throttled_for_30_seconds(), vec![all_queued()], template(2))
                .await;

        assert_stopped_before_the_retry(&sent.unwrap_err(), &server);
    }
}

#[cfg(not(feature = "async-client"))]
mod blocking {
    use std::sync::Arc;

    use super::*;

    /// Sends `batch` on another thread while this one shuts the client down
    /// once its first request arrived, then lets the held answer go.
    fn batch_racing_shutdown(
        answer: Answer,
        then: Vec<Answer>,
        batch: TemplateBatch,
    ) -> (Result<SendBatchResult, crate::EmailError>, ScriptedServer) {
        let (answer, mut arrival, release) = held(answer);
        let server = ScriptedServer::start([vec![answer], then].concat());
        let client = Arc::new(crate::client(
            shutting_down_at_once(&server).build().unwrap(),
        ));
        let sender = Arc::clone(&client);
        let sent = std::thread::spawn(move || {
            let _time = virtual_time::start_at(start());
            sender.emails().batch(batch)
        });

        arrival.blocking_recv();
        client.shutdown();
        release.send(()).unwrap();
        (sent.join().unwrap(), server)
    }

    #[test]
    fn a_shutdown_while_the_last_request_is_answered_keeps_its_results() {
        let (sent, server) = batch_racing_shutdown(all_queued(), vec![], template(2));

        assert_all_queued(&sent.unwrap(), &server);
    }

    #[test]
    fn a_shutdown_before_a_retry_sends_no_retry() {
        let (sent, server) =
            batch_racing_shutdown(throttled_for_30_seconds(), vec![all_queued()], template(2));

        assert_stopped_before_the_retry(&sent.unwrap_err(), &server);
    }
}
