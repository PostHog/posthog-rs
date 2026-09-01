# Releasing

This repository uses [Sampo](https://github.com/bruits/sampo) for versioning, changelogs, and publishing to crates.io.

1. When making changes, include a changeset: `sampo add`
   - Prefer letting `sampo add` create the file for you.
   - If you create or edit a changeset manually, the frontmatter must use this exact package key:

     ```md
     ---
     cargo/posthog-rs: patch
     ---
     ```

   - Replace `patch` with `minor` or `major` when appropriate.

2. Create a PR with your changes and the changeset file
3. Merge to `main` (no release label required)
4. Approve the release in Slack when prompted — this triggers version bump, crates.io publish, git tag, and GitHub Release

You can also trigger a release manually via the workflow's `workflow_dispatch` trigger (still requires pending changesets).

## Validating the v1 release candidate

Do not publish `1.0.0` directly from the long-lived `v1` branch.

1. Merge every intended v1 change into `v1`, including the package rename in #183 if it is part of the release.
2. Open or update the release PR from `v1` to `main` and wait for its normal CI and SDK compliance checks. The candidate commit must have successful async and blocking compliance jobs for gzip, deflate, Brotli, and Zstandard.
3. Run the **V1 Release Candidate Validation** workflow with the immutable candidate commit SHA. Do not use a moving branch name in the final record. The workflow:
   - reruns formatting, Clippy, public API, examples, async, blocking, and blocking error-tracking checks;
   - runs `cargo publish --workspace --dry-run --locked`, which validates every publishable workspace crate after #183 lands;
   - verifies the candidate SHA already has every required CI and SDK compliance check;
   - uploads a record containing the SHA, validation time, and minimum soak period.
4. Publish a prerelease version such as `1.0.0-rc.1` only after the validation workflow succeeds. The candidate version must already be committed in every publishable manifest and `Cargo.lock`. Follow the package order and retry handling from #183; do not use the current single-crate release workflow to publish two crates.
5. Record the prerelease URL, validation workflow URL, candidate SHA, publication time, and chosen soak duration on issue #207.
6. Exercise capture, immediate capture, error tracking, feature flag events, historical migration, `before_send`, and `on_error` in production during the soak period. Record any incidents and the final decision on issue #207.
7. After the full soak period, rerun validation against the exact commit that will become `1.0.0`. Only then approve the stable release.

The workflow validates readiness but does not claim that a candidate was published or that the production soak elapsed. Keep those issue tasks unchecked until their links and dates are recorded.

The same preflight can be run locally:

```sh
scripts/validate-release-candidate.sh
GH_TOKEN=... scripts/verify-release-candidate-checks.sh <candidate-sha>
```
