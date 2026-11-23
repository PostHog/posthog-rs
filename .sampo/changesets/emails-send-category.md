---
cargo/posthog: minor
---

`CodeEmail::category` files a code email under a message category that the send defines in code. When code-send categories are enabled for the project and `template_key` doesn't match an existing saved template, PostHog creates the category with its name and description on the first send with its key; later sends keep its name and description, so change them in PostHog. When code-send categories aren't enabled for the project yet, an omitted `category` sends as transactional with no category, an explicit `MessageCategory` returns HTTP 400 `categories_not_enabled`, and an older server rejects `category` with HTTP 400. A marketing category's send honors the recipient's opt-outs, requires a clickable `<a href="{{ unsubscribe_url }}">` link when `html` is non-empty (or `{{ unsubscribe_url }}` in `text` when `html` is unset or empty), cannot include `cc` or `bcc` (`copies_not_allowed`), and cannot carry attachments (`workflow_can_wait` on a verified sender, or `sandbox_fields_unsupported` on the PostHog test sender, because marketing sending limits can hold delivery; both the async and blocking clients upload attachments before PostHog validates and refuses the send). Without a category, the email follows the transactional category of the template that `template_key` names (when code-send categories are enabled), or stays transactional with no category. `EmailCategory::Transactional` asks for transactional email with no category, so PostHog refuses it with `category_mismatch` on a template in a category; passing an explicit `MessageCategory` when `template_key` names an existing saved template with no category also returns `category_mismatch` (for marketing emails, use a different `template_key`; omit `category` or pass `EmailCategory::Transactional` only for transactional emails). PostHog refuses a send with the `EmailError` code `categories_not_enabled`, `category_mismatch`, `too_many_categories`, `unsubscribe_link_required`, `copies_not_allowed`, `workflow_can_wait`, `sandbox_fields_unsupported` or `recipient_opted_out`. A template made in PostHog sends transactional email only: PostHog checks whether a saved template is enabled for transactional sends first (`template_not_transactional`), and refuses an otherwise sendable template in a marketing category with `marketing_template`.

```rust
use posthog::{CodeEmail, MessageCategory, MessageCategoryType};

let email = CodeEmail::new("jane@example.com", "monthly-digest", "Your month in review")
    .html(r#"<p>What's new this month.</p><p><a href="{{ unsubscribe_url }}">Unsubscribe</a></p>"#)
    .category(
        MessageCategory::new("newsletter", MessageCategoryType::Marketing, "Newsletter")
            .description("Monthly product news"),
    );
```
