#!/usr/bin/env python3
"""Three extensions to the design board's data: module docs, feature graphs, a registry corpus.

Reads only local files (no cargo, no network, no git mutation) and reuses extract_world.py and
extract_trust.py (lock/semver/sparse-cache helpers, Rust masking, the item scan, README and
trust scans). Run it AFTER both of them: it post-processes their output and takes the package
list from data/world.json and data/trust.json.

  1. docs       data/pkg/<file>.json   every module gains  "doc"  and  "doc_full"
  2. features   data/trust.json        every record gains  "feature_graph"  and  "feature_deps_size"
  3. registry   data/registry.json     crates in the registry src dir that are not in Cargo.lock
                data/categories.json   category -> crates, plus a "yours" map

Steps 1 and 2 rewrite their files IN PLACE, keeping every existing field, and are idempotent (a
re-run overwrites the added fields with identical values). Re-running extract_trust.py regenerates
the pkg files and trust.json from scratch, so run this script again after it.

    python3 extract_more.py                  # all three
    python3 extract_more.py docs features    # a subset

--------------------------------------------------------------------------- 1. module docs

"doc"       first sentence of the module's own `//!` inner doc, else of the `///` on its `mod x;`
            declaration in the parent (any visibility; the declaration is found even when it shares
            a line with `#[cfg(..)]`), else null. Same text rules as the item `d` field and the
            world.json `lede`: markdown links reduced to their text, backticks dropped, ~240 cap.
"doc_full"  the first paragraph of that same doc, links reduced to their text, backticked code KEPT,
            cut to <= 400 characters at a sentence end when one falls past 40%, else at a word +
            "...". null exactly when "doc" is null.
Module -> file: a module key in data/pkg is a file path under the library root, depth capped at
2 ("a::b" is a/b.rs or a/b/mod.rs; deeper files fold into it), "lib" is the crate root. "lib" takes
its doc from lib.rs. The doc source is the module's own primary file only, never a folded child.
Headings, fenced and indented code, lists, tables, link definitions, badge rows and HTML blocks are
skipped when looking for the first paragraph; a doc whose first section heading is Examples, Usage,
Errors, Panics, Safety... (before any prose) counts as having no summary. `#![doc = include_str!(..)]`
(also inside cfg_attr) and `#![doc = "..."]` are honoured, in source order with `//!` lines; an
included README is read with the same paragraph rules as the trust.json readme.

--------------------------------------------------------------------------- 2. feature graph

"feature_graph": { "<feature>": { "enables": [features], "deps": [optional deps], "default": bool } }
  * keys: every feature in `[features]` except `default` itself (which is the `default` flag), plus
    the implicit feature Cargo creates for each optional dependency that no `dep:` entry mentions.
  * enables: entries that name another feature of this crate, in manifest order.
  * deps: optional dependencies the feature activates, by manifest key: `dep:x`, a bare `x` that is
    not a feature but an optional dependency, and `x/feat` (just `x`) when x is optional. A weak
    `x?/feat` activates nothing and is not recorded; `x/feat` on a non-optional x is not recorded.
    An implicit feature `x` carries deps [x].
  * default: true when the feature is reachable from `default` through `enables`; an implicit feature
    `x` is true when its optional dependency is activated by the default list or by any default-on
    feature (which is how Cargo behaves: activating an optional dependency turns its implicit feature on).
"feature_deps_size": { "<optional dep key>": sloc | null } for every optional dependency (normal,
  build and target tables); sloc from trust.json when the dependency's package is a lock package
  (world.json), picking the lock version that satisfies the requirement, else the highest; null
  otherwise. Records with no local source carry null for both fields.

--------------------------------------------------------------------------- 3. registry corpus

Every crate NAME in the registry src dir whose normalised name (lower case, - == _) is not in
Cargo.lock under any version, at its highest local version (stable preferred, as in candidates.json).
registry.json is { "<name@version>": record }, sorted by name. Record fields:
  id name version lede license categories keywords repository edition rust_version
  releases {n, first, last} | null   (index cache; n counts unyanked versions, as in trust.json)
  items modules                      counts only (same scan as world.json)
  sloc unsafe forbid_unsafe build_rs proc_macro   (same scans as trust.json)
  caps {net, fs, process, env, ffi}  the per-capability counts only
  brings { new, shared, new_total, new_partial }
      new       package names of its non-optional, non-dev dependencies (normal, build, target
                tables) that are not in the lock
      shared    the same kind of dependency when it IS in the lock, as lock ids (the lock version
                that satisfies the requirement, else the highest)
      new_total distinct not-in-lock crates in the transitive closure of `new` (the direct ones
                included), following each crate's own non-optional non-dev deps through the highest
                local version in the registry src dir; a crate with no local source is counted but
                cannot be followed, and new_partial says one was hit
  readme     first README paragraph, <= 300 characters (160 if registry.json would top 3 MB)
categories.json is { "<category slug>": { "count": n, "crates": [ids] } , "yours": { "<slug>": [lock
ids] } }: crates = the lock packages AND the corpus crates that declare the category (so `count` is
their union), yours = only the lock packages, so the view can split "in your project" from "not yet".

Standard library only (Python >= 3.11 for tomllib).
"""
from __future__ import annotations

