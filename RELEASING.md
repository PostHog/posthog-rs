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
4. Approve the release in Slack when prompted — this triggers the version bump, publishes the same source as both `posthog-rs` and `posthog`, creates the git tag, and creates the GitHub Release

You can also trigger a release manually via the workflow's `workflow_dispatch` trigger (still requires pending changesets).

Both crates must configure crates.io Trusted Publishing for the `posthog/posthog-rs` repository, `release.yml` workflow, and `Release` environment. The single short-lived CI token can then publish both packages.
