---
cargo/posthog: minor
---

`RequestContext::option` sets a capture option on every event in a request context, including exceptions. An option the event sets, other than `null`, wins. A context `process_person_profile` replaces the `false` that `Event::new_anon` sets for a personless event. A context `process_person_profile` does not set `process_person_profile` on identify-style events (`$identify`, `$create_alias`, `$merge_dangerously`, `$groupidentify`, `$set`, `$set_once`) whose distinct ID the SDK did not generate; those default to `true` unless the event sets it.
