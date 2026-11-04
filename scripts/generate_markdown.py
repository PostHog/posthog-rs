"""Generate the SDK reference Markdown from rustdoc JSON.

Needs the nightly that CI pins for the public API check, which only builds the docs and is never
a dependency of the crate: `rustup toolchain install nightly-2026-06-12 --profile minimal`.
A `posthog-capability: <slug>` doc comment line puts a symbol under that PostHog/sdk-specs
capability, in the sdk-specs README's order. Untagged symbols go to SDK utils.

    python3 scripts/generate_markdown.py --sdk-specs DIR [--major 1]
"""

import argparse
import json
import os
import re
import subprocess
import textwrap
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
# The nightly that CI already pins for the public API check.
TOOLCHAIN = os.environ.get("DOCS_TOOLCHAIN", "nightly-2026-06-12")
# Other public client classes: their untagged methods get an H2 of their own.
CLASSES = ["FeatureFlagEvaluations", "LocalEvaluator", "FlagPoller", "EndpointManager"]
FIRST = ["setup", "capture", "identify"]
TAG = re.compile(r"^[ \t]*posthog-capability:[ \t]*([a-z0-9-]+)[ \t]*$\n?", re.M)
LINK = re.compile(r"\[(`[^`]+`)\](?:\[[^\]]*\]|\((?!https?:)[^)]*\))?(?!\()")
DEFINITION = re.compile(r"^\[[^\]]+\]:\s+\S+$\n?", re.M)
HIDDEN = re.compile(r"^\s*#(?: .*)?$")
SKIP = {"Eq", "PartialEq", "Clone", "Debug", "Default", "Serialize", "Deserialize"}


class Docs:
    def __init__(self, path: Path):
        data = json.loads(path.read_text())
        self.index = data["index"]
        self.root = self.index[str(data["root"])]["inner"]["module"]["items"]
        self.src = {}

    def lines(self, name: str) -> list[str]:
        if name not in self.src:
            self.src[name] = (ROOT / name).read_text().split("\n")
        return self.src[name]

    def text(self, item: dict) -> str:
        """The item's source, from its first line to the end of its span."""
        (b, bc), (e, ec) = item["span"]["begin"], item["span"]["end"]
        rows = self.lines(item["span"]["filename"])[b - 1 : e]
        rows[-1] = rows[-1][:ec]
        rows[0] = rows[0][bc - 1 :]
        return "\n".join(rows)

    def signature(self, item: dict, name: str) -> str:
        """The declaration up to its body, as written in the source."""
        src = re.sub(r"^\s*#\[.*\]\s*$", "", self.text(item), flags=re.M).strip()
        sig = textwrap.dedent(" " * (item["span"]["begin"][1] - 1) + re.split(r"\s*[{;]", src, maxsplit=1)[0])
        sig = sig.strip().rstrip(",")
        return re.sub(r"\bfn \w+", f"fn {name}", sig, count=1)


def clean(docs: str, level: int) -> str:
    """Drops the tag and hidden example lines, links to plain code and nests headings."""
    out, fence = [], False
    for line in LINK.sub(r"\1", DEFINITION.sub("", TAG.sub("", docs or ""))).split("\n"):
        if line.lstrip().startswith("```"):
            info = line.strip()[3:]
            line = "```" if fence else "```text" if info == "text" else "```rust"
            fence = not fence
        elif fence and HIDDEN.match(line):
            continue
        elif not fence and re.match(r"#{1,4} ", line):
            line = "#" * level + line.lstrip("#")
        out.append(line)
    # The site reads Markdown as MDX 1: HTML must be JSX.
    return "\n".join(out).strip().replace("<br>", "<br />")


def entry(docs: Docs, item: dict, label: str, level: int, name: str = "") -> str:
    sig = docs.signature(item, name or item["name"])
    parts = [f"{'#' * level} `{label}`", f"```rust\n{sig}\n```", clean(item["docs"], level + 1)]
    return "\n\n".join(p for p in parts if p)


def members(docs: Docs, item: dict) -> list[dict]:
    """Public inherent methods and trait items, in source order."""
    kind = next(k for k in ("struct", "enum", "trait") if k in item["inner"])
    ids = list(item["inner"][kind].get("items", []))
    for i in item["inner"][kind].get("impls", []):
        impl = docs.index[str(i)]["inner"]["impl"]
        if not (impl["trait"] or impl["is_synthetic"] or impl["blanket_impl"]):
            ids += impl["items"]
    found = [docs.index[str(i)] for i in ids]
    return [m for m in found if m["visibility"] in ("public", "default") and "function" in m["inner"]]


def fields(docs: Docs, item: dict) -> str:
    inner = item["inner"]
    if "enum" in inner:
        found = [docs.index[str(i)] for i in inner["enum"]["variants"]]
        heading = "Variants"
    else:
        plain = inner.get("struct", {}).get("kind", {}).get("plain", {})
        found = [docs.index[str(i)] for i in plain.get("fields", [])]
        found = [f for f in found if f["visibility"] == "public"]
        heading = "Fields"
    rows = []
    for f in found:
        code = re.sub(r"\s+", " ", re.sub(r"#\[.*?\]\s*", "", docs.text(f))).strip(" ,")
        code = re.sub(r"^pub ", "", code)
        doc = clean(f["docs"], 6).replace("\n", "\n  ")
        rows.append(f"- `{code}`" + (f" - {doc}" if doc else ""))
    return f"**{heading}**\n\n" + "\n".join(rows) if rows else ""


