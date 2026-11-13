use std::io::Read;
use std::sync::OnceLock;

use reqwest::blocking::Client as HttpClient;
use reqwest::header::HeaderMap;
use reqwest::Method;

use super::error::EmailError;
use super::shared::{self, Accepted, EmailTracker, Reply, Upload};
use super::types::{EmailAttachment, EmailStatus, SendEmail, SendEmailResult};
use crate::client::{parse_retry_after, ClientOptions};

#[doc = include_str!("guide.md")]
pub struct Emails<'a> {
    options: &'a ClientOptions,
    http: Option<&'a HttpClient>,
    upload_http: &'a OnceLock<HttpClient>,
    tracker: &'a EmailTracker,
}

impl<'a> Emails<'a> {
    pub(crate) fn new(
        options: &'a ClientOptions,
        http: Option<&'a HttpClient>,
        upload_http: &'a OnceLock<HttpClient>,
        tracker: &'a EmailTracker,
    ) -> Self {
        Self {
            options,
            http,
            upload_http,
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
    /// A [`CodeEmail`](crate::CodeEmail) with attachments first asks PostHog
    /// which files it lacks, uploads those straight to PostHog's file store,
    /// then sends the email naming each file by its SHA-256.
    ///
    /// # Errors
    ///
    /// Returns an [`EmailError`] when an argument check fails, the client is
    /// disabled, shut down or has no `secret_key`, the file store refused an
    /// upload (`attachment_upload_failed`), or PostHog did not queue the
    /// email.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use posthog_rs::{ClientOptionsBuilder, CodeEmail, TemplateEmail};
    /// use serde_json::json;
    ///
    /// # fn example(rendered_html: String) -> Result<(), posthog_rs::EmailError> {
    /// let options = ClientOptionsBuilder::default()
    ///     .api_key("phc_...".to_string())
    ///     .secret_key("phs_...")
    ///     .host("https://eu.i.posthog.com")
    ///     .build()
    ///     .unwrap();
    /// let posthog = posthog_rs::client(options);
    ///
    /// // A template made in PostHog.
    /// let sent = posthog
    ///     .emails()
    ///     .send(
    ///         TemplateEmail::new("jane@example.com", "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b")
    ///             .template_data(json!({ "name": "Jane" }))
    ///             .distinct_id("user_123")
    ///             .idempotency_key("welcome:user_123"),
    ///     )?;
    /// println!("queued {}", sent.id);
    ///
    /// // Content your code rendered, such as React Email output.
    /// posthog
    ///     .emails()
    ///     .send(
    ///         CodeEmail::new("jane@example.com", "password-reset", "Reset your password")
    ///             .html(rendered_html)
    ///             .distinct_id("user_123"),
    ///     )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn send(&self, email: impl Into<SendEmail>) -> Result<SendEmailResult, EmailError> {
        let prepared = shared::prepare_send(email.into())?;
        let _in_flight = self
            .tracker
            .admit(self.options.is_disabled(), shared::SEND_NOT_DONE)?;
        let connection = shared::connect(self.options, shared::SEND_KEY_NEEDED)?;
        let max_retries = prepared.max_retries.unwrap_or(connection.max_retries);
        if !prepared.attachments.is_empty() {
            self.upload(&prepared.attachments, &connection, max_retries)?;
        }
        let headers = connection.headers(Some(&prepared.idempotency_key))?;
        let url = format!("{}{}", connection.app_host, shared::SEND_PATH);
        let reply = self.request(
            Method::POST,
            &url,
            &headers,
            Some(&prepared.body),
            &connection,
            max_retries,
        )?;
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
    pub fn get(&self, message_id: &str) -> Result<EmailStatus, EmailError> {
        let path = shared::status_path(message_id)?;
        let _in_flight = self
            .tracker
            .admit(self.options.is_disabled(), shared::GET_NOT_DONE)?;
        let connection = shared::connect(self.options, shared::GET_KEY_NEEDED)?;
        let headers = connection.headers(None)?;
        let url = format!("{}{}", connection.app_host, path);
        let reply = self.request(
            Method::GET,
            &url,
            &headers,
            None,
            &connection,
            connection.max_retries,
        )?;
        shared::parse_status_reply(reply)
    }

    /// Uploads each file PostHog lacks, so the send can name it by its hash.
    fn upload(
        &self,
        attachments: &[EmailAttachment],
        connection: &shared::Connection,
        max_retries: u32,
    ) -> Result<(), EmailError> {
        let request = shared::uploads_request(connection, attachments)?;
        let reply = self
            .request(
                Method::POST,
                &request.url,
                &request.headers,
                Some(&request.body),
                connection,
                max_retries,
            )
            .map_err(shared::upload_route_error)?;
        let http = self.upload_http()?;
        for upload in shared::uploads_to_make(reply, attachments, connection)? {
            retrying(max_retries, || self.put(http, &upload))?;
        }
        Ok(())
    }

    /// Sends the request until PostHog answers `2xx`, retrying what
    /// [`shared::retry_delay`] allows. Every attempt sends the same headers, so
    /// a send keeps its idempotency key.
    fn request(
        &self,
        method: Method,
        url: &str,
        headers: &HeaderMap,
        body: Option<&[u8]>,
        connection: &shared::Connection,
        max_retries: u32,
    ) -> Result<Accepted, EmailError> {
        retrying(max_retries, || {
            self.attempt(method.clone(), url, headers, body, connection)
                .and_then(shared::accept)
        })
    }

    fn attempt(
        &self,
        method: Method,
        url: &str,
        headers: &HeaderMap,
        body: Option<&[u8]>,
        connection: &shared::Connection,
    ) -> Result<Reply, EmailError> {
        let mut request = self
            .http()?
            .request(method, url)
            .timeout(connection.timeout)
            .headers(headers.clone());
        if let Some(body) = body {
            request = request.body(body.to_vec());
        }
        let response = request.send().map_err(shared::request_error)?;
        let status = response.status().as_u16();
        let retry_after = parse_retry_after(response.headers());
        shared::reply_from_body(status, retry_after, read_capped(response))
    }

    /// PUTs one file with only the headers PostHog signed, never the secret
    /// key or a supplied client's default headers.
    fn put(&self, http: &HttpClient, upload: &Upload) -> Result<(), EmailError> {
        let response = http
            .request(upload.method.clone(), &upload.url)
            .timeout(upload.timeout)
            .headers(upload.headers.clone())
            .body(upload.content.clone())
            .send()
            .map_err(|error| upload.request_error(error))?;
        let status = response.status().as_u16();
        let retry_after = parse_retry_after(response.headers());
        upload.check_reply(status, retry_after, read_capped(response).ok())
    }

    /// The client that uploads files, built once and kept so its connections
    /// are reused. A client the app supplied may carry default headers, such
    /// as credentials, that must never reach the file store. And it follows no
    /// redirect, which could lead the file to a host that isn't https.
    fn upload_http(&self) -> Result<&'a HttpClient, EmailError> {
        self.http()?;
        if let Some(built) = self.upload_http.get() {
            return Ok(built);
        }
        let built = HttpClient::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(shared::upload_client_error)?;
        Ok(self.upload_http.get_or_init(|| built))
    }

    fn http(&self) -> Result<&'a HttpClient, EmailError> {
        self.http.ok_or_else(|| {
            EmailError::new(
                "client_disabled",
                "The PostHog client has no HTTP client, so it is disabled.",
            )
        })
    }
}

/// Runs `attempt` until it succeeds, retrying what [`shared::retry_delay`]
/// allows.
fn retrying<T>(
    max_retries: u32,
    mut attempt: impl FnMut() -> Result<T, EmailError>,
) -> Result<T, EmailError> {
    let mut attempts = 0;
    loop {
        let error = match attempt() {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let Some(delay) = shared::retry_delay(&error, attempts, max_retries) else {
            return Err(error);
        };
        std::thread::sleep(delay);
        attempts += 1;
    }
}

/// Reads the body, decompressed, up to [`shared::MAX_REPLY_BYTES`].
fn read_capped(response: reqwest::blocking::Response) -> Result<Vec<u8>, EmailError> {
    let mut body = Vec::new();
    response
        .take(shared::MAX_REPLY_BYTES as u64 + 1)
        .read_to_end(&mut body)
        .map_err(shared::read_error)?;
    if body.len() > shared::MAX_REPLY_BYTES {
        return Err(shared::reply_too_large());
    }
    Ok(body)
}
