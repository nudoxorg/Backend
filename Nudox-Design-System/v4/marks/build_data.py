#!/usr/bin/env python3
"""Build marks/data.json (+ data.js) for the identity marks: real data only.

Sources (all local, read-only):
  - ~/.cargo/registry/index/index.crates.io-*/.cache/**   every version, pubtime, yanked, rust_version, deps, features
  - ~/.cargo/registry/src/index.crates.io-*/<crate>-<ver>/ Cargo.toml metadata + sources (scanned for dependency use)
  - <repo>/Cargo.lock + Cargo.toml                          "your tree" and your project's license
  - v4/graph/world.json                                     cross-package edges for workspace packages (present)
  - v4/graph/releases.json                                  measured API diffs (toml), API size, your uses
  - ~/.npm/_cacache                                         npm packuments (@types/react, csstype) + tarball
  - /nix/store/*-fleet-corpus-<lang>                        one real package per other ecosystem

Anything a source does not say is written as null and the marks render it as unknown.
Re-run: python3 Nudox-Design-System/v4/marks/build_data.py
"""
import base64
import glob
import gzip
import io
import json
import os
import re
import tarfile
import tomllib
from collections import Counter, defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
V4 = os.path.dirname(HERE)
REPO = os.path.dirname(os.path.dirname(V4))
HOME = os.path.expanduser("~")
INDEX = glob.glob(f"{HOME}/.cargo/registry/index/index.crates.io-*/.cache")[0]
SRC = glob.glob(f"{HOME}/.cargo/registry/src/index.crates.io-*")[0]
NPM = f"{HOME}/.npm/_cacache"


# --------------------------------------------------------------------------- crates.io index cache
def index_path(name):
    n = name.lower()
    if len(n) == 1:
        return f"{INDEX}/1/{n}"
    if len(n) == 2:
        return f"{INDEX}/2/{n}"
    if len(n) == 3:
        return f"{INDEX}/3/{n[0]}/{n}"
    return f"{INDEX}/{n[:2]}/{n[2:4]}/{n}"


def read_index(name):
    p = index_path(name)
    if not os.path.exists(p):
        return []
    b = open(p, "rb").read()
    parts = b[5:].split(b"\0")
    out = []
    for i in range(2, len(parts), 2):
        if not parts[i]:
            continue
        try:
            out.append(json.loads(parts[i]))
        except json.JSONDecodeError:
            pass
    return out


def semkey(v):
    core, _, pre = v.split("+")[0].partition("-")
    nums = [int(x) if x.isdigit() else 0 for x in core.split(".")] + [0, 0, 0]
    return (nums[0], nums[1], nums[2], 0 if pre else 1, pre)


def releases_from_index(name):
    rows = []
    for e in read_index(name):
        rows.append({"v": e["vers"], "at": e.get("pubtime"), "yanked": bool(e.get("yanked")),
                     "rust": e.get("rust_version")})
    rows.sort(key=lambda r: semkey(r["v"]))
    return rows


# --------------------------------------------------------------------------- your tree (Cargo.lock)
def read_lock():
    t = open(os.path.join(REPO, "Cargo.lock")).read()
    pk = []
    for b in t.split("[[package]]")[1:]:
        m = re.search(r'name = "([^"]+)"\nversion = "([^"]+)"', b)
        if not m:
            continue
        src = re.search(r'source = "([^"]+)"', b)
        deps = re.findall(r'^ "([^"]+)",?$', b, re.M)
        pk.append({"name": m.group(1), "version": m.group(2), "source": src.group(1) if src else None, "deps": deps})
    return pk


LOCK = read_lock()
ROOT_TOML = tomllib.load(open(os.path.join(REPO, "Cargo.toml"), "rb"))
YOUR_LICENSE = ROOT_TOML["workspace"]["package"]["license"]
LOCAL = {p["name"] for p in LOCK if p["source"] is None}
BY_NAME = defaultdict(list)
for p in LOCK:
    BY_NAME[p["name"]].append(p)


def lock_versions(name):
    return sorted({p["version"] for p in BY_NAME.get(name, [])}, key=semkey)


def resolve_in_lock(parent, parent_version, dep):
    """The version of `dep` that `parent@parent_version` resolves to in your Cargo.lock."""
    for p in BY_NAME.get(parent, []):
        if p["version"] != parent_version:
            continue
        for d in p["deps"]:
            n, _, v = d.partition(" ")
            if n == dep:
                return v or (lock_versions(dep)[0] if len(lock_versions(dep)) == 1 else None)
        return None
    return None


def your_users(dep):
    """Workspace packages (yours) that depend on `dep` directly."""
    users = []
    for p in LOCK:
        if p["source"] is not None:
            continue
        if any(d.split(" ")[0] == dep for d in p["deps"]):
            users.append(p["name"])
    return sorted(users)


def lock_parents(dep, version=None):
    out = []
    for p in LOCK:
        for d in p["deps"]:
            n, _, v = d.partition(" ")
            if n == dep and (version is None or not v or v == version):
                out.append(p["name"])
    return sorted(set(out))


def in_tree(dep):
    vs = lock_versions(dep)
    if not vs:
        return None
    via = lock_parents(dep)
    return {"versions": vs, "yours": your_users(dep), "via": via[:6], "viaCount": len(via), "local": dep in LOCAL}


# --------------------------------------------------------------------------- Rust source scan
IDENT = r"[A-Za-z_][A-Za-z0-9_]*"


def strip_comments(src):
    src = re.sub(r"/\*.*?\*/", " ", src, flags=re.S)
    out = []
    for line in src.split("\n"):
        i, q = 0, False
        cut = len(line)
        while i < len(line):
            c = line[i]
            if c == "\\" and q:
                i += 2
                continue
            if c == '"':
                q = not q
            elif not q and line.startswith("//", i):
                cut = i
                break
            i += 1
        out.append(line[:cut])
    return "\n".join(out)


