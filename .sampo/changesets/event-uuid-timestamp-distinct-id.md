---
cargo/posthog-rs: major
---

Assign an event's uuid when the client takes the event, so a cloned or reused `Event` no longer sends one uuid twice. `Event::set_timestamp` no longer rejects future timestamps. `Event::new` with an empty or whitespace-only distinct ID creates an anonymous event.
