#!/usr/bin/env python3
"""Extract this repository's real Rust dependency graph into JSON for the design board.

Reads only local files (no cargo, no network, no git mutation):

  Cargo.lock                      the resolved graph (ids, deps, kinds)
  <member>/Cargo.toml             which lock packages are *direct* deps, rename map
  ~/.cargo/registry/src/...       crate sources: descriptions, licenses, public items
  ~/.cargo/registry/index/...     sparse-index cache: published/yanked versions

Writes  data/world.json  and  data/candidates.json  next to this script.
Standard library only (needs Python >= 3.11 for tomllib).
"""
from __future__ import annotations

import glob
import json
import os
import re
import sys
import time
import tomllib
from concurrent.futures import ProcessPoolExecutor
from collections import Counter, defaultdict, deque
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = Path(os.environ.get("WORLD_ROOT", HERE.parents[2])).resolve()
OUT = HERE / "data"


def _registry_dirs():
    home = Path.home() / ".cargo" / "registry"
    src = sorted(glob.glob(str(home / "src" / "index.crates.io-*")))
    idx = sorted(glob.glob(str(home / "index" / "index.crates.io-*")))
    if not src or not idx:
        sys.exit("no crates.io registry src/index dirs under ~/.cargo/registry")
    return Path(src[0]), Path(idx[0]) / ".cache"


REG_SRC, REG_CACHE = _registry_dirs()
GIT_CHECKOUTS = Path.home() / ".cargo" / "git" / "checkouts"

ITEM_CAP = 240          # listed items per package (largest modules first)
ITEM_CAP_DEEP = 80      # fallback for depth >= 2 if world.json outgrows SIZE_BUDGET
SIZE_BUDGET = 6 * 1024 * 1024
USES_ITEM_CAP = 150     # names listed in uses.items (sites stays the true total)
USES_PATH_CAP = 30      # full paths listed in uses.paths
LEDE_MAX = 240
SCAN_WORKERS = 4         # other agents are building; keep the fan-out modest

WARNINGS: list[str] = []


def warn(msg: str) -> None:
    WARNINGS.append(msg)


# --------------------------------------------------------------------------- semver

_SEMVER = re.compile(r"^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?$")


def strip_build(v: str) -> str:
    return v.split("+", 1)[0]


def sv(v: str):
    """Sort key following semver 2.0 precedence (build metadata ignored); None if unparsable."""
    m = _SEMVER.match(v)
    if not m:
        return None
    maj, mi, pa, pre, _ = m.groups()
    if pre is None:
        prekey = (1,)
    else:
        prekey = (0, tuple((0, int(x), "") if x.isdigit() else (1, 0, x) for x in pre.split(".")))
    return (int(maj), int(mi), int(pa), prekey)


def is_pre(v: str) -> bool:
    m = _SEMVER.match(v)
    return bool(m and m.group(4))


_PART = re.compile(r"^(?:(\*|x|X|\d+)(?:\.(\*|x|X|\d+)(?:\.(\*|x|X|\d+))?)?)(?:-([0-9A-Za-z.-]+))?(?:\+.*)?$")


def _partial(s: str):
    m = _PART.match(s.strip())
    if not m:
        return None
    parts = []
    for g in m.groups()[:3]:
        parts.append(None if g is None or g in "*xX" else int(g))
    return parts, m.group(4)


def req_matches(req: str, version: str) -> bool:
    """Cargo-flavoured version requirement check (good enough to pick between lock versions)."""
    key = sv(version)
    if key is None:
        return False
    v3 = key[:3]
    for comp in [c.strip() for c in req.split(",") if c.strip()]:
        if comp in ("*", ""):
            continue
        m = re.match(r"^(\^|~|=|>=|<=|>|<)?\s*(.+)$", comp)
        op, rest = m.group(1) or "^", m.group(2)
        pp = _partial(rest)
        if pp is None:
            return False
        (maj, mi, pa), pre = pp[0], pp[1]
        if maj is None:  # bare wildcard
            continue
        lo = (maj, mi or 0, pa or 0)
        if op == "=":
            hi_ok = (maj,) == v3[:1] and (mi is None or mi == v3[1]) and (pa is None or pa == v3[2])
            ok = hi_ok
        elif op == "^":
            if maj > 0 or mi is None:
                hi = (maj + 1, 0, 0)
            elif mi > 0 or pa is None:
                hi = (0, mi + 1, 0)
            else:
                hi = (0, 0, pa + 1)
            ok = lo <= v3 < hi
        elif op == "~":
            hi = (maj, mi + 1, 0) if mi is not None else (maj + 1, 0, 0)
            ok = lo <= v3 < hi
        elif op == ">=":
            ok = v3 >= lo
        elif op == "<":
            ok = v3 < lo
        elif op == "<=":
            up = (maj, (mi + 1) if mi is not None else 0, 0) if pa is None and mi is not None else \
                 ((maj + 1, 0, 0) if mi is None else (maj, mi, pa + 1))
            ok = v3 < up
        else:  # ">"
            up = (maj + 1, 0, 0) if mi is None else ((maj, mi + 1, 0) if pa is None else (maj, mi, pa + 1))
            ok = v3 >= up
        if not ok:
            return False
        if key[3] != (1,) and not pre:  # prerelease only satisfies a prerelease requirement
            return False
    return True


# --------------------------------------------------------------------------- sparse cache

_cache_memo: dict[str, dict | None] = {}


def cache_path(name: str) -> Path:
    n = name.lower()
    if len(n) == 1:
        rel = f"1/{n}"
    elif len(n) == 2:
        rel = f"2/{n}"
    elif len(n) == 3:
        rel = f"3/{n[0]}/{n}"
    else:
        rel = f"{n[:2]}/{n[2:4]}/{n}"
    return REG_CACHE / rel


def cache_info(name: str):
    """{'versions': [(vers, yanked)], ...} from the sparse-index cache, or None when absent.

    File framing: a few header bytes, an etag line, then for each published version a
    NUL-separated pair (version string, JSON object). We only need the JSON objects.
    """
    if name in _cache_memo:
        return _cache_memo[name]
    p = cache_path(name)
    info = None
    if p.is_file():
        vs = []
        for part in p.read_bytes().split(b"\0"):
            if part[:1] == b"{":
                try:
                    o = json.loads(part)
                except ValueError:
                    continue
                vs.append((o.get("vers", ""), bool(o.get("yanked"))))
        info = {"versions": vs}
    _cache_memo[name] = info
    return info


