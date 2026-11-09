---
cargo/posthog-rs: minor
---

`CodeEmail::category` sends a code email as marketing email in one of your project's marketing message categories, by its key. A marketing send honors the recipient's opt-outs and needs the `{{ unsubscribe_url }}` placeholder in its body. Without a category, or with `TRANSACTIONAL_CATEGORY`, the email stays transactional.

```rust
use posthog_rs::CodeEmail;

let email = CodeEmail::new("jane@example.com", "monthly-digest", "Your month in review")
    .html(rendered_html)
    .category("product-updates");
```
