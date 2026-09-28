"""Pull the 13 fixture packages out of the harness index into gems/data/packages.json.

Reads a copy of `.local/harness/desktop/data/projection.turso` (a SQLite file with a
turso FTS table SQLite can't parse, hence writable_schema). Kinds are read off the
signature's first line; weight is how many other signatures name the declaration.
"""
import sqlite3, json, re, sys, collections, os

db = sys.argv[1]
c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
c.execute("PRAGMA writable_schema=ON")

LANG = {"toml": "rust", "serde_json": "rust", "serde_core": "rust", "smallvec": "rust", "basic-toml": "rust",
        "toml_edit": "rust", "toml_datetime": "rust", "zod": "ts", "pflag": "go", "backend-runtime": "rust",
        "backend-present": "rust", "rust-authority-rich-fixture": "rust", "toml-pin-fixture": "rust"}
SHOW = {"backend-runtime": "runtime", "backend-present": "present", "rust-authority-rich-fixture": "rich_project",
        "toml-pin-fixture": "toml_pin"}

pk = {}
for rid, sig, label in c.execute("select row_id, signature, label from backend_projection_rows where row_kind=0"):
    pid = rid.split(":", 1)[1]
    name = sig or os.path.basename(label)
    base = os.path.basename(label)
    m = re.match(r"(.+)-(\d+\.\d+\.\d+)$", base)
    version = m.group(2) if m else None
    pk[pid] = {"id": pid[:8], "raw": name, "name": SHOW.get(name, name), "version": version, "lang": LANG.get(name, "go" if name == "pflag" or "pflag" in label else "rust"),
               "yours": "backend/crates" in label or "fixtures/rich_project" in label or "toml_pin" in label}
    if "pflag" in label: pk[pid]["name"] = "pflag"; pk[pid]["lang"] = "go"

def kind(sig):
    s = (sig or "").strip()
    first = s.split("\n")[0]
    if first.startswith("value"):  # an import (use) row
        return "import"
    if re.match(r"(rust|go|ts|typescript|python) module\b", first) or re.match(r"(pub(\(crate\))? )?mod \w+", first):
        return "module"
    f = re.sub(r"^(pub(\([^)]*\))?\s+|export\s+|default\s+|declare\s+|async\s+|unsafe\s+|const\s+(?=fn)|extern \"C\"\s+)+", "", first)
    if re.match(r"(fn|func|function)\b", f) or re.search(r"\bfn \w+", first): return "callable"
    if re.match(r"\w+ (struct|interface)\b", first): return "type"
    if re.match(r"type \w+(<[^>]*>)?\s*(=|:)", first) and not first.startswith(("pub", "export")): return "assoc"
    if re.match(r"(readonly )?[\w$]+\??: ", first) or re.match(r"^\w+ [\w.*\[\]]+$", first): return "field"
    if re.match(r"(struct|class|interface|union)\b", f) or re.match(r"type \w+ struct", f) or re.match(r"type \w+ interface", f): return "type"
    if re.match(r"enum\b", f): return "type"
    if re.match(r"trait\b", f): return "contract"
    if re.match(r"(impl)\b", f): return "impl"
    if re.match(r"macro_rules!", f): return "callable"
    if re.match(r"type\b", f): return "type"
    if re.match(r"(const|static|let|var)\b", f): return "value"
    return "value"

rows = list(c.execute("select row_id, label, package_id, parent_id, signature, document from backend_projection_rows where row_kind=1"))
byid = {}
for rid, label, pid, parent, sig, doc in rows:
    sid = rid.split(":", 1)[1]
    name = label.rsplit("::", 1)[-1]
    fileline = label.split("::", 1)[1].rsplit("::", 1)[0] if "::" in label else ""
    byid[sid] = {"name": name, "pid": pid, "parent": parent, "sig": sig or "", "kind": kind(sig), "file": fileline.split(":")[0], "doc": (doc or "").split("\n")[0][:120]}

# weight: how many other signatures in the world name this declaration (word match)
words = collections.Counter()
for r in byid.values():
    if r["kind"] in ("import", "module"): continue
    for w in set(re.findall(r"[A-Za-z_][A-Za-z0-9_]{2,}", r["sig"])):
        words[w] += 1
for r in byid.values():
    r["weight"] = max(0, words[r["name"]] - 1) if r["kind"] in ("type", "contract") else 0

def uniq(ds):
    seen, out = set(), []
    for d in ds:
        if d["name"] in seen: continue
        seen.add(d["name"]); out.append(d)
    return out

# packages -> modules -> declarations. Modules are grouped by source file's top directory
# component (e.g. src/de/mod.rs -> de, src/value/de.rs -> value), nested one level.
out = []
for pid, p in pk.items():
    decls = [r for r in byid.values() if r["pid"] == pid and r["kind"] not in ("import", "module", "impl", "assoc", "field")]
    if not decls: continue
    mods = collections.OrderedDict()
    for r in decls:
        f = re.sub(r"^(src|lib|packages/[^/]+/src)/", "", r["file"])
        parts = [x for x in re.sub(r"\.(rs|go|ts|tsx|py)$", "", f).split("/") if x]
        if not parts: parts = ["lib"]
        if parts[-1] in ("mod", "index", "lib", "main") and len(parts) > 1: parts = parts[:-1]
        top = parts[0]
        sub = parts[1] if len(parts) > 1 else None
        m = mods.setdefault(top, {"name": top, "subs": collections.OrderedDict(), "decls": []})
        if sub: m["subs"].setdefault(sub, []).append(r)
        else: m["decls"].append(r)
    def pack(ds):
        k = collections.Counter(d["kind"] for d in ds)
        top = uniq(sorted([d for d in ds if d["kind"] in ("type", "contract")], key=lambda d: -d["weight"]))[:4]
        return {"n": len(ds), "kinds": dict(k), "top": [[d["name"], d["kind"], d["weight"], d["doc"]] for d in top],
                "names": sorted({d["name"] for d in ds})}
    modules = []
    for m in mods.values():
        entry = {"name": m["name"], **pack(m["decls"] + [d for s in m["subs"].values() for d in s])}
        entry["subs"] = [{"name": s, **pack(ds)} for s, ds in m["subs"].items()]
        modules.append(entry)
    modules.sort(key=lambda m: -m["n"])
    out.append({**p, "n": len(decls), "kinds": dict(collections.Counter(d["kind"] for d in decls)), "modules": modules,
                "top": [[d["name"], d["kind"], d["weight"], d["doc"], d["sig"].split("\n")[0][:90]] for d in uniq(sorted(decls, key=lambda d: -d["weight"]))[:6]]})

deps = collections.defaultdict(list)
for src, tn, req, resolved, scope in c.execute("select source, target_name, requirement, resolved, scope from backend_projection_package_edges"):
    deps[src].append({"name": tn, "req": req, "scope": scope})
for p in out:
    for key, d in deps.items():
        if re.search(rf"/{re.escape(p['raw'])}@", key) and (p["version"] is None or p["version"] in key):
            p["deps"] = sorted({x["name"]: x for x in d if x["scope"] == 0}.values(), key=lambda x: x["name"])
            p["devdeps"] = sorted({x["name"] for x in d if x["scope"] != 0})
out.sort(key=lambda p: -p["n"])
json.dump(out, open(os.path.join(os.path.dirname(__file__), "data/packages.json"), "w"), indent=1)
for p in out:
    print(f"{p['name']:14} {p['lang']:4} {p['n']:5} mods={len(p['modules']):3} deps={[d['name'] for d in p.get('deps', [])]} top={[t[0] for t in p['top'][:4]]}")