def expand_use(tree, prefix=()):
    """'a::{b, c::d as e, self}' -> [(('a','b'),'b'), (('a','c','d'),'e'), (('a',),'a')]"""
    tree = tree.strip()
    out = []
    depth, start, items = 0, 0, []
    # split top-level commas
    for i, ch in enumerate(tree):
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
        elif ch == "," and depth == 0:
            items.append(tree[start:i])
            start = i + 1
    items.append(tree[start:])
    for it in items:
        it = it.strip()
        if not it:
            continue
        if "{" in it:
            head, _, rest = it.partition("{")
            head = head.strip().rstrip(":").strip(":")
            segs = tuple(s for s in head.split("::") if s)
            out += expand_use(rest[: rest.rindex("}")], prefix + segs)
            continue
        alias = None
        m = re.match(r"(.*?)\s+as\s+(" + IDENT + r")$", it)
        if m:
            it, alias = m.group(1).strip(), m.group(2)
        segs = tuple(s for s in it.split("::") if s)
        if segs == ("self",):
            path = prefix
        elif segs == ("*",):
            out.append((prefix + ("*",), None))
            continue
        else:
            path = prefix + segs
        if not path:
            continue
        local = alias if alias else path[-1]
        out.append((path, None if local == "_" else local))
    return out


def item_key(segs):
    """('ser','ValueSerializer','new') -> 'ser::ValueSerializer'; keep through the last CamelCase segment."""
    last = -1
    for i, s in enumerate(segs):
        if s[:1].isupper():
            last = i
    if last >= 0:
        return "::".join(segs[: last + 1])
    return "::".join(segs)


def scan_rust(dirs, deps_local):
    """deps_local: {local_name: dep_name}. Returns {dep: {'uses': n, 'items': Counter, 'impls': Counter, 'derives': Counter}}"""
    res = defaultdict(lambda: {"uses": 0, "items": Counter(), "impls": Counter(), "derives": Counter(),
                               "reexports": Counter(), "files": set()})
    files = []
    for d in dirs:
        if os.path.isfile(d):
            files.append(d)
        else:
            files += glob.glob(os.path.join(d, "**", "*.rs"), recursive=True)
    texts = {}
    crate_alias = {}
    for f in files:
        try:
            texts[f] = strip_comments(open(f, errors="replace").read())
        except OSError:
            continue
        for m in re.finditer(r"extern\s+crate\s+(" + IDENT + r")\s+as\s+(" + IDENT + r")\s*;", texts[f]):
            if m.group(1) in deps_local:
                crate_alias[m.group(2)] = deps_local[m.group(1)]
    for f, src in texts.items():
        local = dict(deps_local)
        local.update(crate_alias)
        imports = {}  # local name -> (dep, path tuple)
        use_spans = []
        for m in re.finditer(r"\b(pub(?:\([^)]*\))?\s+)?use\s+(::)?([^;]+);", src):
            use_spans.append((m.start(), m.end()))
            public = bool(m.group(1))
            for path, alias in expand_use(m.group(3)):
                if path and path[0] in local:
                    dep = local[path[0]]
                    rest = path[1:]
                    if rest and rest[-1] == "*":
                        continue
                    if public:
                        res[dep]["reexports"][item_key(rest) if rest else path[0]] += 1
                        res[dep]["files"].add(f)
                        continue
                    if alias:
                        imports[alias] = (dep, rest)
                    elif rest:
                        k = item_key(rest)
                        res[dep]["items"][k] += 1
                        res[dep]["uses"] += 1
                        res[dep]["files"].add(f)
        body = src
        for a, b in reversed(use_spans):
            body = body[:a] + " " * (b - a) + body[b:]
        # qualified paths dep::a::b
        for ln, dep in local.items():
            for m in re.finditer(r"(?<![A-Za-z0-9_:])" + re.escape(ln) + r"((?:::" + IDENT + r")+)", body):
                segs = tuple(s for s in m.group(1).split("::") if s)
                res[dep]["items"][item_key(segs)] += 1
                res[dep]["uses"] += 1
                res[dep]["files"].add(f)
        # imported names
        for name, (dep, rest) in imports.items():
            if name in local:
                continue
            n = 0
            lower = not name[:1].isupper()
            body2 = re.sub(r"\bmod\s+" + re.escape(name) + r"\b", " ", body) if lower else body
            pat = (r"(?<![A-Za-z0-9_:.])" + re.escape(name) + r"((?:::" + IDENT + r")+|(?=\s*[(!]))") if lower else \
                  (r"(?<![A-Za-z0-9_:.])" + re.escape(name) + r"((?:::" + IDENT + r")*)(?![A-Za-z0-9_])")
            for m in re.finditer(pat, body2):
                segs = rest + tuple(s for s in m.group(1).split("::") if s)
                res[dep]["items"][item_key(segs) if segs else name] += 1
                n += 1
            if n == 0:  # a trait brought in for its methods, or `as _`
                res[dep]["items"][item_key(rest) if rest else name] += 1
                n = 1
            res[dep]["uses"] += n
            res[dep]["files"].add(f)
        # impl <Trait> for <Type>
        for m in re.finditer(r"\bimpl\b(?:\s*<[^{;]*?>)?\s+([A-Za-z_][A-Za-z0-9_:]*)(?:<[^{;]*?>)?\s+for\s+([A-Za-z_][A-Za-z0-9_]*)", body):
            tr, ty = m.group(1), m.group(2)
            segs = tuple(tr.split("::"))
            dep = None
            if segs[0] in local and len(segs) > 1:
                dep, key = local[segs[0]], segs[-1]
            elif segs[0] in imports:
                dep, key = imports[segs[0]][0], segs[-1]
            elif len(segs) > 1 and segs[0] in imports:
                dep, key = imports[segs[0]][0], segs[-1]
            if dep:
                res[dep]["impls"][(key, ty)] += 1
        # #[derive(...)]
        for m in re.finditer(r"#\[derive\(([^)]*)\)\]", body):
            for part in m.group(1).split(","):
                part = part.strip()
                segs = tuple(part.split("::"))
                if segs[0] in local and len(segs) > 1:
                    res[local[segs[0]]]["derives"][segs[-1]] += 1
                elif part in imports:
                    res[imports[part][0]]["derives"][part] += 1
    return res


# --------------------------------------------------------------------------- purpose in plain words
SER = {"Serialize", "Serializer", "SerializeMap", "SerializeSeq", "SerializeStruct", "SerializeStructVariant",
       "SerializeTuple", "SerializeTupleStruct", "SerializeTupleVariant"}
DE = {"Deserialize", "Deserializer", "Visitor", "MapAccess", "SeqAccess", "EnumAccess", "VariantAccess",
      "IntoDeserializer", "DeserializeSeed", "DeserializeOwned", "Unexpected"}


def last(k):
    return k.split("::")[-1]


def human_list(xs, conj="and"):
    xs = list(xs)
    if len(xs) <= 1:
        return "".join(xs)
    return ", ".join(xs[:-1]) + f" {conj} " + xs[-1]