def newer_yanked(name: str, version: str):
    """(latest, newer, yanked) for a crate at `version`."""
    info = cache_info(name)
    if info is None:
        return None, [], []
    cur = sv(strip_build(version))
    allow_pre = is_pre(strip_build(version))
    newer, yanked = {}, set()
    for vers, y in info["versions"]:
        clean = strip_build(vers)
        k = sv(clean)
        if k is None:
            continue
        if y:
            yanked.add(clean)
            continue
        if cur is not None and k > cur and (allow_pre or not is_pre(clean)):
            newer[clean] = k
    ordered = sorted(newer, key=newer.get)
    yk = sorted(yanked, key=lambda v: sv(v) or (0, 0, 0, (0,)))
    return (ordered[-1] if ordered else None), ordered, yk


# --------------------------------------------------------------------------- text helpers

_ABBR = {"e.g", "i.e", "vs", "etc", "approx", "cf", "resp", "no", "inc", "ltd", "co"}


def clean_md(t: str) -> str:
    t = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", t)
    t = t.replace("`", "")
    return " ".join(t.split())


def first_sentence(text: str | None) -> str | None:
    if not text:
        return None
    t = clean_md(text)
    if not t:
        return None
    out = t
    for m in re.finditer(r"[.!?](?=\s|$)", t):
        head = t[: m.start()]
        last = head.split(" ")[-1].lower().strip("(")
        if m.group() == "." and (last in _ABBR or (len(last) == 1 and last.isalpha())):
            continue
        rest = t[m.end():].lstrip()
        if rest and not (rest[0].isupper() or rest[0].isdigit() or rest[0] in "\"'([*_"):
            continue
        out = t[: m.start()] + ("" if m.group() == "." else m.group())
        break
    else:
        out = t.rstrip(".") if not t.endswith("..") else t
    out = out.strip()
    if len(out) > LEDE_MAX:
        out = out[: LEDE_MAX - 1].rsplit(" ", 1)[0].rstrip(",;:") + "…"
    return out or None


def crate_doc_paragraph(path: Path) -> str | None:
    """First non-heading paragraph of the `//!` inner doc at the top of a crate root file."""
    try:
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return None
    paras, cur = [], []
    for ln in lines:
        s = ln.strip()
        if s.startswith("//!"):
            body = s[3:].strip()
            if body:
                cur.append(body)
            elif cur:
                paras.append(cur)
                cur = []
        elif s == "" or s.startswith("#!") or s.startswith("//") and not s.startswith("///"):
            if cur and s == "":
                pass
            continue
        else:
            break
    if cur:
        paras.append(cur)
    if not paras:
        return None
    for p in paras:
        if not p[0].startswith("#"):
            return " ".join(p)
    return paras[0][0].lstrip("#").strip()


# --------------------------------------------------------------------------- Rust masking

_TOK = re.compile(r"//[^\n]*|/\*|(?<![A-Za-z0-9_])b?r(#*)\"|\"|'")
_STR_END = re.compile(r'(?:\\.|[^"\\])*"', re.S)
_CHAR = re.compile(r"'(?:\\(?:x[0-9A-Fa-f]{2}|u\{[^}\n]*\}|[^xu\n])|[^\\'\n])'")
_BLOCK = re.compile(r"/\*|\*/")


def mask_rust(src: str) -> str:
    """Blank comments and string/char literals (newlines kept) so regexes see code only."""
    out, i, n = [], 0, len(src)
    while True:
        m = _TOK.search(src, i)
        if not m:
            out.append(src[i:])
            break
        out.append(src[i:m.start()])
        t = m.group()
        if t.startswith("//"):
            out.append(" " * (m.end() - m.start()))
            i = m.end()
        elif t == "/*":
            depth, j = 1, m.end()
            while depth and j < n:
                mm = _BLOCK.search(src, j)
                if not mm:
                    j = n
                    break
                depth += 1 if mm.group() == "/*" else -1
                j = mm.end()
            seg = src[m.start():j]
            out.append(re.sub(r"[^\n]", " ", seg))
            i = j
        elif t == "'":
            cm = _CHAR.match(src, m.start())
            if cm:
                out.append(" " * (cm.end() - cm.start()))
                i = cm.end()
            else:  # lifetime
                out.append("'")
                i = m.end()
        elif t == '"':
            em = _STR_END.match(src, m.end())
            j = em.end() if em else n
            out.append(re.sub(r"[^\n]", " ", src[m.start():j]))
            i = j
        else:  # raw string  r#"..."#
            hashes = m.group(1)
            close = '"' + hashes
            j = src.find(close, m.end())
            j = n if j < 0 else j + len(close)
            out.append(re.sub(r"[^\n]", " ", src[m.start():j]))
            i = j
    return "".join(out)


# --------------------------------------------------------------------------- public item scan

_QUAL = r'(?:(?:async|unsafe|const|safe|default)[ \t]+|extern[ \t]+(?:"[^"]*"[ \t]+)?)*'
_EVENT = re.compile(
    r"(?P<b>[{};])"
    r"|(?P<item>^[ \t]*pub[ \t]+" + _QUAL +
    r"(?P<kw>fn|struct|enum|trait|type|const|static|union|mod|macro_rules!)(?![A-Za-z0-9_])"
    r"[ \t]*(?:mut[ \t]+)?(?:r#)?(?P<name>[A-Za-z_][A-Za-z0-9_]*)?)",
    re.M,
)
_MACRO_EXPORT = re.compile(
    r"#\[macro_export[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*macro_rules!\s*([A-Za-z_][A-Za-z0-9_]*)"
)
_ATTR = re.compile(r"#!?\[(?:[^\[\]]|\[[^\]]*\])*\]")
_MOD_HDR = re.compile(r"(?:^|\s)mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*$")
_EXTERN_HDR = re.compile(r'(?:^|\s)extern\s*(?:"[^"]*")?\s*$')
_FAMILY = {"fn": "callable", "macro_rules!": "callable", "struct": "type", "enum": "type",
           "union": "type", "type": "type", "trait": "contract", "const": "value", "static": "value"}
_TESTISH_DIR = {"tests", "test", "benches", "bench", "examples", "example", "fixtures", "fixture",
                "testdata", "corpus", "target", "node_modules", "snapshots"}


def _is_test_file(p: Path) -> bool:
    stem = p.stem
    return stem in ("tests", "test") or stem.endswith("_tests") or stem.endswith("_test")


_MOD_DECL = re.compile(r"^[ \t]*(?P<vis>pub(?:[ \t]*\([^)]*\))?[ \t]+)?(?:unsafe[ \t]+)?mod[ \t]+(?P<name>[A-Za-z_][A-Za-z0-9_]*)[ \t]*;", re.M)


def full_module_path(rel: Path) -> tuple:
    """Module path of a source file as a tuple of segments; () for the crate root."""
    parts = list(rel.parts)
    parts[-1] = parts[-1][:-3]
    if len(parts) > 1 and parts[-1] == "mod":
        parts.pop()
    if parts in (["lib"], ["main"]):
        return ()
    return tuple(parts)


