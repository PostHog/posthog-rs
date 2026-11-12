---
cargo/posthog: major
---

Set per-event capture options with `Event::insert_option`, or `CaptureExceptionOptions::option` for exceptions. The SDK sends them in an `options` map next to `properties`, unchanged, and PostHog validates them. `Event::alias` and `Event::group_identify` are now public, so you can set options on the events that `alias` and `group_identify` send.

Breaking changes:

- Move the `$process_person_profile`, `$cookieless_mode`, `$ignore_sent_at` and `$product_tour_id` properties to the `process_person_profile`, `cookieless_mode`, `disable_skew_correction` and `product_tour_id` options. Legacy option properties still work, but an option overrides its legacy property. Use options.
- The SDK no longer converts option values. PostHog reads common forms such as `"false"`, `"no"` or `0`, and drops an event whose option value it cannot read. That event reaches `on_error` as a per-event `drop` with the detail `invalid_options`.
- Adding a group no longer turns on person processing. An anonymous event with groups stays personless; set the `process_person_profile` option to `true` if you need it in group analytics.
- `Event::new_anon` sets the `process_person_profile` option to `false`. Set the option to `true` to process person profiles for an anonymous event.
- `$os`, `$os_version`, `$is_server` and `$geoip_disable` are added before `before_send`, only for keys the event leaves unset. The hook sees them and can remove them, and they are not added again after the hook.
- `Event::with_flags` keeps flag properties the event already has, and `Event::add_group` groups merge into a `$groups` property key by key.
