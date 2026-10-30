---
cargo/posthog-rs: patch
---

An event from `Event::new_anon` whose `process_person_profile` option is `null` or removed gets `false` again before `before_send`, so it stays personless unless the option is set to `true`.
