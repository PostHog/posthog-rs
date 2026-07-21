#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
    echo "Cannot publish the posthog mirror from a dirty working tree" >&2
    exit 1
fi

files=(Cargo.toml Cargo.lock compliance/adapter/Cargo.toml)
backup_dir="$(mktemp -d)"
for file in "${files[@]}"; do
    mkdir -p "$backup_dir/$(dirname "$file")"
    cp "$file" "$backup_dir/$file"
done

restore() {
    [[ -d "$backup_dir" ]] || return 0
    for file in "${files[@]}"; do
        cp "$backup_dir/$file" "$file"
    done
    git update-index --no-assume-unchanged -- "${files[@]}"
    rm -rf "$backup_dir"
}

on_signal() {
    local status="$1"
    restore
    trap - EXIT INT TERM
    exit "$status"
}

trap restore EXIT
trap 'on_signal 130' INT
trap 'on_signal 143' TERM

# Keep Cargo's generated VCS metadata tied to the clean release commit while
# staging the package-name-only changes in the working tree.
git update-index --assume-unchanged -- "${files[@]}"

python3 - <<'PY'
from pathlib import Path


def replace(path: str, old: str, new: str, expected: int = 1) -> None:
    file = Path(path)
    contents = file.read_text()
    count = contents.count(old)
    if count != expected:
        raise SystemExit(f"expected {expected} occurrence(s) of {old!r} in {path}, found {count}")
    file.write_text(contents.replace(old, new))


replace("Cargo.toml", 'name = "posthog-rs"', 'name = "posthog"')
replace("Cargo.lock", '"posthog-rs"', '"posthog"', expected=2)
replace(
    "compliance/adapter/Cargo.toml",
    'posthog-rs = { path = "../..",',
    'posthog-rs = { package = "posthog", path = "../..",',
)
PY

cargo publish --locked "$@"
