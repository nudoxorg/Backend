"""motion/build.py — extracts the real content the motion board plays on.

Reads (never writes) v4/graph/world.js, v4/graph/releases.json, spec/icons.json,
v4/tpl-ground.html, the workspace Cargo.lock and the cargo registry, and writes
motion/data.js (window.MD = {...}). Run: python3 build.py
"""
import json
import math
import os
import re

HERE = os.path.dirname(os.path.abspath(__file__))
V4 = os.path.dirname(HERE)
DS = os.path.dirname(V4)
REPO = os.path.dirname(DS)

# ------------------------------------------------------------------ icons + ground
icons = {}
for it in json.load(open(os.path.join(DS, "spec", "icons.json"))):
    svg = it["svg"]
    if it["group"] in ("kind", "ui", "capability", "language", "core"):
        icons[f'{it["group"]}:{it["name"]}'] = svg[svg.index(">") + 1: svg.rindex("</svg>")]
ground = open(os.path.join(V4, "tpl-ground.html")).read().strip()

# ------------------------------------------------------------------ the world
raw = open(os.path.join(V4, "graph", "world.js")).read()
W = json.loads(raw[raw.index("{"): raw.rindex("}") + 1])
P, M, N = W["packages"], W["modules"], W["nodes"]
yours_in = W["yoursIn"]


def where(i):
    n = N[i]
    pk = P[n["p"]]["name"].replace("backend-", "")
    mp = M[n["m"]]["path"]
    return f"{pk}::{mp}" if mp else pk


def kids(i):
    return [j for j, n in enumerate(N) if n.get("u") == i]


def item(i, children=False):
    n = N[i]
    out = {"k": n["k"], "n": n["n"], "d": (n.get("d") or "").split("\n")[0], "s": n.get("s", ""),
           "where": where(i), "derives": n.get("derives", []), "yours": yours_in[i],
           "line": n.get("l", 0), "uses": W["inDeg"][i]}
    if children:
        out["kids"] = [item(j) for j in kids(i)]
    return out


def find(name, pkg, mod=None, top=True):
    for i, n in enumerate(N):
        if n["n"] == name and P[n["p"]]["name"] == pkg and (mod is None or M[n["m"]]["path"] == mod) \
                and (not top or n.get("u", -1) == -1):
            return i
    raise KeyError((name, pkg, mod))


glyph_mod = next(i for i, m in enumerate(M) if P[m["pkg"]]["name"] == "backend-present" and m["path"] == "glyph")
glyph = [item(i, True) for i, n in enumerate(N) if n["m"] == glyph_mod and n.get("u", -1) == -1]
present_mods = sorted({m["path"] for m in M if P[m["pkg"]]["name"] == "backend-present" and m["path"] and "::" not in m["path"]})

value_toml = item(find("Value", "toml", "value"), True)
value_json = item(find("Value", "serde_json", "value"), False)
value_json["variants"] = [  # the enum's own docs (registry source, serde_json 1.0.151 value/mod.rs)
    ["Null", "", "Represents a JSON null value."], ["Bool", "bool", "Represents a JSON boolean."],
    ["Number", "Number", "Represents a JSON number, whether integer or floating point."],
    ["String", "String", "Represents a JSON string."], ["Array", "Vec<Value>", "Represents a JSON array."],
    ["Object", "Map<String, Value>", "Represents a JSON object."]]
glyph_lede = open(os.path.join(REPO, "crates", "present", "glyph.rs")).readline()[4:].strip()
xray = [item(find("Deserialize", "serde_core", "de")), item(find("Visitor", "serde_core", "de")),
        item(find("from_str", "serde_json", "de")), item(find("IgnoredAny", "serde_core", "de::ignored_any"))]
semantic = item(find("SemanticLinkKind", "backend-library"), True)

# where the packages sit in the world graph (the across motion travels this way)
geo = {P[i]["name"]: [W["pkg"]["x"][i], W["pkg"]["y"][i]] for i in range(len(P))}

