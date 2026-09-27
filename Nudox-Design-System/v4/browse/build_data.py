#!/usr/bin/env python3
"""D-Browse data: writes browse/data.json from what is actually on this machine.

    python3 build_data.py            # offline: registry index + unpacked sources + Cargo.lock + releases.json
    python3 build_data.py --net      # also refresh cache/net.json (crates.io downloads, RustSec advisory
                                     # listings via `gh api`), read-only GETs, 1 req/s

Sources, each recorded in data.json["sources"]:
  * ~/.cargo/registry/index/.../.cache   every version: deps, features, yanked, rust_version, pubtime
  * ~/.cargo/registry/src/...            unpacked crates: Cargo.toml metadata + the public API we scan
  * <workspace>/Cargo.lock + member Cargo.tomls   "your tree"
  * v4/graph/releases.json               measured API diffs (toml, smallvec) and your real uses
  * rust-src in /nix/store               a std subset, so "you need no package" can be a real answer
  * COUSINS below                        HAND-WRITTEN: other ecosystems are not indexed on this machine
Unknown stays unknown: a field we could not compute is null, never guessed.
"""
import collections
import datetime as dt
import glob
import json
import os
import re
import subprocess
import sys
import time
import tomllib

HERE = os.path.dirname(os.path.abspath(__file__))
V4 = os.path.dirname(HERE)
WS = os.path.abspath(os.path.join(V4, "..", ".."))
HOME = os.path.expanduser("~")
IDX = glob.glob(f"{HOME}/.cargo/registry/index/index.crates.io-*/.cache")[0]
SRC = glob.glob(f"{HOME}/.cargo/registry/src/index.crates.io-*")[0]
RUST_SRC = (sorted(glob.glob("/nix/store/*-rust-src-stable-*/lib/rustlib/src/rust/library")) or [None])[-1]
NOW = dt.datetime(2026, 9, 26, tzinfo=dt.timezone.utc)
NET = "--net" in sys.argv
NET_CACHE = os.path.join(HERE, "cache", "net.json")

# ----------------------------------------------------------------------------- candidates
# families: which crates are compared together, and which capabilities we look for in each.
FAMILIES = {
    "toml": ["toml", "toml_edit", "basic-toml"],
    "formats": ["serde_json", "ron", "serde_yaml", "ciborium", "rmp-serde", "bincode", "postcard", "quick-xml"],
    "cli": ["clap", "pico-args"],
    "regex": ["regex", "fancy-regex"],
    "http": ["reqwest", "ureq", "hyper", "attohttpc"],
    "async": ["tokio", "smol", "async-executor"],
    "errors": ["thiserror", "anyhow", "miette", "snafu", "displaydoc"],
    "inline": ["smallvec", "arrayvec", "tinyvec"],
    "parsers": ["winnow", "nom", "pest"],
}
CANDIDATES = [c for fam in FAMILIES.values() for c in fam]
FAMILY_OF = {c: f for f, cs in FAMILIES.items() for c in cs}

# ----------------------------------------------------------------------------- semver
SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?$")


def pv(v):
    m = SEMVER.match(v.strip())
    if not m:
        return None
    a, b, c, pre, _ = m.groups()
    return (int(a), int(b), int(c), pre or "")


def vkey(v):
    p = pv(v)
    if not p:
        return (0, 0, 0, 0, "")
    return (p[0], p[1], p[2], 1 if not p[3] else 0, p[3])


def bucket(v):
    p = pv(v)
    if not p:
        return v
    if p[0] > 0:
        return f"{p[0]}"
    if p[1] > 0:
        return f"0.{p[1]}"
    return f"0.0.{p[2]}"


def _cmp_one(op, want, have):
    w = [int(x) for x in re.findall(r"\d+", want.split("-")[0].split("+")[0])]
    n = len(w)
    w = (w + [0, 0, 0])[:3]
    h = pv(have)
    if not h:
        return False
    hv = h[:3]
    if op in ("^", ""):
        lo = tuple(w)
        if w[0] > 0 or n == 1:
            hi = (w[0] + 1, 0, 0)
        elif w[1] > 0 or n == 2:
            hi = (0, w[1] + 1, 0)
        else:
            hi = (0, 0, w[2] + 1)
        return lo <= hv < hi
    if op == "~":
        lo = tuple(w)
        hi = (w[0] + 1, 0, 0) if n == 1 else (w[0], w[1] + 1, 0)
        return lo <= hv < hi
    if op == "=":
        if n == 1:
            return hv[0] == w[0]
        if n == 2:
            return hv[:2] == tuple(w[:2])
        return hv == tuple(w)
    if op == ">=":
        return hv >= tuple(w)
    if op == ">":
        return hv > tuple(w)
    if op == "<":
        return hv < tuple(w)
    if op == "<=":
        return hv <= tuple(w)
    return False


def matches(req, ver):
    req = (req or "*").strip()
    if req in ("*", ""):
        return not pv(ver)[3] if pv(ver) else False
    p = pv(ver)
    if not p:
        return False
    for part in req.split(","):
        part = part.strip()
        m = re.match(r"^(\^|~|=|>=|<=|>|<)?\s*([\d.*x]+[^\s]*)$", part)
        if not m:
            return False
        op, want = m.group(1) or "", m.group(2)
        if "*" in want or "x" in want:
            nums = [x for x in re.findall(r"\d+", want)]
            want, op = ".".join(nums) or "0", "^" if nums else ">="
            if not nums:
                continue
        if not _cmp_one(op, want, ver):
            return False
    if p[3] and "-" not in req:
        return False
    return True


# ----------------------------------------------------------------------------- registry index
_INDEX = {}


def idx_path(name):
    n = name.lower()
    if len(n) == 1:
        return f"{IDX}/1/{n}"
    if len(n) == 2:
        return f"{IDX}/2/{n}"
    if len(n) == 3:
        return f"{IDX}/3/{n[0]}/{n}"
    return f"{IDX}/{n[:2]}/{n[2:4]}/{n}"


def index(name):
    if name in _INDEX:
        return _INDEX[name]
    p = idx_path(name)
    out = None
    if os.path.exists(p):
        b = open(p, "rb").read()
        i = b.index(b"\x00", 5)
        parts = b[i + 1:].split(b"\x00")
        out = []
        for j in range(0, len(parts) - 1, 2):
            if parts[j + 1]:
                try:
                    out.append(json.loads(parts[j + 1]))
                except json.JSONDecodeError:
                    pass
    _INDEX[name] = out
    return out


def best_version(name, req):
    recs = index(name)
    if not recs:
        return None
    ok = [r for r in recs if not r["yanked"] and matches(req, r["vers"])]
    if not ok:
        ok = [r for r in recs if matches(req, r["vers"])]
    return max(ok, key=lambda r: vkey(r["vers"])) if ok else None


def latest_stable(name):
    recs = index(name) or []
    ok = [r for r in recs if not r["yanked"] and pv(r["vers"]) and not pv(r["vers"])[3]]
    return max(ok, key=lambda r: vkey(r["vers"])) if ok else None


# ----------------------------------------------------------------------------- cfg evaluation (this machine: aarch64-apple-darwin)
def cfg_true(target):
    if not target:
        return True
    t = target.strip()
    if not t.startswith("cfg("):
        return "apple-darwin" in t
    expr = t[4:-1]

    def ev(e):
        e = e.strip()
        m = re.match(r"^(all|any|not)\((.*)\)$", e, re.S)
        if m:
            args, depth, cur = [], 0, ""
            for ch in m.group(2):
                if ch == "(":
                    depth += 1
                elif ch == ")":
                    depth -= 1
                if ch == "," and depth == 0:
                    args.append(cur)
                    cur = ""
                else:
                    cur += ch
            if cur.strip():
                args.append(cur)
            vals = [ev(a) for a in args]
            return all(vals) if m.group(1) == "all" else any(vals) if m.group(1) == "any" else not vals[0]
        if e == "unix":
            return True
        if e in ("windows", "miri", "test", "debug_assertions", "loom", "docsrs"):
            return False
        m = re.match(r'^(\w+)\s*=\s*"([^"]*)"$', e)
        if m:
            k, v = m.groups()
            return {"target_os": v == "macos", "target_family": v == "unix", "target_arch": v == "aarch64",
                    "target_vendor": v == "apple", "target_pointer_width": v == "64", "target_endian": v == "little",
                    "target_env": v == "", "panic": v == "unwind", "target_has_atomic": True}.get(k, False)
        return False
    try:
        return ev(expr)
    except Exception:
        return False


# ----------------------------------------------------------------------------- dependency closure with feature unification (lite)
def features_of(rec):
    f = dict(rec.get("features") or {})
    f.update(rec.get("features2") or {})
    return f


def closure(root, req="*", feats=(), default=True, kinds=("normal", "build")):
    """Resolve root and everything it pulls in on this machine. Returns {(name,bucket): version} and unknown names."""
    state = {}   # (name,bucket) -> {"rec":rec, "feats":set, "default":bool}
    unknown = set()
    work = [(root, req, set(feats), default)]
    while work:
        name, rq, fs, dflt = work.pop()
        rec = best_version(name, rq)
        if rec is None:
            unknown.add(name)
            continue
        key = (name, bucket(rec["vers"]))
        st = state.get(key)
        changed = False
        if st is None:
            st = state[key] = {"rec": rec, "feats": set(), "default": False}
            changed = True
        if dflt and not st["default"]:
            st["default"] = True
            changed = True
        if not fs <= st["feats"]:
            st["feats"] |= fs
            changed = True
        if not changed:
            continue
        table = features_of(rec)
        on = set(st["feats"]) | ({"default"} if st["default"] and "default" in table else set())
        # expand features
        dep_on, dep_feats = set(), collections.defaultdict(set)
        opt_names = {d["name"] for d in rec["deps"] if d.get("optional")}
        explicit_dep = {x[4:] for vals in table.values() for x in vals if x.startswith("dep:")}
        stack, seen = list(on), set()
        while stack:
            f = stack.pop()
            if f in seen:
                continue
            seen.add(f)
            if f in opt_names and f not in explicit_dep and f not in table:
                dep_on.add(f)
            for x in table.get(f, []):
                if x.startswith("dep:"):
                    dep_on.add(x[4:])
                elif "/" in x:
                    d, sub = x.split("/", 1)
                    weak = d.endswith("?")
                    d = d.rstrip("?")
                    if not weak:
                        dep_on.add(d)
                    dep_feats[d].add(sub)
                else:
                    if x in opt_names and x not in explicit_dep and x not in table:
                        dep_on.add(x)
                    stack.append(x)
        for d in rec["deps"]:
            if d.get("kind", "normal") not in kinds or not cfg_true(d.get("target")):
                continue
            local = d["name"]
            real = d.get("package") or d["name"]
            if d.get("optional") and local not in dep_on:
                continue
            work.append((real, d["req"], set(d.get("features") or []) | dep_feats.get(local, set()),
                         d.get("default_features", True)))
    return {k: v["rec"]["vers"] for k, v in state.items()}, unknown


# ----------------------------------------------------------------------------- your tree
def load_tree():
    lock = tomllib.load(open(os.path.join(WS, "Cargo.lock"), "rb"))
    root = tomllib.load(open(os.path.join(WS, "Cargo.toml"), "rb"))
    wsdeps = root["workspace"]["dependencies"]
    members = []
    for pat in root["workspace"]["members"]:
        for d in sorted(glob.glob(os.path.join(WS, pat))):
            f = os.path.join(d, "Cargo.toml")
            if os.path.exists(f):
                members.append((os.path.relpath(d, WS), tomllib.load(open(f, "rb"))))
    member_names = {t["package"]["name"] for _, t in members if "package" in t}
    pk = lock["package"]
    ext = [p for p in pk if "source" in p and p["name"] not in member_names]
    in_lock = collections.defaultdict(list)
    for p in ext:
        in_lock[p["name"]].append(p["version"])
    lock_keys = {(p["name"], bucket(p["version"])) for p in ext}
    # lock graph
    by_name = collections.defaultdict(list)
    for p in pk:
        by_name[p["name"]].append(p)
    graph = {}
    for p in pk:
        outs = []
        for d in p.get("dependencies", []):
            parts = d.split(" ")
            cands = by_name.get(parts[0], [])
            if len(parts) > 1:
                cands = [c for c in cands if c["version"] == parts[1]] or cands
            if cands:
                outs.append((cands[0]["name"], cands[0]["version"]))
        graph[(p["name"], p["version"])] = outs
    # direct deps of members
    direct = {}
    for path, t in members:
        if "package" not in t:
            continue
        mname = t["package"]["name"].replace("backend-", "")
        secs = [("normal", t.get("dependencies", {})), ("dev", t.get("dev-dependencies", {})),
                ("build", t.get("build-dependencies", {}))]
        for tg in (t.get("target") or {}).values():
            secs += [("normal", tg.get("dependencies", {})), ("dev", tg.get("dev-dependencies", {})),
                     ("build", tg.get("build-dependencies", {}))]
        for kind, deps in secs:
            for key, spec in deps.items():
                if isinstance(spec, dict) and "path" in spec:
                    continue
                real = key
                if isinstance(spec, dict) and spec.get("workspace"):
                    w = wsdeps.get(key)
                    if isinstance(w, dict) and "path" in w:
                        continue
                    if isinstance(w, dict):
                        real = w.get("package", key)
                elif isinstance(spec, dict):
                    real = spec.get("package", key)
                if real in member_names:
                    continue
                e = direct.setdefault(real, {"name": real, "ident": key.replace("-", "_"), "members": {}, "kinds": set()})
                e["members"].setdefault(mname, set()).add(kind)
                e["kinds"].add(kind)
    your_pins = collections.defaultdict(set)
    for p in pk:
        if p["name"] not in member_names:
            continue
        for d in p.get("dependencies", []):
            parts = d.split(" ")
            vs = [c["version"] for c in by_name.get(parts[0], []) if "source" in c]
            if len(parts) > 1:
                your_pins[parts[0]].add(parts[1])
            elif len(vs) == 1:
                your_pins[parts[0]].add(vs[0])
    return {"your_pins": {k: sorted(v, key=vkey) for k, v in your_pins.items()}, "lock": lock, "members": members, "member_names": member_names, "ext": ext, "in_lock": dict(in_lock),
            "lock_keys": lock_keys, "graph": graph, "direct": direct, "license": root["workspace"]["package"].get("license")}