import bisect
import json
import os
import re
import sys
import time
from collections import Counter, defaultdict, deque
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import extract_world as ew
import extract_trust as tr

HERE, ROOT, OUT = ew.HERE, ew.ROOT, ew.OUT
PKG_DIR = OUT / "pkg"
REG_SRC, REG_CACHE = ew.REG_SRC, ew.REG_CACHE

WORKERS = 4                 # other agents are building; keep the fan-out modest
DOC_FULL_MAX = 400
REGISTRY_README_MAX = 300
REGISTRY_README_TRIM = 160
REGISTRY_BUDGET = 3 * 1024 * 1024
STEPS = ("docs", "features", "registry")

_norm = lambda n: n.lower().replace("-", "_")
_DIR_RE = re.compile(r"^(?P<name>.+?)-(?P<ver>\d+\.\d+\.\d+(?:[-+].*)?)$")


# --------------------------------------------------------------------------- shared: where crates live

def load_lock():
    with open(ROOT / "Cargo.lock", "rb") as fh:
        pk = ew.tomllib.load(fh)["package"]
    return [{"name": p["name"], "version": p["version"], "source": p.get("source")} for p in pk]


def build_dir_resolver(world, lock):
    """pid -> crate dir (or None) for every world.json package; everything else is a registry crate."""
    root_manifest = ew.load_toml(ROOT / "Cargo.toml")
    lock_src = {f'{p["name"]}@{p["version"]}': p["source"] for p in lock}
    local_pkg_dirs = {}
    for tp in tr.glob_paths(ROOT / "vendor" / "*" / "Cargo.toml"):
        mf = ew.load_toml(Path(tp))
        if mf and "package" in mf:
            local_pkg_dirs.setdefault(mf["package"]["name"], []).append(Path(tp).parent)
    for spec in (root_manifest.get("patch", {}).get("crates-io", {}) or {}).values():
        if isinstance(spec, dict) and "path" in spec:
            dp = ROOT / spec["path"]
            mf = ew.load_toml(dp / "Cargo.toml")
            if mf and "package" in mf:
                local_pkg_dirs.setdefault(mf["package"]["name"], []).append(dp)
    git_index = defaultdict(list)
    for pat in ("*/*/Cargo.toml", "*/*/*/Cargo.toml"):
        for tp in tr.glob_paths(ew.GIT_CHECKOUTS / pat):
            mf = ew.load_toml(Path(tp))
            if mf and "package" in mf:
                git_index[mf["package"]["name"]].append(Path(tp).parent)

    def dir_of(pid):
        r = world["packages"].get(pid)
        if r is None:                         # candidate / extra / extra on-disk version
            name, ver = pid.rsplit("@", 1)
            d = REG_SRC / f"{name}-{ver}"
        else:
            k, name = r["kind"], r["name"]
            if k == "yours":
                d = ROOT / r["dir"]
            elif k == "path":
                d = (local_pkg_dirs.get(name) or [None])[0]
            elif k == "git":
                cands = git_index.get(name, [])
                sha = (lock_src.get(pid) or "").rsplit("#", 1)[-1][:7]
                pick = [x for x in cands if sha and sha in str(x)] or cands
                d = pick[0] if pick else None
            else:
                d = REG_SRC / f'{name}-{r["version"]}'
        return Path(d) if d is not None and Path(d).is_dir() else None

    def kind_of(pid):
        r = world["packages"].get(pid)
        return r["kind"] if r else "registry"

    return dir_of, kind_of


def lib_root(d: Path, mf, kind: str):
    """(library root dir, crate root file) exactly as extract_trust.analyze picks the scan root."""
    lp = ((mf or {}).get("lib") or {}).get("path") or "src/lib.rs"
    root = (d / lp).parent
    if kind != "yours" and not root.is_dir():
        root = d / "src"
    return root, d / lp


def read_text(p: Path):
    try:
        return p.read_bytes().decode("utf-8", "replace").lstrip("﻿")
    except OSError:
        return None


def write_lines_map(path: Path, items):
    """{ "id": {record} } one record per line, exactly the style extract_trust.py writes."""
    lines = ["{"]
    for j, (k, rec) in enumerate(items):
        lines.append(f"  {json.dumps(k)}: {json.dumps(rec, ensure_ascii=False, separators=(',', ':'))}"
                     + ("," if j < len(items) - 1 else ""))
    lines.append("}")
    data = "\n".join(lines) + "\n"
    Path(path).write_text(data, encoding="utf-8")
    return len(data.encode("utf-8"))


# --------------------------------------------------------------------------- 1. module docs

