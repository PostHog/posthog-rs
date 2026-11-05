"""Generate the SDK reference Markdown from rustdoc JSON.

rustdoc lists every public item with its doc comment, which is already Markdown. This script
only groups items by the `posthog-capability: <slug>` line in their docs, in the PostHog/sdk-specs
README's order, and writes front matter. Untagged items go to SDK utils.

Needs the nightly that CI pins for the public API check. It only builds the docs and is never a
dependency of the crate: `rustup toolchain install nightly-2026-06-12 --profile minimal`.

    python3 scripts/generate_markdown.py --sdk-specs DIR [--major 1]
"""

import argparse
import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TOOLCHAIN = os.environ.get("DOCS_TOOLCHAIN", "nightly-2026-06-12")
# Other public client classes: their untagged methods get an H2 of their own.
CLASSES = ["FeatureFlagEvaluations", "LocalEvaluator", "FlagPoller", "EndpointManager"]
FIRST = ["setup", "capture", "identify"]
TAG = "posthog-capability:"
ITEMS: dict = {}  # rustdoc items by id, for variants whose fields are separate items


class Docs:
    def __init__(self, path: Path):
        data = json.loads(path.read_text())
        self.index = data["index"]
        ITEMS.update(self.index)
        self.root = self.index[str(data["root"])]["inner"]["module"]["items"]

    def markdown(self, item: dict, level: int) -> str:
        """The item's docs without the tag line, with rustdoc-only syntax made plain Markdown."""
        out, fence = [], False
        for line in (item["docs"] or "").split("\n"):
            bare = line.strip()
            if bare.startswith("```"):
                # rustdoc treats an untyped or attribute-only fence (no_run, ignore) as Rust.
                line = "```" if fence else "```text" if bare == "```text" else "```rust"
                fence = not fence
            elif fence and (bare == "#" or bare.startswith("# ")):
                continue  # rustdoc hides these example lines
            elif not fence and (bare.startswith(TAG) or (bare.startswith("[") and "]: " in bare)):
                continue  # the tag, and intra-doc link targets that only rustdoc resolves
            elif not fence and line.startswith("#"):
                line = "#" * level + line.lstrip("#")
            out.append(line)
        # Intra-doc links point at the item's heading on this page.
        links = [f"[{text}]: #{anchor(text)}" for text in item.get("links", {})]
        return "\n".join(out + [""] + links).strip().replace("<br>", "<br />")


def signature(item: dict, name: str = "") -> str:
    """The item's declaration, printed from rustdoc's typed JSON, under its exported name."""
    (kind, inner), name = next(iter(item["inner"].items())), name or item["name"]
    if kind == "function":
        h, sig = inner["header"], inner["sig"]
        mods = "".join(m + " " for m in ("const", "async", "unsafe") if h[f"is_{m}"])
        args = [receiver(t) if n == "self" else f"{n}: {ty(t)}" for n, t in sig["inputs"]]
        out = f" -> {ty(sig['output'])}" if sig["output"] else ""
        return f"pub {mods}fn {name}{generics(inner['generics'])}({', '.join(args)}){out}{where(inner['generics'])}"
    if kind == "struct_field":
        return f"pub {name}: {ty(inner)}"
    if kind == "variant":
        shape = inner["kind"]
        if isinstance(shape, dict) and "tuple" in shape:
            return name + "(" + ", ".join(ty(ITEMS[str(i)]["inner"]["struct_field"]) for i in shape["tuple"] if i is not None) + ")"
        return name + (" { .. }" if isinstance(shape, dict) else "")
    if kind == "constant":
        return f"pub const {name}: {ty(inner['type'])} = {inner['const']['expr']}"
    if kind in ("struct", "enum", "trait"):
        return f"pub {kind} {name}{generics(inner['generics'])}"
    return f"pub {name}"


def receiver(t: dict) -> str:
    ref = t.get("borrowed_ref")
    if ref and ref["type"] == {"generic": "Self"}:
        return "&mut self" if ref["is_mutable"] else "&self"
    return "self" if t == {"generic": "Self"} else f"self: {ty(t)}"


