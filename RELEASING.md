# Releasing

This repository uses [Sampo](https://github.com/bruits/sampo) for versioning, changelogs, and publishing to crates.io.

Starting with 1.0, `posthog` contains the SDK implementation and `posthog-rs` re-exports it. Sampo's fixed package group keeps both versions and the compatibility crate's dependency synchronized. Publishing runs in dependency order. Sampo tags the implementation as `v<version>` and the compatibility crate as `posthog-rs-v<version>`. The GitHub Release continues to use the bare version tag.

Both packages use crates.io Trusted Publishing for the `PostHog/posthog-rs` repository, `release.yml` workflow and `Release` environment.

1. When making changes, include a changeset: `sampo add`
   - Prefer letting `sampo add` create the file for you.
   - If you create or edit a changeset manually, the frontmatter must use this exact package key:

     ```md
     ---
     cargo/posthog: patch
     ---
     ```

   - Replace `patch` with `minor` or `major` when appropriate.

2. Create a PR with your changes and the changeset file
3. Merge to `main` (no release label required)
4. Approve the release in Slack when prompted — this triggers the version bump, publishes `posthog` followed by the `posthog-rs` compatibility crate, creates git tags, and creates the GitHub Release

You can also trigger a release manually via the workflow's `workflow_dispatch` trigger (still requires pending changesets).