_SECTION_HEADS = set(tr._STOP_HEADINGS) | {"errors", "panics", "safety", "features", "feature flags", "cargo features",
                                           "platform support", "platform-specific behavior", "see also"}
_DECL_RE = re.compile(
    r"^[ \t]*(?:#\[[^\]\n]*\][ \t]*)*(?:pub(?:[ \t]*\([^)]*\))?[ \t]+)?(?:unsafe[ \t]+)?mod[ \t]+(?:r#)?"
    r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)[ \t]*;", re.M)
_STR_LIT = re.compile(r'"(?:[^"\\]|\\.)*"')
_BARE_BRACKET = re.compile(r"(?<![\[\]])\[([^\[\]\n]*)\](?![(\[:])")
_LIST_START = re.compile(r"^(\||[-*+]\s|\d+[.)]\s|\[[^\]]+\]:\s)")


def _label(x: str) -> str:
    return " ".join(x.lower().split())


def _protect_prose_brackets(s: str, labels=frozenset()) -> str:
    """Escape `[u8; 4]`, `foo[0]`, `&[u8]` so the link stripper leaves them alone; `[Foo]`, `[`Foo`]`,
    `[crate::Foo]` and shortcut references to a `[label]: url` definition are links and are left for it
    to reduce to their text."""
    def fix(m):
        inner = m.group(1)
        if re.fullmatch(r"`[^`]+`", inner) or _label(inner) in labels:
            return m.group(0)
        before = s[m.start() - 1] if m.start() else " "
        if before.isalnum() or before in "_&>)" or (re.search(r"[\s;,]", inner) and "`" not in inner) or not inner:
            return "\\[" + inner + "\\]"
        return m.group(0)
    return _BARE_BRACKET.sub(fix, s)


_INTRAWORD_EM = re.compile(r"(?<![\w`*])\*([A-Za-z0-9]+)\*(?![`*])")


def _clean_doc(raw: str, labels=frozenset()) -> str:
    """Links reduced to their text, code spans kept; also `*ser*ializing` -> `serializing` outside code."""
    raw = re.sub(r"\]\(\s+", "](", raw)                        # `[text](\n url)`
    raw = re.sub(r"(\]\([^)\s]*)\s+\)", r"\1)", raw)
    parts = re.split(r"(`[^`]*`)", tr._clean_inline(_protect_prose_brackets(raw, labels)))
    return "".join(x if i % 2 else _INTRAWORD_EM.sub(r"\1", x) for i, x in enumerate(parts))


def first_paragraph(text: str):
    """Cleaned first prose paragraph of a markdown text (links reduced, backticks kept), or None."""
    t = tr._pre_html(text)
    labels = {_label(m.group(1)) for m in re.finditer(r"^\s*\[([^\]]+)\]:\s*\S", t, re.M)}
    blocks, cur, fence = [], [], None

    def flush():
        if cur:
            blocks.append(("p", " ".join(cur)))
            cur.clear()

    for line in t.split("\n"):
        st = line.strip()
        if fence:
            if st.startswith(fence):
                fence = None
            continue
        fm = re.match(r"^(```+|~~~+)", st)
        if fm and st.count(fm.group(1)[:3]) < 2:
            flush()
            fence = fm.group(1)[:3]
            continue
        if st == "":
            flush()
            continue
        if re.match(r"^#{1,6}(\s|$)", st):
            flush()
            blocks.append(("h", st.lstrip("#").strip()))
            continue
        if re.match(r"^(={3,}|-{3,})$", st) and cur:                 # setext heading
            blocks.append(("h", " ".join(cur)))
            cur.clear()
            continue
        if re.match(r"^([-*_])(\s*\1){2,}\s*$", st):
            flush()
            continue
        if line.startswith("    ") and not cur:                      # indented code
            continue
        if cur and _LIST_START.match(st):
            flush()
        cur.append(st)
    flush()
    title = None                 # first heading that is not a section name: the summary when no prose precedes a section
    for kind, raw in blocks:
        if kind == "h":
            c = _clean_doc(raw, labels)
            if c.lower().strip(" :#") in _SECTION_HEADS:
                return title
            if title is None and len(re.findall(r"[A-Za-z]", c)) >= 2:
                title = c
            continue
        if _LIST_START.match(raw) or tr._nav_row(raw):
            continue
        c = _clean_doc(raw, labels)
        if len(re.findall(r"[A-Za-z]", c)) >= 2:
            return c
    return title


def _balanced_attr(lines, i):
    """Text of the (possibly multi-line) attribute starting at lines[i], and the index after it."""
    buf, depth, j = [], 0, i
    while j < len(lines):
        buf.append(lines[j].strip())
        code = _STR_LIT.sub('""', lines[j])
        depth += code.count("[") - code.count("]")
        j += 1
        if depth <= 0:
            break
    return " ".join(buf), j


