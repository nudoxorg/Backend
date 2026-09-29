"""Which items of each dependency does a package name? For every lock edge p → d where p's source is
local, scan p's src for `d_ident::Name` paths and `use d_ident::{…}` imports. Writes data/via.json as
{"<p id>><d id>": [top-level names]}. Approximate like world.json's `uses` (a source scan, not the index)."""
import json, os, re, glob
HOME = os.path.expanduser("~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f")
WJ = json.load(open("data/world.json")); W = WJ["packages"]
REPO = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../.."))
out = {}
lines = {}
for p in W.values():
    if p["kind"] not in ("registry", "yours") or not p["deps"]: continue
    src = os.path.join(HOME, f"{p['name']}-{p['version']}", "src") if p["kind"] == "registry" else os.path.join(REPO, WJ["members_info"][p["name"]]["dir"], "src")
    if not os.path.isdir(src): continue
    text, size, files = [], 0, []
    for f in glob.glob(src + "/**/*.rs", recursive=True):
        try: t = open(f, errors="ignore").read()
        except OSError: continue
        text.append(t); files.append((os.path.relpath(f, os.path.dirname(src)), t)); size += len(t)
        if size > 3_000_000: break
    blob = "\n".join(text)
    for did in p["deps"]:
        d = W.get(did)
        if not d: continue
        ident = d["name"].replace("-", "_")
        names = set(re.findall(r"\b" + re.escape(ident) + r"::(\w+)", blob))
        for grp in re.findall(r"use\s+" + re.escape(ident) + r"::\{([^}]*)\}", blob):
            for part in grp.split(","):
                seg = part.strip().split("::")[0].split(" as ")[0].strip()
                if seg and seg != "self": names.add(seg)
        top = {it["n"] for m in d["modules"] for it in m["items"]}
        names = sorted(n for n in names if n in top)
        if not names: continue
        # Up to two real lines per name, where the dependent writes it: the hop's "how it uses it".
        ex = {}
        pat = re.compile(r"\b" + re.escape(ident) + r"::(?:\w+::)*(" + "|".join(map(re.escape, names)) + r")\b")
        for rel, t in files:
            for i, line in enumerate(t.split("\n")):
                m = pat.search(line)
                if not m or line.lstrip().startswith("//"): continue
                n = m.group(1)
                if len(ex.setdefault(n, [])) < 2: ex[n].append([rel, i + 1, line.strip()[:120]])
        out[f"{p['id']}>{did}"] = names
        if ex: lines[f"{p['id']}>{did}"] = ex
json.dump(out, open("data/via.json", "w"))
json.dump(lines, open("data/via_lines.json", "w"))
print(len(out), "edges;", {k: v for k, v in out.items() if k.startswith("toml@") or k.startswith("toml_edit@")})