DESC_WORDS = [  # keyword -> plain words, matched against the dependency's own one-line description
    (r"integer.*to string|integer primitive", "turns integers into text"),
    (r"double-to-string|float.*to string|shortest.*float", "turns floats into text"),
    (r"byte search|substring search", "fast byte search"),
    (r"#\[derive|derive\(", "the derive macros"),
    (r"cast &T to &U", "safe reference casts"),
    (r"compiler version", "gates on the compiler version"),
    (r"compiler diagnostics|ui tests", "compile-fail tests"),
    (r"&\[u8\]|Vec<u8>", "&[u8] and Vec<u8> as bytes, not lists"),
]


def plain_from_desc(desc):
    d = " ".join((desc or "").split())
    for pat, words in DESC_WORDS:
        if re.search(pat, d, re.I):
            return words
    return None


def purpose(dep_name, scan, desc, prefer_types=False):
    """Rules over the computed items. Returns (phrase, rule)."""
    items = scan["items"]
    impls = scan["impls"]
    derives = scan["derives"]
    reex = scan.get("reexports") or Counter()
    total = sum(items.values())
    names = {last(k) for k in items}
    if "derive" in dep_name:
        ds = sorted(names | {last(k) for k in reex})[:2]
        return f"the #[derive] macros{': ' + '/'.join(ds) if ds else ''}", "derive-macros"
    if reex and (sum(reex.values()) >= total * 0.5 or len(reex) >= 4):
        rn = sorted({last(k) for k in reex}, key=lambda n: (not n[:1].isupper(), n))
        rn = [n for n in rn if not n.startswith("__")]
        return f"re-exports {human_list(rn[:3])} as its own", "reexports"
    serde_traits = [t for (t, _), _n in impls.most_common() if t in SER | DE]
    if derives and sum(derives.values()) >= max(3, total * 0.3):
        ds = [d for d, _ in derives.most_common(2)]
        return f"derives {'/'.join(ds)} on {sum(derives.values())} types", "derives"
    if serde_traits:
        traits = []
        for t in serde_traits:
            base = "Serialize" if t in SER else "Deserialize"
            if base not in traits:
                traits.append(base)
        targets = Counter(ty for (t, ty), n in impls.items() if t in ("Serialize", "Deserialize")) or \
            Counter(ty for (t, ty), n in impls.items() if t in SER | DE)
        tg = [t for t, _ in targets.most_common(2)]
        return f"{'/'.join(traits)} on {human_list(tg)}", "implements-serde"
    if "Datetime" in names:
        return "its Datetime type", "type"
    if "Spanned" in names:
        return "spans on values: Spanned<T>", "type"
    if names & {"IndexMap", "Entry", "OccupiedEntry"} or dep_name == "indexmap":
        return "an ordered map for tables", "map"
    words = plain_from_desc(desc)
    owners = Counter()
    for k, n in items.items():
        owners[k.split("::")[0] if k[:1].isupper() else k] += n
    top1 = owners.most_common(1)[0] if owners else None
    fmt_like = r"(Serializer|Deserializer|Parser|Error)"
    has_tail = any(re.fullmatch(r"(to_writer|to_string|to_vec|to_string_pretty|from_str|from_slice|from_reader|from_value)", last(k)) for k in items)
    if words and not has_tail and not prefer_types:
        return words, "description-words"
    if top1 and last(top1[0])[:1].isupper() and not re.search(fmt_like, top1[0]) and (prefer_types or top1[1] >= total * 0.4):
        ty = top1[0].split("::")[0]
        w = [last(k) for k in items if re.fullmatch(r"(to_writer|to_string|to_vec|to_string_pretty)", last(k))]
        r = [last(k) for k in items if re.fullmatch(r"(from_str|from_slice|from_reader|from_value)", last(k))]
        tail = (f", written with {w[0]}" if w else "") + (f" and read with {r[0]}" if r and w else (f", read with {r[0]}" if r else ""))
        if prefer_types and not tail:
            owners = []
            for k, _ in items.most_common():
                o = k.split("::")[0]
                if o not in owners:
                    owners.append(o)
            return f"its {human_list(owners[:3])}", "types"
        return f"its {ty}{tail}", "type-first"
    parse = any(re.search(r"(^|::)(de|parse|Parser|Deserializer)($|::)", k) or last(k) in ("DocumentMut", "ValueDeserializer", "from_str")
                for k in items)
    write = any(re.search(r"(^|::)(ser|write|Serializer)($|::)", k) or last(k) in ("ValueSerializer", "to_writer", "to_string")
                for k in items)
    if parse and write:
        return "the parser and the writer", "parse+write"
    if parse:
        return "the parser", "parse"
    if write:
        return "the writer", "write"
    top = [k for k, _ in items.most_common(2)]
    if words:
        return words, "description-words"
    if top and desc:
        d = " ".join(desc.split()).rstrip(".")
        return f"{human_list(top)}: {d[0].lower() + d[1:]}", "description"
    if top:
        return human_list(top), "items"
    return None, "none"


# --------------------------------------------------------------------------- crate metadata
def crate_dir(name, version):
    d = os.path.join(SRC, f"{name}-{version}")
    return d if os.path.isdir(d) else None


def crate_manifest(name, version):
    d = crate_dir(name, version)
    if not d:
        return None
    return tomllib.load(open(os.path.join(d, "Cargo.toml"), "rb"))


def dep_table(man):
    """[(local, dep_name, kind, spec)] from a normalized Cargo.toml (incl. target tables)."""
    out = []

    def take(tab, kind, target=None):
        for local, spec in (tab or {}).items():
            if isinstance(spec, str):
                spec = {"version": spec}
            out.append((local, spec.get("package", local), kind, spec, target))
    take(man.get("dependencies"), "normal")
    take(man.get("dev-dependencies"), "dev")
    take(man.get("build-dependencies"), "build")
    for tgt, t in (man.get("target") or {}).items():
        take(t.get("dependencies"), "normal", tgt)
        take(t.get("dev-dependencies"), "dev", tgt)
        take(t.get("build-dependencies"), "build", tgt)
    return out


def features_enabling(man, local, optional):
    feats = []
    for f, lst in (man.get("features") or {}).items():
        for x in lst:
            if x in (f"dep:{local}", local) or (x.startswith(f"{local}/") and optional is False):
                feats.append(f)
                break
    if optional and not any(f"dep:{local}" in lst for lst in (man.get("features") or {}).values()) and local not in feats:
        feats.append(local)  # the implicit feature
    return sorted(set(feats))


