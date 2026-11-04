---
cargo/posthog-rs: minor
---

A `CodeEmail` takes copies and files: `cc` and `bcc` each add one address, and `attachment` adds an `EmailAttachment`. `EmailAttachment::new` takes the file's bytes and encodes them as Base64 once; `EmailAttachment::from_base64` sends content you already encoded as is. Add `content_type` and `content_id`, and reference a `content_id` as `cid:<id>` in the HTML to show the file inline. PostHog can't fetch a file from a path or URL, and an attachment's `Debug` output never shows its content. A `TemplateEmail` takes none of these, because the template sets its own recipients and content. A PostHog version that doesn't take one of these fields yet refuses the send with an error that names the field, and never sends the email without it.