# ------------------------------------------------------------------ find: typing "from_str"
QUERY = "from_str"
KINDS = {"function", "method", "struct", "enum", "trait"}
cands = {}
for i, n in enumerate(N):
    if n["k"] not in KINDS:
        continue
    parent = N[n["u"]]["n"] if n.get("u", -1) != -1 else ""
    key = (n["n"], P[n["p"]]["name"], parent)
    prev = cands.get(key)
    if prev is None or yours_in[i] > yours_in[prev]:
        cands[key] = i


def score(name, q):
    low = name.lower()
    if low == q:
        return 1000
    if low.startswith(q):
        return 700 - len(low)
    if re.search(r"(^|_)" + re.escape(q), low):
        return 420 - len(low)
    if q in low:
        return 260 - len(low)
    k = 0
    for ch in low:
        if k < len(q) and ch == q[k]:
            k += 1
    return 60 - len(low) if k == len(q) else None


def matched(name, q):
    low, out = name.lower(), []
    pos = low.find(q)
    if pos >= 0:
        return list(range(pos, pos + len(q)))
    k = 0
    for j, ch in enumerate(low):
        if k < len(q) and ch == q[k]:
            out.append(j)
            k += 1
    return out


search = []
for L in range(1, len(QUERY) + 1):
    q = QUERY[:L]
    rows = []
    for (name, pkg, parent), i in cands.items():
        s = score(name, q)
        if s is None:
            continue
        pkgw = 40 if P[N[i]["p"]]["yours"] else (18 if P[N[i]["p"]]["external"] else 0)
        s += pkgw + 22 * math.log2(1 + yours_in[i])
        rows.append((s, i, name, parent))
    rows.sort(key=lambda r: (-r[0], r[2], where(r[1])))
    out = []
    for s, i, name, parent in rows[:6]:
        out.append({"id": f"{name}@{where(i)}@{parent}", "k": N[i]["k"], "n": name, "parent": parent,
                    "where": where(i), "hit": matched(name, q), "yours": yours_in[i],
                    "d": (N[i].get("d") or "").split("\n")[0][:90], "s": N[i].get("s", "")[:120]})
    search.append({"q": q, "rows": out, "count": len(rows)})

# ------------------------------------------------------------------ toml releases (the scrub + the lens)
REL = json.load(open(os.path.join(V4, "graph", "releases.json")))["toml"]
d16 = REL["diff"]["0.8.23→1.1.6+spec-1.1.0"]
d05 = REL["diff"]["0.8.23→0.5.11"]
d15 = REL["diff"]["0.8.23→1.1.5+spec-1.1.0"]


def pick(d, sec, path):
    return next(x for x in d[sec] if x["path"] == path)


toml = {
    "pinned": REL["pinned"],
    "versions": [{"v": v["v"].split("+")[0], "at": v["at"][:10], "local": v["local"], "yanked": v["yanked"]}
                 for v in REL["versions"] if not v["yanked"] or v["v"] == REL["pinned"]],
    # the page's own numbers (graph/releases-ui.js summary, re-exports folded, spelling-only moves counted apart)
    "summary": {"1.1.6": [43, 77, 35], "1.1.5": [43, 77, 35], "0.5.11": [88, 82, 19], "uses": len(REL["uses"])},
    "from_str": pick(d16, "changed", "toml::from_str"),
    "deserialize_struct": pick(d16, "added", "toml::value::Value::deserialize_struct"),
    "deserialize_enum": pick(d05, "changed", "toml::value::Value::deserialize_enum"),
    "sites": [{"file": "/".join(u["file"].split("/")[-2:]), "line": u["line"], "text": u["text"].strip()}
              for u in REL["impact"]["0.8.23→1.1.6+spec-1.1.0"]],
    "lens115": {"added": len(d15["added"]), "changed": len(d15["changed"]), "removed": len(d15["removed"]),
                "deprecated": len(d15["deprecated"]),
                "rows": [["+", "method", "Deserializer::parse"], ["~", "function", "from_str"],
                         ["+", "method", "Value::deserialize_struct"], ["-", "method", "map::Map::serialize"]]},
}

