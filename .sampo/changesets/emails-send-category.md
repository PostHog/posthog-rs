---
cargo/posthog-rs: minor
---

`CodeEmail::category` files a code email under a message category that the send defines in code. PostHog creates the category by its key on the first send, and updates its name and description when they change. A marketing category's send honors the recipient's opt-outs and needs the `{{ unsubscribe_url }}` placeholder in its body. Without a category, or with `EmailCategory::Transactional`, the email stays transactional.

```rust
use posthog_rs::{CodeEmail, MessageCategory, MessageCategoryType};

let email = CodeEmail::new("jane@example.com", "monthly-digest", "Your month in review")
    .html(rendered_html)
    .category(
        MessageCategory::new("newsletter", MessageCategoryType::Marketing, "Newsletter")
            .description("Monthly product news"),
    );
```