def default_features(man):
    seen, todo = set(), list((man.get("features") or {}).get("default", []))
    while todo:
        f = todo.pop()
        if f in seen or f.startswith("dep:") or "/" in f:
            continue
        seen.add(f)
        todo += (man.get("features") or {}).get(f, [])
    return sorted(seen)


def crate_package(name, version, desc_limit=None):
    man = crate_manifest(name, version)
    d = crate_dir(name, version)
    pkg = man["package"]
    deps = dep_table(man)
    locals_ = {}
    for local, dn, kind, spec, tgt in deps:
        locals_[local.replace("-", "_")] = dn
    scan_src = scan_rust([os.path.join(d, "src")], {k: v for k, v in locals_.items()
                                                  if any(x[1] == v and x[2] == "normal" for x in deps)})
    dev_dirs = [os.path.join(d, x) for x in ("tests", "examples", "benches") if os.path.isdir(os.path.join(d, x))]
    scan_dev = scan_rust(dev_dirs, {k: v for k, v in locals_.items() if any(x[1] == v and x[2] == "dev" for x in deps)}) if dev_dirs else {}
    build_rs = os.path.join(d, "build.rs")
    scan_build = scan_rust([build_rs], {k: v for k, v in locals_.items() if any(x[1] == v and x[2] == "build" for x in deps)}) if os.path.exists(build_rs) else {}
    defaults = default_features(man)
    rows = []
    seen = set()
    for local, dn, kind, spec, tgt in deps:
        if (dn, kind) in seen:
            continue
        seen.add((dn, kind))
        if tgt == "cfg(any())":  # serde_json's never-true target: a version-pin trick, not a dependency
            continue
        optional = bool(spec.get("optional"))
        feats = features_enabling(man, local, optional) if optional else []
        scan = {"normal": scan_src, "dev": scan_dev, "build": scan_build}[kind].get(dn)
        dmeta = None
        resolved = resolve_in_lock(name, version, dn) if kind == "normal" else None
        rv = resolved or (lock_versions(dn)[-1] if lock_versions(dn) else None)
        if rv and crate_dir(dn, rv):
            dmeta = crate_manifest(dn, rv)["package"]
        desc = (dmeta or {}).get("description")
        shipped = kind == "normal" or (kind == "dev" and dev_dirs) or (kind == "build" and os.path.exists(build_rs))
        phrase, rule = purpose(dn, scan, desc) if scan else (None, "not-shipped" if not shipped else "unused-in-shipped-files")
        on = None
        if optional:
            on_default = any(f in defaults for f in feats)
            on_tree = resolved is not None if kind == "normal" and BY_NAME.get(name) else None
            on = {"default": on_default, "tree": on_tree}
        rows.append({
            "name": dn, "alias": local, "kind": kind, "target": tgt, "req": spec.get("version"),
            "resolved": resolved, "optional": optional, "features": feats, "on": on,
            "defaultFeatures": spec.get("default-features", True),
            "uses": (scan["uses"] + sum(scan["reexports"].values())) if scan else (0 if kind == "normal" else None),
            "files": len(scan["files"]) if scan else 0,
            "reexports": sum(scan["reexports"].values()) if scan else 0,
            "items": [[k, n] for k, n in scan["items"].most_common(6)] if scan else [],
            "impls": [[t, ty, n] for (t, ty), n in scan["impls"].most_common(4)] if scan else [],
            "derives": [[k, n] for k, n in scan["derives"].most_common(3)] if scan else [],
            "purpose": phrase, "rule": rule, "desc": desc,
            "inTree": in_tree(dn),
        })
    return {
        "name": name, "eco": "crates", "version": version,
        "description": " ".join((pkg.get("description") or "").split()) or None,
        "license": pkg.get("license"), "licenseFile": pkg.get("license-file"),
        "repository": pkg.get("repository"), "rust": pkg.get("rust-version"),
        "defaultFeatures": defaults, "deps": rows,
        "install": f"cargo add {name}",
    }


# --------------------------------------------------------------------------- workspace package via world.json
def world_package(pkg_name, cargo_dir):
    w = json.load(open(os.path.join(V4, "graph", "world.json")))
    P, N, R = w["packages"], w["nodes"], w["rel"]
    pi = next(i for i, p in enumerate(P) if p["name"] == pkg_name)
    man = tomllib.load(open(os.path.join(REPO, cargo_dir, "Cargo.toml"), "rb"))
    by_pkg = defaultdict(lambda: {"items": Counter(), "rels": Counter(), "derivers": set(), "uses": 0})
    for a, b, code in w["edges"]:
        if N[a]["p"] != pi or N[b]["p"] == pi:
            continue
        tgt = N[b]
        u = tgt.get("u", -1)
        owner = N[u]["n"] if isinstance(u, int) and u >= 0 else ""
        key = f"{owner}::{tgt['n']}" if owner else tgt["n"]
        pn = P[tgt["p"]]["name"]
        slot = by_pkg[pn]
        slot["items"][key] += 1
        slot["uses"] += 1
        for i, r in enumerate(R):
            if code >> i & 1:
                slot["rels"][r] += 1
                if r == "derives":
                    slot["derivers"].add(a)
    nodes = [n for n in N if n["p"] == pi]
    mods = [m for m in w["modules"] if m["pkg"] == pi]
    alias = {"serde": "serde_core"}  # serde re-exports serde_core; world.json records the defining crate
    rows = []
    direct = set()
    for kind, tab in (("normal", man.get("dependencies", {})), ("dev", man.get("dev-dependencies", {}))):
        for local, spec in tab.items():
            dn = local
            wn = alias.get(dn, dn)
            direct.add(wn)
            slot = by_pkg.get(wn)
            items = slot["items"] if slot else Counter()
            phrase, rule = None, "none"
            if slot:
                if slot["rels"].get("derives", 0) >= slot["uses"] * 0.5:
                    ds = [k for k, _ in items.most_common(2)]
                    phrase, rule = f"derives {'/'.join(ds)} on {len(slot['derivers'])} types", "derives"
                else:
                    fake = {"items": items, "impls": Counter(), "derives": Counter(), "reexports": Counter()}
                    desc = None
                    if wn in LOCAL:
                        for p in glob.glob(os.path.join(REPO, "crates", "*", "Cargo.toml")):
                            t = tomllib.load(open(p, "rb"))
                            if t.get("package", {}).get("name") == wn:
                                desc = t["package"].get("description")
                    phrase, rule = purpose(wn, fake, desc, prefer_types=wn in LOCAL)
                    if all(last(k).endswith("Error") or "Error::" in k for k, _ in items.most_common(3)):
                        top = items.most_common(1)[0][0].split("::")[0]
                        phrase, rule = f"its errors: {top}", "errors"
            isws = isinstance(spec, dict) and spec.get("workspace")
            req = None
            if isws:
                wd = ROOT_TOML["workspace"]["dependencies"].get(dn)
                req = wd if isinstance(wd, str) else (wd or {}).get("version")
            elif isinstance(spec, dict):
                req = spec.get("version") or (f"path {spec['path']}" if "path" in spec else None)
            rows.append({
                "name": dn, "kind": kind, "req": req, "resolved": (lock_versions(dn) or [None])[-1] if dn not in LOCAL else None,
                "local": dn in LOCAL or (isinstance(spec, dict) and "path" in spec), "optional": False, "features": [],
                "uses": slot["uses"] if slot else (0 if kind == "normal" else None), "items": [[k, n] for k, n in items.most_common(6)],
                "purpose": phrase, "rule": rule, "inTree": in_tree(dn), "via": "world.json edges",
            })
    through = {p: s["uses"] for p, s in by_pkg.items() if p not in direct and p != "std"}
    return {
        "name": pkg_name.replace("backend-", ""), "crate": pkg_name, "eco": "crates", "local": True,
        "version": ROOT_TOML["workspace"]["package"]["version"], "publish": ROOT_TOML["workspace"]["package"].get("publish", True),
        "path": cargo_dir, "license": YOUR_LICENSE, "description": None,
        "api": {"declarations": len(nodes), "public": sum(1 for n in nodes if n.get("v") == "pub"), "modules": len(mods),
                "source": "world.json"},
        "deps": rows, "through": through,
        "install": f'{pkg_name} = {{ path = "{cargo_dir}" }}',
    }


