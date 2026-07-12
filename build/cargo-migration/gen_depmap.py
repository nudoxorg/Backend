#!/usr/bin/env python3
"""Generate an authoritative alias -> Cargo dependency map from the Buck2
third-party registry + git sources, so the cargo-migration subagents translate
`crate("alias")` BUCK edges into Cargo.toml `[dependencies]` deterministically.

Reads:  build/third-party/registry.bzl   (REGISTRY list)
        build/third-party/git.bzl        (GIT list + *_REV constants)
Writes: build/cargo-migration/dep-map.json
"""
import json
import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TP = os.path.join(ROOT, "build", "third-party")


def load_bzl(path, want):
    """Exec a data-only .bzl file as Python and return the named global."""
    ns = {"select": lambda d: d.get("DEFAULT", [])}  # tolerate stray select()
    with open(path) as f:
        src = f.read()
    exec(compile(src, path, "exec"), ns)
    return ns[want], ns


registry, _ = load_bzl(os.path.join(TP, "registry.bzl"), "REGISTRY")
git, gns = load_bzl(os.path.join(TP, "git.bzl"), "GIT")


def alias_of(e):
    return e["crate_name"] if e.get("crate_name") else e["name"].replace("-", "_")


# --- crates.io entries -------------------------------------------------------
# Map every alias AND registry label to the real crate + version.
by_alias = {}
for e in registry:
    spec = {
        "source": "crates.io",
        "package": e["name"],          # real crates.io name
        "version": e["version"],
        "label": e.get("label"),
        "features_default_on": e.get("features", []),
        "edition": e.get("edition"),
    }
    # The default alias (underscored name / crate_name) only exists for the
    # crate the workspace picked as canonical for that name (alias=True).
    if e.get("alias"):
        by_alias[alias_of(e)] = spec
    # A version-specific label (futures-0_3, http-1, lexical_core-1) is always
    # addressable, even when the default alias points at another version.
    if e.get("label"):
        by_alias.setdefault(e["label"], spec)


def repo_url(urls):
    # "https://github.com/ORG/REPO/archive/REV.tar.gz" -> "https://github.com/ORG/REPO"
    if not urls:
        return None
    m = re.match(r"(https://github\.com/[^/]+/[^/]+)/archive/", urls[0])
    return m.group(1) if m else None


# --- git entries -------------------------------------------------------------
for repo in git:
    urls = repo.get("urls", [])
    url = repo_url(urls)
    # rev token is whatever REV constant got concatenated into the url tail
    rev = None
    if urls:
        tail = urls[0].rsplit("/", 1)[-1]
        rev = tail.replace(".tar.gz", "")
    for c in repo.get("crates", []):
        name = c["name"]
        alias = name  # git crate names are used verbatim in crate("...")
        by_alias.setdefault(alias, {
            "source": "git",
            "package": name.replace("_", "-"),  # best-effort crates.io-style name
            "git": url,
            "rev": rev,
            "subdir": c.get("subdir", ""),
            "edition": c.get("edition"),
            "note": "monorepo git dep; cargo resolves by `package` within the workspace",
        })

out = {
    "_note": "alias/label -> Cargo dependency spec. crate(\"X\") in a BUCK file maps to by_alias[X].",
    "by_alias": dict(sorted(by_alias.items())),
}
dest = os.path.join(ROOT, "build", "cargo-migration", "dep-map.json")
with open(dest, "w") as f:
    json.dump(out, f, indent=2)

n_crates = sum(1 for v in by_alias.values() if v["source"] == "crates.io")
n_git = sum(1 for v in by_alias.values() if v["source"] == "git")
print(f"wrote {dest}")
print(f"  {len(by_alias)} alias/label keys  ({n_crates} crates.io, {n_git} git)")
