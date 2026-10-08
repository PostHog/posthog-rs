---
cargo/posthog-rs: major
---

Set per-event capture options with `Event::insert_option`, or `CaptureExceptionOptions::option` for exceptions. The SDK sends them in an `options` map next to `properties`, unchanged, and PostHog validates them.

Breaking changes:

- The legacy `$process_person_profile`, `$cookieless_mode`, `$ignore_sent_at` and `$product_tour_id` properties move into their options and are removed from `properties`. An option you set wins; a legacy property fills its option only when the option is missing or `null`.
- The SDK no longer converts legacy property values. PostHog reads common forms such as `"false"`, `"no"` or `0`, and drops an event whose option value it cannot read. That event reaches `on_error` as a per-event `drop` with the detail `invalid_options`.
- Adding a group no longer turns on person processing. An anonymous event with groups stays personless; set the `process_person_profile` option to `true` if you need it in group analytics.
- `Event::new_anon` sets the `process_person_profile` option instead of the legacy property, so only an option can turn processing back on.
- `$os`, `$os_version`, `$is_server` and `$geoip_disable` are added before `before_send`, only for keys the event leaves unset. The hook sees them and can remove them, and they are not added again after the hook.
- `Event::with_flags` keeps flag properties the event already has, and `Event::add_group` groups merge into a `$groups` property key by key.
