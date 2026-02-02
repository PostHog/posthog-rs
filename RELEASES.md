# Release Process

This repository uses automated releases via GitHub Actions and [trusted publishing](https://crates.io/docs/trusted-publishing) to crates.io.

## How to Release

1. Create a PR with your changes
2. Add the appropriate labels before merging:
   - `release` - Releases the current version as-is (no version bump)
   - `release` + `bump patch` - Bump patch version and release (e.g., 0.3.7 → 0.3.8)
   - `release` + `bump minor` - Bump minor version and release (e.g., 0.3.7 → 0.4.0)
   - `release` + `bump major` - Bump major version and release (e.g., 0.3.7 → 1.0.0)
3. Merge the PR

## What Happens Automatically

When a PR with the `release` label is merged:

1. **Version bump** (if bump label present) - `Cargo.toml` version is updated
2. **Changelog update** (if bump label present) - `CHANGELOG.md` is updated with commits since the last release
3. **Commit** (if bump label present) - Changes are committed to the base branch
4. **Tag** - A git tag `v{version}` is created and pushed
5. **Publish** - The tag triggers the release workflow which publishes to crates.io

## Version Bumps Without Release

To bump the version without publishing (e.g., for pre-release coordination):

- `bump patch` - Patch version bump only
- `bump minor` - Minor version bump only
- `bump major` - Major version bump only

These labels without `release` will update the version and changelog but won't create a tag or publish.

## Trusted Publishing

This repository uses OIDC-based trusted publishing instead of long-lived API tokens. The `release.yml` workflow authenticates directly with crates.io using GitHub's OIDC provider, eliminating the need to manage crates.io API tokens as repository secrets.
