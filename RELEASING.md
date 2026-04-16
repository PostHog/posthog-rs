# Releasing

This repository uses [Sampo](https://github.com/bruits/sampo) for versioning, changelogs, and publishing to crates.io.

1. When making changes, include a changeset: `sampo add`
2. Create a PR with your changes and the changeset file
3. Add the `release` label and merge to `main`
4. Approve the release in Slack when prompted — this triggers version bump, crates.io publish, git tag, and GitHub Release

You can also trigger a release manually via the workflow's `workflow_dispatch` trigger (still requires pending changesets).