def module_name(rel: Path) -> str:
    parts = list(rel.parts)
    parts[-1] = parts[-1][:-3]  # strip .rs
    if len(parts) > 1 and parts[-1] == "mod":
        parts.pop()
    if parts == ["lib"]:
        return "lib"
    return "::".join(parts[:2])


def scan_items(src_root: Path, skip_dirs: bool):
    """Return (total, [ {path, count, items:[{n,f}]} ]) with modules sorted by count desc."""
    mods: dict[str, list] = defaultdict(list)
    seen: set = set()
    decls: dict[tuple, dict] = defaultdict(dict)   # module path -> {child mod name: declared `pub`?}
    contrib: list = []                             # (module name, full module path) per file that yielded items
    if not src_root.is_dir():
        return 0, []
    files = []
    for dp, dns, fns in os.walk(src_root):
        dpp = Path(dp)
        dns[:] = sorted(d for d in dns if not d.startswith(".") and d.lower() not in _TESTISH_DIR
                        and not (skip_dirs and (dpp / d / "Cargo.toml").exists()))
        for fn in fns:
            if fn.endswith(".rs") and fn != "build.rs":
                p = dpp / fn
                if not _is_test_file(p):
                    files.append(p)
    files.sort()
    for p in files:
        try:
            text = p.read_bytes().decode("utf-8", "replace")
        except OSError:
            continue
        if "pub" not in text and "macro_export" not in text:
            continue
        code = mask_rust(text)
        rel = p.relative_to(src_root)
        mod = module_name(rel)
        mp = full_module_path(rel)
        for dm in _MOD_DECL.finditer(code):
            nm = dm.group("name")   # cfg-gated twins (`pub mod x;` / `mod x;`): public if any declaration is
            decls[mp][nm] = decls[mp].get(nm, False) or (dm.group("vis") or "").strip() == "pub"
        before = len(seen)
        # walk braces and item matches in source order; count items only in module scope
        stack: list[str] = []      # kinds: 'mod' | 'extern' | 'test' | 'other'
        bad = 0                    # number of non-module frames on the stack
        boundary = -1
        for m in _EVENT.finditer(code):
            if m.group("b"):
                c = m.group("b")
                if c == "{":
                    if bad:
                        kind = "other"
                    else:
                        raw_hdr = code[boundary + 1:m.start()]
                        hdr = _ATTR.sub(" ", raw_hdr).strip()
                        mm = _MOD_HDR.search(hdr)
                        if mm:
                            kind = "test" if ("cfg(test)" in raw_hdr.replace(" ", "")
                                              or mm.group(1) in ("tests", "test")) else "mod"
                        elif _EXTERN_HDR.search(hdr):
                            kind = "extern"
                        else:
                            kind = "other"
                    stack.append(kind)
                    if kind in ("other", "test"):
                        bad += 1
                elif c == "}":
                    if stack:
                        k = stack.pop()
                        if k in ("other", "test"):
                            bad -= 1
                boundary = m.start()
                continue
            if bad:
                continue
            kw, name = m.group("kw"), m.group("name")
            if kw == "mod" or not name:
                continue
            fam = _FAMILY[kw]
            key = (mod, fam, name)
            if key in seen:
                continue
            seen.add(key)
            mods[mod].append({"n": name, "f": fam})
        for mm in _MACRO_EXPORT.finditer(code):
            key = (mod, "callable", mm.group(1))
            if key not in seen:
                seen.add(key)
                mods[mod].append({"n": mm.group(1), "f": "callable"})
        if len(seen) > before:
            contrib.append((mod, mp))

    def is_private(mp):
        for i in range(1, len(mp) + 1):
            d = decls.get(mp[: i - 1], {}).get(mp[i - 1])
            if d is False:
                return True
        return False

    private = {}
    for mod, mp in contrib:
        private[mod] = private.get(mod, True) and is_private(mp)
    out = [{"path": k, "count": len(v), "items": v, **({"private": True} if private.get(k) else {})}
           for k, v in mods.items() if v]
    out.sort(key=lambda m: (-m["count"], m["path"]))
    return sum(m["count"] for m in out), out


def cap_modules(mods, cap):
    """Keep at most `cap` items, taking largest modules first (modules are pre-sorted)."""
    left, out = cap, []
    for m in mods:
        if left <= 0:
            break
        take = m["items"][:left]
        out.append({"path": m["path"], "count": m["count"], **({"private": True} if m.get("private") else {}), "items": take})
        left -= len(take)
    return out


# --------------------------------------------------------------------------- manifests

def load_toml(p: Path):
    try:
        with open(p, "rb") as f:
            return tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError):
        return None


def find_workspace_package(start: Path):
    d = start
    for _ in range(4):
        d = d.parent
        m = load_toml(d / "Cargo.toml")
        if m and "workspace" in m:
            return m["workspace"].get("package", {})
    return {}


def pkg_field(manifest, field, mdir: Path, ws_pkg=None):
    v = (manifest or {}).get("package", {}).get(field)
    if isinstance(v, dict) and v.get("workspace"):
        wp = ws_pkg if ws_pkg is not None else find_workspace_package(mdir)
        return wp.get(field)
    return v


def dep_tables(manifest):
    """Yield (kind, table) for every dependency table, including target-specific ones."""
    def tbls(m):
        for kind, keys in (("normal", ("dependencies",)), ("dev", ("dev-dependencies", "dev_dependencies")),
                           ("build", ("build-dependencies", "build_dependencies"))):
            for k in keys:
                if isinstance(m.get(k), dict):
                    yield kind, m[k]
    yield from tbls(manifest)
    for _, tm in (manifest.get("target") or {}).items():
        if isinstance(tm, dict):
            yield from tbls(tm)


# --------------------------------------------------------------------------- main build

