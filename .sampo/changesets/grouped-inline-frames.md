---
posthog-rs: minor
---

Error tracking stacktraces are now sent in call order (main first, crash site last), matching the other PostHog SDKs and the orientation the server's inline expansion assumes. Resolvable frames with local debug info also send their full client-side inline expansion as a marked group (shared `instruction_addr`, `inline: true` members, `client_resolved: true`): PostHog symbolicates the group's address once and replaces the whole group with its own expansion, or keeps the client frames verbatim when no debug symbols were uploaded — so inlined calls survive without symbol uploads and don't duplicate with them. Grouped replacement requires a PostHog version with client-expanded inline group support (PostHog Cloud has it; self-hosted releases picking up cymbal's marker-grouped resolution); older servers keep working, but re-expand group addresses on symbol upload, duplicating inline frames until upgraded.
