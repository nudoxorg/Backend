"""toml for the sidebar/versions/dependents boards, from facet's release fixture
(apps/facet/src/data/release/fixture.json): the 0.8.23 public API with members, the diff to
1.1.6, every use your workspace makes of it, and the impact of the upgrade."""
import json, os, collections
here = os.path.dirname(os.path.abspath(__file__))
d = json.load(open(os.path.join(here, "../../../apps/facet/src/data/release/fixture.json")))["toml"]
api = d["api"]["0.8.23"]
diff = d["diff"]["0.8.23→1.1.6+spec-1.1.0"]
uses = d["uses"]
impact = d["impact"]["0.8.23→1.1.6+spec-1.1.0"]
FAM = {"struct": "type", "enum": "type", "type": "type", "union": "type", "trait": "contract", "function": "callable",
       "method": "callable", "macro": "callable", "const": "value", "static": "value", "variant": "value", "field": "value"}
state = {}
for k in ("added", "removed", "changed", "deprecated"):
    for c in diff.get(k, []): state.setdefault(c["path"], k)
reexports0 = {p: a["via"] for p, a in api.items() if a.get("via")}
def declared(path):
    """A use names the path you wrote (`toml::Value::as_str`); the item lives where it is declared."""
    if path in api and not api[path].get("via"): return path
    parts = path.split("::")
    for n in range(len(parts), 0, -1):
        head = "::".join(parts[:n])
        if head in reexports0: return reexports0[head] + ("::" + "::".join(parts[n:]) if parts[n:] else "")
    return path
use_by = collections.defaultdict(collections.Counter)
for u in uses:
    u["decl"] = declared(u["path"]); use_by[u["decl"]][u["file"].split("/")[1]] += 1
for c in diff.get("changed", []) + diff.get("removed", []) + diff.get("deprecated", []) + diff.get("added", []):
    pass
items = []
for path, a in api.items():
    if a.get("via"): continue  # re-exports: the item lives at its declared path
    items.append({"path": path, "k": a["k"], "f": FAM.get(a["k"], "value"), "sig": a.get("sig", ""), "change": state.get(path),
                  "uses": dict(use_by.get(path, {}))})
reexports = {p: a["via"] for p, a in api.items() if a.get("via")}
out = {"name": "toml", "pinned": d["pinned"], "target": "1.1.6", "versions": [v["v"].split("+")[0] for v in d["versions"]],
       "yanked": [v["v"].split("+")[0] for v in d["versions"] if v.get("yanked")],
       "items": items, "reexports": reexports,
       "diff": {k: [{"path": c["path"], "k": c.get("k"), "before": c.get("before"), "after": c.get("after") or c.get("sig")} for c in diff.get(k, [])] for k in ("added", "removed", "changed", "deprecated")},
       "uses": uses, "impact": impact}
json.dump(out, open(os.path.join(here, "data/toml.json"), "w"))
tops = [i for i in items if i["path"].count("::") <= 2 and i["k"] not in ("method", "variant", "field")]
print(len(items), "items;", len(tops), "top-level;", {k: len(v) for k, v in out["diff"].items()}, len(uses), "uses")
print(sorted(set(i["path"].rsplit("::", 1)[0] for i in tops)))
print([ (i["path"], i["change"], i["uses"]) for i in items if i["uses"]][:12])
