---
cargo/posthog-rs: major
---

Set per-event capture options with `Event::insert_option`, or `CaptureExceptionOptions::option` for exceptions. The SDK sends them in an `options` map next to `properties`, unchanged, and PostHog validates them.

Breaking changes:

- The legacy `$process_person_profile`, `$cookieless_mode`, `$ignore_sent_at` and `$product_tour_id` properties move into their options and are removed from `properties`. An option you set wins; a legacy property fills its option only when the option is missing or `null`.
- The SDK no longer converts legacy property values. PostHog reads common forms such as `"false"`, `"no"` or `0`, and drops an event whose option value it cannot read. That event reaches `on_error` as a per-event `drop` with the detail `invalid_options`.
- Events with groups always process person profiles, even when `process_person_profile` is `false`.