# --------------------------------------------------------------------------- npm cache
def npm_entries():
    out = {}
    for f in glob.glob(f"{NPM}/index-v5/**/*", recursive=True):
        if not os.path.isfile(f):
            continue
        for line in open(f, errors="replace"):
            if "\t" not in line:
                continue
            try:
                e = json.loads(line.split("\t", 1)[1])
            except json.JSONDecodeError:
                continue
            algo, b64 = e["integrity"].split("-", 1)
            hx = base64.b64decode(b64).hex()
            p = f"{NPM}/content-v2/{algo}/{hx[:2]}/{hx[2:4]}/{hx[4:]}"
            if os.path.exists(p):
                accept = e.get("metadata", {}).get("reqHeaders", {}).get("accept", "")
                key = e["key"].split("request-cache:")[-1]
                if "install-v1" in accept and key in out:
                    continue  # prefer the full document
                out[key] = p
    return out


def npm_package(name, dep_names=()):
    ents = npm_entries()
    url = "https://registry.npmjs.org/" + name.replace("/", "%2f")
    doc = json.load(open(ents[url]))
    latest = doc["dist-tags"]["latest"]
    times = doc.get("time", {})
    rel = []
    for v, meta in doc["versions"].items():
        rel.append({"v": v, "at": times.get(v), "yanked": bool(meta.get("deprecated")),
                    "deprecated": meta.get("deprecated")})
    rel.sort(key=lambda r: semkey(r["v"]))
    meta = doc["versions"][latest]
    deps = []
    tgz_key = f"https://registry.npmjs.org/{name}/-/{name.split('/')[-1]}-{latest}.tgz"
    src = ""
    if tgz_key in ents:
        with tarfile.open(fileobj=io.BytesIO(gzip.decompress(open(ents[tgz_key], "rb").read()))) as tf:
            for m in tf.getmembers():
                if m.name.endswith(".d.ts") and "/ts5" not in m.name:
                    src += tf.extractfile(m).read().decode("utf8", "replace") + "\n"
    for dn, req in (meta.get("dependencies") or {}).items():
        items, uses = Counter(), 0
        body = re.sub(r"/\*.*?\*/", " ", src, flags=re.S)
        body = re.sub(r"//[^\n]*", " ", body)
        for m in re.finditer(r'import\s+\*\s+as\s+(\w+)\s+from\s+["\']' + re.escape(dn) + r'["\']', body):
            ns = m.group(1)
            for q in re.finditer(r"(?<![\w.])" + ns + r"\.(\w+)", body):
                items[q.group(1)] += 1
                uses += 1
        for m in re.finditer(r'import\s*(?:type\s*)?\{([^}]*)\}\s*from\s*["\']' + re.escape(dn) + r'["\']', body):
            for nm in m.group(1).split(","):
                nm = nm.strip().split(" as ")[-1].strip()
                if nm:
                    c = len(re.findall(r"(?<![\w.])" + nm + r"(?!\w)", body)) - 1
                    items[nm] += max(1, c)
                    uses += max(1, c)
        dep_doc = json.load(open(ents["https://registry.npmjs.org/" + dn.replace("/", "%2f")])) if ("https://registry.npmjs.org/" + dn) in ents else None
        desc = dep_doc.get("description") if dep_doc else None
        newest = dep_doc["dist-tags"]["latest"] if dep_doc else None
        phrase, rule = None, "none"
        if items:
            top = [k for k, _ in items.most_common(2)]
            if all(re.match(r"Properties", k) for k in top):
                phrase, rule = "the types behind style={…}: " + human_list(top), "types"
            else:
                phrase, rule = human_list(top), "items"
        deps.append({"name": dn, "kind": "normal", "req": req, "resolved": None, "newest": newest,
                     "optional": False, "features": [], "uses": uses, "items": [[k, n] for k, n in items.most_common(6)],
                     "purpose": phrase, "rule": rule, "desc": desc,
                     "inTree": None, "treeNote": "no npm packages in your project"})
    return {
        "name": name, "eco": "npm", "version": latest, "description": doc.get("description"),
        "license": doc.get("license") or meta.get("license"), "repository": (doc.get("repository") or {}).get("url"),
        "deps": deps, "releases": rel, "pin": None, "install": f"npm i {name}",
    }


