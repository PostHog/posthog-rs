use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

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
    pub(crate) tracking: Option<EmailTracking>,
    pub(crate) capture_engagement: Option<bool>,
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
            tracking: None,
            capture_engagement: None,
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

    /// What PostHog tracks for this email: [`EmailTracking::On`],
    /// [`EmailTracking::Off`], or [`EmailTrackingOptions`] to set each kind on
    /// its own. Takes a `bool` too.
    ///
    /// Without it, PostHog counts opens and captures engagement events, but
    /// doesn't rewrite links to count clicks, because a link in an email sent
    /// from code can carry a login or password reset token.
    ///
    /// ```
    /// use posthog_rs::{CodeEmail, EmailTrackingOptions};
    ///
    /// // The digest's links carry no login or reset token, so count clicks.
    /// let email = CodeEmail::new("jane@example.com", "weekly-digest", "Your weekly digest")
    ///     .html("<p>Read this week's posts: https://example.com/blog</p>")
    ///     .tracking(EmailTrackingOptions::new().clicks(true));
    /// ```
    pub fn tracking(mut self, tracking: impl Into<EmailTracking>) -> Self {
        self.tracking = Some(tracking.into());
        self
    }

    /// Whether PostHog captures the email's engagement events (sent,
    /// delivered, opened, clicked, bounced) as events of the recipient.
    /// Defaults to `true`. A project whose admin turned engagement capture off
    /// captures nothing, whatever the send asks. PostHog refuses a send that
    /// sets both this and [`tracking`](Self::tracking).
    #[deprecated(note = "use `tracking` with `EmailTrackingOptions::engagement` instead")]
    pub fn capture_engagement(mut self, capture: bool) -> Self {
        self.capture_engagement = Some(capture);
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

/// What PostHog tracks for a [`CodeEmail`]. A `bool` or an
/// [`EmailTrackingOptions`] converts into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailTracking {
    /// Tracks opens and clicks and captures engagement events. Use it only
    /// when the email's links are safe to send through a redirect.
    On,
    /// Turns off every kind of tracking, including kinds PostHog adds later.
    /// PostHog then can't count the email's opens and clicks, and captures no
    /// engagement events.
    Off,
    /// Sets each kind of tracking on its own.
    Custom(EmailTrackingOptions),
}

impl EmailTracking {
    pub(crate) fn to_json(self) -> Value {
        match self {
            EmailTracking::On => Value::Bool(true),
            EmailTracking::Off => Value::Bool(false),
            EmailTracking::Custom(options) => options.to_json(),
        }
    }
}

impl From<bool> for EmailTracking {
    fn from(track: bool) -> Self {
        if track {
            EmailTracking::On
        } else {
            EmailTracking::Off
        }
    }
}

impl From<EmailTrackingOptions> for EmailTracking {
    fn from(options: EmailTrackingOptions) -> Self {
        EmailTracking::Custom(options)
    }
}

/// Each kind of tracking of a [`CodeEmail`], on or off. A kind you don't set
/// keeps its default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmailTrackingOptions {
    opens: Option<bool>,
    clicks: Option<bool>,
    engagement: Option<bool>,
}

impl EmailTrackingOptions {
    /// Every kind at its default.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether PostHog adds its open pixel to count opens. Defaults to `true`.
    /// Click tracking needs open tracking.
    pub fn opens(mut self, track: bool) -> Self {
        self.opens = Some(track);
        self
    }

    /// Whether PostHog rewrites the email's links to count clicks. Defaults to
    /// `false`. Turn it on only when the links are safe to send through a
    /// redirect, so not for a login or password reset link. A link with a
    /// `data-ph-no-track` attribute is never rewritten.
    pub fn clicks(mut self, track: bool) -> Self {
        self.clicks = Some(track);
        self
    }

    /// Whether PostHog captures the email's engagement events, such as opens
    /// and clicks, as events of the recipient. Defaults to `true`.
    pub fn engagement(mut self, capture: bool) -> Self {
        self.engagement = Some(capture);
        self
    }

    fn to_json(self) -> Value {
        let mut object = Map::new();
        for (key, value) in [
            ("opens", self.opens),
            ("clicks", self.clicks),
            ("engagement", self.engagement),
        ] {
            if let Some(value) = value {
                object.insert(key.into(), Value::Bool(value));
            }
        }
        Value::Object(object)
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
