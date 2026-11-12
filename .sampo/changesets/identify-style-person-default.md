---
cargo/posthog: minor
---

`$identify`, `$create_alias`, `$merge_dangerously`, `$groupidentify`, `$set` and `$set_once` events now send `process_person_profile: true` unless the event sets the option or the legacy `$process_person_profile` property, which then wins over the request context. A request context of `false` no longer reaches `alias` and `group_identify`, so PostHog keeps their events. An event whose distinct ID the SDK generated stays personless, and `before_send` sees the value and can change it. `Event::alias` and `Event::group_identify` are now public, so you can set options on those events before you capture them.