def main():
    t0 = time.time()
    OUT.mkdir(parents=True, exist_ok=True)

    # ---------------- lock
    lock = tomllib.load(open(ROOT / "Cargo.lock", "rb"))
    lock_pkgs = lock["package"]
    ids, by_name = [], defaultdict(list)  # by_name: name -> [entry index]
    for i, p in enumerate(lock_pkgs):
        ids.append(f'{p["name"]}@{p["version"]}')
        by_name[p["name"]].append(i)
    assert len(set(ids)) == len(ids), "duplicate name@version in lock"
    id_index = {pid: i for i, pid in enumerate(ids)}
    norm = lambda n: n.lower().replace("-", "_")
    lock_norm_names = {norm(p["name"]) for p in lock_pkgs}

    def resolve_dep(s: str):
        parts = s.split(" ", 2)
        name = parts[0]
        cands = by_name.get(name, [])
        if len(parts) >= 2:
            cands = [c for c in cands if lock_pkgs[c]["version"] == parts[1]]
        if len(parts) == 3:
            src = parts[2].strip("()")
            cands = [c for c in cands if lock_pkgs[c].get("source", "").startswith(src.split("#")[0])] or cands
        if len(cands) != 1:
            warn(f"unresolved/ambiguous lock dependency {s!r}: {len(cands)} candidates")
            return None
        return cands[0]

    deps_idx = []
    for p in lock_pkgs:
        ds = sorted({r for r in (resolve_dep(s) for s in p.get("dependencies", [])) if r is not None})
        deps_idx.append(ds)

    # ---------------- workspace members
    root_manifest = load_toml(ROOT / "Cargo.toml")
    wsec = root_manifest["workspace"]
    ws_pkg = wsec.get("package", {})
    ws_deps = wsec.get("dependencies", {})
    excluded = {str((ROOT / e).resolve()) for e in wsec.get("exclude", [])}
    member_dirs = []
    for pat in wsec["members"]:
        for d in sorted(glob.glob(str(ROOT / pat))):
            dp = Path(d)
            if dp.is_dir() and (dp / "Cargo.toml").is_file() and str(dp.resolve()) not in excluded:
                member_dirs.append(dp)
    members = {}  # package name -> dict(dir, manifest)
    for dp in member_dirs:
        mf = load_toml(dp / "Cargo.toml")
        if not mf or "package" not in mf:
            continue
        members[mf["package"]["name"]] = {"dir": dp, "manifest": mf}
    member_idx = {}
    for name in members:
        cands = [i for i in by_name.get(name, []) if "source" not in lock_pkgs[i]]
        if len(cands) != 1:
            warn(f"member {name!r} not found as a unique sourceless lock entry")
            continue
        member_idx[name] = cands[0]
    member_set = set(member_idx.values())

    def kind_of(i):
        p = lock_pkgs[i]
        s = p.get("source")
        if s is None:
            return "yours" if i in member_set else "path"
        if s.startswith("git+"):
            return "git"
        return "registry"

    # ---------------- local source dirs (registry / vendored path / git checkout)
    local_pkg_dirs = {}  # name -> [(dir, version)] for path deps found under vendor/ and patch paths
    for tp in list(glob.glob(str(ROOT / "vendor" / "*" / "Cargo.toml"))):
        mf = load_toml(Path(tp))
        if mf and "package" in mf:
            local_pkg_dirs.setdefault(mf["package"]["name"], []).append(Path(tp).parent)
    for k, spec in (root_manifest.get("patch", {}).get("crates-io", {}) or {}).items():
        if isinstance(spec, dict) and "path" in spec:
            dp = ROOT / spec["path"]
            mf = load_toml(dp / "Cargo.toml")
            if mf and "package" in mf:
                local_pkg_dirs.setdefault(mf["package"]["name"], []).append(dp)
    git_index = defaultdict(list)  # name -> [dir]
    for pat in ("*/*/Cargo.toml", "*/*/*/Cargo.toml"):
        for tp in glob.glob(str(GIT_CHECKOUTS / pat)):
            mf = load_toml(Path(tp))
            if mf and "package" in mf:
                git_index[mf["package"]["name"]].append(Path(tp).parent)

    def source_dir(i):
        p = lock_pkgs[i]
        k = kind_of(i)
        if k == "yours":
            return None
        if k == "registry":
            d = REG_SRC / f'{p["name"]}-{p["version"]}'
            return d if d.is_dir() else None
        if k == "path":
            for d in local_pkg_dirs.get(p["name"], []):
                return d
            return None
        if k == "git":
            sha = p["source"].rsplit("#", 1)[-1][:7]
            cands = git_index.get(p["name"], [])
            pick = [d for d in cands if sha in str(d)] or cands
            return pick[0] if pick else None

    # ---------------- member dependency declarations -> lock ids + rust identifiers
    member_lock_deps = {}
    for name, i in member_idx.items():
        by_pkg = defaultdict(list)
        for d in deps_idx[i]:
            by_pkg[norm(lock_pkgs[d]["name"])].append(d)
        member_lock_deps[name] = by_pkg

    lib_name_memo = {}

    def lib_ident(i):
        if i in lib_name_memo:
            return lib_name_memo[i]
        p = lock_pkgs[i]
        ident = p["name"].replace("-", "_")
        d = source_dir(i)
        if d is not None:
            mf = load_toml(d / "Cargo.toml")
            ln = ((mf or {}).get("lib") or {}).get("name")
            if ln:
                ident = ln.replace("-", "_")
        lib_name_memo[i] = ident
        return ident

    direct = {}                 # member name -> {lock idx of depth-1 dep: ident used in code}
    member_direct_all = set()
    declared_missing = []
    for name, info in members.items():
        if name not in member_idx:
            continue
        mf = info["manifest"]
        idents = defaultdict(set)  # ident -> {lock idx}
        for _, table in dep_tables(mf):
            for key, spec in table.items():
                spec_d = dict(spec) if isinstance(spec, dict) else {"version": spec}
                if spec_d.get("workspace"):
                    base = ws_deps.get(key)
                    base_d = dict(base) if isinstance(base, dict) else {"version": base}
                    spec_d = {**base_d, **{k: v for k, v in spec_d.items() if k != "workspace"}}
                pkg = spec_d.get("package", key)
                cands = member_lock_deps[name].get(norm(pkg), [])
                if not cands:
                    declared_missing.append(f"{name}: {pkg}")
                    continue
                if len(cands) > 1:
                    req = spec_d.get("version")
                    ok = [c for c in cands if req and req_matches(req, lock_pkgs[c]["version"])]
                    cands = ok or cands
                for c in cands:
                    if c in member_set:
                        continue
                    renamed = "package" in spec_d
                    ident = key.replace("-", "_") if renamed else lib_ident(c)
                    idents[ident].add(c)
                    member_direct_all.add(c)
        direct[name] = idents
    if declared_missing:
        warn("Cargo.toml deps absent from Cargo.lock (stale lock?): " + ", ".join(sorted(set(declared_missing))))
    # lock-side direct deps not seen in Cargo.toml scan (should be none)
    lock_direct = set()
    for name, i in member_idx.items():
        lock_direct |= {d for d in deps_idx[i] if d not in member_set}
    if lock_direct - member_direct_all:
        warn("lock lists direct deps not found in member Cargo.toml: " +
             ", ".join(sorted(ids[x] for x in lock_direct - member_direct_all)))

    # ---------------- depth (BFS from all members)
    n = len(lock_pkgs)
    depth = [None] * n
    dq = deque()
    for i in member_set:
        depth[i] = 0
        dq.append(i)
    while dq:
        u = dq.popleft()
        for v in deps_idx[u]:
            if depth[v] is None:
                depth[v] = depth[u] + 1
                dq.append(v)
    unreachable = [ids[i] for i in range(n) if depth[i] is None]
    if unreachable:
        warn(f"{len(unreachable)} lock packages unreachable from members")
    d1 = {i for i in range(n) if depth[i] == 1}
    if d1 != (lock_direct):
        warn(f"depth-1 by BFS ({len(d1)}) differs from union of member direct deps ({len(lock_direct)})")

    # ---------------- via (depth-1 closures)
    via_of = defaultdict(list)
    for r in sorted(d1):
        seen = {r}
        dq = deque([r])
        while dq:
            u = dq.popleft()
            for v in deps_idx[u]:
                if v not in seen and v not in member_set:
                    seen.add(v)
                    dq.append(v)
        for v in seen:
            if v != r:
                via_of[v].append(r)

    dependents = defaultdict(list)
    for u in range(n):
        for v in deps_idx[u]:
            dependents[v].append(u)

    # ---------------- per-package metadata + item scan
    def src_root_of(i):
        """(dir to scan for public items, skip_nested_crates)"""
        k = kind_of(i)
        if k == "yours":
            name = lock_pkgs[i]["name"]
            mdir = members[name]["dir"]
            lp = (members[name]["manifest"].get("lib") or {}).get("path") or "src/lib.rs"
            root = (mdir / lp).parent
            return root, root == mdir
        d = source_dir(i)
        if d is None:
            return None, False
        mf = load_toml(d / "Cargo.toml")
        lp = ((mf or {}).get("lib") or {}).get("path") or "src/lib.rs"
        root = (d / lp).parent
        if not root.is_dir():
            root = d / "src"
        return root, root == d

    def lede_and_license(i):
        p = lock_pkgs[i]
        k = kind_of(i)
        if k == "yours":
            info = members[p["name"]]
            mf, mdir = info["manifest"], info["dir"]
            desc = pkg_field(mf, "description", mdir, ws_pkg)
            lic = pkg_field(mf, "license", mdir, ws_pkg)
            lede = first_sentence(desc)
            if not lede:
                lp = (mf.get("lib") or {}).get("path") or "src/lib.rs"
                cands = [mdir / lp, mdir / "src" / "main.rs"]
                for c in cands:
                    if c.is_file():
                        doc = crate_doc_paragraph(c)
                        if doc:
                            lede = first_sentence(doc)
                            break
            return lede, lic
        d = source_dir(i)
        if d is None:
            return None, None
        mf = load_toml(d / "Cargo.toml")
        if mf is None:
            return None, None
        wp = None
        desc = pkg_field(mf, "description", d, wp)
        lic = pkg_field(mf, "license", d, wp)
        if not lic and (mf.get("package", {}).get("license-file")):
            lic = "see " + str(mf["package"]["license-file"])
        return first_sentence(desc), lic

    # public-item scans are the slow part: fan them out over a few processes
    tasks = {}
    for i in range(n):
        root, skip_nested = src_root_of(i)
        if root:
            tasks[i] = (str(root), skip_nested)
    t1 = time.time()
    scan_results = {}
    with ProcessPoolExecutor(max_workers=SCAN_WORKERS) as ex:
        futs = {i: ex.submit(scan_items, Path(r), sk) for i, (r, sk) in tasks.items()}
        for i, f in futs.items():
            scan_results[i] = f.result()
    print(f"scanned public items of {len(tasks)} crates in {time.time()-t1:.1f}s")

    records = {}
    for i, p in enumerate(lock_pkgs):
        pid = ids[i]
        k = kind_of(i)
        lede, lic = lede_and_license(i)
        if k == "yours":
            latest, newer, yanked = None, [], []
        else:
            latest, newer, yanked = newer_yanked(p["name"], p["version"])
        total, mods = scan_results.get(i, (0, []))
        rec = {
            "id": pid, "name": p["name"], "version": p["version"], "kind": k,
            "lede": lede, "license": lic, "latest": latest, "newer": newer, "yanked": yanked,
            "indexed": (k != "yours" and cache_info(p["name"]) is not None),
            "deps": [ids[d] for d in deps_idx[i]],
            "dependents": sorted(ids[d] for d in dependents[i]),
            "depth": depth[i],
            "via": sorted(ids[v] for v in via_of[i]) if (depth[i] or 0) >= 2 else [],
            "items": total,
            "_mods": mods,
        }
        if k == "yours":
            rec["short"] = Path(members[p["name"]]["dir"]).name
            rec["dir"] = str(Path(members[p["name"]]["dir"]).relative_to(ROOT))
        if k in ("path", "git") and source_dir(i) is None:
            warn(f"no local source found for {k} package {pid}: items/lede unavailable")
        records[pid] = rec

    # ---------------- uses (depth-1 packages only)
    SKIP_DIRS = {"target", "node_modules", "fixtures", "fixture", "testdata", "corpus", "snapshots"}
    WS = re.compile(r"\s*")
    SEG = re.compile(r"(?:r#)?([A-Za-z_][A-Za-z0-9_]*)")
    AS_ALIAS = re.compile(r"as\s+(?:r#)?[A-Za-z_][A-Za-z0-9_]*|as\s+_")

    def parse_entry(s, i, prefix, out):
        i = WS.match(s, i).end()
        if i >= len(s):
            return i
        c = s[i]
        if c == "{":
            i += 1
            guard = 0
            while i < len(s):
                guard += 1
                if guard > 4000:
                    return i
                i = WS.match(s, i).end()
                if i >= len(s):
                    return i
                if s[i] == "}":
                    return i + 1
                if s[i] == ",":
                    i += 1
                    continue
                j = parse_entry(s, i, prefix, out)
                j = WS.match(s, j).end()
                am = AS_ALIAS.match(s, j)
                if am:
                    j = am.end()
                    j = WS.match(s, j).end()
                if j == i:
                    j += 1
                i = j
            return i
        if c == "*":
            if prefix:
                out.append(list(prefix))
            return i + 1
        m = SEG.match(s, i)
        if not m:
            if prefix:
                out.append(list(prefix))
            return i
        seg = m.group(1)
        i = m.end()
        j = WS.match(s, i).end()
        if s.startswith("::", j):
            k = WS.match(s, j + 2).end()
            if k < len(s) and (s[k] == "{" or s[k] == "*" or SEG.match(s, k)):
                return parse_entry(s, k, prefix + [seg] if seg != "self" else prefix, out)
            out.append(prefix + [seg])
            return i
        if seg == "self":
            if prefix:
                out.append(list(prefix))
        else:
            out.append(prefix + [seg])
        return i

    uses = {}  # lock idx -> {"items": Counter, "paths": Counter, "by": Counter}
    for i in d1:
        uses[i] = {"items": Counter(), "paths": Counter(), "by": Counter()}

    member_files = {}
    for name, info in members.items():
        files = []
        for dp, dns, fns in os.walk(info["dir"]):
            dpp = Path(dp)
            dns[:] = sorted(d for d in dns if not d.startswith(".") and d not in SKIP_DIRS
                            and not (dpp != info["dir"] and (dpp / d / "Cargo.toml").exists()))
            files.extend(dpp / f for f in fns if f.endswith(".rs"))
        member_files[name] = sorted(files)

    for name, idents in direct.items():
        if not idents:
            continue
        ident_to_ids = {k: sorted(v & d1) for k, v in idents.items()}
        ident_to_ids = {k: v for k, v in ident_to_ids.items() if v}
        if not ident_to_ids:
            continue
        alt = "|".join(sorted((re.escape(k) for k in ident_to_ids), key=len, reverse=True))
        pre = re.compile(r"(?<![A-Za-z0-9_])(" + alt + r")(?=\s*::)")
        quick = re.compile(r"\b(?:" + alt + r")\s*::")
        for f in member_files[name]:
            try:
                text = f.read_bytes().decode("utf-8", "replace")
            except OSError:
                continue
            if not quick.search(text):
                continue
            code = mask_rust(text)
            for m in pre.finditer(code):
                st = m.start()
                # `.ident::<T>()` is a method call; `a::ident::x` / `T>::ident` is not a crate root
                if st > 0 and code[st - 1] == ".":
                    continue
                if code[max(0, st - 2):st] == "::":
                    b = code[st - 3] if st >= 3 else " "
                    if b.isalnum() or b in "_>)":
                        continue
                j = WS.match(code, m.end()).end() + 2
                out = []
                parse_entry(code, j, [], out)
                targets = ident_to_ids[m.group(1)]
                for path in out:
                    if not path:
                        continue
                    for t in targets:
                        u = uses[t]
                        u["items"][path[0]] += 1
                        u["paths"]["::".join(path)] += 1
                        u["by"][name] += 1

    # ---------------- assemble world.json
    def finish(cap_deep):
        pk = {}
        for pid, rec in records.items():
            r = {k: v for k, v in rec.items() if k != "_mods"}
            cap = ITEM_CAP if (rec["depth"] or 0) < 2 else cap_deep
            r["modules"] = cap_modules(rec["_mods"], cap)
            listed = sum(len(m["items"]) for m in r["modules"])
            r["truncated"] = listed < rec["items"]
            if rec["depth"] == 1:
                u = uses[id_index[pid]]
                sites = sum(u["items"].values())
                r["uses"] = {
                    "items": dict(u["items"].most_common(USES_ITEM_CAP)),
                    "distinct": len(u["items"]),
                    "sites": sites,
                    "by": dict(u["by"].most_common()),
                    "paths": dict(u["paths"].most_common(USES_PATH_CAP)),
                }
            pk[pid] = r
        return pk

    dup = {nm: [ids[i] for i in idxs] for nm, idxs in sorted(by_name.items()) if len(idxs) > 1}

    def build_world(cap_deep, note_extra=""):
        pk = finish(cap_deep)
        member_names = sorted(members)
        world = {
            "project": ROOT.name,
            "generated": time.strftime("%Y-%m-%d"),
            "note": NOTE + note_extra,
            "members": member_names,
            "members_info": {nm: {"short": Path(members[nm]["dir"]).name,
                                  "dir": str(Path(members[nm]["dir"]).relative_to(ROOT))}
                             for nm in member_names},
            "duplicates": dup,
            "packages": pk,
        }
        return world

    def dump(world, path):
        head = {k: v for k, v in world.items() if k != "packages"}
        lines = ["{"]
        for k, v in head.items():
            lines.append(f"  {json.dumps(k)}: {json.dumps(v, ensure_ascii=False)},")
        lines.append('  "packages": {')
        items = list(world["packages"].items())
        for j, (pid, rec) in enumerate(items):
            lines.append(f"    {json.dumps(pid)}: {json.dumps(rec, ensure_ascii=False, separators=(',', ':'))}"
                         + ("," if j < len(items) - 1 else ""))
        lines.append("  }")
        lines.append("}")
        data = "\n".join(lines) + "\n"
        Path(path).write_text(data, encoding="utf-8")
        return len(data.encode("utf-8"))

    world = build_world(ITEM_CAP)
    size = dump(world, OUT / "world.json")
    if size > SIZE_BUDGET:
        world = build_world(ITEM_CAP_DEEP, f"\nsize: world.json exceeded ~6 MB with the full cap, so packages at depth >= 2 list at most {ITEM_CAP_DEEP} items (items stays the true total).")
        size = dump(world, OUT / "world.json")
    print(f"world.json: {size/1e6:.2f} MB, {len(world['packages'])} packages ({time.time()-t0:.1f}s)")

    # ---------------- candidates
    build_candidates(lock_norm_names, ids, id_index, by_name, lock_pkgs, norm)

    for w in WARNINGS:
        print("WARN:", w)
    print(f"done in {time.time()-t0:.1f}s")


