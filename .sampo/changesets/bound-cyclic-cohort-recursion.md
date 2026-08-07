---
cargo/posthog-rs: patch
---

Stop malformed cohort definitions from aborting the process during local evaluation.

Cohort resolution recursed through nested cohort references with nothing bounding the recursion, so a cohort that referenced itself, directly or through another cohort, recursed until the thread stack was exhausted. A Rust stack overflow aborts the process rather than panicking catchably, so a single bad definitions response could take down the calling server, and would again on restart as soon as the poller refetched. A long chain of distinct cohorts reached the same abort without any cycle at all, and because each cohort is a separate shallow entry in the manifest, such a chain also cleared `serde_json`'s own nesting limit on the way in.

Cohort resolution now tracks the cohorts on the active resolution path, which bounds both cases: a repeat is reported as a cycle, and the path length is capped at 100. Either is reported as inconclusive, so the flag falls back to server-side evaluation rather than resolving to a wrong value. Referencing the same cohort twice down sibling branches still resolves normally, since IDs are released as the recursion unwinds.