_INC_REL = re.compile(r'doc\s*=\s*include_str!\(\s*"([^"]+)"\s*\)')
_INC_MANIFEST = re.compile(r'doc\s*=\s*include_str!\(\s*concat!\(\s*env!\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*"([^"]+)"\s*\)\s*\)')


def inner_doc_pieces(text: str, fdir: Path, mdir: Path):
    """The crate/module inner docs at the top of a file, in source order: [("lines", [str]) | ("readme", text)]."""
    lines = text.split("\n")
    pieces, cur, i, n = [], [], 0, len(lines)

    def flush():
        if cur:
            pieces.append(("lines", list(cur)))
            cur.clear()

    while i < n:
        s = lines[i].strip()
        if not s:
            i += 1
        elif s.startswith("//!"):
            b = s[3:]
            cur.append(b[1:] if b.startswith(" ") else b)
            i += 1
        elif s.startswith("///") and not s.startswith("////"):
            break
        elif s.startswith("//"):
            i += 1
        elif s.startswith("/*!"):
            body, j = [s[3:]], i
            while "*/" not in lines[j] and j + 1 < n:
                j += 1
                body.append(lines[j])
            joined = "\n".join(body).split("*/", 1)[0]
            for ln in joined.split("\n"):
                ln = re.sub(r"^\s*\*(?!/)\s?", "", ln)
                cur.append(ln[1:] if ln.startswith(" ") else ln)
            i = j + 1
        elif s.startswith("/*"):
            j = i
            while "*/" not in lines[j] and j + 1 < n:
                j += 1
            i = j + 1
        elif s.startswith("#!") and not s.startswith("#!["):        # shebang
            i += 1
        elif s.startswith("#!["):
            attr, i = _balanced_attr(lines, i)
            inc, base = _INC_REL.search(attr), fdir
            if not inc:
                inc, base = _INC_MANIFEST.search(attr), mdir
            if inc:
                flush()
                body = read_text(base / inc.group(1).lstrip("/"))
                if body:
                    pieces.append(("readme", body))
                continue
            lit = re.fullmatch(r'#!\[\s*doc\s*=\s*("(?:[^"\\]|\\.)*")\s*\]', attr)
            if lit:
                try:
                    val = json.loads(lit.group(1))
                except ValueError:
                    val = lit.group(1)[1:-1]
                for ln in val.split("\n"):
                    cur.append(ln[1:] if ln.startswith(" ") else ln)
        else:
            break
    flush()
    return pieces


def piece_paragraph(piece):
    kind, body = piece
    if kind == "lines":
        return first_paragraph("\n".join(body))
    paras = tr.readme_paragraphs(body, want=1)
    return paras[0] if paras else None


def outer_doc_lines(lines, li):
    """`///` lines directly above line `li` (attributes may sit in between), in source order."""
    i, doc = li - 1, []
    while i >= 0:
        s = lines[i].strip()
        if s.startswith("///") and not s.startswith("////"):
            b = s[3:]
            doc.append(b[1:] if b.startswith(" ") else b)
            i -= 1
        elif s.startswith("#[") or s.startswith("#!["):
            i -= 1
        elif s.endswith("]") and not s.startswith("//"):            # tail of a multi-line attribute
            j = i - 1
            while j >= 0 and i - j <= 20 and not lines[j].strip().startswith("#["):
                j -= 1
            if j >= 0 and lines[j].strip().startswith("#["):
                i = j - 1
            else:
                break
        else:
            break
    doc.reverse()
    return doc


class _Files:
    """Per-crate cache of parsed source files."""

    def __init__(self, mdir: Path):
        self.mdir = mdir
        self.text, self.inner, self.decls = {}, {}, {}

    def get(self, p: Path):
        if p not in self.text:
            self.text[p] = read_text(p)
        return self.text[p]

    def inner_doc(self, p: Path):
        """(cleaned first paragraph or None, source tag) from the file's own inner docs."""
        if p not in self.inner:
            t = self.get(p)
            res = (None, None)
            if t:
                for piece in inner_doc_pieces(t, p.parent, self.mdir):
                    para = piece_paragraph(piece)
                    if para:
                        res = (para, "include" if piece[0] == "readme" else "inner")
                        break
            self.inner[p] = res
        return self.inner[p]

    def decl_doc(self, p: Path, name: str):
        """Cleaned first paragraph of the `///` on the declaration `mod <name>;` in file p."""
        t = self.get(p)
        if not t or not re.search(r"\bmod\s+(?:r#)?" + re.escape(name) + r"\s*;", t):
            return None
        if p not in self.decls:
            code = ew.mask_rust(t)
            nl = [m.start() for m in re.finditer("\n", code)]
            d = defaultdict(list)
            for m in _DECL_RE.finditer(code):
                d[m.group("name")].append(bisect.bisect_left(nl, m.start("name")))
            self.decls[p] = (d, t.split("\n"))
        d, lines = self.decls[p]
        for li in d.get(name, []):
            doc = outer_doc_lines(lines, li)
            if doc:
                para = first_paragraph("\n".join(doc))
                if para:
                    return para
        return None


