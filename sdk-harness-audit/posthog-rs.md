# PostHog Rust SDK compliance harness audit

## Summary

The repository already contained a Rust SDK compliance adapter under `compliance/adapter` with standard harness endpoints: `/health`, `/init`, `/capture`, `/flush`, `/state`, and `/reset` (plus `/shutdown`). The local Docker Compose harness setup was not runnable because both compose files referenced the non-existent local image `posthog-sdk-test-harness:debug`.

I changed the v0 and v1 local compose files to use the published harness image `ghcr.io/posthog/sdk-test-harness:0.8.0`. After that, both local Docker Compose harness runs passed.

Note: the requested `context.md` and `plan.md` files were not present at the supplied paths, so implementation proceeded from the repository contents and task instructions.

## Changed files

- `compliance/v0/docker-compose.yml`
  - Replaced `posthog-sdk-test-harness:debug` with `ghcr.io/posthog/sdk-test-harness:0.8.0`.
- `compliance/v1/docker-compose.yml`
  - Replaced `posthog-sdk-test-harness:debug` with `ghcr.io/posthog/sdk-test-harness:0.8.0`.
- `sdk-harness-audit/posthog-rs.md`
  - This audit report.

## Tests added or updated

- None. No source or test-code changes were needed; the adapter and SDK passed the harness after fixing the local harness image reference.

## Commands run

| Command | Exit code | Result |
| --- | ---: | --- |
| `cargo test -p sdk-adapter --all-features` | 0 | Passed; adapter crate compiled and ran 0 tests successfully. |
| `docker compose -f compliance/v0/docker-compose.yml up --build --abort-on-container-exit --exit-code-from test-harness` (before fix) | 1 | Failed because Docker could not pull `posthog-sdk-test-harness:debug`. |
| `docker pull ghcr.io/posthog/sdk-test-harness:0.8.0` | 0 | Passed; published harness image available locally. |
| `docker compose -f compliance/v0/docker-compose.yml up --build --abort-on-container-exit --exit-code-from test-harness` | 0 | Passed; v0 capture compliance suite passed. |
| `docker compose -f compliance/v1/docker-compose.yml up --build --abort-on-container-exit --exit-code-from test-harness` | 0 | Passed; v1 capture compliance suite passed. |
| `docker compose -f compliance/v0/docker-compose.yml down --remove-orphans && docker compose -f compliance/v1/docker-compose.yml down --remove-orphans` | 0 | Passed; cleaned compose containers/networks. |
| `git diff --stat` | 0 | Confirmed compose-only implementation diff: 2 files, 2 insertions, 2 deletions. |
| `git diff --cached --name-only` | 0 | No output; no staged files. |
| `git status --short` | 0 | Shows modified compose files and this report once written. |

## Validation output

- `cargo test -p sdk-adapter --all-features`: `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`.
- v0 harness: `Total: 29 | 29 passed | 0 failed` and `All tests passed! ✓`.
- v1 harness: `Total: 94 | 94 passed | 0 failed` and `All tests passed! ✓`.

## Failing tests fixed

- Fixed local harness startup failure caused by the invalid image `posthog-sdk-test-harness:debug`.
- No SDK compliance assertions failed after the image fix.

## Remaining blockers / residual risks

- None for local compliance execution.
- The repository uses `compliance/adapter` and split v0/v1 workflow files rather than a top-level `sdk_compliance_adapter` directory and a single `.github/workflows/sdk-compliance.yml`; no rename or duplicate workflow was introduced because the existing harness/workflows are functional and changing layout was not required for local compliance pass.

## Git state

- No staged files.
- Working tree changes are limited to the two compose files plus this required audit report.