def ty(t) -> str:
    if t is None:
        return "()"
    (kind, v), = t.items()
    if kind == "resolved_path":
        return path(v)
    if kind in ("generic", "primitive"):
        return v
    if kind == "borrowed_ref":
        life = v["lifetime"] + " " if v["lifetime"] else ""
        return f"&{life}{'mut ' if v['is_mutable'] else ''}{ty(v['type'])}"
    if kind == "raw_pointer":
        return f"*{'mut' if v['is_mutable'] else 'const'} {ty(v['type'])}"
    if kind == "tuple":
        return "(" + ", ".join(map(ty, v)) + ")"
    if kind == "slice":
        return f"[{ty(v)}]"
    if kind == "array":
        return f"[{ty(v['type'])}; {v['len']}]"
    if kind == "impl_trait":
        return "impl " + bounds(v)
    if kind == "dyn_trait":
        return "dyn " + " + ".join(path(p["trait"]) for p in v["traits"])
    if kind == "qualified_path":
        trait = f" as {path(v['trait'])}" if v.get("trait") else ""
        return f"<{ty(v['self_type'])}{trait}>::{v['name']}"
    return "_"


def path(p: dict) -> str:
    args = p.get("args") or {}
    if "angle_bracketed" in args:
        items = [ty(a["type"]) if "type" in a else str(next(iter(a.values()))) for a in args["angle_bracketed"]["args"]]
        return p["path"].removeprefix("crate::") + (f"<{', '.join(items)}>" if items else "")
    if "parenthesized" in args:
        fn = args["parenthesized"]
        out = f" -> {ty(fn['output'])}" if fn["output"] else ""
        return p["path"] + "(" + ", ".join(map(ty, fn["inputs"])) + ")" + out
    return p["path"].removeprefix("crate::")


def bounds(found: list) -> str:
    parts = []
    for b in found:
        if "trait_bound" in b:
            t = b["trait_bound"]
            parts.append(("?" if t["modifier"] == "maybe" else "") + path(t["trait"]))
        else:
            parts.append(str(b.get("outlives", "")))
    return " + ".join(parts)


def generics(g: dict) -> str:
    params = []
    for p in g["params"]:
        kind = p["kind"].get("type")
        if kind and kind["is_synthetic"]:
            continue  # `impl Trait` arguments
        params.append(p["name"] + (f": {bounds(kind['bounds'])}" if kind and kind["bounds"] else ""))
    return f"<{', '.join(params)}>" if params else ""


def where(g: dict) -> str:
    preds = [w["bound_predicate"] for w in g["where_predicates"] if "bound_predicate" in w]
    lines = [f"    {ty(p['type'])}: {bounds(p['bounds'])}," for p in preds]
    return "\nwhere\n" + "\n".join(lines) if lines else ""


def tag(item: dict) -> str | None:
    for line in (item["docs"] or "").split("\n"):
        if line.strip().startswith(TAG):
            return line.strip()[len(TAG) :].strip()
    return None


def entry(docs: Docs, item: dict, label: str, level: int, name: str = "") -> str:
    parts = [f"{'#' * level} `{label}`", f"```rust\n{signature(item, name)}\n```"]
    return "\n\n".join(p for p in [*parts, fields(docs, item), docs.markdown(item, level + 1)] if p)


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
    """A struct's public fields or an enum's variants, each with its first doc paragraph."""
    inner = item["inner"]
    if "enum" in inner:
        found, heading = [docs.index[str(i)] for i in inner["enum"]["variants"]], "Variants"
    elif "struct" in inner:
        plain = inner["struct"]["kind"].get("plain", {}) if isinstance(inner["struct"]["kind"], dict) else {}
        found = [docs.index[str(i)] for i in plain.get("fields", [])]
        found, heading = [f for f in found if f["visibility"] == "public"], "Fields"
    else:
        return ""
    rows = []
    for f in found:
        code = signature(f).removeprefix("pub ")
        doc = docs.markdown(f, 6).split("\n\n")[0].replace("\n", " ")
        rows.append(f"- `{code}`" + (f" - {doc}" if doc else ""))
    return f"**{heading}**\n\n" + "\n".join(rows) if rows else ""