def _module_file(root: Path, segs: tuple):
    """Primary source file of module `segs` under the library root (a/b.rs or a/b/mod.rs), or None."""
    if not segs:
        return None
    base = root.joinpath(*segs)
    for c in (base.with_name(base.name + ".rs"), base / "mod.rs"):
        if c.is_file():
            return c
    return None


def module_doc(files: _Files, root: Path, rootfile: Path, name: str):
    """(cleaned first paragraph | None, source tag | None) for one module key of a detail file."""
    if name == "lib":
        f = root / "lib.rs"
        f = f if f.is_file() else rootfile
        para, src = files.inner_doc(f)
        return (para, src) if para else (None, None)
    segs = tuple(name.split("::"))
    own = _module_file(root, segs)
    if own is not None:
        para, src = files.inner_doc(own)
        if para:
            return para, src
    parent = rootfile if len(segs) == 1 else _module_file(root, segs[:-1])
    if parent is not None:
        para = files.decl_doc(parent, segs[-1])
        if para:
            return para, "decl"
    return None, None


def doc_fields(para):
    if not para:
        return None, None
    return ew.first_sentence(para), tr.assemble_readme([para], DOC_FULL_MAX)


def docs_job(job):
    """Add doc / doc_full to every module of one data/pkg file (rewritten in place)."""
    pid, fpath, d = job["id"], Path(job["file"]), Path(job["dir"]) if job["dir"] else None
    raw = fpath.read_text(encoding="utf-8")
    obj = json.loads(raw)
    stats = Counter()
    if d is None:
        stats["no_source_dir"] = 1
        stats["modules"] = len(obj["modules"])
        return pid, stats
    mf = ew.load_toml(d / "Cargo.toml")
    root, rootfile = lib_root(d, mf, job["kind"])
    files = _Files(d)
    mods = []
    for m in obj["modules"]:
        para, src = module_doc(files, root, rootfile, m["path"])
        doc, full = doc_fields(para)
        new = {}
        for k, v in m.items():
            if k in ("doc", "doc_full"):
                continue
            new[k] = v
            if k == "private":
                new["doc"], new["doc_full"] = doc, full
        if "doc" not in new:
            new["doc"], new["doc_full"] = doc, full
        mods.append(new)
        stats["modules"] += 1
        if doc:
            stats["doc"] += 1
            stats["src_" + src] += 1
            if m["path"] == "lib":
                stats["lib_doc"] += 1
        elif m["path"] == "lib":
            stats["lib_null"] += 1
    obj["modules"] = mods
    out = json.dumps(obj, ensure_ascii=False, separators=(",", ":"))
    if out != raw:
        fpath.write_text(out, encoding="utf-8")
    return pid, stats


def step_docs(world, dir_of, kind_of):
    t0 = time.time()
    jobs = []
    for f in sorted(PKG_DIR.glob("*.json")):
        pid = re.match(r'\{"id":"([^"]+)"', f.read_text(encoding="utf-8")[:400]).group(1)
        d = dir_of(pid)
        jobs.append({"id": pid, "file": str(f), "dir": str(d) if d else None, "kind": kind_of(pid)})
    total = Counter()
    per = {}
    with ProcessPoolExecutor(max_workers=WORKERS) as ex:
        for pid, st in ex.map(docs_job, jobs, chunksize=8):
            total.update(st)
            per[pid] = st
    print(f"[docs] {len(jobs)} detail files, {total['modules']} modules, {total['doc']} with a doc "
          f"({100 * total['doc'] / max(1, total['modules']):.1f}%) in {time.time() - t0:.1f}s")
    print(f"       by source: own //! {total['src_inner']}, `///` on the mod declaration {total['src_decl']}, "
          f"#![doc = include_str!] / README {total['src_include']}; null {total['modules'] - total['doc']}")
    print(f"       crate-root 'lib' modules: {total['lib_doc']} with a doc, {total['lib_null']} null "
          f"({len(per) - total['lib_doc'] - total['lib_null']} detail files have no 'lib' module)")
    if total["no_source_dir"]:
        print(f"       WARN: {total['no_source_dir']} detail files had no local source dir and were left without docs")
    return total


# --------------------------------------------------------------------------- 2. feature graph

def dep_declarations(mf, ws_deps):
    """{manifest key: {"pkg", "req", "optional", "nonopt"}} over normal, build and target tables (never dev)."""
    out = {}
    for kind, table in ew.dep_tables(mf):
        if kind == "dev":
            continue
        for key, spec in table.items():
            sd = dict(spec) if isinstance(spec, dict) else {"version": spec}
            if sd.get("workspace"):
                base = ws_deps.get(key)
                base = dict(base) if isinstance(base, dict) else {"version": base}
                sd = {**base, **{k: v for k, v in sd.items() if k != "workspace"}}
            e = out.setdefault(key, {"pkg": sd.get("package", key), "req": sd.get("version") or "*",
                                     "optional": False, "nonopt": False})
            if sd.get("optional"):
                e["optional"] = True
            else:
                e["nonopt"] = True
    return out