def uses_by_member(uses, tree):
    roots = sorted(((path, t["package"]["name"].replace("backend-", "")) for path, t in tree["members"] if "package" in t), key=lambda x: -len(x[0]))
    c = collections.Counter()
    for u in uses:
        m = next((nm for path, nm in roots if u["file"].startswith(path + "/")), None)
        if m:
            c[m] += 1
    return dict(c.most_common())


def count_uses(tree):
    """How often your code names each direct dependency (by text: `ident::` and `use ident`)."""
    counts = collections.defaultdict(lambda: collections.Counter())
    files_by = collections.defaultdict(set)
    idents = {e["ident"]: n for n, e in tree["direct"].items()}
    pat = re.compile(r"\b(" + "|".join(sorted(map(re.escape, idents), key=len, reverse=True)) + r")::")
    for path, t in tree["members"]:
        if "package" not in t:
            continue
        m = t["package"]["name"].replace("backend-", "")
        for f in glob.glob(os.path.join(WS, path, "**", "*.rs"), recursive=True):
            if "/target/" in f:
                continue
            try:
                txt = open(f, encoding="utf-8", errors="ignore").read()
            except OSError:
                continue
            code = "\n".join(clean_code(l) for l in txt.split("\n") if not l.lstrip().startswith("//"))
            for mm in pat.finditer(code):
                n = idents[mm.group(1)]
                counts[n][m] += 1
                files_by[n].add(os.path.relpath(f, WS))
    return counts, files_by


# ----------------------------------------------------------------------------- roles (derived, with the reason recorded)
ROLE_ORDER = [
    ("tests", "checks our work"),
    ("window", "draws the window"),
    ("languages", "reads the seven languages"),
    ("formats", "speaks formats"),
    ("store", "keeps and finds"),
    ("concurrency", "runs things at once"),
    ("network", "talks to the network"),
    ("hashing", "fingerprints and packs"),
    ("errors", "names what went wrong"),
    ("observe", "watches itself run"),
    ("memory", "shapes memory"),
    ("os", "talks to the system"),
    ("other", "other"),
]
CAT_ROLE = [
    (("gui", "graphics", "rendering", "multimedia::images", "multimedia"), "window"),
    (("encoding", "parser-implementations", "config"), "formats"),
    (("compression",), "hashing"),
    (("cryptography", "cryptography::cryptocurrencies"), "hashing"),
    (("database", "database-implementations", "text-processing", "text-search", "caching"), "store"),
    (("asynchronous", "concurrency"), "concurrency"),
    (("network-programming", "web-programming", "web-programming::http-client", "web-programming::http-server", "web-programming::websocket"), "network"),
    (("development-tools::debugging", "development-tools::profiling"), "observe"),
    (("data-structures", "memory-management", "no-std", "no-std::no-alloc"), "memory"),
    (("os", "os::unix-apis", "os::macos-apis", "os::windows-apis", "filesystem", "api-bindings", "hardware-support"), "os"),
]
KW_ROLE = [(("error", "error-handling"), "errors"), (("tracing", "logging", "telemetry", "metrics"), "observe"),
           (("hash", "sha2", "sha1", "blake3", "digest"), "hashing"), (("serde", "serialization", "json", "toml", "xml", "base64", "zip"), "formats")]


def meta_for(name, version=None):
    """Cargo.toml [package] of an unpacked crate: the locked version if unpacked, else the newest unpacked."""
    dirs = glob.glob(os.path.join(SRC, f"{name}-[0-9]*"))
    dirs = [d for d in dirs if re.match(re.escape(name) + r"-\d", os.path.basename(d))]
    if not dirs:
        return None, None
    pick = None
    if version:
        pick = next((d for d in dirs if os.path.basename(d) == f"{name}-{version}"), None)
    if not pick:
        pick = max(dirs, key=lambda d: vkey(os.path.basename(d)[len(name) + 1:]))
    try:
        t = tomllib.load(open(os.path.join(pick, "Cargo.toml"), "rb"))
    except Exception:
        return pick, None
    return pick, t.get("package", {})


ROLE_RULES = [  # checked in this order; the first rule with evidence wins, and the evidence is recorded
    ("errors", None, {"error", "error-handling"}, None),
    ("observe", {"development-tools::debugging", "development-tools::profiling"}, {"tracing", "logging", "telemetry", "metrics"}, None),
    ("hashing", {"cryptography", "compression"}, {"hash", "digest", "crypto", "zip", "compression", "sha2", "blake3"}, r"\bhash(ing)? function\b|\bcompression\b"),
    ("store", {"database", "database-implementations", "text-search", "caching"}, {"database", "sql", "sqlite", "search"}, r"\bSQLite\b|\bdatabase\b|\bsearch engine\b"),
    ("network", {"web-programming", "web-programming::http-client", "web-programming::http-server", "web-programming::websocket"}, {"http", "https"}, None),
    ("os", {"os", "os::unix-apis", "os::macos-apis", "os::windows-apis", "filesystem"}, {"syscall", "gitignore", "mmap", "memory-map"}, None),
    ("concurrency", {"asynchronous", "concurrency"}, {"async", "futures", "atomic", "lock-free"}, r"\bfutures\b|\basync\b"),
    ("network", {"network-programming"}, {"socket", "network"}, None),
    ("formats", {"encoding", "parser-implementations", "config", "parsing"}, {"serde", "serialization", "json", "toml", "xml", "base64"}, None),
    ("memory", {"data-structures", "memory-management"}, {"vec", "vector", "stack", "allocator"}, None),
]


def derive_role(name, e, meta):
    members = e["members"]
    who = sorted(members)
    cats = [c.lower() for c in (meta or {}).get("categories", []) or []]
    kws = [k.lower() for k in (meta or {}).get("keywords", []) or []]
    desc = (meta or {}).get("description") or ""
    only_dev = all(k == {"dev"} for k in members.values())
    frontend_only = all(m.startswith("frontend-") or m == "compile" for m in who)
    window_only = all(m in ("desktop", "facet", "gui-harness") for m in who)
    by = " · used by " + ", ".join(who)
    if only_dev:
        return "tests", "only a dev-dependency (of " + ", ".join(who) + ")"
    if frontend_only and (name.startswith(("tree-sitter", "oxc_", "ruff_", "ra_ap_", "clang")) or any(c.startswith(("parsing", "parser", "development-tools")) for c in cats)):
        return "languages", "used only by the language frontends (" + ", ".join(who) + ")"
    if window_only and (name.startswith("gpui") or any(c in ("gui", "graphics", "rendering", "multimedia::images", "multimedia") for c in cats)):
        return "window", "used only by " + ", ".join(who) + (f" · category {cats[0]}" if cats else "")
    votes = collections.OrderedDict()
    for role, rc, rk, rd in ROLE_RULES:
        ev = [c for c in cats if rc and c in rc] + [k for k in kws if rk and k in rk]
        if ev:
            votes.setdefault(role, []).extend(x for x in ev if x not in votes.get(role, []))
    if votes:
        role = max(votes, key=lambda r: (len(votes[r]), -list(votes).index(r)))
        return role, " · ".join(votes[role][:3]) + by
    for role, rc, rk, rd in ROLE_RULES:
        if rd and re.search(rd, desc):
            return role, "its description: “" + re.sub(r"\s+", " ", desc)[:60].strip() + "”" + by
    return "other", ("no category, keyword or telling description on disk" if meta else "not unpacked on this machine") + by


# ----------------------------------------------------------------------------- API scan
ITEM_RE = re.compile(r"^(?P<vis>pub(?:\s*\([^)]*\))?\s+)?(?P<mods>(?:(?:const|async|unsafe|default|extern\s+\"[^\"]*\")\s+)*)"
                     r"(?P<kw>fn|struct|enum|trait|type|const|static|mod|union|impl|use|macro_rules!)(?:\b|(?<=!))\s*(?P<name>[A-Za-z_][A-Za-z0-9_]*)?")
MACRO_BLOCK = re.compile(r"^[a-z_][a-z0-9_]*!\s*[\{\(]\s*$")
STR_RE = re.compile(r'b?"(?:\\.|[^"\\])*"')
CHR_RE = re.compile(r"'(?:\\.|[^'\\])'")


def clean_code(line):
    s = STR_RE.sub('""', line)
    s = CHR_RE.sub("''", s)
    # drop trailing // comment
    i = s.find("//")
    if i >= 0:
        s = s[:i]
    return s


def first_sentence(doc):
    doc = " ".join(doc).strip()
    doc = re.sub(r"\[([^\]]+)\]\([^)]*\)", r"\1", doc)
    doc = re.sub(r"\[`([^`]+)`\](\[[^\]]*\])?", r"`\1`", doc)
    doc = re.sub(r"\[([^\]]+)\]\[[^\]]*\]", r"\1", doc)
    doc = re.sub(r"\s+", " ", doc)
    m = re.match(r"(.+?[.!?])(\s|$)", doc)
    s = m.group(1) if m else doc
    return s[:180]


def split_top(s, sep=","):
    out, depth, cur = [], 0, ""
    for ch in s:
        if ch in "<([{":
            depth += 1
        elif ch in ">)]}":
            depth -= 1
        if ch == sep and depth == 0:
            out.append(cur)
            cur = ""
        else:
            cur += ch
    if cur.strip():
        out.append(cur)
    return [x.strip() for x in out]


