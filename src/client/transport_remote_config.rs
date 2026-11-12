//! Remote configuration requests on the client's existing delivery worker.

use std::io::Read;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::flags;
use crate::client::common::apply_on_error_hooks;
use crate::client::on_error::RemoteConfigFailure;
use crate::client::{ClientOptions, PostHogError};
use crate::endpoints::{EU_INGESTION_ENDPOINT, US_INGESTION_ENDPOINT};

pub(crate) const MAX_CONFIG_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteConfig {
    pub(crate) has_feature_flags: Option<bool>,
    pub(crate) session_recording: Option<Value>,
}

impl RemoteConfig {
    pub(crate) fn valid(&self) -> bool {
        self.session_recording
            .as_ref()
            .map_or(true, |value| value.is_boolean() || value.is_object())
            && crate::client::capture::json_len(self) <= MAX_CONFIG_BYTES
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LoadError {
    Network,
    Http(u16),
    InvalidResponse,
    ResponseTooLarge,
}

pub(crate) fn config_url(host: &str, project_token: &str) -> Option<String> {
    let host = match host {
        US_INGESTION_ENDPOINT => "https://us-assets.i.posthog.com",
        EU_INGESTION_ENDPOINT => "https://eu-assets.i.posthog.com",
        host => host,
    };
    let mut url = reqwest::Url::parse(host).ok()?;
    url.path_segments_mut()
        .ok()?
        .pop_if_empty()
        .push("array")
        .push(project_token)
        .push("config");
    Some(url.into())
}

fn decode(bytes: &[u8]) -> Result<RemoteConfig, LoadError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(LoadError::ResponseTooLarge);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| LoadError::InvalidResponse)?;
    let object = value.as_object().ok_or(LoadError::InvalidResponse)?;
    let config = RemoteConfig {
        has_feature_flags: object.get("hasFeatureFlags").and_then(Value::as_bool),
        session_recording: object
            .get("sessionRecording")
            .filter(|value| value.is_boolean() || value.is_object())
            .cloned(),
    };
    if config.valid() {
        Ok(config)
    } else {
        Err(LoadError::ResponseTooLarge)
    }
}

type Complete = dyn FnOnce(Result<RemoteConfig, LoadError>) -> Option<flags::Task> + Send;

pub(crate) struct Task {
    url: String,
    complete: Box<Complete>,
}

impl Task {
    pub(crate) fn new(
        url: String,
        complete: impl FnOnce(Result<RemoteConfig, LoadError>) -> Option<flags::Task> + Send + 'static,
    ) -> Self {
        Self {
            url,
            complete: Box::new(complete),
        }
    }

    pub(crate) fn run(self, http: &reqwest::blocking::Client, options: &ClientOptions) {
        let result = fetch(http, options, &self.url);
        if let Err(error) = result.as_ref() {
            report(options, *error);
        }
        // Startup flag loading follows config on the same worker and snapshots
        // targeting at completion, not before the identity-independent GET.
        let follow_up =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.complete)(result)));
        match follow_up {
            Ok(Some(task)) => task.run(http, options),
            Ok(None) => {}
            Err(_) => tracing::warn!("posthog-rs: remote config completion panicked"),
        }
    }
}

fn fetch(
    http: &reqwest::blocking::Client,
    options: &ClientOptions,
    url: &str,
) -> Result<RemoteConfig, LoadError> {
    let response = http
        .get(url)
        .header(
            reqwest::header::USER_AGENT,
            crate::client::get_default_user_agent(),
        )
        .timeout(Duration::from_secs(options.request_timeout_seconds))
        .send()
        .map_err(|_| LoadError::Network)?;
    if !response.status().is_success() {
        return Err(LoadError::Http(response.status().as_u16()));
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| LoadError::Network)?;
    decode(&bytes)
}