# per-release manifest facts from the registry index (every release, local or not): Rust version and the
# dependencies the default features pull in. The scrub plays these as the head passes each tick.
import glob
idx = glob.glob(os.path.expanduser("~/.cargo/registry/index/index.crates.io-*/.cache/to/ml/toml"))[0]
rows = [json.loads(p) for p in open(idx, "rb").read().split(b"\x00") if p.startswith(b"{")]


def default_deps(o):
    feats = dict(o.get("features", {}))
    feats.update(o.get("features2", {}) or {})
    deps = {d["name"]: d for d in o["deps"] if d.get("kind", "normal") in ("normal", None)}
    active, seen, stack = {n for n, d in deps.items() if not d.get("optional")}, set(), ["default"]
    while stack:
        x = stack.pop()
        if x in seen:
            continue
        seen.add(x)
        for y in feats.get(x, []):
            if y.startswith("dep:"):
                active.add(y[4:])
            elif "/" in y:
                if not y.split("/")[0].endswith("?"):
                    active.add(y.split("/")[0])
            elif y in deps and y not in feats:
                active.add(y)
            else:
                stack.append(y)
    return sorted(active)


facts = {o["vers"].split("+")[0]: {"rust": o.get("rust_version") or "", "deps": default_deps(o)} for o in rows}
toml["facts"] = [dict(v=x["v"], **facts.get(x["v"], {"rust": "", "deps": []})) for x in toml["versions"]]

# ------------------------------------------------------------------ add a package: csv 1.4.0 into backend-facet
reg = os.path.expanduser("~/.cargo/registry/src")
csv_dir = next(os.path.join(r, d) for r, ds, _ in os.walk(reg) for d in ds if d == "csv-1.4.0")
csv_toml = open(os.path.join(csv_dir, "Cargo.toml")).read()
csv_deps = re.findall(r"^\[dependencies\.([A-Za-z0-9_-]+)\]", csv_toml, re.M)
lock = open(os.path.join(REPO, "Cargo.lock")).read()
in_tree = set(re.findall(r'\[\[package\]\]\nname = "([^"]+)"', lock))
facet_deps = []
facet_toml = open(os.path.join(REPO, "apps", "facet", "Cargo.toml")).read()
sect = re.search(r"^\[dependencies\]\n(.*?)(^\[|\Z)", facet_toml, re.M | re.S)
if sect:
    facet_deps = [m for m in re.findall(r"^([A-Za-z0-9_-]+)(?:\.workspace)?\s*=", sect.group(1), re.M)
                  if "optional = true" not in sect.group(1).split(m, 1)[1].split("\n", 1)[0]]
add = {
    "name": "csv", "version": "1.4.0",
    "say": re.search(r'^description = "([^"]+)"', csv_toml, re.M).group(1),
    "license": re.search(r'^license = "([^"]+)"', csv_toml, re.M).group(1),
    "deps": [{"n": d, "new": d not in in_tree} for d in csv_deps],
    "into": "backend-facet", "list": facet_deps,
}

# ------------------------------------------------------------------ source: glyph.rs, folded as_str
src = open(os.path.join(REPO, "crates", "present", "glyph.rs")).read().split("\n")
start = next(k for k, l in enumerate(src) if l.startswith("impl RelationLabel"))
end = next(k for k in range(start, len(src)) if src[k] == "}")
source = {"file": "present/glyph.rs", "first": start + 1, "lines": src[start:end + 1]}

MD = {"icons": icons, "ground": ground, "glyph": glyph, "glyphLede": glyph_lede, "presentMods": present_mods, "valueToml": value_toml,
      "valueJson": value_json, "xray": xray, "semantic": semantic, "geo": geo, "search": search, "toml": toml,
      "add": add, "source": source}
with open(os.path.join(HERE, "data.js"), "w") as fh:
    fh.write("/* generated by motion/build.py from the real workspace: do not edit */\nwindow.MD=")
    json.dump(MD, fh, separators=(",", ":"))
    fh.write(";\n")
print("data.js", os.path.getsize(os.path.join(HERE, "data.js")), "bytes;",
      "search", [(s["q"], s["count"], [r["n"] for r in s["rows"]]) for s in search])
