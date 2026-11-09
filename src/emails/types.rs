use std::fmt;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// What every send takes, whatever its form.
#[derive(Debug, Clone, Default)]
pub(crate) struct SendOptions {
    pub(crate) distinct_id: Option<String>,
    pub(crate) idempotency_key: Option<String>,
    pub(crate) max_retries: Option<u32>,
}

/// One email from an email template made transactional in PostHog, sent by
/// the template's ID.
///
/// The template sets its own sender, subject and content.
///
/// ```
/// use posthog_rs::TemplateEmail;
/// use serde_json::json;
///
/// let email = TemplateEmail::new("jane@example.com", "0190f6d2-7c1e-7b3a-9f5e-1c2d3e4f5a6b")
///     .template_data(json!({ "name": "Jane" }))
///     .distinct_id("user_123")
///     .idempotency_key("welcome:user_123");
/// ```
#[derive(Debug, Clone)]
pub struct TemplateEmail {
    pub(crate) to: String,
    pub(crate) template_id: String,
    pub(crate) template_data: Option<Value>,
    pub(crate) options: SendOptions,
}

impl TemplateEmail {
    /// An email to one recipient address from the template with this UUID.
    pub fn new(to: impl Into<String>, template_id: impl Into<String>) -> Self {
        Self {
            to: to.into(),
            template_id: template_id.into(),
            template_data: None,
            options: SendOptions::default(),
        }
    }

    /// Values for the template's `{{ variables.* }}` placeholders, as a JSON
    /// object. Sent as `message_data`.
    pub fn template_data(mut self, data: Value) -> Self {
        self.template_data = Some(data);
        self
    }

    /// The recipient's distinct ID in your product. The email's events go to
    /// this person. Without it, they go to a person whose distinct ID is the
    /// email address.
    pub fn distinct_id(mut self, distinct_id: impl Into<String>) -> Self {
        self.options.distinct_id = Some(distinct_id.into());
        self
    }

    /// 1 to 255 visible ASCII characters. See [`CodeEmail::idempotency_key`].
    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.options.idempotency_key = Some(key.into());
        self
    }

    /// Overrides the client's `email_request_max_retries` for this send. `0`
    /// turns retries off.
    pub fn max_retries(mut self, max_retries: u32) -> Self {
        self.options.max_retries = Some(max_retries);
        self
    }
}

/// One email whose content your code rendered, such as React Email output,
/// named by a key. PostHog sends it as is.
///
/// It needs [`html`](Self::html), [`text`](Self::text) or both. Without
/// `text`, PostHog derives it from the `html`. To send a React email, render
/// it to HTML in your app and pass the HTML.
///
/// ```
/// use posthog_rs::CodeEmail;
///
/// # let rendered_html = String::new();
/// let email = CodeEmail::new("jane@example.com", "password-reset", "Reset your password")
///     .html(rendered_html)
///     .from("Acme <security@example.com>")
///     .distinct_id("user_123");
/// ```
#[derive(Debug, Clone)]
pub struct CodeEmail {
    pub(crate) to: String,
    pub(crate) template_key: String,
    pub(crate) subject: String,
    pub(crate) html: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) from: Option<String>,
    pub(crate) reply_to: Vec<String>,
    pub(crate) cc: Vec<String>,
    pub(crate) bcc: Vec<String>,
    pub(crate) attachments: Vec<EmailAttachment>,
    pub(crate) capture_engagement: bool,
    pub(crate) options: SendOptions,
}

impl CodeEmail {
    /// An email to one recipient address, with a subject.
    ///
    /// `template_key` names the email, such as `"password-reset"`, so its
    /// metrics, logs and status group under it in PostHog. It takes letters,
    /// digits, hyphens and underscores, at most 400.
    pub fn new(
        to: impl Into<String>,
        template_key: impl Into<String>,
        subject: impl Into<String>,
    ) -> Self {
        Self {
            to: to.into(),
            template_key: template_key.into(),
            subject: subject.into(),
            html: None,
            text: None,
            from: None,
            reply_to: Vec::new(),
            cc: Vec::new(),
            bcc: Vec::new(),
            attachments: Vec::new(),
            capture_engagement: true,
            options: SendOptions::default(),
        }
    }

    /// The HTML body.
    pub fn html(mut self, html: impl Into<String>) -> Self {
        self.html = Some(html.into());
        self
    }

    /// The plain-text body.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// A verified sender, such as `"Acme <security@example.com>"`. Without
    /// it, PostHog uses the project's default sender.
    pub fn from(mut self, from: impl Into<String>) -> Self {
        self.from = Some(from.into());
        self
    }

