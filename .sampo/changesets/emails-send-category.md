---
cargo/posthog-rs: minor
---

`CodeEmail::category` files a code email under a message category that the send defines in code. PostHog creates the category with its name and description on the first send with its key. Later sends keep its name and description, so change them in PostHog. A marketing category's send honors the recipient's opt-outs and needs the `{{ unsubscribe_url }}` placeholder in its body. Without a category, the email follows the transactional category of the template that `template_key` names, or stays transactional with no category. `EmailCategory::Transactional` asks for transactional email with no category, so PostHog refuses it on a template in a category. PostHog refuses a send with the `EmailError` code `category_mismatch`, `too_many_categories`, `unsubscribe_link_required`, `copies_not_allowed` or `recipient_opted_out`. A template made in PostHog sends transactional email only, so PostHog refuses a `TemplateEmail` of a template in a marketing category, or a `CodeEmail` whose `template_key` names one, with `marketing_template`.

```rust
use posthog_rs::{CodeEmail, MessageCategory, MessageCategoryType};

let email = CodeEmail::new("jane@example.com", "monthly-digest", "Your month in review")
    .html(rendered_html)
    .category(
        MessageCategory::new("newsletter", MessageCategoryType::Marketing, "Newsletter")
            .description("Monthly product news"),
    );
```
