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

Each `posthog-rs-ffi` release is tagged `posthog-rs-ffi-v<version>` and gets its own GitHub Release, separate from the `posthog` release and not marked as latest. The GitHub Release is published only once all of these archives are built, with the archives attached:

- `posthog-rs-ffi-<version>-x86_64-unknown-linux-gnu.tar.gz`
- `posthog-rs-ffi-<version>-aarch64-unknown-linux-gnu.tar.gz`
- `posthog-rs-ffi-<version>-x86_64-apple-darwin.tar.gz`
- `posthog-rs-ffi-<version>-aarch64-apple-darwin.tar.gz`
- `posthog-rs-ffi-<version>-x86_64-pc-windows-msvc.zip`

Each archive contains the shared and static libraries, `posthog.h`, the C and C++ examples, the README, the license, `lib/native-static-libs.txt` (the system libraries to link with the static library), a CMake package in `lib/cmake/posthog-rs-ffi/`, and pkg-config files in `lib/pkgconfig/` on Linux and macOS. [`crates/posthog-rs-ffi/packaging/package.sh`](crates/posthog-rs-ffi/packaging/package.sh) builds it. Each archive has a `.sha256` file next to it.

All of this runs in [`release.yml`](.github/workflows/release.yml), in the same approved release as `posthog`:

- "Release and publish" bumps whichever packages have changesets and commits the release. It publishes `posthog` and `posthog-rs` only when their version changed, and pushes the `posthog-rs-ffi-v<version>` tag only when the bindings' version changed. Every tag points at the release commit itself, even if `main` has moved on.
- "Build C bindings" builds that tag for each platform with [`ffi-build.yml`](.github/workflows/ffi-build.yml). It compiles both examples, runs the C++ smoke test against the shared and static libraries, packages the archive, then unpacks it elsewhere and builds the smoke test through CMake and pkg-config, as a customer would.
- "Publish the C bindings GitHub Release" checks that all five archives and their checksums are present, then publishes the GitHub Release with them attached.

The two C bindings jobs run even if publishing `posthog` fails, as long as the `posthog-rs-ffi` tag exists. A failure in either sends the usual release failure alert to Slack. Pull requests run the same build without uploading, as the "C bindings release build" CI job.

If anything fails after the release commit lands, use **Re-run failed jobs** on that Release run. Every Release run first checks whether the bindings' version on `main` has a published GitHub Release. If it doesn't, the run finishes that release and nothing else, even if new changesets have landed since:

- It finds the commit that set the version and tags it, or checks that an existing tag points to it. It fails if the tag points anywhere else.
- It builds that tag. Each build, and the publish job, check that the tag still points to the release commit.
- It publishes the GitHub Release with the archives, finishing a draft left by an interrupted run.

Changesets that landed in the meantime stay on `main` and go out in the next Release run, once the unfinished bindings release is published. A new changeset push starts that run.

Once the GitHub Release is published, a rerun has nothing to do. Its downloads never change.

### When an unpublished bindings release can't build

Until a bindings version is published, every Release run tries to finish it, and releases nothing else. If its tagged code can't build, give up on that version and release the fix as a new one:

1. Merge the fix with a `cargo/posthog-rs-ffi` changeset, usually a patch.
2. In the Actions tab, run the **Release** workflow on `main` and set **abandon_ffi_version** to the stuck version, for example `0.1.0`.

That run skips the stuck version and releases the new one, along with any other pending changesets. The abandoned version keeps its tag, but never gets a published GitHub Release, even if someone reruns the old run's jobs: the publish job checks that the version is still the one on `main`. The run fails without releasing anything if `main` has no unpublished bindings version, or if no changeset releases a new bindings version.

If that run commits the new version but fails before publishing it, use **Re-run failed jobs** as usual. The rerun still carries the old **abandon_ffi_version**, sees that `main` has moved on to the new version, and finishes that one instead. The same happens if you enter a version that isn't the one on `main`: the run finishes the unpublished version rather than skipping anything.

### Known gap: a failed crates.io publish isn't retried

If "Release and publish" commits a new `posthog` version but publishing it to crates.io fails, **Re-run failed jobs** doesn't publish it. The rerun finds no changesets left, or only finishes an unfinished bindings release, and skips the crates.io steps, so `posthog` and `posthog-rs` stay unpublished at the committed version. Publish them by hand from the release commit, then create their tags and GitHub Release:

```bash
git checkout <release commit>
cargo publish --locked --package posthog
cargo publish --locked --package posthog-rs
```

When a run finishes or abandons an unfinished bindings release, it checks crates.io for the committed `posthog` and `posthog-rs` versions, and checks that their GitHub Release is published. If anything is missing, the run still does the bindings work, then fails with this instruction instead of reporting success.

The release workflow should publish the crates itself in this case, as it does for the bindings. That's left for a separate change.

## Failed or cancelled release

The independent [release failure notification workflow](.github/workflows/release-failure-notification.yml) checks every completed `main` release attempt and posts to the release approvals Slack channel if a job failed, timed out, or was cancelled. It also alerts when the "Release and publish" job never ran in a failed run, which is what happens when GitHub Actions dies mid-run. It only acts on runs that a push or a manual dispatch started in this repository.

Cancelled runs with no jobs are ignored, including superseded queued runs and manual cancellations before any job is created. A rejected approval fails "Release and publish" with no steps and is indistinguishable from a runner startup failure, so it also alerts.

Once "Release and publish" succeeds, alerts describe follow-up problems rather than claiming the release failed. If the "Notify Slack - Released" job was skipped, the alert says so.

Alerts reply to the approval thread. The release workflow saves the thread timestamp in a run artifact for 30 days, and failed-job reruns reuse the latest available thread. If it is missing or invalid, the alert falls back to a new channel message.

If job inspection fails, the workflow posts a fallback alert with the overall run status and a logs link. The workflow must be present on the default branch to run.