def capabilities(checkout: str) -> dict[str, str]:
    """Each sdk-specs capability's slug and title in sentence case, in the README's order."""
    readme = (Path(checkout) / "README.md").read_text()
    rows = re.findall(r"^\| ([^|]+?) \| \[`openspec/specs/([a-z0-9-]+)/spec\.md`\]", readme, re.M)
    acronyms = {"id": "ID", "ai": "AI", "http": "HTTP", "mcp": "MCP"}
    titles = {s: " ".join(acronyms.get(w.lower(), w.lower()) for w in n.split()) for n, s in rows}
    order = [*FIRST, *(s for s in titles if s not in FIRST)]
    return {s: titles[s][0].upper() + titles[s][1:] for s in order if s in titles}


def reference(docs: Docs, caps: dict[str, str]) -> list[str]:
    tagged: dict[str, list[str]] = {slug: [] for slug in caps}
    utils, classes, types = [], {}, []
    for use in (docs.index[str(i)]["inner"].get("use") for i in docs.root):
        if not use:
            continue
        item, name = docs.index[str(use["id"])], use["name"]
        kind = next(iter(item["inner"]))
        found = TAG.search(item["docs"] or "")
        if kind in ("function", "constant"):
            text = entry(docs, item, f"posthog_rs::{name}", 3, name)
            (tagged[found.group(1)] if found else utils).append(text)
            continue
        if kind not in ("struct", "enum", "trait"):
            continue
        rest = []
        for m in members(docs, item):
            slug = TAG.search(m["docs"] or "")
            inside = not slug and name not in CLASSES and name != "Client"
            text = entry(docs, m, f"{name}::{m['name']}", 4 if inside else 3)
            if slug:
                tagged[slug.group(1)].append(text)
            elif name == "Client":
                utils.append(text)
            elif name in CLASSES:
                classes.setdefault(name, []).append(text)
            else:
                rest.append(text)
        if name in CLASSES:
            classes[name] = [entry(docs, item, name, 3), fields(docs, item), *classes.get(name, [])]
        elif name != "Client":
            sections = [entry(docs, item, name, 3), fields(docs, item), *rest]
            types.append("\n\n".join(s for s in sections if s))
    out = []
    for slug, title in caps.items():
        if tagged[slug]:
            out += [f"## {title}", *tagged[slug]]
    out += ["## SDK utils", *utils]
    for name, parts in classes.items():
        out += [f"## {name}", *(p for p in parts if p)]
    return out + ["## Types", *types]


def front_matter(title: str, sections: list[str]) -> str:
    """The docs site's front matter. Its ToC lists the page's H2 sections only."""
    toc = []
    for heading in [s[3:] for s in sections if s.startswith("## ")]:
        anchor = re.sub(r"[^\w\- ]", "", heading.lower()).replace(" ", "-")
        toc.append(f'  - value: "{heading}"\n    url: {anchor}\n    depth: 0')
    head = [f'title: "{title}"', "sidebar: Docs", "showTitle: true"]
    head += ["contentMaxWidthClass: max-w-full", "platformLogo: rust", "tableOfContents:", *toc]
    return "\n".join(["---", *head, "---"])


def nest(markdown: str) -> str:
    """Moves each heading down one level and drops the H1. Code stays as written."""
    out, in_fence = [], False
    for line in markdown.strip().split("\n"):
        in_fence ^= bool(re.match(r"\s*```", line))
        if in_fence or not re.match(r"#{1,5} ", line):
            out.append(line)
        elif not line.startswith("# "):
            out.append("#" + line)
    return "\n".join(out).strip()


def write(name: str, sections: list[str]) -> None:
    (ROOT / "references" / name).write_text("\n\n".join(sections) + "\n")
    print(f"wrote references/{name}")


def main() -> None:
    args = argparse.ArgumentParser(description="Generate the SDK reference Markdown.")
    args.add_argument("--sdk-specs", required=True, help="PostHog/sdk-specs checkout")
    args.add_argument("--major", type=int, help="target major, defaults to the declared one")
    opts = args.parse_args()
    subprocess.run(
        ["cargo", f"+{TOOLCHAIN}", "rustdoc", "--lib", "--", "-Z", "unstable-options", "--output-format", "json"],
        cwd=ROOT,
        check=True,
    )
    docs = Docs(ROOT / "target/doc/posthog_rs.json")
    version = re.search(r'^version = "(\d+)', (ROOT / "Cargo.toml").read_text(), re.M)
    declared = int(version.group(1))
    major = opts.major or declared
    body = ["## Getting started", nest((ROOT / "docs/getting-started.md").read_text())]
    body += reference(docs, capabilities(opts.sdk_specs))
    (ROOT / "references").mkdir(exist_ok=True)
    title = f"PostHog Rust SDK {major}.x" + ("" if declared >= major else " (experimental)")
    write(f"posthog-rust-{major}.md", [front_matter(title, body), *body])
    if declared >= major:
        write("posthog-rust-reference.md", [front_matter("PostHog Rust SDK", body), *body])


if __name__ == "__main__":
    main()