    /// Adds a reply-to address. Call it again to add more.
    pub fn reply_to(mut self, address: impl Into<String>) -> Self {
        self.reply_to.push(address.into());
        self
    }

    /// Adds an address that gets a visible copy. Call it again to add more,
    /// at most 10. Each is a bare address, such as `"finance@example.com"`,
    /// with no name.
    pub fn cc(mut self, address: impl Into<String>) -> Self {
        self.cc.push(address.into());
        self
    }

    /// Adds an address that gets a copy the other recipients never see. Call
    /// it again to add more, at most 10. Each is a bare address, with no name.
    pub fn bcc(mut self, address: impl Into<String>) -> Self {
        self.bcc.push(address.into());
        self
    }

    /// Adds a file to the email. Call it again to add more, at most 10 files
    /// and 40 MB for the whole email after Base64 encoding, which leaves about
    /// 29 MB for the files. PostHog refuses a file type that mailbox providers
    /// block, such as `.exe`.
    pub fn attachment(mut self, attachment: EmailAttachment) -> Self {
        self.attachments.push(attachment);
        self
    }

    /// Whether PostHog captures the email's engagement events (sent,
    /// delivered, opened, clicked, bounced) as events of the recipient.
    /// Defaults to `true`. A project whose admin turned engagement capture off
    /// captures nothing, whatever the send asks.
    pub fn capture_engagement(mut self, capture: bool) -> Self {
        self.capture_engagement = capture;
        self
    }

    /// The recipient's distinct ID in your product. The email's events go to
    /// this person. Without it, they go to a person whose distinct ID is the
    /// email address.
    pub fn distinct_id(mut self, distinct_id: impl Into<String>) -> Self {
        self.options.distinct_id = Some(distinct_id.into());
        self
    }

    /// 1 to 255 visible ASCII characters.
    ///
    /// A send with a key that PostHog already accepted queues nothing and
    /// returns the first send's result. The same key for another email returns
    /// `idempotency_key_reused`. Without a key, the client makes a UUID that
    /// covers its own retries of this call. Pass a key derived from what the
    /// email is for, such as `password-reset:user_123:<request id>`, to cover
    /// retries outside this call too.
    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.options.idempotency_key = Some(key.into());
        self
    }

    /// Overrides the client's `email_request_max_retries` for this send. `0`
    /// turns retries off.
    pub fn max_retries(mut self, max_retries: u32) -> Self {
        self.options.max_retries = Some(max_retries);
        self
    }
}

/// A file sent with a [`CodeEmail`].
///
/// PostHog can't fetch a file from a path or URL: read the file and pass its
/// bytes. Before the send, the client uploads each file PostHog doesn't have
/// yet straight to PostHog's file store, then sends the email naming each file
/// by its SHA-256. Its `Debug` output never shows the content.
///
/// ```
/// use posthog_rs::{CodeEmail, EmailAttachment};
///
/// # let invoice_pdf: Vec<u8> = Vec::new();
/// # let logo_png: Vec<u8> = Vec::new();
/// let email = CodeEmail::new("jane@example.com", "invoice", "Your invoice")
///     .html(r#"<img src="cid:logo"><p>Your invoice is attached.</p>"#)
///     .attachment(EmailAttachment::new("invoice.pdf", invoice_pdf))
///     .attachment(EmailAttachment::new("logo.png", logo_png).content_id("logo"));
/// ```
#[derive(Clone)]
pub struct EmailAttachment {
    pub(crate) filename: String,
    pub(crate) content: Bytes,
    pub(crate) sha256: String,
    content_type: Option<String>,
    content_id: Option<String>,
}

impl EmailAttachment {
    /// A file from its bytes, with the name the recipient sees, such as
    /// `"invoice.pdf"`. The client hashes the bytes here, once.
    pub fn new(filename: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        let content = Bytes::from(content.into());
        let sha256 = format!("{:x}", Sha256::digest(&content));
        Self {
            filename: filename.into(),
            content,
            sha256,
            content_type: None,
            content_id: None,
        }
    }

