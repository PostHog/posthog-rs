//! Exercise real TLS initialization failures through the public constructors.
//! Linux uses SSL_CERT_FILE / SSL_CERT_DIR for its platform trust store.
#![cfg(all(target_os = "linux", feature = "tls"))]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use httpmock::prelude::*;
use posthog_rs::{
    ClientOptionsBuilder, Error, Event, FlagCache, FlagPoller, LocalEvaluationConfig,
};

/// Isolate the empty trust store in a subprocess so parallel tests keep their
/// normal certificates. The mock server stays in the parent for the same reason.
fn without_ca_certificates(test_name: &str, test: impl FnOnce(String)) {
    if std::env::var("POSTHOG_RS_EMPTY_CA_TEST").as_deref() == Ok(test_name) {
        let error = reqwest::Client::builder()
            .build()
            .expect_err("the empty trust store must cause an HTTP builder error");
        assert!(format!("{error:?}").contains("No CA certificates were loaded"));
        tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .with_writer(std::io::stderr)
            .init();
        test(std::env::var("POSTHOG_RS_EMPTY_CA_HOST").expect("parent mock server URL"));
        return;
    }

    let server = MockServer::start();
    let requests = server.mock(|_when, then| {
        then.status(200).json_body(serde_json::json!({}));
    });
    let empty_dir = std::env::temp_dir().join(format!("posthog-empty-ca-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&empty_dir).expect("create empty certificate directory");
    let mut child = Command::new(std::env::current_exe().expect("integration test executable"))
        .args(["--exact", test_name, "--nocapture"])
        .env("POSTHOG_RS_EMPTY_CA_TEST", test_name)
        .env("POSTHOG_RS_EMPTY_CA_HOST", server.base_url())
        .env("SSL_CERT_FILE", "/dev/null")
        .env("SSL_CERT_DIR", &empty_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start empty-trust-store subprocess");

    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().expect("check subprocess status").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("terminate stalled subprocess");
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("collect subprocess output");
    std::fs::remove_dir(&empty_dir).expect("remove certificate directory");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "empty-trust-store subprocess failed: {}\n{}\n{stderr}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
    );
    assert!(
        stderr.contains("No CA certificates were loaded"),
        "warning must include the builder error's cause: {}",
        stderr,
    );
    requests.assert_calls(0);
}

#[cfg(feature = "async-client")]
fn complete<T>(operation: impl std::future::Future<Output = T>) -> T {
    futures::executor::block_on(operation)
}

#[cfg(not(feature = "async-client"))]
fn complete<T>(result: T) -> T {
    result
}

#[test]
fn client_without_ca_certificates_is_noop() {
    without_ca_certificates("client_without_ca_certificates_is_noop", |host| {
        for local_evaluation in [false, true] {
            let options = ClientOptionsBuilder::default()
                .api_key("phc_test".to_string())
                .secret_key("phx_test")
                .host(host.clone())
                .enable_local_evaluation(local_evaluation)
                .request_timeout_seconds(1u64)
                .build()
                .expect("valid client options");
            assert!(!options.is_disabled());
            let client = complete(posthog_rs::client(options));
            let event = Event::new("test_event", "test_user");
            client.capture(event.clone());
            client.capture_batch(vec![event.clone()], false);
            client.capture_batch(vec![event.clone()], true);
            let summary = complete(client.capture_immediate(event))
                .expect("disabled capture should succeed without sending");
            assert_eq!(summary.submitted(), 0);

            let evaluations = complete(client.evaluate_flags("test_user", Default::default()))
                .expect("disabled evaluation should succeed");
            assert_eq!(evaluations.get_flag("test_flag"), None);
            assert_eq!(evaluations.get_flag_payload("test_flag"), None);

            complete(client.flush());
            complete(client.shutdown());
            complete(client.shutdown());
        }
    });
}

fn poller_config(host: String) -> LocalEvaluationConfig {
    LocalEvaluationConfig {
        secret_key: "phx_test".to_string(),
        project_api_key: "phc_test".to_string(),
        api_host: host,
        poll_interval: Duration::from_secs(30),
        request_timeout: Duration::from_secs(1),
    }
}

#[test]
fn blocking_poller_without_ca_certificates_stays_stopped() {
    without_ca_certificates(
        "blocking_poller_without_ca_certificates_stays_stopped",
        |host| {
            let mut poller = FlagPoller::new(poller_config(host), FlagCache::new());
            assert!(matches!(poller.load_flags(), Err(Error::Connection(_))));
            poller.start();
            poller.stop();
        },
    );
}

#[cfg(feature = "async-client")]
#[test]
fn async_poller_without_ca_certificates_stays_stopped() {
    without_ca_certificates(
        "async_poller_without_ca_certificates_stays_stopped",
        |host| {
            let mut poller =
                posthog_rs::AsyncFlagPoller::new(poller_config(host), FlagCache::new());
            assert!(matches!(
                complete(poller.load_flags()),
                Err(Error::Connection(_))
            ));
            complete(poller.start());
            assert!(!complete(poller.is_running()));
            complete(poller.stop());
        },
    );
}
