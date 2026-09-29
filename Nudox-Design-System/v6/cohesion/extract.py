"""present, from v4/graph/world.json, for the cohesion board: modules, items (kind, name,
visibility, doc, signature), and the relations among them (and out of the package).

Edges are [from, to, mask]; bit i of mask is world['rel'][i]."""
import json, collections, os
here = os.path.dirname(os.path.abspath(__file__))
w = json.load(open(os.path.join(here, "../../v4/graph/world.json")))
P = next(i for i, p in enumerate(w["packages"]) if p["name"] == "backend-present")
mods = {i: m for i, m in enumerate(w["modules"]) if m["pkg"] == P}
nodes = {i: n for i, n in enumerate(w["nodes"]) if n["p"] == P}
FAM = {"struct": "type", "enum": "type", "union": "type", "type": "type", "trait": "contract",
       "function": "callable", "method": "callable", "macro": "callable", "constant": "value",
       "field": "value", "variant": "value", "static": "value"}
# items on the map: public top-level declarations (members live on their parent's page)
items = {i: n for i, n in nodes.items() if n.get("u", -1) == -1 and n["k"] in FAM and n.get("v") in ("pub", "pub(crate)")}
order = {}
out_items = []
for i, n in sorted(items.items(), key=lambda kv: (kv[1]["m"], kv[1].get("l", 0))):
    order[i] = len(out_items)
    members = [c for c in nodes.values() if c.get("u") == i]
    out_items.append({"id": len(out_items), "n": n["n"], "k": n["k"], "f": FAM[n["k"]], "m": n["m"], "v": n.get("v"),
                      "d": (n.get("d") or "")[:140], "s": (n.get("s") or "")[:120], "members": len(members),
                      "variants": [c["n"] for c in members if c["k"] == "variant"][:8]})
by_mod = collections.defaultdict(list)
for it in out_items: by_mod[it["m"]].append(it["id"])
modules = [{"id": m, "path": mods[m]["path"] or "lib", "items": ids} for m, ids in by_mod.items()]
modules.sort(key=lambda m: -len(m["items"]))
# relations between map items (parents stand in for their members), and out of the package
parent = {}
for i, n in nodes.items():
    j = i
    while j in nodes and nodes[j].get("u", -1) != -1: j = nodes[j]["u"]
    parent[i] = j
rels = collections.Counter(); outs = collections.Counter()
for a, b, mask in w["edges"]:
    kinds = [w["rel"][k] for k in range(len(w["rel"])) if mask >> k & 1]
    pa, pb = parent.get(a), parent.get(b)
    if pa in order and pb in order and pa != pb:
        for k in kinds: rels[(order[pa], order[pb], k)] += 1
    elif pa in order and b < len(w["nodes"]) and w["nodes"][b]["p"] != P:
        outs[(order[pa], w["packages"][w["nodes"][b]["p"]]["name"])] += 1
json.dump({"package": "present", "modules": modules, "items": out_items,
           "rel": [[a, b, k, c] for (a, b, k), c in rels.items()],
           "out": [[a, p, c] for (a, p), c in outs.items()]},
          open(os.path.join(here, "data/present.json"), "w"))
print(len(modules), "modules", len(out_items), "items", len(rels), "relations", len(outs), "outward")
for m in modules[:24]: print(f"  {m['path']:18} {len(m['items'])}")
rl = next(it for it in out_items if it["n"] == "RelationLabel")
print(rl, [(out_items[b]["n"], k) for a, b, k, c in [[a, b, k, c] for (a, b, k), c in rels.items()] if a == rl["id"]][:10],
      [(out_items[a]["n"], k) for (a, b, k), c in rels.items() if b == rl["id"]][:10])