class Scan:
    def __init__(self, crate, root_dir):
        self.crate = crate.replace("-", "_")
        self.root = root_dir
        self.items = []          # dicts: mod, name, k, sig, doc, owner, dep
        self.mods = {"": True}   # module path -> public?
        self.uses = []           # (mod, source path str, name or '*', alias)
        self.impls = []          # (mod, type, trait)
        self.no_std = False
        self.forbid_unsafe = False
        self.crate_doc = []

    def scan_file(self, path, mod, public=True):
        try:
            lines = open(path, encoding="utf-8", errors="ignore").read().split("\n")
        except OSError:
            return
        self.mods[mod] = public
        base_dir = os.path.dirname(path)
        stem = os.path.basename(path)[:-3]
        child_dir = base_dir if stem in ("lib", "mod", "main") else os.path.join(base_dir, stem)
        # context stack: (kind, data)  kind in mod|impl|enum|trait|skip|body|macro
        stack = [("mod", (mod, public))]
        docs, attrs, pending, header, header_lines = [], [], None, None, 0
        in_block_comment = False
        for raw in lines:
            line = raw.rstrip()
            st = line.strip()
            if in_block_comment:
                if "*/" in st:
                    in_block_comment = False
                continue
            if st.startswith("/*") and not st.startswith("/**"):
                if "*/" not in st:
                    in_block_comment = True
                continue
            if st.startswith("//!"):
                if mod == "" and len(stack) == 1:
                    self.crate_doc.append(st[3:].strip())
                continue
            if st.startswith("///"):
                docs.append(st[3:].strip())
                continue
            if st.startswith("//") or not st:
                if not st:
                    pass
                continue
            if mod == "" and len(stack) == 1:
                if re.match(r"#!\[\s*no_std", st) or re.search(r"cfg_attr\([^]]*no_std", st):
                    self.no_std = True
                if re.match(r"#!\[\s*forbid\([^)]*unsafe_code", st):
                    self.forbid_unsafe = True
            top = stack[-1]
            live = all(k in ("mod", "impl", "macro", "enum", "trait") for k, _ in stack) and top[0] != "skip"
            code = clean_code(line)
            cst = code.strip()
            if header is not None:
                header += " " + cst
                header_lines += 1
                done = self._header_end(header)
                if done is None:
                    if header_lines > 60:   # not a header after all (macro soup); drop it
                        header, docs, attrs = None, [], []
                    continue
                cst = header
                header = None
                self._emit(cst, docs, attrs, stack, child_dir, live)
                opened = self._push_for(cst, attrs, stack)
                docs, attrs = [], []
                self._count(cst, stack, opened)
                continue
            if st.startswith("#[") or st.startswith("#!["):
                attrs.append(st)
                self._count(cst, stack, None)
                continue
            if top[0] == "enum" and live:
                m = re.match(r"^([A-Z][A-Za-z0-9_]*)\s*(\(|\{|,|=|$)", cst)
                if m and not any("doc(hidden)" in a for a in attrs):
                    ename = top[1]
                    self.items.append({"mod": stack[-2][1][0] if stack[-2][0] == "mod" else top[1], "name": m.group(1), "k": "variant",
                                       "owner": ename, "sig": cst.rstrip(",{").strip()[:160], "doc": first_sentence(docs),
                                       "emod": self._cur_mod(stack)})
                docs, attrs = [], []
                self._count(cst, stack, None)
                continue
            m = ITEM_RE.match(cst)
            if m and top[0] in ("mod", "impl", "macro", "trait"):
                if self._header_end(cst) is None and m.group("kw") in ("fn", "struct", "enum", "trait", "impl", "union", "type", "const", "static", "use"):
                    header = cst
                    header_lines = 1
                    continue
                self._emit(cst, docs, attrs, stack, child_dir, live)
                opened = self._push_for(cst, attrs, stack)
                docs, attrs = [], []
                self._count(cst, stack, opened)
                continue
            if top[0] in ("mod",) and MACRO_BLOCK.match(cst):
                docs, attrs = [], []
                stack.append(("macro", None)) if cst.endswith("{") else None
                if cst.endswith("("):
                    stack.append(("macro", None))
                continue
            docs, attrs = [], []
            self._count(cst, stack, None)

    def _cur_mod(self, stack):
        for k, d in reversed(stack):
            if k == "mod":
                return d[0]
        return ""

    def _header_end(self, h):
        m = ITEM_RE.match(h.strip())
        kw = m.group("kw") if m else None
        depth = 0
        if kw in ("use", "const", "static"):
            for i, ch in enumerate(h):
                if ch in "([{":
                    depth += 1
                elif ch in ")]}":
                    depth -= 1
                elif ch == ";" and depth <= 0:
                    return i
            return None
        for i, ch in enumerate(h):
            if ch in "(<[":
                depth += 1
            elif ch in ")>]":
                depth -= 1 if not (ch == ">" and i > 0 and h[i - 1] in "-=") else 0
            elif ch in "{;" and depth <= 0:
                return i
        return None

    def _count(self, code, stack, opened):
        """Apply brace deltas. `opened` is a context pushed for this line's first brace (already on stack)."""
        s = code
        if opened is not None:
            i = s.find("{")
            s = s[i + 1:] if i >= 0 else ""
        for ch in s:
            if ch == "{":
                stack.append(("body", None))
            elif ch == "}":
                if len(stack) > 1:
                    stack.pop()
        if code.strip().endswith(")") and stack and stack[-1][0] == "macro" and code.strip() in (")", ");"):
            stack.pop()
        if code.strip() in (");",) and stack and stack[-1][0] == "macro":
            stack.pop()

    def _push_for(self, h, attrs, stack):
        end = self._header_end(h)
        if end is None or h[end] != "{":
            return None
        m = ITEM_RE.match(h)
        kw = m.group("kw") if m else None
        hidden = any("doc(hidden)" in a for a in attrs) or any(re.search(r"cfg\(\s*(all\(\s*)?(test|doctest|docsrs)\b", a) for a in attrs)
        if hidden:
            stack.append(("skip", None))
        elif kw == "mod":
            parent = self._cur_mod(stack)
            pub = bool(m.group("vis")) and m.group("vis").strip() == "pub" and self.mods.get(parent, True)
            path = (parent + "::" if parent else "") + m.group("name")
            self.mods[path] = pub
            stack.append(("mod", (path, pub)))
        elif kw == "impl":
            ty, tr = parse_impl(h[:end])
            self.impls.append((self._cur_mod(stack), ty, tr))
            stack.append(("impl", (ty, tr)))
        elif kw == "enum":
            stack.append(("enum", m.group("name")))
        elif kw == "trait":
            stack.append(("trait", m.group("name")))
        else:
            stack.append(("body", None))
        return True

    def _emit(self, h, docs, attrs, stack, child_dir, live):
        m = ITEM_RE.match(h)
        if not m:
            return
        kw, name, vis = m.group("kw"), m.group("name"), (m.group("vis") or "").strip()
        hidden = any("doc(hidden)" in a for a in attrs) or any(re.search(r"cfg\(\s*(all\(\s*)?(test|doctest|docsrs)\b", a) for a in attrs)
        cur = self._cur_mod(stack)
        top = stack[-1]
        if kw == "mod" and name and self._header_end(h) is not None and h[self._header_end(h)] == ";":
            if hidden:
                return
            pathattr = next((re.search(r'path\s*=\s*"([^"]+)"', a).group(1) for a in attrs if re.search(r'path\s*=\s*"', a)), None)
            cands = [os.path.join(child_dir, pathattr)] if pathattr else [os.path.join(child_dir, name + ".rs"), os.path.join(child_dir, name, "mod.rs")]
            sub = (cur + "::" if cur else "") + name
            pub = vis == "pub" and self.mods.get(cur, True) and live
            for c in cands:
                if os.path.exists(c):
                    if sub not in self.mods or pub:
                        self.scan_file(c, sub, pub)
                    break
            return
        if not live or hidden:
            return
        if kw == "use":
            if vis == "pub":
                end = self._header_end(h)
                body = h[m.end("kw"):end] if end is not None else h[m.end("kw"):]
                for src, nm, alias in expand_use(body.strip()):
                    self.uses.append((cur, src, nm, alias))
            return
        if kw == "macro_rules!":
            if any("macro_export" in a for a in attrs):
                self.items.append({"mod": "", "name": name, "k": "macro", "sig": f"{name}!(…)", "doc": first_sentence(docs), "owner": None})
            return
        if kw == "impl":
            return
        is_pub = vis == "pub" or (top[0] == "trait")
        if top[0] == "impl":
            ty, tr = top[1]
            if tr is not None or vis != "pub" or kw != "fn":
                return
            sig = norm_sig(h)
            self.items.append({"mod": cur, "name": name, "k": "method", "owner": ty, "sig": sig, "doc": first_sentence(docs),
                               "dep": any("deprecated" in a for a in attrs)})
            return
        if top[0] == "trait":
            if kw == "fn":
                self.items.append({"mod": cur, "name": name, "k": "method", "owner": top[1], "sig": norm_sig(h), "doc": first_sentence(docs),
                                   "trait_item": True})
            return
        if not is_pub or not name:
            return
        k = {"fn": "function", "struct": "struct", "enum": "enum", "trait": "trait", "type": "type", "const": "constant",
             "static": "static", "union": "struct"}.get(kw)
        if not k:
            return
        self.items.append({"mod": cur, "name": name, "k": k, "sig": norm_sig(h), "doc": first_sentence(docs), "owner": None,
                           "dep": any("deprecated" in a for a in attrs)})


def parse_impl(h):
    s = h.strip()[4:].strip()
    if s.startswith("<"):
        depth = 0
        for i, ch in enumerate(s):
            if ch == "<":
                depth += 1
            elif ch == ">":
                depth -= 1
                if depth == 0:
                    s = s[i + 1:].strip()
                    break
    s = re.split(r"\bwhere\b", s)[0].strip()
    parts = re.split(r"\s+for\s+", s, maxsplit=1)
    tr, ty = (parts[0], parts[1]) if len(parts) == 2 else (None, parts[0])
    def last(x):
        x = re.sub(r"<.*", "", x.strip().lstrip("&").replace("mut ", "").replace("dyn ", ""))
        return x.split("::")[-1].strip()
    return last(ty), (last(tr) if tr else None)


def expand_use(body):
    body = body.strip().rstrip(";")
    out = []

    def rec(prefix, s):
        s = s.strip()
        if "{" in s and s.endswith("}"):
            i = s.index("{")
            pre = s[:i].rstrip(":").strip()
            full = (prefix + "::" + pre if prefix and pre else prefix or pre)
            for part in split_top(s[i + 1:-1]):
                rec(full, part)
            return
        m = re.match(r"^(.*?)(?:\s+as\s+(\w+))?$", s)
        path, alias = m.group(1).strip(), m.group(2)
        full = prefix + "::" + path if prefix else path
        segs = full.split("::")
        if segs[-1] == "self":
            segs = segs[:-1]
            out.append(("::".join(segs[:-1]), segs[-1], alias))
            return
        out.append(("::".join(segs[:-1]), segs[-1], alias))
    rec("", body)
    return out


def norm_sig(h):
    s = re.sub(r"\s+", " ", h).strip()
    end = None
    depth = 0
    for i, ch in enumerate(s):
        if ch in "(<[":
            depth += 1
        elif ch in ")>]":
            if not (ch == ">" and i > 0 and s[i - 1] == "-"):
                depth -= 1
        elif ch in "{;" and depth <= 0:
            end = i
            break
    if end is not None:
        s = s[:end].strip()
    s = re.sub(r"^pub(\([^)]*\))?\s+", "", s)
    s = s.replace("( ", "(").replace(" )", ")").replace(" ,", ",").replace("< ", "<").replace(" >", ">")
    return s[:260]


def resolve_mod(cur, path):
    segs = path.split("::") if path else []
    if not segs:
        return cur, False
    if segs[0] == "crate":
        return "::".join(segs[1:]), False
    if segs[0] == "self":
        base = cur
        segs = segs[1:]
    elif segs[0] == "super":
        base = cur
        while segs and segs[0] == "super":
            base = "::".join(base.split("::")[:-1]) if base else ""
            segs = segs[1:]
    else:
        return path, True  # external crate or a child module (decided by caller)
    return "::".join(([base] if base else []) + segs), False


def scan_crate(name, version=None, depth=0):
    d, meta = meta_for(name, version)
    if not d:
        return None
    lib = os.path.join(d, "src", "lib.rs")
    if meta and isinstance(meta.get("lib"), dict) and meta["lib"].get("path"):
        lib = os.path.join(d, meta["lib"]["path"])
    t = None
    try:
        t = tomllib.load(open(os.path.join(d, "Cargo.toml"), "rb"))
        if t.get("lib", {}).get("path"):
            lib = os.path.join(d, t["lib"]["path"])
    except Exception:
        pass
    sc = Scan(name, d)
    if os.path.exists(lib):
        sc.scan_file(lib, "", True)
    sc.dir = d
    sc.meta = meta
    sc.toml = t
    sc.proc = []
    # proc-macro items, when this *is* a proc-macro crate
    if t and t.get("lib", {}).get("proc-macro"):
        txt = open(lib, encoding="utf-8", errors="ignore").read()
        for mm in re.finditer(r"((?:\s*///[^\n]*\n)*)\s*#\[proc_macro_derive\(\s*(\w+)", txt):
            docs = [x.strip()[3:].strip() for x in mm.group(1).strip().split("\n") if x.strip().startswith("///")]
            sc.proc.append({"name": mm.group(2), "k": "derive", "sig": f"#[derive({mm.group(2)})]", "doc": first_sentence(docs)})
        for mm in re.finditer(r"((?:\s*///[^\n]*\n)*)\s*#\[proc_macro_attribute\]\s*pub fn (\w+)", txt):
            docs = [x.strip()[3:].strip() for x in mm.group(1).strip().split("\n") if x.strip().startswith("///")]
            sc.proc.append({"name": mm.group(2), "k": "attr", "sig": f"#[{mm.group(2)}]", "doc": first_sentence(docs)})
    return sc


_API = {}


def public_api(name, version=None, cap=1400):
    key = (name, version)
    if key not in _API:
        _API[key] = None
        _API[key] = _public_api(name, version, cap)
    return _API[key]