def feature_graph(mf, decls):
    """(graph, optional dep keys in manifest order, counters, optional deps active by default). See the docstring, section 2."""
    feats = {k: v for k, v in ((mf or {}).get("features") or {}).items() if isinstance(v, list)}
    optional = [k for k, e in decls.items() if e["optional"]]
    opt_set = set(optional)
    referenced = {e[4:] for v in feats.values() for e in v if isinstance(e, str) and e.startswith("dep:")}
    implicit = [k for k in optional if k not in feats and k not in referenced]
    graph, weak = {}, 0
    for f, entries in feats.items():
        if f == "default":
            continue
        enables, deps = [], []
        for e in entries:
            if not isinstance(e, str):
                continue
            if e.startswith("dep:"):
                x = e[4:]
                if x not in deps:
                    deps.append(x)
            elif "/" in e:
                x = e.split("/", 1)[0]
                if x.endswith("?"):
                    weak += 1
                elif x in opt_set and x not in deps:
                    deps.append(x)
            elif e in feats:
                if e != "default" and e not in enables:
                    enables.append(e)
            elif e in opt_set and e not in deps:
                deps.append(e)
        graph[f] = {"enables": enables, "deps": deps, "default": False}
    for k in implicit:
        graph[k] = {"enables": [], "deps": [k], "default": False}
    # default set: features reachable from `default` through `enables`; an implicit feature is on when its
    # optional dependency is activated by any default-on feature (or by the default list itself)
    on, active, q = set(), set(), deque(["default"])
    while q:
        for e in feats.get(q.popleft(), []):
            if not isinstance(e, str):
                continue
            if e.startswith("dep:"):
                active.add(e[4:])
            elif "/" in e:
                x = e.split("/", 1)[0]
                if x in opt_set:
                    active.add(x)
            elif e in feats:
                if e != "default" and e not in on:
                    on.add(e)
                    q.append(e)
            elif e in opt_set:
                active.add(e)
    for f in on:
        graph[f]["default"] = True
        active.update(graph[f]["deps"])
    for k in implicit:
        if k in active:
            graph[k]["default"] = True
    return graph, optional, {"implicit": len(implicit), "weak": weak}, active


def lock_pick(by_norm, lock_ids, name, req):
    """The lock id for package `name` best matching `req` (satisfying version, else the highest), or None."""
    idxs = by_norm.get(_norm(name), [])
    if not idxs:
        return None
    ok = [i for i in idxs if req and ew.req_matches(req, lock_ids[i][1])]
    pool = ok or idxs
    i = max(pool, key=lambda j: ew.sv(ew.strip_build(lock_ids[j][1])) or (0, 0, 0, (0,)))
    return lock_ids[i][0]


def step_features(world, trust, dir_of, kind_of, lock):
    t0 = time.time()
    root_manifest = ew.load_toml(ROOT / "Cargo.toml") or {}
    ws_deps = (root_manifest.get("workspace") or {}).get("dependencies", {}) or {}
    lock_ids = [(f'{p["name"]}@{p["version"]}', p["version"]) for p in lock]
    by_norm = defaultdict(list)
    for i, p in enumerate(lock):
        by_norm[_norm(p["name"])].append(i)
    n_with = n_null = n_feat = n_edges = n_opt = n_sized = 0
    counters = Counter()
    for pid, rec in trust.items():
        d = dir_of(pid)
        mf = ew.load_toml(d / "Cargo.toml") if d else None
        if mf is None:
            rec["feature_graph"], rec["feature_deps_size"] = None, None
            n_null += 1
            continue
        decls = dep_declarations(mf, ws_deps)
        graph, optional, c, _ = feature_graph(mf, decls)
        counters.update(c)
        sizes = {}
        for key in optional:
            e = decls[key]
            lid = lock_pick(by_norm, lock_ids, e["pkg"], e["req"])
            t = trust.get(lid) if lid else None
            sizes[key] = t["sloc"] if t else None
        rec["feature_graph"], rec["feature_deps_size"] = graph, sizes
        n_with += 1
        n_feat += len(graph)
        n_edges += sum(len(g["deps"]) for g in graph.values())
        n_opt += len(sizes)
        n_sized += sum(1 for v in sizes.values() if v is not None)
        counters["with_features"] += 1 if graph else 0
        counters["default_on"] += sum(1 for g in graph.values() if g["default"])
    size = write_lines_map(OUT / "trust.json", list(trust.items()))
    print(f"[features] {n_with} records with a feature graph ({counters['with_features']} non-empty), {n_null} without local source; "
          f"{n_feat} features ({counters['implicit']} implicit, {counters['default_on']} on by default), "
          f"{n_edges} feature->dep edges, {counters['weak']} weak x?/f entries ignored; "
          f"{n_opt} optional deps, {n_sized} sized from the lock, {n_opt - n_sized} null "
          f"-> trust.json {size / 1e6:.2f} MB in {time.time() - t0:.1f}s")
    return size


