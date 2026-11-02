use reqwest::header::HeaderMap;
use reqwest::{Client as HttpClient, Method};

use super::error::EmailError;
use super::shared::{self, EmailTracker, Reply};
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
    /// use posthog_rs::{ClientOptionsBuilder, CodeEmail, TemplateEmail};
    /// use serde_json::json;
    ///
    /// # async fn example(rendered_html: String) -> Result<(), posthog_rs::EmailError> {
    /// let options = ClientOptionsBuilder::default()
    ///     .api_key("phc_...".to_string())
    ///     .secret_key("phs_...")
    ///     .host("https://eu.i.posthog.com")
    ///     .build()
    ///     .unwrap();
    /// let posthog = posthog_rs::client(options).await;
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
        let mut attempt = 0;
        let result = loop {
            let error = match self
                .request(
                    Method::POST,
                    &url,
                    &headers,
                    Some(&prepared.body),
                    &connection,
                )
                .await
                .and_then(shared::parse_send_reply)
            {
                Ok(result) => break result,
                Err(error) => error,
            };
            let Some(delay) = shared::retry_delay(&error, attempt, max_retries) else {
                return Err(error);
            };
            tokio::time::sleep(delay).await;
            attempt += 1;
        };
        shared::log_warnings(&result);
        Ok(result)
    }

    /// Reads the status of an email that [`send`](Self::send) queued, for 30
    /// days after the send. A key with only `hog_flow:read` can read statuses.
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
        let mut attempt = 0;
        loop {
            let error = match self
                .request(Method::GET, &url, &headers, None, &connection)
                .await
                .and_then(shared::parse_status_reply)
            {
                Ok(status) => return Ok(status),
                Err(error) => error,
            };
            let Some(delay) = shared::retry_delay(&error, attempt, connection.max_retries) else {
                return Err(error);
            };
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }

    async fn request(
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
        match response.bytes().await {
            Ok(bytes) => Ok(Reply {
                status,
                payload: serde_json::from_slice(&bytes).ok(),
                retry_after,
            }),
            // An accepted send may already be queued, so a failed read stays a
            // retryable network error.
            Err(error) if (200..300).contains(&status) => Err(shared::request_error(error)),
            // The status and Retry-After still decide the retry when an error
            // body can't be read.
            Err(_) => Ok(Reply {
                status,
                payload: None,
                retry_after,
            }),
        }
    }
}
