#!/usr/bin/env bash
set -euo pipefail

candidate=${1:-}
repository=${GITHUB_REPOSITORY:-PostHog/posthog-rs}

if [[ -z "$candidate" ]]; then
    echo "usage: $0 <candidate-ref-or-sha>" >&2
    exit 2
fi

sha=$(gh api "repos/${repository}/commits/${candidate}" --jq .sha)
successful_checks=$(mktemp)
trap 'rm -f "$successful_checks"' EXIT

gh api "repos/${repository}/commits/${sha}/check-runs?per_page=100" \
    --paginate \
    --jq '.check_runs[] | select(.conclusion == "success") | .name' \
    | sort -u > "$successful_checks"

required_checks=(
    "Format"
    "Clippy"
    "Public API"
    "Build (default features)"
    "Build (blocking client)"
    "Cargo publish dry run"
    "Unit test (default features)"
    "Unit test (workspace)"
    "Unit test (blocking client)"
    "Unit test (error-tracking, blocking client)"
    "E2E test"
    "async / gzip"
    "async / deflate"
    "async / br"
    "async / zstd"
    "blocking / gzip"
    "blocking / deflate"
    "blocking / br"
    "blocking / zstd"
)

missing=0
for check in "${required_checks[@]}"; do
    if ! grep -Fxq "$check" "$successful_checks"; then
        echo "missing successful check: $check" >&2
        missing=1
    fi
done

if (( missing )); then
    echo "Candidate ${sha} is not release-ready." >&2
    exit 1
fi

echo "Candidate ${sha} has every required CI and SDK compliance check."
