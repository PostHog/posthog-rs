---
cargo/posthog-rs: minor
---

Report `$release_id` on every event when `posthog-cli` has injected one into the binary. The crate compiles a fixed placeholder into the build, and `posthog-cli symbol-sets upload --release-mode=event` overwrites it with the id of the release it created. This is the native equivalent of the `$release_id` a web build injects into its bundle: the release id is the primary key the server resolves an exception's release from, so it no longer depends on the crate version, the app name, or git metadata matching between the build and the upload. `injected_release_id()` returns the id when a build was injected, for programs that want to log which release they run. An un-injected binary sends nothing.
