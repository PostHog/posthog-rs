Transactional email on a [`Client`](crate::Client): sends email from
templates made in PostHog or from content your code rendered, and reads the
status of each email it sent. Get it with
[`Client::emails`](crate::Client::emails).

# Setup

1. Create a project secret API key (`phs_...`) with Workflow write access
   (`hog_flow:write`) in your project settings, and set it as the client's
   `secret_key`. The project API key (`phc_...`) captures events but can't
   send email.
2. On PostHog Cloud, set `host` to your region, such as
   `https://eu.i.posthog.com`, and the client sends email to that region's
   app host. For any other `host`, such as a reverse proxy, set `app_host`
   to your PostHog app host: the proxy must not receive the secret key.
3. To send a template made in PostHog, open it under Workflows, make it
   transactional and copy its ID.

A result means PostHog queued the email. Delivery happens later: read it
with [`Emails::get`].

# Timeouts and retries

Each request times out after `email_request_timeout_seconds`, 15 by
default. A request retries network errors, rate limits, a send still in
flight and an unavailable service `email_request_max_retries` times, 2 by
default, after about 1 and then 2 seconds, or after the server's
`Retry-After` when that is longer. A `Retry-After` over 30 seconds fails at
once. Every attempt of a send carries the same idempotency key, so a retry
never sends twice. When a job runner already retries the send, set
`max_retries(0)` on it and pass an idempotency key that names the email.

# Moving from another email provider

Set `migrated_from`, such as `"resend"`, on the client, so PostHog knows
where your sends came from.