# --------------------------------------------------------------------------- 3. registry corpus

def default_on_optional(mf, decls):
    """Keys of optional deps switched on by the crate's default features (for the report only)."""
    return feature_graph(mf, decls)[3]


def registry_job(job):
    name, ver, d = job["name"], job["version"], Path(job["dir"])
    mf = ew.load_toml(d / "Cargo.toml")
    pkg = (mf or {}).get("package", {})
    root, _ = lib_root(d, mf, "registry")
    total, mods = ew.scan_items(root, root == d)
    f = tr.manifest_fields(mf, d, None)
    t = tr.trust_scan(d, mf, root)
    decls = dep_declarations(mf, {}) if mf else {}
    nonopt = sorted({(e["pkg"], e["req"]) for e in decls.values() if e["nonopt"]})
    default_opt = sorted({decls[k]["pkg"] for k in default_on_optional(mf, decls) if k in decls and not decls[k]["nonopt"]}) if mf else []
    lic = pkg.get("license") or (("see " + pkg["license-file"]) if pkg.get("license-file") else None)
    paras = t["readme_paras"]
    return {
        "id": f"{name}@{ver}", "name": name, "version": ver,
        "lede": ew.first_sentence(pkg.get("description")),
        "license": lic, "categories": f["categories"], "keywords": f["keywords"],
        "repository": f["repository"], "edition": f["edition"], "rust_version": f["rust_version"],
        "items": total, "modules": len(mods),
        "sloc": t["sloc"], "unsafe": t["unsafe"], "forbid_unsafe": t["forbid_unsafe"],
        "build_rs": bool((t["build_rs"] or f["build_set"]) and not f["build_off"]),
        "proc_macro": f["proc_macro"],
        "caps": {c: t["caps"][c + "_n"] for c in tr.CAPS},
        "_deps": nonopt, "_default_opt": default_opt,
        "_readme": paras[0] if paras else None,
    }


