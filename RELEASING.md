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

## C bindings

`crates/posthog-ffi` isn't published to crates.io. Each release attaches prebuilt archives of it to the GitHub Release, built from the same tag and named with the same version as `posthog`:

- `posthog-ffi-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `posthog-ffi-<version>-aarch64-unknown-linux-gnu.tar.gz`
- `posthog-ffi-<version>-x86_64-apple-darwin.tar.gz`
- `posthog-ffi-<version>-aarch64-apple-darwin.tar.gz`
- `posthog-ffi-<version>-x86_64-pc-windows-msvc.zip`

Each archive contains the shared and static libraries, `posthog.h`, the C and C++ examples, the README, the license, and `lib/native-static-libs.txt`, which lists the system libraries to link with the static library. Each archive has a `.sha256` file next to it.

The bindings are part of the `posthog` release. They share its changeset, version, tag and GitHub Release, so `cargo/posthog` is the only changeset key they need. After "Release and publish" creates the GitHub Release, two more jobs in [`release.yml`](.github/workflows/release.yml) run:

- "Build C bindings" builds the new release tag for each platform with [`ffi-build.yml`](.github/workflows/ffi-build.yml) and runs the C++ smoke test against each built library.
- "Attach C bindings to the GitHub Release" uploads the archives to the release.

They run even if a later step of "Release and publish" fails, as long as the GitHub Release exists. A failure in either job sends the usual release failure alert to Slack. Pull requests run the same build without uploading, as the "C bindings release build" CI job.

The release step also updates `crates/posthog-ffi/Cargo.lock` to the new `posthog` version, because the bindings crate has its own workspace and lockfile.

If a C bindings job fails, the crates are still published. Use **Re-run failed jobs** on that Release run. It rebuilds from the same release tag and replaces any archives already attached. If the build itself needs a fix, release a patch version instead.

A change that affects only the bindings still needs a changeset for `cargo/posthog` to produce a release, because the bindings ship with each `posthog` release.

## Failed or cancelled release

The independent [release failure notification workflow](.github/workflows/release-failure-notification.yml) checks every completed `main` release attempt and posts to the release approvals Slack channel if a job failed, timed out, or was cancelled. It also alerts when the "Release and publish" job never ran in a failed run, which is what happens when GitHub Actions dies mid-run. It only acts on runs that a push or a manual dispatch started in this repository.

Cancelled runs with no jobs are ignored, including superseded queued runs and manual cancellations before any job is created. A rejected approval fails "Release and publish" with no steps and is indistinguishable from a runner startup failure, so it also alerts.

Once "Release and publish" succeeds, alerts describe follow-up problems rather than claiming the release failed. If the "Notify Slack - Released" job was skipped, the alert says so.

Alerts reply to the approval thread. The release workflow saves the thread timestamp in a run artifact for 30 days, and failed-job reruns reuse the latest available thread. If it is missing or invalid, the alert falls back to a new channel message.

If job inspection fails, the workflow posts a fallback alert with the overall run status and a logs link. The workflow must be present on the default branch to run.