def _public_api(name, version=None, cap=1400):
    """The crate's public items with their shortest public path. Follows pub use, globs, and re-exported crates."""
    sc = scan_crate(name, version)
    if sc is None:
        return None
    if sc.toml and (sc.toml.get("lib") or {}).get("proc-macro"):
        sc.items = []
    crate = name.replace("-", "_")
    pub_mods = {m for m, p in sc.mods.items() if p}
    items = [dict(x) for x in sc.items]
    defined = collections.defaultdict(list)
    for it in items:
        defined[(it.get("emod", it["mod"]) if it["k"] == "variant" else it["mod"], it["name"] if it["k"] != "method" else None)].append(it)
    paths = collections.defaultdict(set)   # id(item) -> display paths
    by_id = {id(it): it for it in items}
    # direct public paths
    for it in items:
        if it["k"] in ("method", "variant"):
            continue
        if it["mod"] in pub_mods:
            paths[id(it)].add((crate + "::" + it["mod"] + "::" + it["name"]).replace("::::", "::") if it["mod"] else crate + "::" + it["name"])
    extern_reexports = []
    # re-exports (repeat to follow chains)
    deps_of = {}
    if sc.toml:
        for sec in ("dependencies",):
            for k, v in (sc.toml.get(sec) or {}).items():
                real = v.get("package", k) if isinstance(v, dict) else k
                req = v.get("version", "*") if isinstance(v, dict) else v
                deps_of[k.replace("-", "_")] = (real, req)
    for _ in range(4):
        for cur, src, nm, alias in sc.uses:
            if cur not in pub_mods:
                continue
            target_mod, external = resolve_mod(cur, src)
            first = src.split("::")[0] if src else nm
            if external and first in deps_of:
                extern_reexports.append((cur, src, nm, alias))
                continue
            if external:
                # child module of cur?
                target_mod = (cur + "::" + src) if cur else src
                if target_mod not in sc.mods and src in sc.mods:
                    target_mod = src
            here = crate + ("::" + cur if cur else "")
            if nm == "*":
                for it in items:
                    if it["k"] in ("method", "variant"):
                        continue
                    if it["mod"] == target_mod:
                        paths[id(it)].add(here + "::" + it["name"])
            else:
                for it in items:
                    if it["k"] in ("method", "variant"):
                        continue
                    if it["name"] == nm and (it["mod"] == target_mod or not target_mod):
                        paths[id(it)].add(here + "::" + (alias or nm))
                # re-exported module
                sub = (target_mod + "::" + nm) if target_mod else nm
                if sub in sc.mods:
                    for it in items:
                        if it["k"] in ("method", "variant"):
                            continue
                        if it["mod"] == sub:
                            paths[id(it)].add(here + "::" + (alias or nm) + "::" + it["name"])
    out = []
    type_path = {}
    for it in items:
        if it["k"] in ("method", "variant"):
            continue
        ps = paths.get(id(it))
        if not ps:
            continue
        p = min(ps, key=lambda x: (x.count("::"), len(x)))
        it["path"] = p
        out.append(it)
        if it["k"] in ("struct", "enum", "trait", "type"):
            type_path.setdefault(it["name"], p)
    for it in items:
        if it["k"] in ("method", "variant"):
            owner = it.get("owner")
            if owner in type_path:
                it["path"] = type_path[owner] + "::" + it["name"]
                out.append(it)
    for it in items:
        if it["k"] == "macro":
            it["path"] = crate + "::" + it["name"]
            if it not in out:
                out.append(it)
    # re-exported crates (clap -> clap_builder, thiserror -> thiserror_impl, smol -> async_*)
    for cur, src, nm, alias in extern_reexports:
        first = (src or nm).split("::")[0]
        real, req = deps_of[first]
        rec = best_version(real, req if isinstance(req, str) else "*")
        sub = public_api(real, rec["vers"] if rec else None, cap=cap) if real != name else None
        if not sub:
            continue
        here = crate + ("::" + cur if cur else "")
        inner = ((src + "::" + nm) if src else nm).split("::")[1:]
        if nm == "*":
            prefix = "::".join(inner[:-1])
            for it in sub["items"]:
                ip = it["path"].split("::", 1)[1] if "::" in it["path"] else ""
                if prefix and not ip.startswith(prefix + "::"):
                    continue
                q = dict(it)
                q["path"] = here + "::" + (ip[len(prefix) + 2:] if prefix else ip)
                q["via"] = real
                out.append(q)
            for p in sub.get("proc", []):
                q = dict(p)
                q["path"] = here + "::" + p["name"]
                q["via"] = real
                out.append(q)
            continue
        target = "::".join(inner)
        name_out = alias or nm
        for it in sub["items"]:
            ip = it["path"].split("::", 1)[1] if "::" in it["path"] else ""
            if target == "":
                np = here + "::" + name_out + "::" + ip
            elif ip == target:
                np = here + "::" + name_out
            elif ip.startswith(target + "::"):
                np = here + "::" + name_out + ip[len(target):]
            else:
                continue
            q = dict(it)
            q["path"] = np
            q["via"] = real
            out.append(q)
        for p in sub.get("proc", []):
            if p["name"] == target:
                q = dict(p)
                q["path"] = here + "::" + name_out
                q["via"] = real
                out.append(q)
    # dedupe by path
    seen, final = set(), []
    for it in out:
        key = (it["path"], it["k"])
        if key in seen:
            continue
        seen.add(key)
        final.append(it)
    if sc.toml and (sc.toml.get("lib") or {}).get("proc-macro") and not final:
        for pm in sc.proc:
            q = dict(pm)
            q["path"] = crate + "::" + pm["name"]
            final.append(q)
    final.sort(key=lambda x: (x["path"].count("::"), 0 if x["k"] in ("function", "struct", "enum", "trait", "derive", "macro") else 1, x["path"]))
    return {"crate": crate, "items": final[:cap], "total": len(final), "no_std": sc.no_std, "forbid_unsafe": sc.forbid_unsafe,
            "impls": sc.impls, "proc": sc.proc, "dir": sc.dir, "crate_doc": first_sentence(sc.crate_doc[:6]) if sc.crate_doc else ""}


# ----------------------------------------------------------------------------- shapes: a signature in words
TEXTY = re.compile(r"^&?('\w+\s+)?(mut\s+)?(str|String|Cow<[^>]*str>|Box<str>|Arc<str>|&str)$")


def generic_bounds(sig):
    b = {}
    m = re.search(r"fn\s+\w+\s*<(.*?)>\s*\(", sig)
    parts = split_top(m.group(1)) if m else []
    w = re.search(r"\bwhere\b(.*)$", sig)
    if w:
        parts += split_top(w.group(1))
    for p in parts:
        if ":" in p and not p.strip().startswith("'"):
            n, bound = p.split(":", 1)
            b.setdefault(n.strip(), []).append(bound.strip())
        elif p.strip() and not p.strip().startswith("'") and not p.strip().startswith("const"):
            b.setdefault(p.strip(), [])
    return b


def word_for(ty, bounds, owner=None):
    t = ty.strip()
    t = re.sub(r"^&\s*('\w+\s+)?", "", t)
    t = re.sub(r"^mut\s+", "", t)
    if t.startswith("impl "):
        inner = t[5:]
        return bound_word(inner) or last_seg(inner)
    if t.startswith("dyn "):
        return bound_word(t[4:]) or last_seg(t[4:])
    if TEXTY.match(t) or t in ("str", "String"):
        return "text"
    if re.match(r"^(\[u8\]|Vec<u8>|Bytes|BytesMut|&\[u8\])$", t):
        return "bytes"
    if t in ("Path", "PathBuf"):
        return "path"
    if t in ("bool",):
        return "bool"
    if re.match(r"^[iu](8|16|32|64|128|size)$|^f(32|64)$", t):
        return "number"
    if t == "char":
        return "char"
    if t == "()":
        return "nothing"
    if t == "Self" and owner:
        return owner
    m = re.match(r"^(?:[\w:]*::)?(Result)<(.*)>$", t)
    if m:
        inner = split_top(m.group(2))
        return word_for(inner[0], bounds, owner) + " or fails"
    m = re.match(r"^(?:[\w:]*::)?Option<(.*)>$", t)
    if m:
        return "maybe " + word_for(m.group(1), bounds, owner)
    m = re.match(r"^(?:[\w:]*::)?(Vec|VecDeque|HashSet|BTreeSet)<(.*)>$", t)
    if m:
        return "list of " + word_for(m.group(2), bounds, owner)
    m = re.match(r"^\[(.*)\]$", t)
    if m:
        return "list of " + word_for(m.group(1).split(";")[0], bounds, owner)
    if t in bounds:
        bw = [bound_word(x) for x in bounds[t]]
        bw = [x for x in bw if x]
        return bw[0] if bw else "T"
    return last_seg(t)


def bound_word(b):
    b = b.strip()
    if re.search(r"Deserialize", b):
        return "your type"
    if re.search(r"\bSerialize\b", b):
        return "any value"
    if re.search(r"AsRef<str>|Into<String>|ToString|Display|Borrow<str>", b):
        return "text"
    if re.search(r"AsRef<\[u8\]>|Into<Vec<u8>>|Buf\b", b):
        return "bytes"
    if re.search(r"AsRef<(std::path::|::std::path::)?Path>|Into<PathBuf>", b):
        return "path"
    if re.search(r"\bBufRead\b|\bRead\b|AsyncRead", b):
        return "reader"
    if re.search(r"\bWrite\b|AsyncWrite", b):
        return "writer"
    if re.search(r"IntoUrl|AsRef<Url>|Into<Uri>|TryInto<Uri>|AsSendBody", b):
        return "url" if "Url" in b or "Uri" in b else None
    if re.search(r"\bFuture\b", b):
        return "future"
    if re.search(r"\bFn(Once|Mut)?\b", b):
        return "function"
    if re.search(r"Iterator", b):
        return "items"
    return None


def last_seg(t):
    t = re.sub(r"<.*$", "", t.strip())
    return t.split("::")[-1] or t


def shape_of(it):
    if it["k"] not in ("function", "method"):
        return None
    sig = it["sig"]
    m = re.search(r"\((.*)\)\s*(->\s*(.*?))?\s*(\bwhere\b.*)?$", sig)
    if not m:
        return None
    # find the params paren that follows the fn name
    fm = re.search(r"fn\s+\w+\s*(<.*?>)?\s*\(", sig)
    if not fm:
        return None
    start = fm.end() - 1
    depth, end = 0, None
    for i in range(start, len(sig)):
        if sig[i] == "(":
            depth += 1
        elif sig[i] == ")":
            depth -= 1
            if depth == 0:
                end = i
                break
    if end is None:
        return None
    params = split_top(sig[start + 1:end])
    rest = sig[end + 1:]
    ret = None
    rm = re.match(r"\s*->\s*(.*?)(\s+where\b.*)?$", rest)
    if rm:
        ret = rm.group(1).strip()
    bounds = generic_bounds(sig)
    ins, recv = [], None
    for p in params:
        if re.match(r"^(&\s*('\w+\s+)?)?(mut\s+)?self$", p) or p.startswith("self:") or p.startswith("mut self"):
            recv = "reads" if p.startswith("&") and "mut" not in p else ("changes" if "mut" in p and p.startswith("&") else "consumes")
            continue
        if ":" in p:
            ty = p.split(":", 1)[1]
            ins.append(word_for(ty, bounds, it.get("owner")))
    out = word_for(ret, bounds, it.get("owner")) if ret else "nothing"
    if it.get("owner") and recv:
        ins = [it["owner"]] + ins
    return {"in": ins, "out": out}


# ----------------------------------------------------------------------------- std subset (so "you need no package" is a real answer)
STD_FILES = [("std/src/fs.rs", "std::fs"), ("std/src/env.rs", "std::env"), ("std/src/thread/mod.rs", "std::thread"),
             ("std/src/sync/mpsc.rs", "std::sync::mpsc"), ("std/src/time.rs", "std::time"), ("std/src/process.rs", "std::process"),
             ("std/src/net/tcp.rs", "std::net"), ("std/src/io/mod.rs", "std::io"), ("core/src/str/mod.rs", "std::str"),
             ("alloc/src/string.rs", "std::string"), ("std/src/collections/hash/map.rs", "std::collections"),
             ("core/src/num/mod.rs", "std::num")]


