---
cargo/posthog-rs: minor
---

A `$set`, `$set_once`, `$groups` or `$group_set` from the request context fills in only the keys the event's own value lacks, as in the other v1 server SDKs. In 0.x, the context value was used only when the event had none of that property.

`$feature_flag_called` events now take a request context value over the client's `$is_server` and `$geoip_disable` defaults, as other events already did. A per-call `disable_geoip` still wins.
