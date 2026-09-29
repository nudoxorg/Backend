"""The fixture library for the browse boards: every package's public top-level items by module.

Reads a copy of the harness index (`projection.turso`, SQLite plus a turso FTS table, hence
writable_schema). Public = `pub ` (Rust), exported (Go: capitalised), `export` (TS). Top-level =
the parent row is a module. Methods are counted as members of their type."""
import sqlite3, json, re, sys, os, collections
db = sys.argv[1]
c = sqlite3.connect(f"file:{db}?mode=ro", uri=True); c.execute("PRAGMA writable_schema=ON")
NAME = {"backend-runtime": "runtime", "backend-present": "present", "rust-authority-rich-fixture": "rich_project", "toml-pin-fixture": "toml_pin"}
pk = {}
for rid, sig, label in c.execute("select row_id, signature, label from backend_projection_rows where row_kind=0"):
    base = os.path.basename(label); m = re.match(r"(.+)-(\d+\.\d+\.\d+)$", base)
    name = NAME.get(sig, sig) if sig else ("pflag" if "pflag" in label else base)
    lang = "go" if "pflag" in label else "ts" if "/ts/" in label else "rust"
    pk[rid.split(":", 1)[1]] = {"name": name, "version": m.group(2) if m else None, "lang": lang}
rows = {}
for rid, label, pid, parent, sig, doc in c.execute("select row_id, label, package_id, parent_id, signature, document from backend_projection_rows where row_kind=1"):
    rows[rid.split(":", 1)[1]] = (label, pid, parent, sig or "", doc or "")
def fam(first):
    f = re.sub(r"^(pub\s+|export\s+|default\s+|declare\s+|async\s+|unsafe\s+|const\s+(?=fn))+", "", first)
    if re.match(r"(fn|func|function)\b", f) or re.match(r"macro_rules!", f): return "callable"
    if re.match(r"trait\b|interface\b", f) or re.match(r"\w+ interface\b", first): return "contract"
    if re.match(r"(struct|enum|union|class|type)\b", f) or re.match(r"\w+ struct\b", first): return "type"
    if re.match(r"(const|static|let|var)\b", f): return "value"
    return None
members = collections.Counter(r[2] for r in rows.values())
out = []
for pid, p in pk.items():
    mods = collections.OrderedDict()
    for sid, (label, rpid, parent, sig, doc) in rows.items():
        if rpid != pid: continue
        par = rows.get(parent)
        if par and not re.match(r"(rust|go|ts|typescript) module\b|(pub )?mod \w", par[3].split("\n")[0]): continue
        first = sig.split("\n")[0].strip()
        name = label.rsplit("::", 1)[-1]
        public = first.startswith("pub ") or first.startswith("export ") or (p["lang"] == "go" and name[:1].isupper())
        f = fam(first)
        if not public or not f: continue
        path = label.split("::", 1)[1].rsplit("::", 1)[0].split(":")[0]
        parts = [x for x in re.sub(r"\.(rs|go|ts)$", "", re.sub(r"^(src|lib)/", "", path)).split("/") if x]
        if parts and parts[-1] in ("mod", "index", "lib", "main") and len(parts) > 1: parts = parts[:-1]
        mod = "::".join(parts[:2]) or "lib"
        if p["lang"] == "go": mod = "flag"   # one Go package
        mods.setdefault(mod, []).append({"n": name, "f": f, "s": first[:110], "d": doc.split("\n")[0][:120], "members": members[sid]})
    if not mods: continue
    out.append({**p, "modules": [{"path": k, "items": sorted(v, key=lambda i: (i["f"] != "type", i["n"]))} for k, v in sorted(mods.items(), key=lambda kv: -len(kv[1]))]})
out.sort(key=lambda p: -sum(len(m["items"]) for m in p["modules"]))
json.dump(out, open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "data/library.json"), "w"))
for p in out: print(f"{p['name']:14} {p['lang']:4} {sum(len(m['items']) for m in p['modules']):4} items in {len(p['modules']):2} modules: {[m['path'] for m in p['modules']][:6]}")