def std_api():
    if not RUST_SRC:
        return None
    items = []
    for rel, mod in STD_FILES:
        p = os.path.join(RUST_SRC, rel)
        if not os.path.exists(p):
            continue
        sc = Scan("std", RUST_SRC)
        sc.scan_file(p, "", True)
        for it in sc.items:
            if it["k"] == "variant":
                continue
            if it["k"] == "method":
                owner = it["owner"]
                if mod == "std::str" and owner == "str":
                    it["path"] = f"str::{it['name']}"
                elif mod == "std::num" and owner in ("i32", "u32", "u64", "i64", "usize", "f64"):
                    continue
                else:
                    it["path"] = f"{mod}::{owner}::{it['name']}"
            else:
                it["path"] = f"{mod}::{it['name']}"
            if it.get("trait_item"):
                continue
            it["sig"] = re.sub(r"#\[[^\]]*\]\s*", "", it["sig"])
            items.append(it)
    seen, out = set(), []
    for it in items:
        if it["path"] in seen:
            continue
        seen.add(it["path"])
        out.append(it)
    return out


# ----------------------------------------------------------------------------- licenses
PERMISSIVE = {"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "0BSD", "BSL-1.0", "Unicode-3.0",
              "Unicode-DFS-2016", "MIT-0", "Apache-2.0 WITH LLVM-exception", "BlueOak-1.0.0", "Python-2.0"}
WEAK = {"MPL-2.0", "LGPL-2.1", "LGPL-2.1-only", "LGPL-2.1-or-later", "LGPL-3.0", "LGPL-3.0-only", "LGPL-3.0-or-later", "EPL-2.0"}
STRONG = {"GPL-2.0", "GPL-2.0-only", "GPL-2.0-or-later", "GPL-3.0", "GPL-3.0-only", "GPL-3.0-or-later", "AGPL-3.0", "AGPL-3.0-only"}
PD = {"Unlicense", "CC0-1.0", "WTFPL"}


def fam_of(lic):
    if lic in PERMISSIVE:
        return "permissive"
    if lic in WEAK:
        return "weak"
    if lic in STRONG:
        return "strong"
    if lic in PD:
        return "public-domain"
    return "unknown"


def license_info(expr, project="MIT OR Apache-2.0"):
    if not expr:
        return {"spdx": None, "family": "unknown", "fit": "no license on disk — not measured", "fits": None}
    e = expr.replace("/", " OR ").strip()
    ors = [x.strip().strip("()") for x in re.split(r"\s+OR\s+", e)]
    choices = []
    for o in ors:
        ands = [x.strip().strip("()") for x in re.split(r"\s+AND\s+", o)]
        fams = [fam_of(a) for a in ands]
        worst = "strong" if "strong" in fams else "weak" if "weak" in fams else "unknown" if "unknown" in fams else "permissive" if "permissive" in fams else "public-domain"
        choices.append({"terms": ands, "family": worst})
    order = {"public-domain": 0, "permissive": 1, "weak": 2, "unknown": 3, "strong": 4}
    best = min(choices, key=lambda c: order[c["family"]])
    fam = best["family"]
    pick = " AND ".join(best["terms"])
    if fam in ("permissive", "public-domain"):
        fit = f"Fits your {project} project" + (f" — take it as {pick}" if len(choices) > 1 else "")
        fits = True
    elif fam == "weak":
        fit = f"Fits: {pick} keeps only its own files copyleft"
        fits = True
    elif fam == "strong":
        fit = f"{pick}: shipping it would make backend {best['terms'][0].split('-')[0]}"
        fits = False
    else:
        fit = f"{pick}: not a license we can read — check by hand"
        fits = None
    return {"spdx": expr, "family": fam, "choices": choices, "fit": fit, "fits": fits, "op": "OR" if len(choices) > 1 else ("AND" if len(choices[0]["terms"]) > 1 else None)}


# ----------------------------------------------------------------------------- stability
def stability(name, releases):
    recs = index(name) or []
    vs = []
    for r in recs:
        p = pv(r["vers"])
        if not p:
            continue
        vs.append({"v": r["vers"], "at": r.get("pubtime"), "y": r["yanked"], "pre": bool(p[3]), "msrv": r.get("rust_version")})
    vs.sort(key=lambda x: vkey(x["v"]))
    seen, prev = set(), None
    for x in vs:
        b = bucket(x["v"])
        x["maj"] = (not x["pre"]) and b not in seen and prev is not None
        if not x["pre"]:
            seen.add(b)
            prev = b
    stable = [x for x in vs if not x["pre"]]
    three = NOW - dt.timedelta(days=3 * 365)
    def when(x):
        return dt.datetime.fromisoformat(x["at"].replace("Z", "+00:00")) if x.get("at") else None
    recent_major = [x for x in stable if x["maj"] and not x["y"] and when(x) and when(x) >= three]
    last_major = max((x for x in stable if x["maj"] and when(x)), key=lambda x: when(x), default=None)
    claimed = {
        "releases": len(vs),
        "first": vs[0]["at"] if vs else None,
        "last": stable[-1]["at"] if stable else None,
        "breakingPerYear": round(len(recent_major) / 3, 2),
        "breaking3y": [x["v"] for x in recent_major],
        "lastBreaking": {"v": last_major["v"], "at": last_major["at"]} if last_major else None,
        "yanked": sum(1 for x in vs if x["y"]),
        "source": "version numbers (what the authors claim)",
    }
    measured = None
    rel = releases.get(name)
    if rel:
        pairs = []
        for k, d in rel["diff"].items():
            a, b = d["from"], d["to"]
            if vkey(b) <= vkey(a):
                continue
            brk = sum(1 for c in d["changed"] if c.get("kind") == "breaking") + len(d["removed"])
            pairs.append({"from": a, "to": b, "breaking": brk, "added": len(d["added"]), "removed": len(d["removed"]),
                          "changed": len(d["changed"]), "deprecated": len(d["deprecated"]), "semverSlip": bool(d.get("semverSlip")),
                          "claimedBreaking": bucket(a) != bucket(b)})
        measured = {"pairs": pairs, "slips": sum(1 for p in pairs if p["semverSlip"]),
                    "versions": list(rel["api"].keys()), "source": "graph/releases.json — real API diffs"}
    return vs, claimed, measured


# ----------------------------------------------------------------------------- capabilities per family
# Each capability: plain words + alternatives; an alternative is (path regex, kinds or None).
def C(label, *alts, fact=None, why=None):
    return {"label": label, "alts": alts, "fact": fact, "why": why}


CAPS = {
    "toml": [
        C("read text into your type", (r"::(de::)?from_str$", {"function"})),
        C("read bytes into your type", (r"::(de::)?from_slice$", {"function"})),
        C("a value you can walk without a type", (r"::Value$", {"enum"}), (r"::DocumentMut$", {"struct"}), (r"::Item$", {"enum"})),
        C("write your type as text", (r"::(ser::)?to_string$", {"function"})),
        C("pretty text", (r"::(ser::)?to_string_pretty$", {"function"})),
        C("keep comments and layout when you edit", (r"::Decor$", {"struct"}), (r"::RawString$", {"struct"})),
        C("point at where parsing failed", (r"Error::span$", {"method"}), (r"Error::line_col$", {"method"})),
        C("spans on your own fields", (r"::Spanned$", {"struct"})),
    ],
    "formats": [
        C("read text into your type", (r"::(de::)?from_str$", {"function"})),
        C("read bytes into your type", (r"::(de::)?(from_slice|from_bytes|take_from_bytes)$", {"function"}), (r"::decode_from_slice$", {"function"}), (r"::deserialize$", {"function"})),
        C("read from a reader", (r"::(de::)?from_reader$", {"function"}), (r"::decode_from_std_read$", {"function"})),
        C("write your type as text", (r"::(ser::)?to_string$", {"function"})),
        C("write bytes", (r"::(ser::)?(to_vec|to_allocvec|to_stdvec)$", {"function"}), (r"::encode_to_vec$", {"function"}), (r"::serialize$", {"function"})),
        C("write to a writer", (r"::(ser::)?(to_writer|into_writer|to_io|encode_into_std_write)$", {"function"})),
        C("pretty text", (r"::(ser::)?to_string_pretty$", {"function"})),
        C("a value you can walk without a type", (r"::Value$", {"enum", "struct"})),
        C("a stream of many values", (r"::StreamDeserializer$", {"struct"}), (r"Deserializer::into_iter$", {"method"})),
    ],
    "cli": [
        C("declare arguments with a derive", (r"::Parser$", {"derive"}),),
        C("declare arguments in code", (r"::Command::new$", {"method"}), (r"::Arguments::from_env$", {"method"})),
        C("subcommands", (r"::Subcommand$", {"derive"}), (r"::Arguments::subcommand$", {"method"})),
        C("typed values", (r"::value_parser$", {"macro"}), (r"::Arguments::value_from_str$", {"method"})),
        C("generated --help", (r"::Command::print_help$", {"method"})),
        C("read from environment variables", (r"::Arg::env$", {"method"})),
        C("free-standing flags", (r"::Arguments::contains$", {"method"}), (r"::ArgAction$", {"enum"})),
    ],
    "regex": [
        C("compile a pattern", (r"::Regex::new$", {"method"})),
        C("test for a match", (r"::Regex::is_match$", {"method"})),
        C("find every match", (r"::Regex::find_iter$", {"method"})),
        C("capture groups", (r"::Regex::captures$", {"method"})),
        C("replace", (r"::Regex::replace_all$", {"method"})),
        C("many patterns at once", (r"::RegexSet$", {"struct"})),
        C("look-around and backreferences", fact="docs", why=r"(?i)backreference|look-?around"),
        C("search bytes, not text", (r"::bytes::Regex$", {"struct"})),
        C("escape user input", (r"::escape$", {"function"})),
    ],
    "http": [
        C("GET a URL in one call", (r"^\w+::get$", {"function"})),
        C("a reusable client", (r"::Client$", {"struct"}), (r"::Agent$", {"struct"}), (r"::Session$", {"struct"})),
        C("send JSON", (r"RequestBuilder::json$", {"method"}), (r"::send_json$", {"method"}), (r"::RequestBuilder::json$", {"method"})),
        C("read JSON", (r"::Response::json$", {"method"}), (r"::read_json$", {"method"}), (r"Body::read_json$", {"method"})),
        C("set a timeout", (r"::timeout$", {"method"}), (r"::timeout_global$", {"method"})),
        C("blocking calls", (r"::blocking::Client$", {"struct"}), (r"::Agent::run$", {"method"}), (r"::RequestBuilder::send$", {"method"})),
        C("async calls", (r"::Client::execute$", {"method"}), (r"::client::conn::http1::handshake$", {"function"})),
        C("proxies", (r"::Proxy$", {"struct"}), (r"::Proxy::new$", {"method"})),
    ],
    "async": [
        C("spawn a task", (r"::spawn$", {"function"}), (r"::Executor::spawn$", {"method"})),
        C("run a future to the end", (r"::block_on$", {"function"}), (r"::Runtime::block_on$", {"method"}), (r"::Executor::run$", {"method"})),
        C("sleep", (r"::time::sleep$", {"function"}), (r"::Timer::after$", {"method"})),
        C("channels", (r"::sync::mpsc::channel$", {"function"}), (r"::channel::unbounded$", {"function"}), (r"::channel::bounded$", {"function"})),
        C("files", (r"::fs::read_to_string$", {"function"}), (r"::fs::File$", {"struct"})),
        C("TCP", (r"::net::TcpStream$", {"struct"}), (r"::Async$", {"struct"})),
        C("wait on the first of many", (r"::select$", {"macro"}), (r"::future::or$", {"function"})),
        C("time limits", (r"::time::timeout$", {"function"}), (r"::Timer::after$", {"method"})),
    ],
    "errors": [
        C("derive an error type", (r"::Error$", {"derive"}), (r"::Snafu$", {"derive"}), (r"::Diagnostic$", {"derive"}), (r"::Display$", {"derive"})),
        C("one type for any error", (r"^anyhow::Error$", {"struct"}), (r"::Report$", {"struct"}), (r"::Whatever$", {"struct"})),
        C("add context as it rises", (r"::Context$", {"trait"}), (r"::WrapErr$", {"trait"}), (r"::ResultExt$", {"trait"})),
        C("return early with a message", (r"::bail$", {"macro"}), (r"::whatever$", {"macro"})),
        C("check a condition", (r"::ensure$", {"macro"})),
        C("show the source it came from", (r"::NamedSource$", {"struct"}), (r"::SourceSpan$", {"struct"})),
        C("the chain of causes", (r"::Chain$", {"struct"}), (r"::ChainCompat$", {"struct"})),
    ],
    "inline": [
        C("keeps the first few on the stack", (r"::SmallVec$", {"struct"}), (r"::ArrayVec$", {"struct"}), (r"::TinyVec$", {"enum"})),
        C("spills to the heap when full", (r"::SmallVec::spilled$", {"method"}), (r"::TinyVec::is_heap$", {"method"})),
        C("never allocates", (r"::ArrayVec$", {"struct"}),),
        C("build with a macro", (r"::smallvec$", {"macro"}), (r"::array_vec$", {"macro"}), (r"::tiny_vec$", {"macro"})),
        C("no unsafe code", fact="forbid_unsafe"),
        C("works without std", fact="no_std"),
    ],
    "parsers": [
        C("combinators you compose", (r"::combinator$", None), (r"::Parser$", {"trait"})),
        C("a grammar file", (r"::Parser$", {"derive"})),
        C("errors that point at input", (r"::error::ContextError$", {"struct"}), (r"::error::VerboseError$", {"struct"}), (r"::error::Error$", {"struct"})),
        C("streaming input", (r"::Partial$", {"struct"}), (r"::Needed$", {"enum"})),
    ],
}
CAPS_COMMON_FACTS = [("works without std", "no_std")]


