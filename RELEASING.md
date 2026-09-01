# Releasing

This repository uses [Sampo](https://github.com/bruits/sampo) for versioning and changelogs, then publishes the workspace crates to crates.io in dependency order.

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
4. Approve the release in Slack when prompted — this triggers the version bump, publishes the `posthog` implementation followed by the `posthog-rs` compatibility crate, creates the git tags, and creates the GitHub Release

You can also trigger a release manually via the workflow's `workflow_dispatch` trigger (still requires pending changesets).

If a release fails, use **Re-run failed jobs** on that workflow run. The retry locates the exact release commit created by the original attempt, validates both packages, skips crate versions already on crates.io, and completes any missing publication, tag, or GitHub Release.

Both crates must configure crates.io Trusted Publishing for the `PostHog/posthog-rs` repository, `release.yml` workflow, and `Release` environment. The single short-lived CI token can then publish both packages.
