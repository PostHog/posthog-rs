---
cargo/posthog-rs: minor
---

A `CodeEmail` takes copies and files: `cc` and `bcc` each add one address, and `attachment` adds an `EmailAttachment` made from the file's bytes. An email carries at most 10 files and 40 MB after Base64 encoding, about 29 MB of files; the client checks both before it sends anything. The client hashes each file, uploads the files PostHog doesn't have yet straight to PostHog's file store, and then sends the email naming each file by its SHA-256. A refused upload fails the send with `attachment_upload_failed` and sends no email. Add `content_type` and `content_id`, and reference a `content_id` as `cid:<id>` in the HTML to show the file inline. PostHog can't fetch a file from a path or URL, and an attachment's `Debug` output never shows its content. A `TemplateEmail` takes none of these, because the template sets its own recipients and content.