def detect(caps, api, crate_doc):
    out = []
    items = api["items"] if api else []
    for c in caps:
        cell = None
        if c["fact"] == "no_std":
            cell = {"fact": "no_std"} if api and api["no_std"] else None
        elif c["fact"] == "forbid_unsafe":
            cell = {"fact": "forbid(unsafe)"} if api and api["forbid_unsafe"] else None
        elif c["fact"] == "docs":
            txt = crate_doc or ""
            cell = {"fact": "yes — per its docs"} if re.search(c["why"], txt) else None
        else:
            for rx, kinds in c["alts"]:
                hits = [it for it in items if re.search(rx, it["path"]) and (kinds is None or it["k"] in kinds)]
                if hits:
                    it = min(hits, key=lambda x: (x["path"].count("::"), len(x["path"])))
                    cell = {"path": it["path"], "k": it["k"], "sig": it["sig"], "doc": it.get("doc", "")}
                    break
        out.append(cell)
    return out


# ----------------------------------------------------------------------------- incumbent equivalents (by name)
KIND_CLASS = {"derive": "derive", "attr": "derive", "macro": "call", "function": "call", "struct": "type", "enum": "type", "type": "type",
              "trait": "type", "method": "member", "variant": "member", "constant": "value", "static": "value"}


def equivalent(path, kind, api):
    """Find the candidate item that stands where `path` (an item your code uses) stands. Matched by name, within the same kind
    of thing: a derive only answers a derive, a type a type, a call a call."""
    if not api:
        return None
    e = _equivalent(path, api)
    if e and kind and KIND_CLASS.get(kind) and KIND_CLASS.get(e["k"]) and KIND_CLASS[kind] != KIND_CLASS[e["k"]]:
        return None
    return e


def _equivalent(path, api):
    segs = path.split("::")
    name = segs[-1]
    owner = segs[-2] if len(segs) > 2 and segs[-2][:1].isupper() else None
    items = api["items"]
    VALUEISH = ["Value", "Item", "DocumentMut", "Table", "Array", "Document"]
    if owner:  # method or variant on a type
        pool = [it for it in items if it["path"].split("::")[-1] == name and it["k"] in ("method", "variant")]
        if not pool:
            return None
        def rank(it):
            o = it["path"].split("::")[-2]
            return (0 if o == owner else 1 + (VALUEISH.index(o) if o in VALUEISH else 9), it["path"].count("::"))
        it = min(pool, key=rank)
        if it["path"].split("::")[-2] not in VALUEISH + [owner]:
            return None
        return {"path": it["path"], "k": it["k"], "sig": it["sig"], "doc": it.get("doc", "")}
    if name[:1].isupper():
        pool = [it for it in items if it["k"] in ("struct", "enum", "type", "trait") and it["path"].split("::")[-1] == name]
        if not pool and name == "Value":
            pool = [it for it in items if it["k"] in ("struct", "enum") and it["path"].split("::")[-1] in ("Item", "DocumentMut")]
        if not pool:
            return None
        it = min(pool, key=lambda x: (x["path"].count("::"), len(x["path"])))
        return {"path": it["path"], "k": it["k"], "sig": it["sig"], "doc": it.get("doc", "")}
    pool = [it for it in items if it["k"] in ("function", "macro") and it["path"].split("::")[-1] == name]
    if not pool:
        return None
    it = min(pool, key=lambda x: (x["path"].count("::"), len(x["path"])))
    return {"path": it["path"], "k": it["k"], "sig": it["sig"], "doc": it.get("doc", "")}


# the family's main type: lets SmallVec stand for ArrayVec, toml's Value for toml_edit's Value
TWINS = {
    "inline": {"smallvec": "SmallVec", "arrayvec": "ArrayVec", "tinyvec": "TinyVec"},
    "regex": {"regex": "Regex", "fancy-regex": "Regex"},
    "http": {"reqwest": "Client", "ureq": "Agent", "attohttpc": "Session", "hyper": "Client"},
    "errors": {"anyhow": "Error", "miette": "Report", "snafu": "Whatever"},
}


def twin_path(path, fam, frm, to):
    tw = TWINS.get(fam) or {}
    a, b = tw.get(frm), tw.get(to)
    if not a or not b:
        return path
    segs = path.split("::")
    return "::".join(b if x == a else x for x in segs)


def code_uses(tree, names):
    """Where your code names each of `names`, by text: qualified paths (`serde_json::from_str`) and
    imported names (`use serde_json::Value;` then `Value::...`). Returns name -> {path: [sites]}"""
    out = {n: collections.defaultdict(list) for n in names}
    ident_of = {n: n.replace("-", "_") for n in names}
    # a renamed dependency key (gpui = { package = "gpui-ce" }) spells the key, not the package
    for n, e in tree["direct"].items():
        if n in ident_of:
            ident_of[n] = e["ident"]
    for path, t in tree["members"]:
        if "package" not in t:
            continue
        for f in glob.glob(os.path.join(WS, path, "**", "*.rs"), recursive=True):
            if "/target/" in f:
                continue
            try:
                txt = open(f, encoding="utf-8", errors="ignore").read()
            except OSError:
                continue
            present = [n for n, i in ident_of.items() if i + "::" in txt]
            if not present:
                continue
            rel = os.path.relpath(f, WS)
            lines = txt.split("\n")
            for n in present:
                ident = ident_of[n]
                imports = {}
                for mm in re.finditer(r"\buse\s+(" + re.escape(ident) + r"(?:::[^;]+)?);", txt, re.S):
                    for src, nm, alias in expand_use(re.sub(r"\s+", " ", mm.group(1))):
                        if nm in ("*", "self", ""):
                            continue
                        full = (src + "::" + nm) if src else nm
                        imports[alias or nm] = full
                shadowed = {nm for nm in imports if re.search(r"\b(enum|struct|type|trait|fn|union)\s+" + re.escape(nm) + r"\b", txt)}
                rx_q = re.compile(r"(?<![\w:])" + re.escape(ident) + r"((?:::[A-Za-z_]\w*)+)")
                rx_i = re.compile(r"(?<![\w:.])(" + "|".join(map(re.escape, imports)) + r")((?:::[A-Za-z_]\w*)*)\b") if imports else None
                for i, raw_line in enumerate(lines):
                    st = raw_line.strip()
                    if st.startswith(("//", "use ", "pub use ", "*", "/*")) or not st:
                        continue
                    line = clean_code(raw_line)
                    for m in rx_q.finditer(line):
                        segs = m.group(1).split("::")[1:]
                        pth = ident + "::" + "::".join(segs)
                        out[n][pth].append({"f": rel, "l": i + 1, "t": st[:150], "sp": m.group(0)})
                    if rx_i:
                        in_derive = re.search(r"#\[derive\(([^)]*)\)", line)
                        for m in rx_i.finditer(line):
                            if m.start() > 0 and line[m.start() - 1] == "'":
                                continue
                            nxt = line[m.end():m.end() + 1]
                            derive_hit = bool(in_derive and in_derive.start(1) <= m.start() < in_derive.end(1))
                            if m.group(1) in shadowed and not derive_hit:
                                continue
                            if not (derive_hit or m.group(2) or nxt in ("(", "!", "<", "{")):
                                continue
                            pth = imports[m.group(1)] + (m.group(2) or "")
                            out[n][pth].append({"f": rel, "l": i + 1, "t": st[:150], "sp": m.group(0)})
    return out


def rewrite_line(text, spelled, target, incumbent_ident):
    if spelled.startswith(incumbent_ident + "::"):
        new = target
    else:
        k = spelled.count("::") + 1
        new = "::".join(target.split("::")[-k:])
    return text.replace(spelled, new, 1)


def why_paths(tree):
    """Shortest path from one of your packages to every package in the lock (BFS over Cargo.lock)."""
    start = [(p["name"], p["version"]) for p in tree["lock"]["package"] if p["name"] in tree["member_names"]]
    # prefer paths that start at your apps, then crates
    start.sort(key=lambda k: (0 if k[0] in ("backend-desktop", "backend-facet", "backend-cli", "backend-mcp", "backend-worker", "backend-locald") else 1, k[0]))
    parent, q = {}, collections.deque()
    for s0 in start:
        parent[s0] = None
        q.append(s0)
    while q:
        cur = q.popleft()
        for nxt in tree["graph"].get(cur, []):
            if nxt not in parent:
                parent[nxt] = cur
                q.append(nxt)
    def chain(k):
        out = []
        while k is not None:
            out.append(k)
            k = parent.get(k)
        return list(reversed(out))
    res = {}
    for p in tree["ext"]:
        k = (p["name"], p["version"])
        if k in parent:
            c = chain(k)
            res[k] = [(n.replace("backend-", "") if n in tree["member_names"] else n) + ("" if n in tree["member_names"] else " " + v) for n, v in c]
    return res


