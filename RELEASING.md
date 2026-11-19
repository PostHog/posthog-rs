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

`crates/posthog-rs-ffi` is released on its own, with its own version, changesets and changelog (`crates/posthog-rs-ffi/CHANGELOG.md`). It isn't published to crates.io. Use the `cargo/posthog-rs-ffi` changeset key for changes to the bindings:

```md
---
cargo/posthog-rs-ffi: minor
---
```

The bindings report their own version as `$lib_version`, next to `$lib` set to `posthog-rs-ffi`.

Because `posthog-rs-ffi` depends on `posthog`, Sampo also gives `posthog-rs-ffi` a patch release whenever `posthog` is released. Those releases ship the bindings with the new SDK.

Sampo tags each `posthog-rs-ffi` release as `posthog-rs-ffi-v<version>`. The release workflow then creates a GitHub Release for that tag, separate from the `posthog` release and not marked as latest. It attaches these archives:

- `posthog-rs-ffi-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `posthog-rs-ffi-<version>-aarch64-unknown-linux-gnu.tar.gz`
- `posthog-rs-ffi-<version>-x86_64-apple-darwin.tar.gz`
- `posthog-rs-ffi-<version>-aarch64-apple-darwin.tar.gz`
- `posthog-rs-ffi-<version>-x86_64-pc-windows-msvc.zip`

Each archive contains the shared and static libraries, `posthog.h`, the C and C++ examples, the README, the license, and `lib/native-static-libs.txt`, which lists the system libraries to link with the static library. Each archive has a `.sha256` file next to it.

All of this runs in [`release.yml`](.github/workflows/release.yml), in the same approved release as `posthog`:

- "Release and publish" bumps whichever packages have changesets, publishes `posthog` and `posthog-rs` only when they changed, and creates the `posthog-rs-ffi` GitHub Release when its version changed.
- "Build C bindings" builds the `posthog-rs-ffi-v<version>` tag for each platform with [`ffi-build.yml`](.github/workflows/ffi-build.yml) and runs the C++ smoke test against each built library.
- "Attach C bindings to the GitHub Release" checks that all five archives and their checksums are present, then uploads them.

The two C bindings jobs run even if publishing `posthog` fails, as long as the `posthog-rs-ffi` GitHub Release exists. A failure in either sends the usual release failure alert to Slack. Pull requests run the same build without uploading, as the "C bindings release build" CI job.

If a C bindings job fails, use **Re-run failed jobs** on that Release run. It rebuilds from the same tag and replaces any archives already attached. If the build itself needs a fix, release a `posthog-rs-ffi` patch version instead.

## Failed or cancelled release

The independent [release failure notification workflow](.github/workflows/release-failure-notification.yml) checks every completed `main` release attempt and posts to the release approvals Slack channel if a job failed, timed out, or was cancelled. It also alerts when the "Release and publish" job never ran in a failed run, which is what happens when GitHub Actions dies mid-run. It only acts on runs that a push or a manual dispatch started in this repository.

Cancelled runs with no jobs are ignored, including superseded queued runs and manual cancellations before any job is created. A rejected approval fails "Release and publish" with no steps and is indistinguishable from a runner startup failure, so it also alerts.

Once "Release and publish" succeeds, alerts describe follow-up problems rather than claiming the release failed. If the "Notify Slack - Released" job was skipped, the alert says so.

Alerts reply to the approval thread. The release workflow saves the thread timestamp in a run artifact for 30 days, and failed-job reruns reuse the latest available thread. If it is missing or invalid, the alert falls back to a new channel message.

If job inspection fails, the workflow posts a fallback alert with the overall run status and a logs link. The workflow must be present on the default branch to run.
