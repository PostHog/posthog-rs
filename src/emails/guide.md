Transactional email on a [`Client`](crate::Client): sends email from
templates made in PostHog or from content your code rendered, and reads the
status of each email it sent. Get it with
[`Client::emails`](crate::Client::emails).

# Setup

1. Create a project secret API key (`phs_...`) with the "Send transactional
   email" preset (`transactional_email:write`) in your project settings, and
   set it as the client's `secret_key`. Add `transactional_email:read` too if
   you read email statuses with a key that can't send. Keys made with
   `hog_flow:write` still work. The project API key (`phc_...`) captures
   events but can't send email.
2. On PostHog Cloud, set `host` to your region, such as
   `https://eu.i.posthog.com`, and the client sends email to that region's
   app host. For any other `host`, such as a reverse proxy, set `app_host`
   to your PostHog app host: the proxy must not receive the secret key.
3. To send a template made in PostHog, open it under Workflows, make it
   transactional and copy its ID.

A result means PostHog queued or scheduled the email. Delivery happens
later: read it with [`Emails::get`].

# Copies and attachments

A [`CodeEmail`](crate::CodeEmail) takes `cc`, `bcc` and files: add each
with [`cc`](crate::CodeEmail::cc), [`bcc`](crate::CodeEmail::bcc) and
[`attachment`](crate::CodeEmail::attachment). PostHog takes at most 10
addresses in each of `cc` and `bcc`. An email carries at most 10 files and
40 MB after Base64 encoding, which leaves about 29 MB for the files. The
40 MB counts the bodies and the subject too, and the HTML twice when the
email has no text, because PostHog then sends text it derives from the HTML.
The client checks both before it sends anything, and refuses the send with
`too_many_attachments` or `message_too_large`.
A template made in PostHog sets its own recipients and content, so a
[`TemplateEmail`](crate::TemplateEmail) takes none of them.

The client sends files in two steps. It asks PostHog which files it lacks,
naming each by its SHA-256 and size, and uploads those straight to
PostHog's file store, without your secret key. A file this project sent
before isn't uploaded again. Once every upload succeeded, it sends the email
naming each file by its SHA-256. If the file store refuses an upload, the
send fails with `attachment_upload_failed` and the store's answer in
[`why`](crate::EmailError::why), and no email is sent. A PostHog without
the upload route, a project with sending email from code off, or an
`app_host` that isn't PostHog's answers the upload request with `404`: the
send fails with PostHog's answer, such as `not_found`, on `attachments`,
and no email is sent. Allow your app to reach the file store's host as
well as your PostHog app host.

Uploads go only over https: when PostHog names a file store URL that isn't
https, the send fails with `api_error` on that attachment before any
upload, and no email is sent. Fix the file store's URL. Only an `app_host` that is itself plain
`http`, such as a PostHog on your own machine, lets files upload over plain
`http`. Uploads go through a client the SDK builds, even when you supply
your own HTTP client, so your client's default headers never reach the
file store. That client follows no redirect, so a file never moves to
another host, trusts the system's root certificates, and takes its proxy
from the environment, such as `HTTPS_PROXY`. None of your client's own
settings apply to uploads: a root certificate, TLS option or proxy set in
code doesn't reach them.

# Scheduling

Pass a time to `scheduled_at` on a [`TemplateEmail`](crate::TemplateEmail)
or a [`CodeEmail`](crate::CodeEmail) to send the email up to 7 days later.
Only PostHog Cloud schedules emails: with any other app host, such as a
self-hosted PostHog, a reverse proxy or a custom domain, the send fails with
`scheduling_requires_cloud` before any request.

The client rounds the time up to the next millisecond, so the email never
goes out early. A scheduled result carries the time PostHog confirmed in
`scheduled_at`, and its [`status`](crate::SendEmailResult::status) is
[`SendEmailStatus::Scheduled`](crate::SendEmailStatus::Scheduled).
[`Emails::get`] reads [`EmailDeliveryStatus::Scheduled`](crate::EmailDeliveryStatus::Scheduled)
until the email goes out. A time that has already passed sends the email
now, and the result's status is `Queued`. When PostHog accepts the email
without confirming its time, the send fails with `schedule_not_confirmed`
and the error's [`message_id`](crate::EmailError::message_id): read the
email's status before you send it again. That includes PostHog sending the
email now while its time is more than a minute after the send's first
attempt, by this machine's clock. The error can also come for an email
that went out on time: when this machine's clock runs over a minute slow,
or when retries reach PostHog after a time due just over a minute after the
first attempt.

Work out the time once and reuse it on your own retries: the idempotency key
covers the time in UTC to the millisecond, so the same key with a time in
another millisecond fails with `idempotency_key_reused`. To stop scheduled
emails before they go out, disable the email's workflow in PostHog until
they have reached a final status. That refuses every new send of it meanwhile.

# Batches

[`Emails::batch`] sends the same email to many recipients, one email each:
a [`TemplateBatch`](crate::TemplateBatch) with `template_data` per
recipient for a template made in PostHog, or a
[`CodeBatch`](crate::CodeBatch) with content your code rendered. The client
sends at most 100 recipients and 4 MiB of JSON per request, one request
after another, and waits at least 45 seconds for each answer. Each
[`BatchRecipient`](crate::BatchRecipient) has its own idempotency key,
given or made by the client, that every retry reuses. Check each result's
[`status`](crate::BatchEmailResult::status):

- `Queued`: PostHog accepted the email.
- `Failed`: PostHog refused it, and `error` says why, such as a suppressed
  address.
- `Unconfirmed`: the client can't tell whether PostHog accepted it, such as
  after a lost answer. Send it again with the same key within 24 hours, so
  it goes out once. It stays `Unconfirmed` when a later answer refuses it,
  since that doesn't prove it wasn't sent: `later_error` holds that answer.
- `NotAttempted`: an earlier request of the batch failed, so the client
  sent nothing for it.

At the end of a batch, the client reads by its key each `Unconfirmed` email
that has no message ID, and adds the ID it finds. That read never changes
the status. A request that fails as a whole, such as a rate limit past its
retries, stops the batch with an [`EmailError`](crate::EmailError) whose
[`batch_result`](crate::EmailError::batch_result) holds every recipient.
The code `batch_unavailable` (`409`) means the project can't send batches
yet, and a `404` that the template doesn't exist or batch sends are off for
the project: send each email the batch didn't queue with [`Emails::send`]
and its recipient's idempotency key. A batch takes no `cc`, `bcc`,
attachments or `scheduled_at`.

# Timeouts and retries

Each request times out after `email_request_timeout_seconds`, 15 by
default. A request retries network errors, rate limits, a send still in
flight and an unavailable service `email_request_max_retries` times, 2 by
default, after about 1 and then 2 seconds, or after the server's
`Retry-After` when that is longer. A `Retry-After` over 30 seconds fails at
once. An upload also retries the file store's `500`, `502` and `504`. An
upload gets one more second for each 128 KiB of its file, so a
29 MB file has about 4 minutes. Every attempt of a send carries the same
idempotency key, so a retry never sends twice. When a job runner already retries the send, set
`max_retries(0)` on it and pass an idempotency key that names the email.

# Moving from another email provider

Set `migrated_from`, such as `"resend"`, on the client, so PostHog knows
where your sends came from.
