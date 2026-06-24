---
posthog-rs: patch
---

Harden the transport's in-flight event counter against underflow. The counter is decremented from several paths (before_send drops, partial v1 batch results, terminal outcomes, shutdown drops, channel drain); a decrement bug on any of them would previously underflow the `AtomicUsize` and wrap to a huge value, making the bounded queue look permanently full and silently dropping every subsequent event. Decrements now saturate at 0 (with a `debug_assert` to surface the bug in tests), so a release build degrades gracefully instead of wedging the queue.
