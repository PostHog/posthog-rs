use reqwest::header::HeaderMap;
use reqwest::{Client as HttpClient, Method};

use super::error::EmailError;
use super::shared::{self, Accepted, EmailTracker, Reply};
use super::types::{EmailStatus, SendEmail, SendEmailResult};
use crate::client::{parse_retry_after, ClientOptions};

#[doc = include_str!("guide.md")]
pub struct Emails<'a> {
    options: &'a ClientOptions,
    http: Option<&'a HttpClient>,
    tracker: &'a EmailTracker,
}

impl<'a> Emails<'a> {
    pub(crate) fn new(
        options: &'a ClientOptions,
        http: Option<&'a HttpClient>,
        tracker: &'a EmailTracker,
    ) -> Self {
        Self {
            options,
            http,
            tracker,
        }
    }

    /// Queues one email, from a template made in PostHog or from content your
    /// code rendered.
    ///
    /// `Ok` means PostHog queued the email, not that it was delivered. Network
    /// errors, rate limits, a send with the same idempotency key still in
    /// flight and an unavailable service are retried with the same idempotency
    /// key.
    ///
    /// # Errors
    ///
    /// Returns an [`EmailError`] when an argument check fails, the client is
    /// disabled, shut down or has no `secret_key`, or PostHog did not queue the
    /// email.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use posthog::{ClientOptionsBuilder, CodeEmail, TemplateEmail};
    /// use serde_json::json;
    ///
    /// # async fn example(rendered_html: String) -> Result<(), posthog::EmailError> {
    /// let options = ClientOptionsBuilder::default()
    ///     .api_key("phc_...".to_string())
    ///     .secret_key("phs_...")
    ///     .host("https://eu.i.posthog.com")
    ///     .build()
    ///     .unwrap();
    /// let posthog = posthog::client(options).await;
    ///
    /// // A template made in PostHog.
    /// let sent = posthog
    ///     .emails()
    ///     .send(
    ///         TemplateEmail::new("jane@example.com", "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b")
    ///             .template_data(json!({ "name": "Jane" }))
    ///             .distinct_id("user_123")
    ///             .idempotency_key("welcome:user_123"),
    ///     )
    ///     .await?;
    /// println!("queued {}", sent.id);
    ///
    /// // Content your code rendered, such as React Email output.
    /// posthog
    ///     .emails()
    ///     .send(
    ///         CodeEmail::new("jane@example.com", "password-reset", "Reset your password")
    ///             .html(rendered_html)
    ///             .distinct_id("user_123"),
    ///     )
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send(&self, email: impl Into<SendEmail>) -> Result<SendEmailResult, EmailError> {
        let prepared = shared::prepare_send(email.into())?;
        let _in_flight = self
            .tracker
            .admit(self.options.is_disabled(), shared::SEND_NOT_DONE)?;
        let connection = shared::connect(self.options, shared::SEND_KEY_NEEDED)?;
        let headers = connection.headers(Some(&prepared.idempotency_key))?;
        let url = format!("{}{}", connection.app_host, shared::SEND_PATH);
        let max_retries = prepared.max_retries.unwrap_or(connection.max_retries);
        let reply = self
            .request(
                Method::POST,
                &url,
                &headers,
                Some(&prepared.body),
                &connection,
                max_retries,
            )
            .await?;
        let result = shared::parse_send_reply(reply)?;
        shared::log_warnings(&result);
        Ok(result)
    }

    /// Reads the status of an email that [`send`](Self::send) queued, for 30
    /// days after the send. A key with only `transactional_email:read` can read
    /// statuses but not send.
    ///
    /// # Errors
    ///
    /// Returns an [`EmailError`] when `message_id` is not a message ID, the
    /// client can't reach PostHog, or PostHog answered with an error. The code
    /// is `not_found` when no email of this project has the ID, when the email
    /// is older than 30 days, or for a few seconds after the send.
    pub async fn get(&self, message_id: &str) -> Result<EmailStatus, EmailError> {
        let path = shared::status_path(message_id)?;
        let _in_flight = self
            .tracker
            .admit(self.options.is_disabled(), shared::GET_NOT_DONE)?;
        let connection = shared::connect(self.options, shared::GET_KEY_NEEDED)?;
        let headers = connection.headers(None)?;
        let url = format!("{}{}", connection.app_host, path);
        let reply = self
            .request(
                Method::GET,
                &url,
                &headers,
                None,
                &connection,
                connection.max_retries,
            )
            .await?;
        shared::parse_status_reply(reply)
    }

    /// Sends the request until PostHog answers `2xx`, retrying what
    /// [`shared::retry_delay`] allows. Every attempt sends the same headers, so
    /// a send keeps its idempotency key.
    async fn request(
        &self,
        method: Method,
        url: &str,
        headers: &HeaderMap,
        body: Option<&[u8]>,
        connection: &shared::Connection,
        max_retries: u32,
    ) -> Result<Accepted, EmailError> {
        let mut attempt = 0;
        loop {
            let error = match self
                .attempt(method.clone(), url, headers, body, connection)
                .await
                .and_then(shared::accept)
            {
                Ok(reply) => return Ok(reply),
                Err(error) => error,
            };
            let Some(delay) = shared::retry_delay(&error, attempt, max_retries) else {
                return Err(error);
            };
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    async fn attempt(
        &self,
        method: Method,
        url: &str,
        headers: &HeaderMap,
        body: Option<&[u8]>,
        connection: &shared::Connection,
    ) -> Result<Reply, EmailError> {
        let http = self.http.ok_or_else(|| {
            EmailError::new(
                "client_disabled",
                "The PostHog client has no HTTP client, so it is disabled.",
            )
        })?;
        let mut request = http
            .request(method, url)
            .timeout(connection.timeout)
            .headers(headers.clone());
        if let Some(body) = body {
            request = request.body(body.to_vec());
        }
        let response = request.send().await.map_err(shared::request_error)?;
        let status = response.status().as_u16();
        let retry_after = parse_retry_after(response.headers());
        shared::reply_from_body(status, retry_after, read_capped(response).await)
    }
}

/// Reads the body, decompressed, up to [`shared::MAX_REPLY_BYTES`].
async fn read_capped(mut response: reqwest::Response) -> Result<Vec<u8>, EmailError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(shared::read_error)? {
        if body.len() + chunk.len() > shared::MAX_REPLY_BYTES {
            return Err(shared::reply_too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