    /// The MIME type, such as `"application/pdf"`. Without it, the type
    /// follows from the file name.
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }

    /// Shows the file inline: the HTML references it as `cid:<content_id>`,
    /// such as `<img src="cid:logo">` for `"logo"`.
    pub fn content_id(mut self, content_id: impl Into<String>) -> Self {
        self.content_id = Some(content_id.into());
        self
    }

    /// The file as the upload request names it.
    pub(crate) fn to_upload_json(&self) -> Value {
        json!({"sha256": self.sha256, "size": self.content.len()})
    }

    /// The file as the send request references it.
    pub(crate) fn to_reference_json(&self) -> Value {
        let mut reference = json!({"sha256": self.sha256, "filename": self.filename});
        if let Some(content_type) = &self.content_type {
            reference["content_type"] = json!(content_type);
        }
        if let Some(content_id) = &self.content_id {
            reference["content_id"] = json!(content_id);
        }
        reference
    }
}

impl fmt::Debug for EmailAttachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmailAttachment")
            .field("filename", &self.filename)
            .field("content", &format_args!("<{} bytes>", self.content.len()))
            .field("sha256", &self.sha256)
            .field("content_type", &self.content_type)
            .field("content_id", &self.content_id)
            .finish()
    }
}

/// An email for [`Emails::send`](crate::Emails::send): a template made in
/// PostHog, or content your code rendered.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum SendEmail {
    /// A template made transactional in PostHog.
    Template(TemplateEmail),
    /// Content your code rendered.
    Code(CodeEmail),
}

impl From<TemplateEmail> for SendEmail {
    fn from(email: TemplateEmail) -> Self {
        SendEmail::Template(email)
    }
}

impl From<CodeEmail> for SendEmail {
    fn from(email: CodeEmail) -> Self {
        SendEmail::Code(email)
    }
}

/// A warning PostHog returned with a queued email.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EmailWarning {
    /// The warning's stable code.
    pub code: String,
    /// What the warning is about, in words.
    pub message: String,
    /// How to fix it, when PostHog said.
    pub fix: Option<String>,
}

/// The queued email.
///
/// A result means PostHog queued the email, not that it was delivered. Read
/// its delivery status with [`Emails::get`](crate::Emails::get).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SendEmailResult {
    /// The queued email's message ID.
    pub id: String,
    /// True when PostHog had already accepted this idempotency key and
    /// returned the first send's result.
    pub replayed: bool,
    /// Warnings PostHog returned with the email. The client also logs each one.
    pub warnings: Vec<EmailWarning>,
}

/// What happened to an email. Later SDK versions can know statuses that this
/// one passes through as [`Other`](Self::Other).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EmailDeliveryStatus {
    /// The email waits to go out.
    Queued,
    /// PostHog handed the email to the email provider.
    Sent,
    /// The email was not sent, and the status's `reason` says why.
    Failed,
    /// The recipient's mail server accepted the email.
    Delivered,
    /// The recipient's mail provider reported a bounce after delivery.
    Bounced,
    /// The recipient marked the email as spam.
    Complained,
    /// A status this SDK version doesn't know, as PostHog sent it.
    Other(String),
}

impl EmailDeliveryStatus {
    pub(crate) fn parse(status: &str) -> Self {
        match status {
            "queued" => EmailDeliveryStatus::Queued,
            "sent" => EmailDeliveryStatus::Sent,
            "failed" => EmailDeliveryStatus::Failed,
            "delivered" => EmailDeliveryStatus::Delivered,
            "bounced" => EmailDeliveryStatus::Bounced,
            "complained" => EmailDeliveryStatus::Complained,
            other => EmailDeliveryStatus::Other(other.to_string()),
        }
    }

    /// The status as PostHog sends it, such as `"delivered"`.
    pub fn as_str(&self) -> &str {
        match self {
            EmailDeliveryStatus::Queued => "queued",
            EmailDeliveryStatus::Sent => "sent",
            EmailDeliveryStatus::Failed => "failed",
            EmailDeliveryStatus::Delivered => "delivered",
            EmailDeliveryStatus::Bounced => "bounced",
            EmailDeliveryStatus::Complained => "complained",
            EmailDeliveryStatus::Other(status) => status,
        }
    }
}

/// The status of an email that [`Emails::send`](crate::Emails::send) queued.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EmailStatus {
    /// The message ID that `send` returned.
    pub id: String,
    /// What happened to the email.
    pub status: EmailDeliveryStatus,
    /// When PostHog queued the email.
    pub created_at: DateTime<Utc>,
    /// When the email reached its current status.
    pub status_at: DateTime<Utc>,
    /// Why a failed email was not sent, such as `provider_error` or
    /// `recipient_suppressed`, or the bounce type of a bounced email: `hard`,
    /// `soft` or `unknown`. `None` otherwise.
    pub reason: Option<String>,
}
