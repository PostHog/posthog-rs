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

## Failed or cancelled release

The independent [release failure notification workflow](.github/workflows/release-failure-notification.yml) checks every completed `main` release attempt and posts to the release approvals Slack channel if a job failed, timed out, or was cancelled. It also alerts when the "Release and publish" job never ran in a failed run, which is what happens when GitHub Actions dies mid-run.

Cancelled runs with no jobs are ignored, including superseded queued runs and manual cancellations before any job is created. A rejected approval fails "Release and publish" with no steps and is indistinguishable from a runner startup failure, so it also alerts.

Once "Release and publish" succeeds, alerts describe follow-up problems rather than claiming the release failed. If the "Notify Slack - Released" job was skipped, the alert says so.

Alerts reply to the approval thread. The release workflow saves the thread timestamp in a run artifact for 30 days, and failed-job reruns reuse the latest available thread. If it is missing or invalid, the alert falls back to a new channel message.

If job inspection fails, the workflow posts a fallback alert with the overall run status and a logs link. The workflow must be present on the default branch to run.