def step_registry(world, trust, dir_of, kind_of, lock):
    t0 = time.time()
    lock_norm = {_norm(p["name"]) for p in lock}
    lock_ids = [(f'{p["name"]}@{p["version"]}', p["version"]) for p in lock]
    by_norm = defaultdict(list)
    for i, p in enumerate(lock):
        by_norm[_norm(p["name"])].append(i)

    # ---- highest local version per crate name (stable preferred, as in candidates.json)
    local = defaultdict(list)
    for dn in os.listdir(REG_SRC):
        m = _DIR_RE.match(dn)
        if m and ew.sv(m.group("ver")) is not None and (REG_SRC / dn).is_dir():
            local[m.group("name")].append((ew.sv(m.group("ver")), m.group("ver")))
    best, literal_diff = {}, 0
    for name, lst in local.items():
        stable = [x for x in lst if not ew.is_pre(ew.strip_build(x[1]))]
        best[name] = max(stable or lst)[1]
        literal_diff += best[name] != max(lst)[1]
    local_norm = {}
    for name in best:
        local_norm.setdefault(_norm(name), name)
    corpus_names = sorted(n for n in best if _norm(n) not in lock_norm)
    print(f"[registry] {len(local)} crate names in the registry src dir, {len(local) - len(corpus_names)} in the lock "
          f"-> corpus {len(corpus_names)} ({literal_diff} names where the highest stable differs from the literal highest)")

    # ---- per-crate scans
    jobs = [{"name": n, "version": best[n], "dir": str(REG_SRC / f"{n}-{best[n]}")} for n in corpus_names]
    recs = {}
    with ProcessPoolExecutor(max_workers=WORKERS) as ex:
        for r in ex.map(registry_job, jobs, chunksize=4):
            recs[r["name"]] = r
    print(f"           scanned {len(jobs)} crates in {time.time() - t0:.1f}s")

    # ---- brings: not-in-lock names, lock ids, transitive closure through local sources
    manifests = {}

    def deps_of(name):
        """(non-optional non-dev [(pkg, req)] of the highest local version | None when there is no local source)."""
        real = local_norm.get(_norm(name))
        if real is None:
            return None
        if real not in manifests:
            mf = ew.load_toml(REG_SRC / f"{real}-{best[real]}" / "Cargo.toml")
            manifests[real] = sorted({(e["pkg"], e["req"]) for e in dep_declarations(mf, {}).values() if e["nonopt"]}) \
                if mf else None
        return manifests[real]

    def closure(names):
        seen, partial, q = {}, False, deque(names)
        while q:
            nm = q.popleft()
            if _norm(nm) in seen:
                continue
            seen[_norm(nm)] = nm
            ds = deps_of(nm)
            if ds is None:
                partial = True
                continue
            for dn, _ in ds:
                if _norm(dn) not in lock_norm and _norm(dn) not in seen:
                    q.append(dn)
        return len(seen), partial

    stat = Counter()
    out = {}
    for name in corpus_names:
        r = recs[name]
        new, shared = [], []
        for pkg, req in r.pop("_deps"):
            if _norm(pkg) in lock_norm:
                lid = lock_pick(by_norm, lock_ids, pkg, req)
                if lid not in shared:
                    shared.append(lid)
            elif pkg not in new:
                new.append(pkg)
        total, partial = closure(new)
        default_opt = r.pop("_default_opt")
        if any(_norm(x) not in lock_norm and x not in new for x in default_opt):
            stat["default_opt_new"] += 1
        rel, first, last, n, rv = tr.releases_of(name)
        r["releases"] = {"n": n, "first": first, "last": last} if rel else None
        if not r["rust_version"]:
            r["rust_version"] = rv.get(r["version"])
        r["brings"] = {"new": sorted(new), "shared": sorted(shared), "new_total": total, "new_partial": partial}
        r["_readme_para"] = r.pop("_readme")
        stat["new_partial"] += partial
        stat["with_new"] += bool(new)
        stat["releases"] += r["releases"] is not None
        out[r["id"]] = r

    def assemble(cap):
        res = []
        for rid, r in out.items():
            rec = {k: v for k, v in r.items() if k != "_readme_para"}
            para = r["_readme_para"]
            rec["readme"] = tr.assemble_readme([para], cap) if para else None
            res.append((rid, rec))
        return res

    items = assemble(REGISTRY_README_MAX)
    size = write_lines_map(OUT / "registry.json", items)
    trimmed = False
    if size > REGISTRY_BUDGET:
        items = assemble(REGISTRY_README_TRIM)
        size = write_lines_map(OUT / "registry.json", items)
        trimmed = True
    reg = dict(items)
    print(f"           registry.json {size / 1e3:.1f} KB, {len(reg)} records" + (" (readme trimmed to 160)" if trimmed else "")
          + f"; {stat['with_new']} bring at least one new crate, {stat['new_partial']} have a partial closure, "
          f"{stat['releases']} have index-cache releases; {stat['default_opt_new']} more would bring new crates via "
          f"default-on optional deps (not counted, per spec)")

    # ---- categories: lock packages + corpus crates, and the lock-only "yours" map
    cats, yours = defaultdict(set), defaultdict(set)
    for pid in world["packages"]:
        for c in (trust.get(pid) or {}).get("categories") or []:
            if c.strip():
                cats[c.strip()].add(pid)
                yours[c.strip()].add(pid)
    for rid, rec in reg.items():
        for c in rec["categories"]:
            if c.strip():
                cats[c.strip()].add(rid)
    cat_out = {c: {"count": len(ids), "crates": sorted(ids)}
               for c, ids in sorted(cats.items(), key=lambda kv: (-len(kv[1]), kv[0]))}
    cat_out["yours"] = {c: sorted(ids) for c, ids in sorted(yours.items())}
    lines = ["{"]
    keys = list(cat_out)
    for j, k in enumerate(keys):
        lines.append(f"  {json.dumps(k)}: {json.dumps(cat_out[k], ensure_ascii=False, separators=(',', ':'))}"
                     + ("," if j < len(keys) - 1 else ""))
    lines.append("}")
    text = "\n".join(lines) + "\n"
    (OUT / "categories.json").write_text(text, encoding="utf-8")
    print(f"           categories.json {len(text.encode('utf-8')) / 1e3:.1f} KB, {len(cats)} categories "
          f"({sum(1 for c in cats if any(i in reg for i in cats[c]))} touched by the corpus), "
          f"yours covers {len(yours)}; done in {time.time() - t0:.1f}s")
    return reg, cat_out


# --------------------------------------------------------------------------- main

def main(argv):
    want = [a for a in argv if a in STEPS] or list(STEPS)
    bad = [a for a in argv if a not in STEPS]
    if bad:
        sys.exit(f"unknown step(s) {bad}; choose from {STEPS}")
    world = json.loads((OUT / "world.json").read_text(encoding="utf-8"))
    lock = load_lock()
    if {f'{p["name"]}@{p["version"]}' for p in lock} != set(world["packages"]):
        print("WARN: Cargo.lock and data/world.json list different packages; re-run extract_world.py")
    dir_of, kind_of = build_dir_resolver(world, lock)
    if "docs" in want:
        step_docs(world, dir_of, kind_of)
    trust = None
    if "features" in want or "registry" in want:
        trust = json.loads((OUT / "trust.json").read_text(encoding="utf-8"))
    if "features" in want:
        step_features(world, trust, dir_of, kind_of, lock)
    if "registry" in want:
        step_registry(world, trust, dir_of, kind_of, lock)


if __name__ == "__main__":
    main(sys.argv[1:])