def capabilities(checkout: str) -> dict[str, str]:
    """Each sdk-specs capability's slug and title in sentence case, in the README's order."""
    acronyms = {"id": "ID", "ai": "AI", "http": "HTTP", "mcp": "MCP"}
    titles = {}
    for line in (Path(checkout) / "README.md").read_text().split("\n"):
        cells = line.split("|")
        if len(cells) > 2 and "openspec/specs/" in cells[2]:
            slug = cells[2].split("openspec/specs/")[1].split("/")[0]
            titles[slug] = " ".join(acronyms.get(w.lower(), w.lower()) for w in cells[1].split())
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
        if kind in ("function", "constant"):
            text = entry(docs, item, f"posthog_rs::{name}", 3, name)
            (tagged[tag(item)] if tag(item) else utils).append(text)
            continue
        if kind not in ("struct", "enum", "trait"):
            continue
        rest = []
        for m in members(docs, item):
            inside = not tag(m) and name not in CLASSES and name != "Client"
            text = entry(docs, m, f"{name}::{m['name']}", 4 if inside else 3)
            if tag(m):
                tagged[tag(m)].append(text)
            elif name == "Client":
                utils.append(text)
            elif name in CLASSES:
                classes.setdefault(name, []).append(text)
            else:
                rest.append(text)
        if name in CLASSES:
            classes[name] = [entry(docs, item, name, 3), *classes.get(name, [])]
        elif name != "Client":
            types.append("\n\n".join([entry(docs, item, name, 3), *rest]))
    out = []
    for slug, title in caps.items():
        if tagged[slug]:
            out += [f"## {title}", *tagged[slug]]
    out += ["## SDK utils", *utils]
    for name, parts in classes.items():
        out += [f"## {name}", *parts]
    return out + ["## Types", *types]


def anchor(heading: str) -> str:
    """The github-slugger slug the docs site gives a heading."""
    kept = (c for c in heading.lower() if c.isalnum() or c in "-_ ")
    return "".join(kept).replace(" ", "-")


def front_matter(title: str, sections: list[str]) -> str:
    """The docs site's front matter. Its ToC lists the page's H2 sections only."""
    toc = []
    for heading in [s[3:] for s in sections if s.startswith("## ")]:
        toc.append(f'  - value: "{heading}"\n    url: {anchor(heading)}\n    depth: 0')
    head = [f'title: "{title}"', "sidebar: Docs", "showTitle: true"]
    head += ["contentMaxWidthClass: max-w-full", "platformLogo: rust", "tableOfContents:", *toc]
    return "\n".join(["---", *head, "---"])


def nest(markdown: str) -> str:
    """Moves each heading down one level and drops the H1. Code stays as written."""
    out, fence = [], False
    for line in markdown.strip().split("\n"):
        fence ^= line.strip().startswith("```")
        if fence or not line.startswith("#"):
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
    version = next(l for l in (ROOT / "Cargo.toml").read_text().split("\n") if l.startswith("version = "))
    declared = int(version.split('"')[1].split(".")[0])
    major = opts.major or declared
    body = ["## Getting started", nest((ROOT / "docs/getting-started.md").read_text())]
    body += reference(docs, capabilities(opts.sdk_specs))
    (ROOT / "references").mkdir(exist_ok=True)
    title = f"PostHog Rust SDK {major}.x" + ("" if declared >= major else " (experimental)")
    write(f"posthog-rust-{major}.md", [front_matter(title, body), *body])
    if declared >= major:
        write("posthog-rust-reference.md", [front_matter("PostHog Rust SDK", body), *body])
    if guides := sorted((ROOT / "docs").glob("migration-*.md"), reverse=True):
        head = "---\ntitle: PostHog Rust SDK migration guides\nsidebar: Docs\nshowTitle: true\n---"
        write("posthog-rust-migration.md", [head, *(g.read_text().strip() for g in guides)])


if __name__ == "__main__":
    main()
