#!/usr/bin/env python3
"""Filter cargo-public-api output to the externally meaningful API surface.

cargo-public-api already ignores ordinary `pub(crate)` items, but derive_builder
can generate public setters for `pub(crate)` fields on public builder structs.
Those setters expose implementation knobs in the snapshot even though the source
fields are crate-private. This filter removes those generated setters so the
snapshot tracks the API that is intentionally public.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-root", default=".", help="repository root")
    parser.add_argument(
        "--package",
        required=True,
        help="Cargo package name whose public API is being filtered",
    )
    return parser.parse_args()


def crate_name(package: str) -> str:
    return package.replace("-", "_")


def is_builder_derive(attrs: list[str]) -> bool:
    return any(
        re.match(r"#\[derive\([^\]]*\bBuilder\b", attr) is not None for attr in attrs
    )


def has_custom_or_skipped_setter(attrs: list[str]) -> bool:
    attr_text = " ".join(attrs)
    return "setter(custom" in attr_text or "setter(skip" in attr_text


def pub_crate_builder_setters(repo_root: Path) -> set[tuple[str, str]]:
    """Return (BuilderType, setter_name) generated from pub(crate) fields."""
    setters: set[tuple[str, str]] = set()
    src_dir = repo_root / "src"
    if not src_dir.exists():
        return setters

    struct_name: str | None = None
    brace_depth = 0
    attrs: list[str] = []

    for path in sorted(src_dir.rglob("*.rs")):
        struct_name = None
        brace_depth = 0
        attrs = []

        for line in path.read_text(encoding="utf-8").splitlines():
            stripped = line.strip()

            if struct_name is not None:
                if stripped.startswith("#["):
                    attrs.append(stripped)
                else:
                    field_match = re.match(
                        r"pub\s*\(\s*crate\s*\)\s+([A-Za-z_][A-Za-z0-9_]*)\s*:",
                        stripped,
                    )
                    if field_match is not None:
                        if not has_custom_or_skipped_setter(attrs):
                            setters.add((f"{struct_name}Builder", field_match.group(1)))
                        attrs = []
                    elif stripped and not stripped.startswith("//"):
                        attrs = []

                brace_depth += line.count("{") - line.count("}")
                if brace_depth <= 0:
                    struct_name = None
                    attrs = []
                continue

            if stripped.startswith("#["):
                attrs.append(stripped)
                continue

            struct_match = re.match(
                r"pub\s+struct\s+([A-Za-z_][A-Za-z0-9_]*)\b", stripped
            )
            if struct_match is not None:
                if is_builder_derive(attrs):
                    struct_name = struct_match.group(1)
                    brace_depth = line.count("{") - line.count("}")
                    if brace_depth <= 0:
                        struct_name = None
                attrs = []
                continue

            if stripped and not stripped.startswith("//"):
                attrs = []

    return setters


def main() -> int:
    args = parse_args()
    repo_root = Path(args.repo_root)
    crate = re.escape(crate_name(args.package))
    ignored_setters = pub_crate_builder_setters(repo_root)

    restricted_visibility = re.compile(r"^pub\s*\((?:crate|self|super|in\b)")
    builder_setter = re.compile(
        rf"^pub (?:async )?fn {crate}::"
        r"(?P<builder>[A-Za-z_][A-Za-z0-9_]*Builder)::"
        r"(?P<setter>[A-Za-z_][A-Za-z0-9_]*)(?:<|\()"
    )

    for line in sys.stdin:
        if restricted_visibility.match(line):
            continue
        match = builder_setter.match(line)
        if match is not None and (
            match.group("builder"),
            match.group("setter"),
        ) in ignored_setters:
            continue
        sys.stdout.write(line)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