# ----------------------------------------------------------------------------- cousins (HAND-WRITTEN)
COUSINS = {
    "toml": [
        {"eco": "npm", "name": "smol-toml", "item": "parse", "sig": "parse(toml: string): TomlTable", "say": "a small, fast, correct TOML parser"},
        {"eco": "pypi", "name": "tomllib", "item": "tomllib.loads", "sig": "loads(s: str, /) -> dict[str, Any]", "say": "reads TOML; in the standard library since 3.11", "stdlib": True},
        {"eco": "pypi", "name": "tomlkit", "item": "tomlkit.parse", "sig": "parse(string: str | bytes) -> TOMLDocument", "say": "keeps comments and layout, like toml_edit"},
        {"eco": "go", "name": "BurntSushi/toml", "item": "toml.Decode", "sig": "func Decode(data string, v any) (MetaData, error)", "say": "TOML into your struct"},
        {"eco": "java", "name": "tomlj", "item": "Toml.parse", "sig": "static TomlParseResult parse(String input)", "say": "a TOML parser for the JVM"},
        {"eco": "csharp", "name": "Tomlyn", "item": "Toml.ToModel", "sig": "static TomlTable ToModel(string text)", "say": "TOML for .NET"},
        {"eco": "cpp", "name": "toml++", "item": "toml::parse", "sig": "toml::table parse(std::string_view doc)", "say": "header-only TOML for C++17"},
    ],
    "formats": [
        {"eco": "npm", "name": "JSON", "item": "JSON.parse", "sig": "parse(text: string): any", "say": "built into every JavaScript runtime", "stdlib": True},
        {"eco": "pypi", "name": "json", "item": "json.loads", "sig": "loads(s: str | bytes) -> Any", "say": "in the standard library", "stdlib": True},
        {"eco": "go", "name": "encoding/json", "item": "json.Unmarshal", "sig": "func Unmarshal(data []byte, v any) error", "say": "in the standard library", "stdlib": True},
        {"eco": "java", "name": "jackson-databind", "item": "ObjectMapper.readValue", "sig": "<T> T readValue(String content, Class<T> valueType)", "say": "JSON into your class"},
        {"eco": "csharp", "name": "System.Text.Json", "item": "JsonSerializer.Deserialize", "sig": "static T? Deserialize<T>(string json)", "say": "in the base library", "stdlib": True},
        {"eco": "cpp", "name": "nlohmann/json", "item": "json::parse", "sig": "static basic_json parse(InputType&& i)", "say": "JSON for modern C++"},
    ],
    "regex": [
        {"eco": "npm", "name": "RegExp", "item": "new RegExp", "sig": "new RegExp(pattern: string, flags?: string)", "say": "built in; backtracking, with look-around", "stdlib": True},
        {"eco": "pypi", "name": "re", "item": "re.compile", "sig": "compile(pattern: str, flags=0) -> Pattern[str]", "say": "in the standard library", "stdlib": True},
        {"eco": "go", "name": "regexp", "item": "regexp.Compile", "sig": "func Compile(expr string) (*Regexp, error)", "say": "RE2 semantics, like regex; in the standard library", "stdlib": True},
        {"eco": "java", "name": "java.util.regex", "item": "Pattern.compile", "sig": "static Pattern compile(String regex)", "say": "in the JDK", "stdlib": True},
        {"eco": "csharp", "name": "System.Text.RegularExpressions", "item": "new Regex", "sig": "Regex(string pattern)", "say": "in the base library", "stdlib": True},
        {"eco": "cpp", "name": "RE2", "item": "RE2::FullMatch", "sig": "static bool FullMatch(const StringPiece& text, const RE2& re)", "say": "linear-time matching, like regex"},
    ],
    "http": [
        {"eco": "npm", "name": "fetch", "item": "fetch", "sig": "fetch(input: string | URL, init?: RequestInit): Promise<Response>", "say": "built into Node 18+", "stdlib": True},
        {"eco": "pypi", "name": "requests", "item": "requests.get", "sig": "get(url: str, params=None, **kwargs) -> Response", "say": "blocking HTTP, like ureq"},
        {"eco": "pypi", "name": "httpx", "item": "httpx.get", "sig": "get(url: URL | str, **kwargs) -> Response", "say": "sync and async, like reqwest"},
        {"eco": "go", "name": "net/http", "item": "http.Get", "sig": "func Get(url string) (resp *Response, err error)", "say": "in the standard library", "stdlib": True},
        {"eco": "java", "name": "java.net.http", "item": "HttpClient.send", "sig": "<T> HttpResponse<T> send(HttpRequest req, BodyHandler<T> h)", "say": "in the JDK since 11", "stdlib": True},
        {"eco": "csharp", "name": "HttpClient", "item": "HttpClient.GetStringAsync", "sig": "Task<string> GetStringAsync(string? requestUri)", "say": "in the base library", "stdlib": True},
        {"eco": "cpp", "name": "cpr", "item": "cpr::Get", "sig": "Response Get(Ts&&... ts)", "say": "curl for people"},
    ],
    "cli": [
        {"eco": "npm", "name": "commander", "item": "program.option", "sig": "option(flags: string, description?: string): this", "say": "declare flags in code, like clap's builder"},
        {"eco": "pypi", "name": "argparse", "item": "ArgumentParser.add_argument", "sig": "add_argument(*name_or_flags, **kwargs) -> Action", "say": "in the standard library", "stdlib": True},
        {"eco": "go", "name": "flag", "item": "flag.String", "sig": "func String(name string, value string, usage string) *string", "say": "in the standard library", "stdlib": True},
        {"eco": "java", "name": "picocli", "item": "@Command", "sig": "@Command(name = \"…\") class App implements Callable<Integer>", "say": "annotations, like clap's derive"},
        {"eco": "csharp", "name": "System.CommandLine", "item": "RootCommand", "sig": "new RootCommand(string description = \"\")", "say": "Microsoft's parser"},
        {"eco": "cpp", "name": "CLI11", "item": "CLI::App::add_option", "sig": "Option* add_option(std::string name, T& variable)", "say": "header-only, like pico-args"},
    ],
    "async": [
        {"eco": "npm", "name": "event loop", "item": "Promise", "sig": "new Promise<T>(executor)", "say": "built into every runtime", "stdlib": True},
        {"eco": "pypi", "name": "asyncio", "item": "asyncio.run", "sig": "run(main: Coroutine[Any, Any, T]) -> T", "say": "in the standard library", "stdlib": True},
        {"eco": "go", "name": "goroutines", "item": "go f()", "sig": "go f(x, y, z)", "say": "built into the language", "stdlib": True},
    ],
    "errors": [
        {"eco": "go", "name": "errors", "item": "fmt.Errorf", "sig": "func Errorf(format string, a ...any) error  // %w wraps", "say": "wrapping with context, like anyhow", "stdlib": True},
        {"eco": "pypi", "name": "exceptions", "item": "raise … from …", "sig": "raise NewError(…) from err", "say": "the cause chain is built in", "stdlib": True},
    ],
}


# ----------------------------------------------------------------------------- network facts (optional, cached)
def net_facts(names):
    cache = {}
    if os.path.exists(NET_CACHE):
        cache = json.load(open(NET_CACHE))
    if not NET:
        return cache
    os.makedirs(os.path.dirname(NET_CACHE), exist_ok=True)
    cache.setdefault("crates", {})
    cache.setdefault("advisories", {})
    for n in names:
        if n in ("std",):
            continue
        try:
            out = subprocess.run(["curl", "-sf", "-A", "nudox-design-prototype (contactnudox@gmail.com)",
                                  f"https://crates.io/api/v1/crates/{n}"], capture_output=True, text=True, timeout=30).stdout
            j = json.loads(out)["crate"]
            cache["crates"][n] = {"downloads": j.get("downloads"), "recent": j.get("recent_downloads"), "fetched": NOW.date().isoformat()}
        except Exception as e:
            cache["crates"][n] = {"error": str(e)[:80]}
        time.sleep(1.05)
        try:
            r = subprocess.run(["gh", "api", f"repos/rustsec/advisory-db/contents/crates/{n}"], capture_output=True, text=True, timeout=30)
            if r.returncode != 0:
                cache["advisories"][n] = {"ids": [], "fetched": NOW.date().isoformat(), "note": "no directory in rustsec/advisory-db"}
            else:
                files = [f["name"] for f in json.loads(r.stdout) if f["name"].endswith(".md")]
                advs = []
                for f in files:
                    rr = subprocess.run(["gh", "api", "-H", "Accept: application/vnd.github.raw", f"repos/rustsec/advisory-db/contents/crates/{n}/{f}"],
                                        capture_output=True, text=True, timeout=30)
                    txt = rr.stdout
                    title = re.search(r"^#\s+(.+)$", txt, re.M)
                    patched = re.search(r"patched\s*=\s*\[([^\]]*)\]", txt)
                    date = re.search(r'date\s*=\s*"([^"]+)"', txt)
                    inf = re.search(r'informational\s*=\s*"([^"]+)"', txt)
                    advs.append({"id": f[:-3], "title": title.group(1).strip() if title else "", "date": date.group(1) if date else None,
                                 "patched": [x.strip().strip('"') for x in patched.group(1).split(",") if x.strip()] if patched else [],
                                 "informational": inf.group(1) if inf else None})
                cache["advisories"][n] = {"ids": advs, "fetched": NOW.date().isoformat()}
        except Exception as e:
            cache["advisories"][n] = {"error": str(e)[:80]}
    json.dump(cache, open(NET_CACHE, "w"), indent=1)
    return cache


def affected(adv, version):
    """Is `version` affected by a RustSec advisory? patched reqs are OR'ed; unaffected if any matches."""
    if not version:
        return None
    if adv.get("informational") and adv["informational"] in ("unmaintained", "notice"):
        return adv["informational"]
    pats = adv.get("patched") or []
    if not pats:
        return True
    for p in pats:
        if matches(p.replace(" ", ""), version) or matches(p, version):
            return False
    return True


