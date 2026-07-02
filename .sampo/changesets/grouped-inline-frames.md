---
posthog-rs: minor
---

Resolvable error tracking frames with local debug info now send their full client-side inline expansion as a marked group (the physical frame leads; `inline: true` members share its `instruction_addr`; all carry `client_resolved: true`): PostHog symbolicates the group's address once and replaces the whole group with its own expansion, or keeps the client frames verbatim when no debug symbols were uploaded — so inlined calls survive without symbol uploads and don't duplicate with them. Stripped builds still send bare addressed frames. Grouped replacement requires a PostHog version with client-expanded inline group support (PostHog Cloud has it; self-hosted releases picking up cymbal's marker-grouped resolution); older servers keep working, but re-expand group addresses on symbol upload, duplicating inline frames until upgraded.
