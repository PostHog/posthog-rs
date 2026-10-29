---
cargo/posthog-rs: minor
---

`RequestContext::option` sets a capture option on every event in a request context, including exceptions. An option the event sets, other than `null`, wins. A context `process_person_profile` replaces the `false` that `Event::new_anon` sets for a personless event.