# --------------------------------------------------------------------------- other ecosystems (fleet corpora)
def corpus(lang):
    c = sorted(glob.glob(f"/nix/store/*-fleet-corpus-{lang}"))
    return c[0] if c else None


def other_ecosystems():
    out = {}
    p = corpus("python")
    if p and os.path.isdir(f"{p}/tomli-2.2.1"):
        info = open(f"{p}/tomli-2.2.1/PKG-INFO").read()
        lic = "MIT" if "License :: OSI Approved :: MIT License" in info else None
        summ = re.search(r"^Summary: (.*)$", info, re.M)
        out["tomli"] = {"name": "tomli", "eco": "pypi", "version": "2.2.1", "license": lic,
                        "description": summ.group(1) if summ else None, "install": "pip install tomli"}
    g = corpus("go")
    if g and os.path.isdir(f"{g}/github.com/BurntSushi/toml@v1.4.0"):
        cp = open(f"{g}/github.com/BurntSushi/toml@v1.4.0/COPYING").read()
        out["github.com/BurntSushi/toml"] = {"name": "github.com/BurntSushi/toml", "eco": "go", "version": "v1.4.0",
                                             "license": "MIT" if cp.startswith("The MIT License") else None,
                                             "description": None,
                                             "install": "go get github.com/BurntSushi/toml@v1.4.0"}
    j = corpus("java")
    pom = f"{j}/joda-time/joda-time/2.12.7/META-INF/maven/joda-time/joda-time/pom.xml" if j else None
    if pom and os.path.exists(pom):
        t = open(pom).read()
        lic = "Apache-2.0" if "Apache License, Version 2.0" in t else None
        desc = re.search(r"<description>(.*?)</description>", t)
        out["joda-time"] = {"name": "joda-time:joda-time", "eco": "maven", "version": "2.12.7", "license": lic,
                            "description": desc.group(1) if desc else None,
                            "install": 'implementation("joda-time:joda-time:2.12.7")'}
    n = corpus("csharp")
    if n:
        vs = sorted(os.listdir(f"{n}/polyfill"), key=semkey)
        v = vs[-1]
        spec = glob.glob(f"{n}/polyfill/{v}/*.nuspec")
        t = open(spec[0]).read() if spec else ""
        lic = re.search(r'<license type="expression">([^<]+)</license>', t)
        desc = re.search(r"<description>(.*?)</description>", t)
        out["Polyfill"] = {"name": "Polyfill", "eco": "nuget", "version": v, "license": lic.group(1) if lic else None,
                           "description": desc.group(1) if desc else None,
                           "install": f"dotnet add package Polyfill --version {v}"}
        ti = f"{n}/tinyioc/1.3.0/TinyIoC.nuspec"
        if os.path.exists(ti):
            t = open(ti).read()
            lu = re.search(r"<licenseUrl>([^<]+)</licenseUrl>", t)
            out["TinyIoC"] = {"name": "TinyIoC", "eco": "nuget", "version": "1.3.0", "license": None,
                              "licenseUrl": lu.group(1) if lu else None, "install": "dotnet add package TinyIoC --version 1.3.0"}
    c = corpus("clang")
    if c and os.path.isdir(f"{c}/tomlplusplus"):
        mb = open(f"{c}/tomlplusplus/meson.build").read()
        ver = re.search(r"version:\s*'([^']+)'", mb)
        lic = open(f"{c}/tomlplusplus/LICENSE").read()
        out["tomlplusplus"] = {"name": "tomlplusplus", "eco": "cpp", "version": ver.group(1) if ver else None,
                               "license": "MIT" if lic.startswith("MIT License") else None, "description": None,
                               "headerOnly": os.path.exists(f"{c}/tomlplusplus/toml.hpp"),
                               "install": "#include <toml++/toml.hpp>"}
    return out


# --------------------------------------------------------------------------- licenses seen in your registry
def license_examples():
    want = ["MIT", "MIT OR Apache-2.0", "Apache-2.0", "Unlicense OR MIT", "MPL-2.0", "Apache-2.0 OR GPL-2.0-only",
            "MIT OR Apache-2.0 OR LGPL-2.1-or-later", "(MIT OR Apache-2.0) AND Unicode-3.0", "CC0-1.0", "Unlicense",
            "BSD-3-Clause", "Zlib", "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT"]
    found, nolicense, counts = {}, [], Counter()
    in_lock = {(p["name"], p["version"]) for p in LOCK}
    dirs = sorted(os.listdir(SRC), key=lambda d: (tuple(d.rsplit("-", 1)) not in in_lock, d))
    for d in dirs:
        p = os.path.join(SRC, d, "Cargo.toml")
        if not os.path.exists(p):
            continue
        try:
            pkg = tomllib.load(open(p, "rb")).get("package", {})
        except tomllib.TOMLDecodeError:
            continue
        lic = pkg.get("license")
        if lic:
            counts[lic] += 1
            if lic in want and lic not in found:
                found[lic] = {"crate": pkg.get("name"), "version": pkg.get("version"),
                              "inTree": (pkg.get("name"), pkg.get("version")) in in_lock}
        else:
            nolicense.append({"crate": pkg.get("name"), "version": pkg.get("version"), "licenseFile": pkg.get("license-file"),
                              "inTree": (pkg.get("name"), pkg.get("version")) in in_lock})
    return {"examples": found, "noLicenseField": nolicense, "crates": sum(counts.values()) + len(nolicense),
            "top": counts.most_common(8)}


# --------------------------------------------------------------------------- what the marks know about your whole tree
FAMILY = {
    "MIT": 1, "MIT-0": 1, "Apache-2.0": 1, "BSD-1-Clause": 1, "BSD-2-Clause": 1, "BSD-3-Clause": 1, "0BSD": 1, "ISC": 1, "Zlib": 1,
    "zlib-acknowledgement": 1, "BSL-1.0": 1, "Unicode-3.0": 1, "Unicode-DFS-2016": 1, "NCSA": 1, "CDLA-Permissive-2.0": 1,
    "Unlicense": 0, "CC0-1.0": 0, "MPL-2.0": 2, "LGPL-2.1": 2, "LGPL-3.0": 2, "EPL-2.0": 2, "GPL-2.0": 3, "GPL-3.0": 3, "AGPL-3.0": 3,
}
FAM_NAME = {0: "public", 1: "permissive", 2: "weak", 3: "strong", 4: "unknown"}


