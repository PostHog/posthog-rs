---
cargo/posthog: patch
---

Local flag evaluation now evaluates flag dependencies in the format `/flags/definitions` sends (`type: "flag"`, operator `flag_evaluates_to`). Before, every flag that depended on another flag fell back to the server. The referenced flag is evaluated with the same person and group context, and its value is compared as the flags service compares it. A `flag` filter inside a cohort, and a `$feature/<key>` filter, now match the way the flags service matches them.