pub(crate) fn report(options: &ClientOptions, error: LoadError) {
    let (message, status) = match error {
        LoadError::Network => ("Remote configuration request failed", None),
        LoadError::Http(status) => (
            "Remote configuration returned an unsuccessful HTTP status",
            Some(status),
        ),
        LoadError::InvalidResponse => ("Remote configuration response is invalid", None),
        LoadError::ResponseTooLarge => {
            ("Remote configuration response exceeds its byte limit", None)
        }
    };
    if options.on_error.is_empty() {
        tracing::warn!("posthog-rs: {message}; status={status:?}");
    } else {
        apply_on_error_hooks(
            &options.on_error,
            &PostHogError::RemoteConfig(RemoteConfigFailure { message, status }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_diagnostics_report_once_and_omit_request_and_response_data() {
        use crate::client::{ClientOptionsBuilder, OnErrorHook};
        use httpmock::prelude::*;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct Log(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Log {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        for (error, message, status) in [
            (
                LoadError::Network,
                "Remote configuration request failed",
                None,
            ),
            (
                LoadError::Http(503),
                "Remote configuration returned an unsuccessful HTTP status",
                Some(503),
            ),
            (
                LoadError::InvalidResponse,
                "Remote configuration response is invalid",
                None,
            ),
            (
                LoadError::ResponseTooLarge,
                "Remote configuration response exceeds its byte limit",
                None,
            ),
        ] {
            for with_hook in [false, true] {
                let server = MockServer::start();
                let body = if error == LoadError::ResponseTooLarge {
                    format!("private-body-marker{}", "x".repeat(MAX_CONFIG_BYTES))
                } else {
                    "private-body-marker".to_owned()
                };
                let response = server.mock(|when, then| {
                    when.method(GET)
                        .path("/private-url-marker/array/private-token-marker/config");
                    then.status(if let LoadError::Http(status) = error {
                        status
                    } else {
                        200
                    })
                    .body(body);
                });
                let host = if error == LoadError::Network {
                    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                    let host =
                        format!("http://{}/private-url-marker", socket.local_addr().unwrap());
                    drop(socket);
                    host
                } else {
                    server.url("/private-url-marker")
                };
                let mut options = ClientOptionsBuilder::default()
                    .api_key("private-token-marker".to_owned())
                    .host(host)
                    .request_timeout_seconds(1u64)
                    .build()
                    .unwrap();
                let diagnostics = Arc::new(Mutex::new(Vec::new()));
                if with_hook {
                    let seen = diagnostics.clone();
                    options.on_error.push(OnErrorHook::new(move |diagnostic| {
                        if let PostHogError::RemoteConfig(failure) = diagnostic {
                            seen.lock().unwrap().push((
                                failure.message().to_owned(),
                                failure.status(),
                                format!("{diagnostic:?}"),
                            ));
                        }
                    }));
                }
                let logs = Log(Arc::new(Mutex::new(Vec::new())));
                let writer = logs.clone();
                let subscriber = tracing_subscriber::fmt()
                    .with_env_filter("posthog=warn")
                    .without_time()
                    .with_ansi(false)
                    .with_writer(move || writer.clone())
                    .finish();
                let _guard = tracing::subscriber::set_default(subscriber);
                let (tx, rx) = std::sync::mpsc::channel();
                Task::new(
                    config_url(options.endpoints().base_host(), &options.api_key).unwrap(),
                    move |result| {
                        tx.send(result.err()).unwrap();
                        None
                    },
                )
                .run(&reqwest::blocking::Client::new(), &options);
                assert_eq!(rx.recv().unwrap(), Some(error));
                let diagnostics = diagnostics.lock().unwrap();
                assert_eq!(diagnostics.len(), usize::from(with_hook));
                if with_hook {
                    assert_eq!(diagnostics[0].0, message);
                    assert_eq!(diagnostics[0].1, status);
                }
                let warnings = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
                if !with_hook {
                    assert!(warnings.contains(message));
                }
                for marker in [
                    "private-token-marker",
                    "private-url-marker",
                    "private-body-marker",
                ] {
                    assert!(!warnings.contains(marker));
                    for diagnostic in diagnostics.iter() {
                        assert!(!diagnostic.2.contains(marker));
                    }
                }
                response.assert_calls(usize::from(error != LoadError::Network));
            }
        }
    }

    #[test]
    fn asset_routing_preserves_proxy_prefix_and_encodes_token() {
        for (host, expected) in [
            (
                US_INGESTION_ENDPOINT,
                "https://us-assets.i.posthog.com/array/test/config",
            ),
            (
                EU_INGESTION_ENDPOINT,
                "https://eu-assets.i.posthog.com/array/test/config",
            ),
            (
                "https://example.com/proxy/",
                "https://example.com/proxy/array/test/config",
            ),
        ] {
            assert_eq!(config_url(host, "test").as_deref(), Some(expected));
        }
        assert_eq!(
            config_url("https://example.com/prefix", "a/b?c").unwrap(),
            "https://example.com/prefix/array/a%2Fb%3Fc/config"
        );
    }

    #[test]
    fn feature_fields_are_independent_and_extensions_are_tolerated() {
        let config = decode(
            br#"{"hasFeatureFlags":false,"sessionRecording":{"endpoint":"/s/"},"future":true}"#,
        )
        .unwrap();
        assert_eq!(config.has_feature_flags, Some(false));
        assert!(config.session_recording.unwrap().is_object());
        for raw in [
            br#"{}"#.as_slice(),
            br#"{"hasFeatureFlags":"false","sessionRecording":null}"#,
            br#"{"sessionRecording":42}"#,
        ] {
            let config = decode(raw).unwrap();
            assert_eq!(config.has_feature_flags, None);
            assert_eq!(config.session_recording, None);
        }
        assert_eq!(decode(b"[]"), Err(LoadError::InvalidResponse));
        assert_eq!(decode(b"broken"), Err(LoadError::InvalidResponse));
        assert_eq!(
            decode(&vec![b' '; MAX_CONFIG_BYTES + 1]),
            Err(LoadError::ResponseTooLarge)
        );
    }
}
