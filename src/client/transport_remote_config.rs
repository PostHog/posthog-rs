//! Project configuration requests on the client's existing delivery worker.

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
pub(crate) struct ProjectConfig {
    pub(crate) has_feature_flags: Option<bool>,
    pub(crate) session_recording: Option<Value>,
}

impl ProjectConfig {
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

fn decode(bytes: &[u8]) -> Result<ProjectConfig, LoadError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(LoadError::ResponseTooLarge);
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| LoadError::InvalidResponse)?;
    let object = value.as_object().ok_or(LoadError::InvalidResponse)?;
    let config = ProjectConfig {
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

type Complete = dyn FnOnce(Result<ProjectConfig, LoadError>) -> Option<flags::Task> + Send;

pub(crate) struct Task {
    url: String,
    complete: Box<Complete>,
}

impl Task {
    pub(crate) fn new(
        url: String,
        complete: impl FnOnce(Result<ProjectConfig, LoadError>) -> Option<flags::Task> + Send + 'static,
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
            Err(_) => tracing::warn!("posthog-rs: project config completion panicked"),
        }
    }
}

fn fetch(
    http: &reqwest::blocking::Client,
    options: &ClientOptions,
    url: &str,
) -> Result<ProjectConfig, LoadError> {
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
        LoadError::Network => ("Project configuration request failed", None),
        LoadError::Http(status) => (
            "Project configuration returned an unsuccessful HTTP status",
            Some(status),
        ),
        LoadError::InvalidResponse => ("Project configuration response is invalid", None),
        LoadError::ResponseTooLarge => (
            "Project configuration response exceeds its byte limit",
            None,
        ),
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
