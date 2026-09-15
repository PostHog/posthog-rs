#!/usr/bin/env bash
set -euo pipefail

cargo metadata --format-version 1 --no-deps | python3 -c '
import json
import sys

packages = {package["name"]: package for package in json.load(sys.stdin)["packages"]}
canonical = packages["posthog"]
compatibility = packages["posthog-rs"]

canonical_version = canonical["version"]
compatibility_version = compatibility["version"]
if canonical_version != compatibility_version:
    raise SystemExit(
        f"posthog@{canonical_version} and posthog-rs@{compatibility_version} must have the same version"
    )

canonical_features = set(canonical["features"])
compatibility_features = set(compatibility["features"])
if canonical_features != compatibility_features:
    missing = sorted(canonical_features - compatibility_features)
    extra = sorted(compatibility_features - canonical_features)
    raise SystemExit(f"compatibility feature mismatch: missing={missing}, extra={extra}")

for feature in sorted(canonical_features):
    expected = [f"posthog/{feature}"]
    actual = compatibility["features"][feature]
    if actual != expected:
        raise SystemExit(
            f"posthog-rs feature {feature!r} must forward to {expected}, found {actual}"
        )

print("posthog-rs version and features match posthog")
'
