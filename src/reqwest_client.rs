use std::collections::HashSet;

use reqwest::{header::HeaderValue, Client, ClientBuilder, Request, Response};

use crate::RequestContext;

/// An opt-in application HTTP client that sends analytics context to exact allowed hosts.
///
/// This client disables automatic redirects and returns redirect responses to the caller.
/// Reqwest otherwise forwards custom headers across redirects, which can expose analytics IDs.
/// Configure a separate client for requests that need automatic redirects.
///
/// The supplied builder retains its other settings. Do not set PostHog tracing headers as
/// client default headers: those defaults belong to the application and can apply to any host.
/// This client does not change the SDK's event ingestion transport.
#[derive(Clone)]
pub struct PostHogReqwestClient {
    client: Client,
    allowed_hosts: HashSet<String>,
}

impl PostHogReqwestClient {
    /// Builds a dedicated client with automatic redirects disabled.
    ///
    /// Hosts are trimmed and compared without case sensitivity. Each entry must be a hostname
    /// without a scheme, port, path, or wildcard. An empty list disables header injection.
    pub fn new<I, S>(builder: ClientBuilder, allowed_hosts: I) -> Result<Self, reqwest::Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Ok(Self {
            client: builder
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            allowed_hosts: allowed_hosts
                .into_iter()
                .map(|host| host.as_ref().trim().to_ascii_lowercase())
                .collect(),
        })
    }

    /// Use this client to build requests, then pass them to [`Self::execute`].
    /// Calling reqwest's `send` or `execute` directly does not inject context.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Sends a request with the context active when this future is first polled.
    ///
    /// Build the request with [`Self::client`] to retain its client configuration.
    /// On allowed hosts, current values replace stale tracing headers. Missing values and
    /// values that cannot form valid HTTP headers are omitted. Other hosts are unchanged.
    /// Spawned tasks can use an explicit [`RequestContext::scope`] to carry context.
    pub async fn execute(&self, mut request: Request) -> Result<Response, reqwest::Error> {
        if request
            .url()
            .host_str()
            .map(|host| self.allowed_hosts.contains(&host.to_ascii_lowercase()))
            .unwrap_or(false)
        {
            let (distinct_id, session_id) = RequestContext::current_ids();
            for (name, value) in [
                ("x-posthog-distinct-id", distinct_id.as_deref()),
                ("x-posthog-session-id", session_id.as_deref()),
            ] {
                request.headers_mut().remove(name);
                if let Some(mut value) = value.and_then(|value| HeaderValue::from_str(value).ok()) {
                    value.set_sensitive(true);
                    request.headers_mut().insert(name, value);
                }
            }
        }
        self.client.execute(request).await
    }
}