NOTE = "\n".join([
    "lede: first sentence of the Cargo.toml description (markdown links/backticks flattened, trailing period dropped, capped at 240 chars); workspace members without a Cargo.toml description (all of them) fall back to the first non-heading paragraph of the crate root's //! doc, else null; every other package uses its description only (all non-members have one).",
    "kind: 'yours' = sourceless lock entry that is a workspace member; 'path' = other sourceless entry (vendor/ patches); 'git' = git+ source; 'registry' = crates.io.",
    "members: lock package names (backend-*), the same keys used by uses.by; members_info gives the short directory name and path.",
    "latest/newer/yanked: from the local sparse-index cache, which is a snapshot from whenever cargo last refreshed it, not live crates.io. Build metadata (+spec-1.1.0) is stripped. newer = every non-yanked version above this one (prereleases skipped unless this version is one). yanked lists every yanked version of the crate, not just newer ones. Members get null/[]. `latest` is null both when the package is already the newest and when there is no cache file; `indexed` tells them apart (false = no cache file, so unknown). path and git packages are compared by version number against crates.io under the same name, which is only indicative.",
    "depth: 0 members, 1 = direct dependency of any member (normal, build, dev, any target), 2+ = shortest distance through the lock graph. Direct deps are cross-checked between Cargo.toml and Cargo.lock.",
    "via: for depth >= 2, the depth-1 packages whose dependency closure contains this package (closure follows the whole lock, all targets and dev/build edges, so it is a superset of what one platform links).",
    "dependents: reverse of deps within this lock only, members included.",
    "items/modules: APPROXIMATE regex scan of the crate's public items (fn/struct/enum/trait/type/const/static/union/macro_rules). Module = file path under the library root (src/, or the directory of [lib] path) without extension, /mod collapsed, root = 'lib', depth capped at 2 levels. Module scope only: brace tracking drops methods and associated items inside impl/trait bodies (impl methods are NOT counted), struct fields, enum variants, macro_rules bodies and #[cfg(test)]/tests inline modules; test files (tests.rs, *_tests.rs) and tests/benches/examples/fixtures dirs are skipped. `pub use` re-exports are not counted, so facade crates under-report; items generated by macros are not counted; inline `pub mod x { }` items are attributed to the file's module; the same (module, family, name) is counted once so cfg-gated twins do not double count. Qualifiers in any order (pub const unsafe fn, pub unsafe extern \"C\" fn) are accepted. Modules whose every contributing file sits under a non-`pub` `mod` declaration (private, e.g. serde_json's lexical) carry `private: true`; their items may still be re-exported, which is not tracked. Each module carries its true `count`; `truncated` is true when the 240-item cap (largest modules first) cut the list; `items` is the true total.",
    "uses: APPROXIMATE, depth-1 packages only. Scans every workspace member's .rs files (src, tests, benches, examples; skips target/, fixtures/, testdata/, corpus/ and nested crates) with comments and string literals blanked, counting `ident::Name` paths and names inside `use ident::{...}` groups (globs and `self` handled, `as` aliases ignored). The ident is the dependency's [lib] name, or the Cargo.toml key when renamed (package = ...). `items` keys are the TOP-LEVEL name after the crate (for tokio::sync::mpsc::Sender that is `sync`); `paths` keeps the top 30 full relative paths (`sync::mpsc::Sender`). Bare uses after an import (`Serialize` after `use serde::Serialize`) and derive/attribute macros written without a path are not seen, so counts under-report. `sites` is the true total; `items` lists the top 150 names (`distinct` is how many exist); `by` is per member package name.",
    "duplicates: crate names with more than one version in this lock (each version is its own package id).",
])


