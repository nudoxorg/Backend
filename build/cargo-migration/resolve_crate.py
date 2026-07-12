#!/usr/bin/env python3
"""For each BUCK file being migrated to Cargo, resolve its `crate(...)`/`member(...)`
edges against dep-map.json and emit a per-crate JSON the subagent can turn
straight into a Cargo.toml. Flags any alias missing from the map.
"""
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
MAP = json.load(open(os.path.join(ROOT, "build", "cargo-migration", "dep-map.json")))["by_alias"]

# crate("name", features = ["a","b"], pin_only = True)
CRATE_RE = re.compile(r'crate\(\s*"([^"]+)"((?:[^()]|\([^()]*\))*)\)')
FEAT_RE = re.compile(r'features\s*=\s*\[([^\]]*)\]')
MEMBER_RE = re.compile(r'member\(\s*"([^"]+)"\s*\)')


def parse_deps_block(text):
    """Return (crate_edges, member_edges). crate_edges: list of (alias, [features], pin_only)."""
    crates, members = [], []
    for m in CRATE_RE.finditer(text):
        alias, rest = m.group(1), m.group(2)
        feats = []
        fm = FEAT_RE.search(rest)
        if fm:
            feats = [s.strip().strip('"') for s in fm.group(1).split(",") if s.strip()]
        pin_only = "pin_only" in rest and "True" in rest
        crates.append((alias, feats, pin_only))
    for m in MEMBER_RE.finditer(text):
        members.append(m.group(1))
    return crates, members


def resolve(buck_path):
    text = open(os.path.join(ROOT, buck_path)).read()
    crates, members = parse_deps_block(text)
    deps, missing = {}, []
    for alias, feats, pin_only in crates:
        spec = MAP.get(alias)
        if spec is None:
            missing.append(alias)
            continue
        entry = {"source": spec["source"], "package": spec["package"], "pin_only": pin_only}
        if spec["source"] == "crates.io":
            entry["version"] = spec["version"]
        else:
            entry.update({"git": spec.get("git"), "rev": spec.get("rev")})
        if feats:
            entry["features"] = sorted(set(feats))
        # keep the strongest feature set / union across duplicate edges
        if alias in deps:
            old = deps[alias].get("features", [])
            entry["features"] = sorted(set(old) | set(entry.get("features", [])))
            entry["pin_only"] = entry["pin_only"] and deps[alias]["pin_only"]
        deps[alias] = entry
    return {
        "buck": buck_path,
        "workspace_members": sorted(set(members)),
        "third_party": dict(sorted(deps.items())),
        "unresolved_aliases": sorted(set(missing)),
    }


TARGETS = {
    "heart": "workspace/heart/BUCK",
    "ir": "workspace/compiler/intermediate-representation/BUCK",
    "caching": "workspace/util/caching/BUCK",
    "cas": "workspace/cas/BUCK",
    "version": "workspace/version/BUCK",
    "sandbox": "workspace/util/sandbox/BUCK",
    "telemetry": "workspace/telemetry/BUCK",
    "runtime": "workspace/runtime/BUCK",
    "registry": "workspace/registry/BUCK",
    "server": "workspace/server/BUCK",
    "compiler": "workspace/compiler/BUCK",
}

outdir = os.path.join(ROOT, "build", "cargo-migration", "resolved")
os.makedirs(outdir, exist_ok=True)
summary = {}
for name, buck in TARGETS.items():
    r = resolve(buck)
    json.dump(r, open(os.path.join(outdir, f"{name}.json"), "w"), indent=2)
    summary[name] = {
        "members": r["workspace_members"],
        "n_tp": len(r["third_party"]),
        "git_deps": sorted(a for a, e in r["third_party"].items() if e["source"] == "git"),
        "unresolved": r["unresolved_aliases"],
    }

print(json.dumps(summary, indent=2))