def spdx_options(expr):
    """SPDX (or legacy 'A/B') -> list of options, each a list of ids that all apply."""
    toks = re.findall(r"\(|\)|[^\s()]+", re.sub(r"\s*/\s*", " OR ", expr))
    pos = [0]

    def peek():
        return toks[pos[0]] if pos[0] < len(toks) else None

    def atom():
        if peek() == "(":
            pos[0] += 1
            e = por()
            pos[0] += 1
            return e
        i = toks[pos[0]]
        pos[0] += 1
        if peek() == "WITH":
            pos[0] += 2
        return [[i]]

    def pand():
        acc = atom()
        while peek() == "AND":
            pos[0] += 1
            nxt = atom()
            acc = [a + b for a in acc for b in nxt]
        return acc

    def por():
        acc = pand()
        while peek() == "OR":
            pos[0] += 1
            acc = acc + pand()
        return acc
    return por()


def fam_of(expr, has_file=False):
    if not expr:
        return 4, None
    best, pick = 9, None
    for opt in spdx_options(expr):
        worst = max(FAMILY.get(re.sub(r"-only$|-or-later$|\+$", "", i), 4) for i in opt)
        if worst < best:
            best, pick = worst, opt
    return best, pick


LOCAL_MANIFESTS = {}
for _m in glob.glob(os.path.join(REPO, "*", "**", "Cargo.toml"), recursive=True):
    if "/target/" in _m or "/node_modules/" in _m or "/turso/" in _m:
        continue
    try:
        _pk = tomllib.load(open(_m, "rb")).get("package", {})
    except (tomllib.TOMLDecodeError, OSError):
        continue
    if _pk.get("name") in LOCAL:
        LOCAL_MANIFESTS.setdefault(_pk["name"], _m)


def edge_kinds(parent, pver, dep):
    """How `parent` depends on `dep`: {'normal','build','dev'} from its manifest (local or registry)."""
    path = LOCAL_MANIFESTS.get(parent) if parent in LOCAL else (os.path.join(crate_dir(parent, pver), "Cargo.toml") if crate_dir(parent, pver) else None)
    if not path:
        return None
    try:
        man = tomllib.load(open(path, "rb"))
    except (tomllib.TOMLDecodeError, OSError):
        return None
    kinds = set()
    for local, dn, kind, spec, tgt in dep_table(man):
        if dn == dep:
            kinds.add(kind)
    return kinds or None


def build_only(name, version):
    ks = set()
    for par, pv in PARENTS.get((name, version), ()):
        k = edge_kinds(par, pv, name)
        if k is None:
            return False
        ks |= k
    return bool(ks) and ks <= {"build", "dev"}


def lock_license(p):
    if p["source"] is None:
        return YOUR_LICENSE, None
    d = crate_dir(p["name"], p["version"])
    if d:
        try:
            pk = tomllib.load(open(os.path.join(d, "Cargo.toml"), "rb")).get("package", {})
            return pk.get("license"), pk.get("license-file")
        except tomllib.TOMLDecodeError:
            return None, None
    if p["source"] and p["source"].startswith("registry"):
        return "UNREAD", None
    for g in glob.glob(f"{HOME}/.cargo/git/checkouts/*/*/**/Cargo.toml", recursive=True):
        try:
            pk = tomllib.load(open(g, "rb")).get("package", {})
        except (tomllib.TOMLDecodeError, OSError):
            continue
        if pk.get("name") == p["name"] and str(pk.get("version")) == p["version"]:
            lic = pk.get("license")
            return (lic if isinstance(lic, str) else None), pk.get("license-file")
    return None, None


PARENTS = defaultdict(set)  # (name, version) -> {(parent name, parent version)}
for _p in LOCK:
    for _d in _p["deps"]:
        _n, _, _v = _d.partition(" ")
        _v = _v or (lock_versions(_n)[0] if len(lock_versions(_n)) == 1 else None)
        PARENTS[(_n, _v)].add((_p["name"], _p["version"]))


def reaches(name, version, limit=4000):
    """Which of your packages pull (name, version) in, and the nearest parents on the way."""
    seen, todo, roots = set(), [(name, version)], set()
    while todo and len(seen) < limit:
        cur = todo.pop()
        for par in PARENTS.get(cur, ()):
            if par in seen:
                continue
            seen.add(par)
            if par[0] in LOCAL:
                roots.add(par[0])
            else:
                todo.append(par)
    return sorted(roots)


def tree_licenses():
    fams, notable = Counter(), []
    for p in LOCK:
        if p["source"] is None:
            continue
        lic, lf = lock_license(p)
        if lic == "UNREAD" or (lic is None and lf is None and p["source"] and not p["source"].startswith("registry")):
            fams["unread"] += 1
            continue
        f, pick = fam_of(lic)
        if not lic and lf:
            f = 4
        fams[FAM_NAME[f]] += 1
        if f >= 2:
            notable.append({"crate": p["name"], "version": p["version"], "license": lic or ("custom" if lf else "none"),
                            "licenseFile": lf, "fam": FAM_NAME[f], "via": sorted(n for n, _ in PARENTS.get((p["name"], p["version"]), ()))[:4],
                            "yours": reaches(p["name"], p["version"])[:6], "buildOnly": build_only(p["name"], p["version"])})
        elif lic and len(spdx_options(lic)) > 1 and any(FAMILY.get(re.sub(r"-only$|-or-later$", "", i), 4) >= 2 for o in spdx_options(lic) for i in o):
            notable.append({"crate": p["name"], "version": p["version"], "license": lic, "fam": FAM_NAME[f], "chosen": pick,
                            "via": sorted(n for n, _ in PARENTS.get((p["name"], p["version"]), ()))[:4], "yours": reaches(p["name"], p["version"])[:6]})
    total = sum(fams.values())
    return {"total": total, "families": dict(fams), "notable": notable, "local": len(LOCAL)}


def duplicates():
    out = []
    for name, ps in BY_NAME.items():
        vs = sorted({p["version"] for p in ps}, key=semkey)
        if len(vs) < 2 or any(p["source"] is None for p in ps):
            continue
        out.append({"name": name, "versions": [{"v": v, "via": sorted(n for n, _ in PARENTS.get((name, v), ()))[:5],
                                                  "yours": reaches(name, v)[:8]} for v in vs]})
    out.sort(key=lambda d: (-len(d["versions"]), d["name"]))
    return out