# --------------------------------------------------------------------------- candidates

def build_candidates(lock_norm_names, ids, id_index, by_name, lock_pkgs, norm):
    dir_re = re.compile(r"^(?P<name>.+?)-(?P<ver>\d+\.\d+\.\d+(?:[-+].*)?)$")
    local = defaultdict(list)  # name -> [(sortkey, version, dir)]
    for d in os.listdir(REG_SRC):
        m = dir_re.match(d)
        if not m:
            continue
        k = sv(m.group("ver"))
        if k is None:
            continue
        local[m.group("name")].append((k, m.group("ver"), REG_SRC / d))
    best = {}
    for name, lst in local.items():
        stable = [x for x in lst if not is_pre(strip_build(x[1]))]
        best[name] = max(stable or lst, key=lambda x: x[0])
    local_norm = {}
    for name in best:
        local_norm.setdefault(norm(name), name)

    manifest_memo = {}

    def manifest_of(name):
        if name in manifest_memo:
            return manifest_memo[name]
        b = best.get(name) or best.get(local_norm.get(norm(name), ""), None)
        mf = load_toml(b[2] / "Cargo.toml") if b else None
        manifest_memo[name] = (b, mf)
        return manifest_memo[name]

    def lock_id_for(name, req):
        idxs = _lock_by_norm.get(norm(name), [])
        if not idxs:
            return None
        ok = [i for i in idxs if req and req_matches(req, lock_pkgs[i]["version"])]
        pool = ok or idxs
        return ids[max(pool, key=lambda i: sv(strip_build(lock_pkgs[i]["version"])) or (0, 0, 0, (0,)))]

    _lock_by_norm = defaultdict(list)
    for i, p in enumerate(lock_pkgs):
        _lock_by_norm[norm(p["name"])].append(i)

    def default_features_deps(mf):
        """Keys of optional deps switched on by the crate's default features."""
        feats = mf.get("features") or {}
        keys = set()
        for _, t in dep_tables(mf):
            keys |= {k for k, v in t.items() if isinstance(v, dict) and v.get("optional")}
        on, seen, q = set(), set(), deque(["default"])
        while q:
            f = q.popleft()
            if f in seen:
                continue
            seen.add(f)
            for e in feats.get(f, []):
                if e.startswith("dep:"):
                    on.add(e[4:])
                elif "/" in e:
                    dname = e.split("/")[0]
                    if not dname.endswith("?"):
                        on.add(dname)
                elif e in feats:
                    q.append(e)
                elif e in keys:
                    on.add(e)
        return on

    def brings_of(mf):
        """Non-optional, non-dev deps (plus optional ones on by default): [(pkg name, req, optional?)]."""
        if not mf:
            return []
        enabled = default_features_deps(mf)
        got = {}
        for kind, t in dep_tables(mf):
            if kind == "dev":
                continue
            for key, spec in t.items():
                sd = spec if isinstance(spec, dict) else {"version": spec}
                opt = bool(sd.get("optional"))
                if opt and key not in enabled:
                    continue
                pkg = sd.get("package", key)
                req = sd.get("version", "*")
                if pkg not in got:
                    got[pkg] = (req, opt)
        return [(k, v[0], v[1]) for k, v in sorted(got.items())]

    def closure(names):
        """Transitive closure of not-in-lock names through crates present in the registry src."""
        seen, partial, q = {}, False, deque(names)
        while q:
            nm = q.popleft()
            if norm(nm) in seen:
                continue
            seen[norm(nm)] = nm
            b, mf = manifest_of(nm)
            if not b or mf is None:
                partial = True
                continue
            for dn, _, _ in brings_of(mf):
                if norm(dn) not in lock_norm_names and norm(dn) not in seen:
                    q.append(dn)
        return seen, partial

    scan_memo = {}

    def base_record(name, ver, mf):
        """Fields shared by candidates and `extra` records (same shape as a world.json package)."""
        rid = f"{name}@{ver}"
        if rid in scan_memo:
            return dict(scan_memo[rid])
        pkg = (mf or {}).get("package", {})
        latest, newer, yanked = newer_yanked(name, ver)
        src_root = REG_SRC / f"{name}-{ver}"
        lp = ((mf or {}).get("lib") or {}).get("path") or "src/lib.rs"
        root = (src_root / lp).parent
        if not root.is_dir():
            root = src_root / "src"
        total, mods = scan_items(root, root == src_root)
        capped = cap_modules(mods, ITEM_CAP)
        rec = {
            "id": rid, "name": name, "version": ver, "kind": "registry",
            "lede": first_sentence(pkg.get("description")),
            "license": pkg.get("license") or (("see " + pkg["license-file"]) if pkg.get("license-file") else None),
            "latest": latest, "newer": newer, "yanked": yanked,
            "indexed": cache_info(name) is not None,
            "items": total,
            "modules": capped,
            "truncated": sum(len(m["items"]) for m in capped) < total,
        }
        scan_memo[rid] = rec
        return dict(rec)

    extra = {}
    queries = ["toml", "json", "diff", "color", "http", "parse"]
    result = {"queries": {}}
    for q in queries:
        ql = q.lower()
        matches = []
        for name, b in best.items():
            if norm(name) in lock_norm_names:
                continue
            _, mf = manifest_of(name)
            desc = ((mf or {}).get("package", {}).get("description") or "")
            in_name, in_desc = ql in name.lower(), ql in desc.lower()
            if not (in_name or in_desc):
                continue
            nl = name.lower()
            tier = 0 if nl == ql else 1 if nl.startswith(ql) else 2 if in_name else 3
            matches.append((tier, len(name), name))
        matches.sort()
        recs = []
        for rank, (tier, _, name) in enumerate(matches[:8]):
            b, mf = manifest_of(name)
            ver = b[1]
            new, shared, dep_ids = [], [], []
            for dn, req, opt in brings_of(mf):
                entry = {"name": dn, "req": req}
                if opt:
                    entry["optional"] = True  # on through a default feature
                lid = lock_id_for(dn, req) if norm(dn) in lock_norm_names else None
                if lid:
                    entry["id"] = lid
                    shared.append(entry)
                    dep_ids.append(lid)
                else:
                    bb, bmf = manifest_of(dn)
                    if bb:
                        # present locally: attach a full package record under `extra`
                        xr = base_record(dn, bb[1], bmf)
                        xr["deps"] = [d for d, _, _ in brings_of(bmf)]
                        extra[xr["id"]] = xr
                        entry["id"] = xr["id"]
                        dep_ids.append(xr["id"])
                    new.append(entry)
            seen, partial = closure([e["name"] for e in new])
            rec = base_record(name, ver, mf)
            rec["deps"] = dep_ids
            rec["brings"] = {"new": new, "shared": shared}
            rec["brings_total"] = len(seen)
            rec["brings_total_partial"] = partial
            rec["brings_closure"] = sorted(seen.values())
            rec["match"] = "name" if ql in name.lower() else "description"
            rec["pick"] = False
            rec["_rank"] = rank
            recs.append(rec)
        # ---- pick: most interesting consequences
        def stats(r):
            new_res = sum(1 for e in r["brings"]["new"] if "id" in e)
            return new_res, len(r["brings"]["new"]), len(r["brings"]["shared"])
        strict = [r for r in recs
                  if stats(r)[1] >= 1 and stats(r)[2] >= 2 and stats(r)[0] * 2 > stats(r)[1]]
        pool, basis = (strict, "strict") if strict else (
            [r for r in recs if stats(r)[1] >= 1 and stats(r)[0] >= 1], "relaxed")
        if pool:
            best_r = max(pool, key=lambda r: (min(stats(r)[0], 5), min(stats(r)[2], 5), -r["_rank"]))
            nr, nn, ns = stats(best_r)
            best_r["pick"] = True
            best_r["pick_reason"] = (f"{basis}: brings {nn} new ({nr} resolved locally) and shares {ns} "
                                     f"with the lock" + ("" if basis == "strict" else
                                                          "; no candidate for this query met the shared>=2 rule"))
        for r in recs:
            r.pop("_rank", None)
        result["queries"][q] = recs
    result["extra"] = extra
    result["note"] = "\n".join([
        "Candidates are crates present in the local registry src dir whose normalized name is not in Cargo.lock (any version). Matching: query is a case-insensitive substring of the name or the Cargo.toml description; ranking is exact name, name prefix, name contains, description only, then shorter name; highest local stable version per crate (prerelease only if that is all there is); up to 8 per query. Few hits for `toml`/`json` are real: almost every TOML/JSON crate in the registry dir is already in the lock.",
        "brings: from the candidate's own Cargo.toml, non-dev dependencies that are non-optional, plus optional ones switched on by the crate's default features (marked optional:true). Target-specific and build deps are included, matching the all-targets lock. `new` = name not in the lock; `shared` carries the lock id (the lock version satisfying req, else the highest).",
        "brings.new[].id: when the new crate has source in the registry dir (highest local version), a full package record is stored in the top-level `extra` map under that id; entries without an id could not be resolved locally and carry only name and req. extra records use the world.json package shape minus graph fields, with `deps` as crate NAMES (the candidate's own `deps` are ids).",
        "brings_total: distinct new crate names in the transitive closure of `new`, following each crate's own non-dev deps through the highest version present in the registry src (deps already in the lock are shared and not followed). The candidate itself is not counted. brings_total_partial is true when the closure reached a crate with no local source, which is counted but cannot be followed. brings_closure lists those names; only direct `new` crates get `extra` records.",
        "pick: exactly one candidate per query (when any qualifies) is marked pick:true; the top-level `picks` map gives query -> id, or null when no candidate brings a resolvable new package (true for toml and json: their only candidates bring nothing new). Strict rule: brings >= 1 new package, shares >= 2 lock packages, and more than half of `new` resolved locally; among those, prefer more resolved new packages (capped at 5), then more shared (capped at 5), then better search rank. If nothing meets the strict rule the pick falls back to the candidate with the most resolved new packages and pick_reason says so.",
        "deps: lock ids for shared deps and name@localversion for new deps that exist locally; new deps with no local source appear only in brings. indexed=false means the sparse-index cache has no file for the crate (so latest/newer are unknown rather than 'up to date').",
        "latest/newer/yanked come from the local sparse-index cache (a snapshot). items/modules use the same approximate scan as world.json.",
    ])
    picks = {q: next((r["id"] for r in v if r["pick"]), None) for q, v in result["queries"].items()}
    lines = ["{", f'  "note": {json.dumps(result["note"], ensure_ascii=False)},',
             f'  "picks": {json.dumps(picks)},', '  "queries": {']
    qs = list(result["queries"].items())
    for qi, (q, recs) in enumerate(qs):
        lines.append(f"    {json.dumps(q)}: [")
        for ri, r in enumerate(recs):
            lines.append("      " + json.dumps(r, ensure_ascii=False, separators=(",", ":")) + ("," if ri < len(recs) - 1 else ""))
        lines.append("    ]" + ("," if qi < len(qs) - 1 else ""))
    lines.append("  },")
    lines.append('  "extra": {')
    ex = list(extra.items())
    for ei, (k, r) in enumerate(ex):
        lines.append(f"    {json.dumps(k)}: " + json.dumps(r, ensure_ascii=False, separators=(",", ":")) + ("," if ei < len(ex) - 1 else ""))
    lines.append("  }")
    lines.append("}")
    Path(OUT / "candidates.json").write_text("\n".join(lines) + "\n", encoding="utf-8")
    sz = (OUT / "candidates.json").stat().st_size
    print(f"candidates.json: {sz/1e3:.1f} KB, " + ", ".join(f"{q}={len(v)}" for q, v in result["queries"].items())
          + f"; extra={len(extra)}; picks=" + ", ".join(
              f"{q}:{next((r['name'] for r in v if r['pick']), None)}" for q, v in result["queries"].items()))

if __name__ == "__main__":
    main()