# ----------------------------------------------------------------------------- main
def main():
    t0 = time.time()
    tree = load_tree()
    releases = json.load(open(os.path.join(V4, "graph", "releases.json")))
    use_counts, use_files = count_uses(tree)
    net = net_facts(CANDIDATES + list(tree["direct"].keys()))

    # reverse dependencies among the crates indexed on this machine
    revdeps = collections.Counter()
    indexed = 0
    for f in glob.glob(f"{IDX}/**/*", recursive=True):
        if os.path.isdir(f):
            continue
        n = os.path.basename(f)
        rec = latest_stable(n)
        if not rec:
            continue
        indexed += 1
        for d in rec["deps"]:
            if d.get("kind", "normal") == "normal":
                revdeps[d.get("package") or d["name"]] += 1

    lock_names = set(tree["in_lock"])
    ext_count = len(tree["ext"])

    def advisory_for(name, ver):
        adv = (net.get("advisories") or {}).get(name)
        if not adv or "ids" not in adv or not ver:
            return None
        hits = [a for a in adv["ids"] if affected(a, ver)]
        return {"checked": adv.get("fetched"), "known": len(adv["ids"]), "version": ver, "source": "rustsec/advisory-db via gh api",
                "affecting": [{"id": a["id"], "title": a["title"], "date": a["date"], "patched": a["patched"], "informational": a.get("informational")} for a in hits]}

    # ---- your tree: direct deps with roles + transitive attribution
    direct_out = []
    role_of = {}
    for name, e in sorted(tree["direct"].items()):
        pin = sorted(tree["in_lock"].get(name, []), key=vkey)
        d, meta = meta_for(name, pin[-1] if pin else None)
        yp = tree["your_pins"].get(name) or pin[-1:]
        role, why = derive_role(name, e, meta)
        role_of[name] = role
        uses = sum(use_counts[name].values())
        direct_out.append({
            "name": name, "role": role, "roleWhy": why, "pin": yp[-1] if yp else None, "pins": pin, "yourPins": yp,
            "members": {m: sorted(k) for m, k in sorted(e["members"].items())},
            "kinds": sorted(e["kinds"]), "uses": uses, "usesBy": dict(use_counts[name].most_common()),
            "files": len(use_files[name]), "desc": (meta or {}).get("description"),
            "license": (meta or {}).get("license"), "categories": (meta or {}).get("categories"),
            "advisory": advisory_for(name, pin[-1] if pin else None),
            "history": [{"v": x["v"], "at": x["at"], "y": x["y"]} for x in stability(name, {})[0] if not x["pre"]][-80:],
        })
    # transitive attribution through the lock graph
    by_nv = {(p["name"], p["version"]): p for p in tree["lock"]["package"]}
    reach = {}
    for dd in direct_out:
        for v in dd["pins"]:
            seen, stack = set(), [(dd["name"], v)]
            while stack:
                cur = stack.pop()
                if cur in seen:
                    continue
                seen.add(cur)
                for nxt in tree["graph"].get(cur, []):
                    if nxt[0] in tree["member_names"]:
                        continue
                    stack.append(nxt)
            for s in seen:
                if s[0] != dd["name"]:
                    reach.setdefault(s, set()).add(dd["role"])
    trans_by_role = collections.defaultdict(list)
    direct_names = set(tree["direct"])
    for p in tree["ext"]:
        key = (p["name"], p["version"])
        if p["name"] in direct_names:
            continue
        roles = reach.get(key, set())
        r = next(iter(roles)) if len(roles) == 1 else ("shared" if roles else "unreached")
        trans_by_role[r].append(p["name"] + " " + p["version"])
    roles_out = []
    for rid, label in ROLE_ORDER + [("shared", "shared by several roles"), ("unreached", "not reached from a direct dependency")]:
        ds = [d for d in direct_out if d["role"] == rid]
        tr = sorted(trans_by_role.get(rid, []))
        if not ds and not tr:
            continue
        who = collections.Counter()
        for d in ds:
            for m in d["members"]:
                who[m] += 1
        roles_out.append({"id": rid, "label": label, "direct": [d["name"] for d in sorted(ds, key=lambda x: -x["uses"])],
                          "transitive": len(tr), "transitiveSample": tr[:40], "for": [m for m, _ in who.most_common(4)]})

    # ---- candidates
    std = std_api()
    pkgs = {}
    all_caps = {}
    for fam, names in FAMILIES.items():
        for name in names:
            recs = index(name)
            latest = latest_stable(name)
            if not latest:
                print("skip (not indexed):", name, file=sys.stderr)
                continue
            pin = sorted(tree["in_lock"].get(name, []), key=vkey)
            api = public_api(name, latest["vers"])
            d, meta = meta_for(name, latest["vers"])
            src_ver = os.path.basename(d)[len(name) + 1:] if d else None
            cl, unknown = closure(name, "=" + latest["vers"])
            keys = set(cl)
            missing = sorted(k for k in keys if k not in tree["lock_keys"] and k[0] != name)
            second = sorted(k for k in missing if k[0] in lock_names)
            self_new = (name, bucket(latest["vers"])) not in tree["lock_keys"]
            vs, claimed, measured = stability(name, releases)
            lic = license_info((meta or {}).get("license") or latest.get("license"), tree["license"])
            feats = features_of(latest)
            items = []
            for it in (api["items"] if api else []):
                sh = shape_of(it)
                q = {"p": it["path"], "k": it["k"], "s": it["sig"][:220], "d": it.get("doc", "")[:170]}
                if sh:
                    q["sh"] = sh
                if it.get("dep"):
                    q["dep"] = 1
                items.append(q)
            adv = (net.get("advisories") or {}).get(name)
            advisory = None
            if adv and "ids" in adv:
                ver = pin[-1] if pin else latest["vers"]
                hits = [a for a in adv["ids"] if affected(a, ver)]
                advisory = {"checked": adv.get("fetched"), "known": len(adv["ids"]), "affecting": [
                    {"id": a["id"], "title": a["title"], "date": a["date"], "patched": a["patched"], "informational": a.get("informational")} for a in hits],
                    "version": ver, "source": "rustsec/advisory-db via gh api"}
            dl = (net.get("crates") or {}).get(name)
            pkgs[name] = {
                "name": name, "eco": "rust", "family": fam,
                "desc": (meta or {}).get("description"), "keywords": (meta or {}).get("keywords") or [],
                "categories": (meta or {}).get("categories") or [], "repo": (meta or {}).get("repository"),
                "edition": (meta or {}).get("edition"), "latest": latest["vers"], "msrv": latest.get("rust_version"),
                "srcVersion": src_ver, "license": lic,
                "tree": {"pins": pin, "yourPin": (tree["your_pins"].get(name) or [None])[-1], "direct": name in tree["direct"],
                         "uses": (len(releases[name]["uses"]) if name in releases else sum(use_counts[name].values())) if name in tree["direct"] else 0,
                         "usesSource": "graph/releases.json" if name in releases else "text search of your sources",
                         "usesBy": (uses_by_member(releases[name]["uses"], tree) if name in releases else dict(use_counts[name].most_common())) if name in tree["direct"] else {},
                         "pulledBy": sorted({p["name"] for p in tree["lock"]["package"] for dd in p.get("dependencies", []) if dd.split(" ")[0] == name})[:8],
                         "role": role_of.get(name)},
                "cost": {"closure": len(keys), "adds": [f"{k[0]} {cl[k]}" for k in missing], "addsSelf": self_new,
                         "second": [f"{k[0]} {cl[k]}" for k in second], "unknown": sorted(unknown),
                         "source": "registry index on this machine, default features, resolved for aarch64-apple-darwin"},
                "features": {"default": feats.get("default", []), "all": sorted(k for k in feats if k != "default")[:40]},
                "versions": [{"v": x["v"], "at": x["at"], "y": x["y"], "maj": x["maj"], "pre": x["pre"]} for x in vs],
                "stability": {"claimed": claimed, "measured": measured},
                "api": {"size": api["total"] if api else None, "items": items, "no_std": api["no_std"] if api else None,
                        "deser": sorted({ty for (_, ty, tr) in (api["impls"] if api else []) if tr == "Deserialize"}),
                        "forbidUnsafe": api["forbid_unsafe"] if api else None, "crateDoc": api["crate_doc"] if api else ""},
                "revdeps": {"count": revdeps.get(name, 0), "of": indexed},
                "advisory": advisory,
                "downloads": {"recent": dl.get("recent"), "all": dl.get("downloads"), "fetched": dl.get("fetched")} if dl and "recent" in dl else None,
            }
            print(f"{name:12s} {latest['vers']:22s} items {len(items):5d}/{api['total'] if api else '-':>5} closure {len(keys):3d} adds {len(missing):3d} "
                  f"{'yours' if pin else ''}", file=sys.stderr)
    if std:
        items = []
        for it in std:
            sh = shape_of(it)
            q = {"p": it["path"], "k": it["k"], "s": it["sig"][:220], "d": it.get("doc", "")[:170]}
            if sh:
                q["sh"] = sh
            items.append(q)
        pkgs["std"] = {"name": "std", "eco": "rust", "family": "std", "stdlib": True, "desc": "The Rust standard library",
                       "keywords": [], "categories": [], "license": license_info("MIT OR Apache-2.0", tree["license"]),
                       "tree": {"pins": ["(toolchain)"], "direct": True, "uses": None, "role": None}, "cost": {"closure": 0, "adds": [], "addsSelf": False, "second": [], "unknown": []},
                       "versions": [], "stability": {"claimed": None, "measured": None},
                       "api": {"size": len(items), "items": items, "scope": "subset: " + ", ".join(m for _, m in STD_FILES)}, "revdeps": None, "advisory": None, "downloads": None}

    # ---- compare tables
    compare = {}
    for fam, names in FAMILIES.items():
        rows = []
        present = [n for n in names if n in pkgs]
        apis = {n: {"items": [{"path": it["p"], "k": it["k"], "sig": it["s"], "doc": it["d"]} for it in pkgs[n]["api"]["items"]],
                    "no_std": pkgs[n]["api"]["no_std"], "forbid_unsafe": pkgs[n]["api"]["forbidUnsafe"]} for n in present}
        for c in CAPS.get(fam, []):
            cells = {}
            for n in present:
                cells[n] = detect([c], apis[n], (pkgs[n]["api"]["crateDoc"] or "") + " " + (pkgs[n]["desc"] or ""))[0]
            rows.append({"label": c["label"], "cells": cells})
        compare[fam] = {"caps": rows}
    # uses of an incumbent, mapped onto every other candidate in its family (by name), with your lines rewritten
    text_uses = code_uses(tree, [n for n in pkgs if n != "std" and pkgs[n]["tree"]["direct"] and n not in releases])
    incumbents = {}
    api_view = {n: {"items": [{"path": it["p"], "k": it["k"], "sig": it["s"], "doc": it["d"], "sh": it.get("sh")} for it in pkgs[n]["api"]["items"]]}
                for n in pkgs}
    for name in pkgs:
        if name == "std" or not pkgs[name]["tree"]["direct"]:
            continue
        fam = pkgs[name]["family"]
        ident = name.replace("-", "_")
        if name in releases:
            raw = [{"path": u["path"], "f": u["file"], "l": u["line"], "t": u["text"].strip()[:150]} for u in releases[name]["uses"]]
            src = "graph/releases.json (the index's resolved uses)"
        else:
            raw = []
            for pth, sites in text_uses.get(name, {}).items():
                for x in sites:
                    raw.append({"path": pth, "f": x["f"], "l": x["l"], "t": x["t"], "sp": x["sp"]})
            src = "text search of your sources (qualified paths and imported names)"
        if not raw:
            continue
        byp = collections.Counter(u["path"] for u in raw)
        files = collections.defaultdict(set)
        for u in raw:
            files[u["path"]].add(u["f"])
        others = [o for o in FAMILIES[fam] if o != name and o in pkgs]
        rows = []
        eqs = {}
        for pth, n in byp.most_common(24):
            own = next((it for it in pkgs[name]["api"]["items"] if it["p"] == pth), None)
            own_kind = own["k"] if own else None
            row = {"path": pth, "count": n, "k": own_kind, "files": len(files[pth]), "sig": own["s"] if own else None, "doc": own["d"] if own else None,
                   "sh": own.get("sh") if own else None, "cells": {}}
            for o in others:
                e = equivalent(twin_path(pth, fam, name, o), own_kind, api_view[o])
                if e:
                    osh = next((it.get("sh") for it in api_view[o]["items"] if it["path"] == e["path"]), None)
                    e["same"] = (osh == row["sh"]) if (osh and row["sh"]) else None
                    e["sh"] = osh
                row["cells"][o] = e
                eqs[(pth, o)] = e
            rows.append(row)
        # your lines, rewritten per candidate
        by_line = collections.OrderedDict()
        for u in sorted(raw, key=lambda u: (u["f"], u["l"])):
            k = (u["f"], u["l"])
            by_line.setdefault(k, {"f": u["f"], "l": u["l"], "t": u["t"], "uses": []})
            if u["path"] not in [x[0] for x in by_line[k]["uses"]]:
                by_line[k]["uses"].append((u["path"], u.get("sp")))
        sites = []
        for k, site in list(by_line.items())[:90]:
            rw = {}
            for o in others:
                txt, miss = site["t"], []
                for pth, sp in sorted(site["uses"], key=lambda x: -len(x[0])):
                    e = eqs.get((pth, o))
                    if e is None and (pth, o) not in eqs:
                        ok = next((it["k"] for it in pkgs[name]["api"]["items"] if it["p"] == pth), None)
                        e = equivalent(twin_path(pth, fam, name, o), ok, api_view[o])
                    if sp is None:
                        segs = pth.split("::")
                        sp = next((("::".join(segs[i:])) for i in range(len(segs)) if "::".join(segs[i:]) in txt), None)
                    if not sp or sp not in txt:
                        continue
                    if e is None:
                        miss.append(pth)
                    else:
                        txt = rewrite_line(txt, sp, e["path"], ident)
                rw[o] = {"t": txt, "miss": miss}
            sites.append({"f": site["f"], "l": site["l"], "t": site["t"], "uses": [x[0] for x in site["uses"]], "rw": rw})
        incumbents[name] = {"uses": rows, "total": len(raw), "paths": len(byp), "sites": sites, "source": src,
                            "match": "by name: the same function, the same method on the value type, or the family's main type"}
        print(f"incumbent {name}: {len(raw)} uses, {len(byp)} paths, {len(sites)} lines", file=sys.stderr)

    # one number everywhere: the richer incumbent count (qualified paths + imported names) wins
    for name, inc in incumbents.items():
        if name not in releases:
            pkgs[name]["tree"]["uses"] = inc["total"]
            pkgs[name]["tree"]["usesSource"] = inc["source"]
    for dd in direct_out:
        if dd["name"] in incumbents:
            dd["uses"] = incumbents[dd["name"]]["total"]

    # why is it in your tree, and which crates are there twice
    whys = why_paths(tree)
    trans = []
    for p in tree["ext"]:
        k = (p["name"], p["version"])
        roles = reach.get(k, set())
        trans.append({"n": p["name"], "v": p["version"], "why": whys.get(k), "role": (next(iter(roles)) if len(roles) == 1 else "shared" if roles else "unreached"),
                      "direct": p["name"] in direct_names})
    dupes = []
    for nm, vs in sorted(tree["in_lock"].items()):
        if len(vs) > 1:
            yp = (tree["your_pins"].get(nm) or [None])[-1] if nm in tree["direct"] else None
            up = None
            if yp and nm in releases:
                for v in vs:
                    key = f"{yp}\u2192{v}"
                    if v != yp and key in releases[nm]["impact"]:
                        imp = releases[nm]["impact"][key]
                        items = {}
                        for x in imp:
                            ch = x["change"]
                            items.setdefault(x["path"], {"path": x["path"], "before": ch.get("before"), "after": ch.get("after"), "kind": ch.get("kind"), "n": 0})["n"] += 1
                        up = {"from": yp, "to": v, "touched": len(imp), "of": len(releases[nm]["uses"]), "items": list(items.values()), "source": "graph/releases.json"}
            dupes.append({"n": nm, "yours": yp, "upgrade": up,
                          "versions": [{"v": v, "why": whys.get((nm, v)), "yours": v == yp} for v in sorted(vs, key=vkey)]})
    dupes.sort(key=lambda d: (0 if d["yours"] else 1, min((len(v["why"]) if v["why"] else 99) for v in d["versions"]), d["n"]))
    checked, hits = 0, []
    for nm, vs in tree["in_lock"].items():
        adv = (net.get("advisories") or {}).get(nm)
        if not adv or "ids" not in adv:
            continue
        checked += 1
        for v in vs:
            for a in adv["ids"]:
                if affected(a, v):
                    hits.append({"n": nm, "v": v, "id": a["id"], "title": a["title"], "informational": a.get("informational"), "why": whys.get((nm, v))})
    health = {"checked": checked, "of": ext_count, "affecting": hits, "fetched": (next(iter((net.get("advisories") or {}).values()), {}) or {}).get("fetched")}
    cousins = {k: v for k, v in COUSINS.items()}
    data = {
        "generated": NOW.date().isoformat(),
        "project": {"name": "backend", "license": tree["license"], "members": len(tree["member_names"]),
                    "direct": len(direct_out), "inTree": ext_count, "lockPackages": len(tree["lock"]["package"])},
        "roles": roles_out, "direct": direct_out, "transitive": trans, "duplicates": dupes, "health": health,
        "packages": pkgs, "families": FAMILIES, "compare": compare, "incumbents": incumbents, "cousins": cousins,
        "sources": {
            "index": IDX.replace(HOME, "~"), "src": SRC.replace(HOME, "~"), "lock": "Cargo.lock", "releases": "v4/graph/releases.json",
            "std": (RUST_SRC or "").split("/lib/")[0] if RUST_SRC else None, "indexedCrates": indexed,
            "cousins": "HAND-WRITTEN in build_data.py: npm/PyPI/Go/Java/C#/C++ are not indexed on this machine",
            "roles": "derived: a dependency's crates.io categories/keywords × which of your packages use it; unjustified → other",
            "downloads": "crates.io API (cache/net.json)" if net.get("crates") else None,
            "advisories": "rustsec/advisory-db via gh api (cache/net.json)" if net.get("advisories") else None,
        },
    }
    out = os.path.join(HERE, "data.json")
    json.dump(data, open(out, "w"), separators=(",", ":"))
    print(f"wrote {out} {os.path.getsize(out) / 1e6:.2f} MB in {time.time() - t0:.1f}s", file=sys.stderr)


if __name__ == "__main__":
    main()