# --------------------------------------------------------------------------- measured releases (releases.json)
def measured(name):
    r = json.load(open(os.path.join(V4, "graph", "releases.json"))).get(name)
    if not r:
        return None
    pin = r["pinned"]
    out = {"pin": pin, "apiVersions": list(r["api"].keys()), "diffs": {}, "yourUses": len(r["uses"]),
           "apiSize": {v: len(items) for v, items in r["api"].items()}}
    for k, d in r["diff"].items():
        if d["from"] != pin:
            continue
        brk = sum(1 for c in d["changed"] if c.get("kind") == "breaking") if d["changed"] and isinstance(d["changed"][0], dict) else None
        hit = r["impact"].get(k, [])

        def bare(sig):
            sig = re.sub(r"'[A-Za-z_]+\s*,\s*|'[A-Za-z_]+|'_", "", sig or "")
            return re.sub(r"<\s*>", "", re.sub(r"\s+", "", sig))
        respelled = sorted({h["path"] for h in hit if bare(h["change"].get("before")) == bare(h["change"].get("after"))})
        out["diffs"][d["to"]] = {"added": len(d["added"]), "removed": len(d["removed"]), "changed": len(d["changed"]),
                                 "breaking": brk, "semverSlip": d["semverSlip"],
                                 "yourSitesTouched": len(hit), "yourItemsTouched": sorted({h["path"] for h in hit}),
                                 "yourItemsRespelled": respelled,
                                 "yourSitesChanged": sum(1 for h in hit if h["path"] not in respelled)}
    return out


# --------------------------------------------------------------------------- assemble
def crate_hero(name, pin):
    base = crate_package(name, pin)
    base["releases"] = releases_from_index(name)
    base["pin"] = pin
    base["alsoInTree"] = [{"v": v, "via": lock_parents(name, v), "yours": reaches(name, v)} for v in lock_versions(name) if v != pin]
    base["pinYours"] = reaches(name, pin)
    base["yourUsers"] = your_users(name)
    m = measured(name)
    if m:
        base["measured"] = m
        base["api"] = {"declarations": m["apiSize"].get(pin), "source": "releases.json api[%s]" % pin}
    else:
        base["api"] = None
    return base


def crate_modules(name, version):
    d = crate_dir(name, version)
    if not d:
        return []
    src = strip_comments(open(os.path.join(d, "src", "lib.rs"), errors="replace").read())
    mods = re.findall(r"^\s*pub\s+mod\s+(" + IDENT + r")\s*[;{]", src, re.M)
    for m in re.finditer(r"pub\s+use\s+[A-Za-z_:]+::\{([^}]*)\}", src):
        mods += [x.strip() for x in m.group(1).split(",") if x.strip() in ("de", "ser", "value", "map")]
    seen = []
    for x in mods:
        if x not in seen:
            seen.append(x)
    return seen


def dup_package(name):
    vs = lock_versions(name)
    return {"name": name, "eco": "crates", "releases": releases_from_index(name),
            "pins": [{"v": v, "via": lock_parents(name, v), "yours": reaches(name, v), "direct": [u for u in your_users(name)
                      if any(d == name or d == f"{name} {v}" for p in BY_NAME.get(u, []) for d in p["deps"])]} for v in vs]}


def main():
    data = {
        "generated": "build_data.py", "generatedAt": __import__("datetime").date.today().isoformat(),
        "you": {"project": "backend", "license": YOUR_LICENSE, "version": ROOT_TOML["workspace"]["package"]["version"],
                "publish": ROOT_TOML["workspace"]["package"].get("publish", True), "localPackages": len(LOCAL)},
        "packages": {},
    }
    data["packages"]["toml"] = crate_hero("toml", "0.8.23")
    data["packages"]["serde"] = crate_hero("serde", lock_versions("serde")[-1])
    data["packages"]["serde_json"] = crate_hero("serde_json", lock_versions("serde_json")[-1])
    sv = crate_hero("smallvec", "1.16.0")
    data["packages"]["smallvec"] = {k: sv[k] for k in ("name", "eco", "version", "license", "releases", "pin", "install", "measured")}
    data["packages"]["present"] = world_package("backend-present", "crates/present")
    w = json.load(open(os.path.join(V4, "graph", "world.json")))
    pi = next(i for i, p in enumerate(w["packages"]) if p["name"] == "backend-present")
    first = open(os.path.join(REPO, "crates/present/lib.rs")).readline()
    data["packages"]["present"]["description"] = first[3:].strip() if first.startswith("//!") else None
    data["packages"]["present"]["modules"] = [m["path"] for m in w["modules"] if m["pkg"] == pi and m["path"]][:24]
    for k, v in (("toml", "0.8.23"), ("serde", data["packages"]["serde"]["version"]), ("serde_json", data["packages"]["serde_json"]["version"])):
        data["packages"][k]["modules"] = crate_modules(k, v)
    data["packages"]["syn"] = dup_package("syn")
    data["packages"]["@types/react"] = npm_package("@types/react")
    data["packages"].update(other_ecosystems())
    data["licenses"] = license_examples()
    data["tree"] = {"licenses": tree_licenses(), "duplicates": duplicates()}
    for key in ("self_cell", "option-ext", "memchr", "unicode-ident", "cfg_block", "r-efi", "notify", "dwrote", "cbindgen", "tokio", "bytes"):
        vs = sorted([d.rsplit("-", 1)[1] for d in os.listdir(SRC) if d.rsplit("-", 1)[0] == key], key=semkey)
        if vs:
            man = crate_manifest(key, vs[-1])["package"]
            data["licenses"].setdefault("crates", 0)
            data["licenses"].setdefault("named", {})[key] = {"version": vs[-1], "license": man.get("license"),
                                                            "licenseFile": man.get("license-file"),
                                                            "inTree": in_tree(key)}
    js = json.dumps(data, indent=1, sort_keys=False)
    open(os.path.join(HERE, "data.json"), "w").write(js)
    open(os.path.join(HERE, "data.js"), "w").write("// generated by build_data.py — do not edit\nwindow.MARKS_DATA = " + js + ";\n")
    # summary
    for k, p in data["packages"].items():
        print(k, p.get("version"), p.get("license"), "releases:", len(p.get("releases") or []))
        for d in p.get("deps", []):
            print(f"   {d['kind']:6} {d['name']:16} req={d.get('req')} res={d.get('resolved')} opt={d.get('optional')} "
                  f"feats={d.get('features')} uses={d.get('uses')} :: {d.get('purpose')}  [{d.get('rule')}] {d.get('items')[:3]}")


if __name__ == "__main__":
    main()
