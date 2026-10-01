#!/usr/bin/env python3
"""Trust signals, per-package detail files and per-version contents for the design board.

Reads only local files (no cargo, no network, no git mutation) and reuses extract_world.py
(lock/semver/sparse-cache helpers, Rust masking, the public-item scan rules). Run it AFTER
extract_world.py: it takes the package list from data/world.json and data/candidates.json.

Writes, next to this script:

  data/trust.json      { "<name@version>": record }  one legitimacy record per world.json
                       package, candidate and `extra` record (see build_trust_record)
  data/pkg/<file>.json { id, modules:[{path,count,private,items:[{n,f,s,d}]}] }  one item-detail
                       file per package, plus one per extra on-disk version (see below)
  data/licenses.json   { "<license expr>": { terms, op, count } }

FILENAME RULE for data/pkg/: the id ("name@version") with every "/" and every "+" replaced by "_",
plus ".json".  toml@0.8.23 -> toml@0.8.23.json, toml@1.1.5+spec-1.1.0 -> toml@1.1.5_spec-1.1.0.json.

VERSION CONTENTS: for every registry package whose crate name has 2+ versions in the registry src
dir (any versions on disk, not only the locked ones) a detail file is written for each of them, and
the record of the package's own version carries "local_versions": [...] (semver-sorted dir versions).

Standard library only (Python >= 3.11 for tomllib).
"""
from __future__ import annotations

import bisect
import html
import json
import os
import re
import sys
import time
from collections import Counter, defaultdict
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import extract_world as ew

HERE, ROOT, OUT = ew.HERE, ew.ROOT, ew.OUT
PKG_DIR = OUT / "pkg"
REG_SRC, REG_CACHE = ew.REG_SRC, ew.REG_CACHE

WORKERS = 4                # other agents are building; keep the fan-out modest
DETAIL_CAP_DEEP = 400      # items per detail file for depth >= 2 packages
DETAIL_HARD_CAP = 5000     # safety ceiling for the "no cap" cases (only windows-sys@0.61.2, 148k items, reaches it)
SET_UNCAPPED_MAX = 8000    # multi-version sets whose biggest version is under this are listed in full (diffs stay exact)
SIG_MAX = 140
DOC_MAX = 200
README_MAX = 700
README_MAX_TRIM = 450
CAP_EXAMPLES = 3
CAP_EXAMPLES_TRIM = 2
TRUST_BUDGET = 4 * 1024 * 1024
EX_TEXT = 110
CAPS = ("net", "fs", "process", "env", "ffi")


def file_name(pid: str) -> str:
    return pid.replace("/", "_").replace("+", "_") + ".json"


# --------------------------------------------------------------------------- sparse cache (full)

_cache_memo: dict[str, list | None] = {}


def cache_entries(name: str):
    """[{vers, yanked, pubtime, rust_version}] for every published version, or None without a cache file."""
    if name in _cache_memo:
        return _cache_memo[name]
    p = ew.cache_path(name)
    out = None
    if p.is_file():
        out = []
        for part in p.read_bytes().split(b"\0"):
            if part[:1] == b"{":
                try:
                    o = json.loads(part)
                except ValueError:
                    continue
                out.append({"vers": o.get("vers", ""), "yanked": bool(o.get("yanked")),
                            "pubtime": o.get("pubtime"), "rust_version": o.get("rust_version")})
    _cache_memo[name] = out
    return out


def vkey(v: str):
    return (ew.sv(ew.strip_build(v)) or (-1, 0, 0, (0,)), v)


def releases_of(name: str):
    """(releases, first, last, n, rust_version_by_vers)."""
    ent = cache_entries(name)
    if not ent:
        return [], None, None, 0, {}
    ent = sorted(ent, key=lambda e: vkey(e["vers"]))
    rel = [{"v": e["vers"], "t": (e["pubtime"] or "")[:10] or None, "y": e["yanked"]} for e in ent]
    ok = [r["t"] for r in rel if not r["y"] and r["t"]]
    n = sum(1 for r in rel if not r["y"])
    return rel, (min(ok) if ok else None), (max(ok) if ok else None), n, {e["vers"]: e["rust_version"] for e in ent}


# --------------------------------------------------------------------------- masking (comments only)

_TOK = re.compile(r"//[^\n]*|/\*|(?<![A-Za-z0-9_])b?r(#*)\"|\"|'")


def mask_comments(src: str) -> str:
    """Like extract_world.mask_rust but string and char literals are kept: only comments are blanked."""
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
                mm = ew._BLOCK.search(src, j)
                if not mm:
                    j = n
                    break
                depth += 1 if mm.group() == "/*" else -1
                j = mm.end()
            out.append(re.sub(r"[^\n]", " ", src[m.start():j]))
            i = j
        elif t == "'":
            cm = ew._CHAR.match(src, m.start())
            if cm:
                out.append(src[m.start():cm.end()])
                i = cm.end()
            else:
                out.append("'")
                i = m.end()
        elif t == '"':
            em = ew._STR_END.match(src, m.end())
            j = em.end() if em else n
            out.append(src[m.start():j])
            i = j
        else:  # raw string
            close = '"' + m.group(1)
            j = src.find(close, m.end())
            j = n if j < 0 else j + len(close)
            out.append(src[m.start():j])
            i = j
    return "".join(out)


# --------------------------------------------------------------------------- public item scan with detail

def _newlines(text: str):
    return [m.start() for m in re.finditer("\n", text)]


def _line_of(nl, off):
    return bisect.bisect_left(nl, off)


def _sig(line: str) -> str:
    s = line.strip()
    return s if len(s) <= SIG_MAX else s[:SIG_MAX - 1].rstrip() + "…"


_DOC_PAR_SKIP = re.compile(r"^(#|```|~~~)")


def _doc_of(lines, li):
    """First sentence of the `///` block directly above line `li` (attributes may sit in between)."""
    i, doc = li - 1, []
    while i >= 0:
        s = lines[i].strip()
        if s.startswith("///") and not s.startswith("////"):
            doc.append(s[3:])
            i -= 1
        elif s.startswith("#[") or s.startswith("#!["):
            i -= 1
        elif s.endswith("]") and not s.startswith("//"):      # tail of a multi-line attribute
            j = i - 1
            while j >= 0 and i - j <= 20 and not lines[j].strip().startswith("#["):
                j -= 1
            if j >= 0 and lines[j].strip().startswith("#["):
                i = j - 1
            else:
                break
        else:
            break
    if not doc:
        return None
    doc.reverse()
    paras, cur = [], []
    for ln in doc:
        if ln.strip() == "":
            if cur:
                paras.append(cur)
                cur = []
        else:
            cur.append(ln.strip())
    if cur:
        paras.append(cur)
    for p in paras:
        if _DOC_PAR_SKIP.match(p[0]):
            continue
        txt = re.sub(r"\[([^\[\]]+)\]\[[^\]]*\]", r"\1", " ".join(p))   # [text][ref] -> text
        txt = re.sub(r"\[([^\[\]]+)\](?!\()", r"\1", txt)                # [`Item`] -> `Item`
        d = ew.first_sentence(txt)
        if d and len(d) > DOC_MAX:
            d = d[:DOC_MAX - 1].rsplit(" ", 1)[0].rstrip(",;:") + "…"
        return d
    return None


def scan_items_detail(src_root: Path, skip_dirs: bool):
    """Same walk, item rule and module grouping as extract_world.scan_items, but every item also
    carries `s` (first source line) and `d` (first sentence of its `///` doc). Modules are returned
    as [{path, count, private, items:[{n,f,s,d}]}] sorted by (-count, path)."""
    mods: dict[str, list] = defaultdict(list)
    seen: set = set()
    decls: dict[tuple, dict] = defaultdict(dict)
    contrib: list = []
    if not src_root.is_dir():
        return 0, []
    files = []
    for dp, dns, fns in os.walk(src_root):
        dpp = Path(dp)
        dns[:] = sorted(d for d in dns if not d.startswith(".") and d.lower() not in ew._TESTISH_DIR
                        and not (skip_dirs and (dpp / d / "Cargo.toml").exists()))
        for fn in fns:
            if fn.endswith(".rs") and fn != "build.rs":
                p = dpp / fn
                if not ew._is_test_file(p):
                    files.append(p)
    files.sort()
    for p in files:
        try:
            text = p.read_bytes().decode("utf-8", "replace")
        except OSError:
            continue
        if "pub" not in text and "macro_export" not in text:
            continue
        code = ew.mask_rust(text)
        rel = p.relative_to(src_root)
        mod = ew.module_name(rel)
        mp = ew.full_module_path(rel)
        for dm in ew._MOD_DECL.finditer(code):
            nm = dm.group("name")
            decls[mp][nm] = decls[mp].get(nm, False) or (dm.group("vis") or "").strip() == "pub"
        before = len(seen)
        nl = _newlines(text)
        lines = text.split("\n")
        stack: list[str] = []
        bad = 0
        boundary = -1
        for m in ew._EVENT.finditer(code):
            if m.group("b"):
                c = m.group("b")
                if c == "{":
                    if bad:
                        kind = "other"
                    else:
                        raw_hdr = code[boundary + 1:m.start()]
                        hdr = ew._ATTR.sub(" ", raw_hdr).strip()
                        mm = ew._MOD_HDR.search(hdr)
                        if mm:
                            kind = "test" if ("cfg(test)" in raw_hdr.replace(" ", "")
                                              or mm.group(1) in ("tests", "test")) else "mod"
                        elif ew._EXTERN_HDR.search(hdr):
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
            fam = ew._FAMILY[kw]
            key = (mod, fam, name)
            if key in seen:
                continue
            seen.add(key)
            li = _line_of(nl, m.start("item"))
            mods[mod].append({"n": name, "f": fam, "s": _sig(lines[li]) if li < len(lines) else "",
                              "d": _doc_of(lines, li)})
        for mm in ew._MACRO_EXPORT.finditer(code):
            key = (mod, "callable", mm.group(1))
            if key not in seen:
                seen.add(key)
                off = mm.start() + mm.group(0).index("macro_rules!")
                li = _line_of(nl, off)
                mods[mod].append({"n": mm.group(1), "f": "callable",
                                  "s": _sig(lines[li]) if li < len(lines) else "",
                                  "d": _doc_of(lines, li)})
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
    out = [{"path": k, "count": len(v), "private": bool(private.get(k)), "items": v}
           for k, v in mods.items() if v]
    out.sort(key=lambda m: (-m["count"], m["path"]))
    return sum(m["count"] for m in out), out


def cap_detail(mods, cap):
    """Keep at most `cap` items (None = all), largest modules first; `count` stays the true size."""
    if cap is None:
        return mods
    left, out = cap, []
    for m in mods:
        if left <= 0:
            break
        out.append({**m, "items": m["items"][:left]})
        left -= len(out[-1]["items"])
    return out


# --------------------------------------------------------------------------- README

_HTML_TAGS = (r"(?:a|abbr|b|blockquote|br|center|code|dd|details|div|dl|dt|em|figure|figcaption|font|h[1-6]|hr|"
              r"i|iframe|img|kbd|li|ol|p|picture|pre|s|small|source|span|strong|sub|summary|sup|table|tbody|"
              r"td|th|thead|tr|u|ul|video|svg|section|article|nav|header|footer|main|aside)")
_STOP_HEADINGS = {"install", "installation", "installing", "usage", "getting started", "quick start", "quickstart",
                  "license", "licence", "licensing", "contributing", "contribution", "contributions", "examples",
                  "example", "minimum supported rust version", "msrv", "changelog", "documentation", "docs",
                  "usage example", "usage examples", "basic usage", "example usage", "sponsors", "contributors",
                  "acknowledgements", "acknowledgments", "development", "testing", "benchmarks"}


def _readme_file(d: Path, mf):
    r = (mf or {}).get("package", {}).get("readme")
    if r is False:
        return None
    cands = []
    if isinstance(r, str):
        cands.append(d / r)
    try:
        names = os.listdir(d)
    except OSError:
        names = []
    pri = {".md": 0, ".markdown": 1, "": 2, ".txt": 3}
    found = sorted((n for n in names if re.fullmatch(r"(?i)readme(\.(md|markdown|txt))?", n)),
                   key=lambda n: (pri.get(os.path.splitext(n)[1].lower(), 9), n))
    cands.extend(d / n for n in found)
    for c in cands:
        if c.is_file() and c.suffix.lower() in (".md", ".markdown", ".txt", ""):
            return c
    return None


def _pre_html(t: str) -> str:
    t = re.sub(r"<!--.*?-->", "", t, flags=re.S)
    t = re.sub(r"<(pre|picture|svg|table|script|style|h[1-6])\b[^>]*>.*?</\1\s*>", "\n\n", t, flags=re.S | re.I)
    t = re.sub(r"<img\b[^>]*>", "", t, flags=re.S | re.I)
    t = re.sub(r"</?(?:p|div|center|li|ul|ol|blockquote|hr|section|article|nav|header|footer|main|aside|dl|dt|dd|"
               r"details|summary|figure|figcaption)\b[^>]*>", "\n\n", t, flags=re.I)
    t = re.sub(r"<br\s*/?>", " ", t, flags=re.I)
    return t


def _nav_row(raw: str) -> bool:
    """A row of links with no words of its own ("[Docs](..) | [Crate](..)")."""
    links = len(re.findall(r"\]\(|\]\[", raw))
    rest = re.sub(r"!?\[(?:[^\[\]]|\[[^\]]*\])*\](?:\([^)]*\)|\[[^\]]*\])?", "", raw)
    return links >= 2 and len(re.findall(r"[A-Za-z]", rest)) < 3


def _clean_inline(s: str) -> str:
    codes: list[str] = []

    def stash(m):
        codes.append("`" + m.group(2).strip() + "`")
        return f"\x00{len(codes) - 1}\x00"

    s = re.sub(r"<code\b[^>]*>(.*?)</code>", lambda m: "`" + m.group(1) + "`", s, flags=re.S | re.I)
    s = re.sub(r"(?<!`)(`+)(?!`)(.+?)(?<!`)\1(?!`)", stash, s, flags=re.S)
    s = re.sub(r"</?" + _HTML_TAGS + r"\b[^>]*>", "", s, flags=re.I)
    s = s.replace("\\[", "\x01").replace("\\]", "\x02")
    s = re.sub(r"!\[[^\]]*\](?:\((?:[^()\s]|\([^)]*\))*(?:\s+\"[^\"]*\")?\)|\[[^\]]*\])", "", s)
    s = re.sub(r"\[((?:[^\[\]]|\[[^\]]*\])*)\]\((?:[^()\s]|\([^)]*\))*(?:\s+(?:\"[^\"]*\"|'[^']*'))?\)", r"\1", s)
    s = re.sub(r"\[((?:[^\[\]]|\[[^\]]*\])*)\]\[[^\]]*\]", r"\1", s)
    s = re.sub(r"\[([^\[\]]+)\]", r"\1", s)
    s = re.sub(r"<https?://[^>\s]+>", "", s)
    s = re.sub(r"\s*\((?:[A-Za-z ]+:)?\s*\)", "", s)
    s = s.replace("\x01", "[").replace("\x02", "]")
    s = re.sub(r"\*\*(.+?)\*\*|__(.+?)__", lambda m: m.group(1) or m.group(2), s)
    s = re.sub(r"(?<![\w*])\*(\S(?:.*?\S)?)\*(?![\w*])", r"\1", s)
    s = re.sub(r"(?<![\w_])_(\S(?:.*?\S)?)_(?![\w_])", r"\1", s)
    s = html.unescape(s)
    s = re.sub(r"\\([\\`*_{}\[\]()#+\-.!|<>~])", r"\1", s)
    s = re.sub(r"\x00(\d+)\x00", lambda m: codes[int(m.group(1))], s)
    return " ".join(s.split())


def readme_paragraphs(text: str, want: int = 3):
    t = _pre_html(text.replace("\r\n", "\n").replace("\r", "\n"))
    blocks: list[tuple[str, str]] = []
    cur: list[str] = []
    fence = None

    def flush():
        if cur:
            blocks.append(("p", " ".join(cur)))
            cur.clear()

    for line in t.split("\n"):
        line = re.sub(r"^\s*(?:>\s?)+", "", line)            # block quotes read as ordinary text
        st = line.strip()
        if re.fullmatch(r"\[![A-Za-z]+\]", st):
            continue
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
        if re.match(r"^(={3,}|-{3,})$", st) and cur:
            blocks.append(("h", " ".join(cur)))
            cur.clear()
            continue
        if re.match(r"^([-*_])(\s*\1){2,}\s*$", st):
            flush()
            continue
        if (line.startswith("    ") or line.startswith("\t")) and not cur:
            continue
        if cur and re.match(r"^([-*+]|\d+[.)])\s|^\|", st):
            flush()
        cur.append(st)
    flush()

    out: list[str] = []
    for kind, raw in blocks:
        if kind == "h":
            title = _clean_inline(raw).lower().strip(" :#")
            if out and title in _STOP_HEADINGS:
                break
            continue
        if re.match(r"^(\||[-*+]\s|\d+[.)]\s|\[[^\]]+\]:\s)", raw):
            continue
        if _nav_row(raw):
            continue
        c = _clean_inline(raw)
        words = c.split()
        if len(c) < 20 or len(words) < 3:
            continue
        if c.count(" | ") >= 2 or c.count(" · ") >= 2:
            continue
        if c.endswith(":"):                            # drop a dangling lead-in sentence ("It is:")
            lead = re.search(r"(?<=[.!?])\s+[^.!?]{1,40}:$", c)
            if lead:
                c = c[:lead.start()]
        out.append(c)
        if len(out) >= want:
            break
    return out


def assemble_readme(paras, cap):
    """Join up to 3 paragraphs with a blank line, cutting to ~`cap` characters at a sentence or word end."""
    if not paras:
        return None
    res, room = [], cap
    for i, p in enumerate(paras[:3]):
        if i and room < 100:
            break
        if len(p) > room:
            cut = p[:room]
            k = max(cut.rfind(". "), cut.rfind("! "), cut.rfind("? "))
            if k > room * 0.4:
                p = cut[:k + 1]
            else:
                p = cut.rsplit(" ", 1)[0].rstrip(",;:") + "…"
            res.append(p)
            break
        res.append(p)
        room -= len(p) + 2
    return "\n\n".join(res)


# --------------------------------------------------------------------------- per-crate trust scan

def _rx(pats):
    return re.compile("|".join(pats))


_ID = r"(?<![A-Za-z0-9_])"
_ENDID = r"(?![A-Za-z0-9_])"
CAP_RX = {
    "net": _rx([_ID + r"std::net" + _ENDID, _ID + r"TcpStream" + _ENDID, _ID + r"TcpListener" + _ENDID,
                _ID + r"UdpSocket" + _ENDID, _ID + r"tokio::net" + _ENDID, _ID + r"hyper::", _ID + r"reqwest::",
                _ID + r"ureq::"]),
    "fs": _rx([_ID + r"std::fs" + _ENDID, _ID + r"File::open" + _ENDID, _ID + r"File::create" + _ENDID,
               _ID + r"OpenOptions" + _ENDID, _ID + r"tokio::fs" + _ENDID, _ID + r"read_dir" + _ENDID]),
    "process": _rx([_ID + r"std::process::Command", _ID + r"Command::new\("]),
    "env": _rx([_ID + r"std::env::var", _ID + r"env::var\(", _ID + r"env::vars\(", _ID + r"env::set_var"]),
    "ffi": _rx([r'extern\s+"C"', r"#\[link\(", r"dlopen", r"libloading"]),
}
UNSAFE_RX = re.compile(r"(?<![A-Za-z0-9_])unsafe\s*\{"
                       r"|(?<![A-Za-z0-9_])unsafe\s+(?:extern\s*(?:\"[^\"]*\"\s*)?)?fn(?![A-Za-z0-9_])"
                       r"|(?<![A-Za-z0-9_])unsafe\s+impl(?![A-Za-z0-9_])"
                       r"|(?<![A-Za-z0-9_])unsafe\s+trait(?![A-Za-z0-9_])")
PROC_IMPORT_RX = re.compile(r"process::(?:Command|\{[^}]*\bCommand\b|\*)")
FORBID_RX = re.compile(r"#!\[\s*(?:forbid|deny)\s*\(([^)]*)\)\s*\]")
TEST_RX = re.compile(r"#\[test\]")


def _first_examples(hits, k):
    """Up to `k` examples, one per distinct file first (in path order), then the remaining hits."""
    picked, files = [], set()
    for h in hits:
        if h[0] not in files:
            files.add(h[0])
            picked.append(h)
    if len(picked) < k:
        rest = [h for h in hits if h not in picked]
        picked.extend(rest[:k - len(picked)])
    return sorted(picked[:k], key=lambda h: (h[0], h[1]))


def trust_scan(d: Path, mf, lib_root: Path):
    """Everything that needs the crate's files (README, sloc, unsafe, capability evidence...)."""
    files = []   # (path, relpath tuple)
    for dp, dns, fns in os.walk(d):
        dpp = Path(dp)
        dns[:] = sorted(x for x in dns if not x.startswith(".") and x not in ("target", "node_modules")
                        and not (dpp / x / "Cargo.toml").exists())
        for fn in sorted(fns):
            if fn.endswith(".rs"):
                p = dpp / fn
                files.append((p, p.relative_to(d).parts))
    try:
        rr = lib_root.relative_to(d).parts
    except ValueError:
        rr = ()
    sloc = unsafe = 0
    have_test = False
    cap_hits = {c: [] for c in CAPS}
    cap_n = {c: 0 for c in CAPS}
    examples_n = 0
    for p, rel in files:
        top = rel[0].lower() if len(rel) > 1 else ""
        if top in ("examples", "example"):
            examples_n += 1
        cap_ok = not any(part.lower() in ew._TESTISH_DIR for part in rel[:-1])
        # `src/**` per the brief; a lib root elsewhere (`[lib] path = "lib.rs"`, "rust/lib.rs") counts as src too
        in_src = top == "src" or (
            rel != ("build.rs",) and rel[:len(rr)] == rr and rr != ("src",)
            and not any(part.lower() in ew._TESTISH_DIR for part in rel[len(rr):-1]))
        if not (in_src or cap_ok):
            continue
        try:
            text = p.read_bytes().decode("utf-8", "replace")
        except OSError:
            continue
        code = mask_comments(text)
        if in_src:
            for ln in text.split("\n"):
                s = ln.strip()
                if s and not s.startswith("//"):
                    sloc += 1
            unsafe += len(UNSAFE_RX.findall(code))
            if not have_test and TEST_RX.search(code):
                have_test = True
        if cap_ok:
            nl = None
            proc_ok = bool(PROC_IMPORT_RX.search(code))     # bare `Command::new(` may be clap's, not std::process
            for c in CAPS:
                seen_lines = set()
                for m in CAP_RX[c].finditer(code):
                    if c == "process" and m.group().startswith("Command::new") and not proc_ok:
                        continue
                    if nl is None:
                        nl = _newlines(text)
                        lines = text.split("\n")
                    li = _line_of(nl, m.start())
                    if li in seen_lines:
                        continue
                    seen_lines.add(li)
                    cap_n[c] += 1
                    if len(cap_hits[c]) < 40:
                        cap_hits[c].append(("/".join(rel), li + 1, lines[li].strip()[:EX_TEXT]))
    lib_path = ((mf or {}).get("lib") or {}).get("path") or "src/lib.rs"
    libf = d / lib_path
    if not libf.is_file():
        libf = d / "src" / "main.rs"
    forbid = False
    if libf.is_file():
        code = mask_comments(libf.read_bytes().decode("utf-8", "replace"))
        forbid = any("unsafe_code" in [x.strip() for x in m.group(1).split(",")] for m in FORBID_RX.finditer(code))
    tests_dir = (d / "tests").is_dir()
    rf = _readme_file(d, mf)
    paras = None
    if rf is not None:
        try:
            paras = readme_paragraphs(rf.read_bytes().decode("utf-8", "replace")) or None
        except OSError:
            paras = None
    caps = {}
    for c in CAPS:
        caps[c] = [list(h) for h in _first_examples(cap_hits[c], CAP_EXAMPLES)]
        caps[c + "_n"] = cap_n[c]
    return {"sloc": sloc, "unsafe": unsafe, "forbid_unsafe": forbid, "caps": caps, "readme_paras": paras,
            "tests": bool(tests_dir or have_test), "examples": examples_n,
            "build_rs": (d / "build.rs").is_file()}


def manifest_fields(mf, d: Path, ws_pkg):
    def fld(name):
        return ew.pkg_field(mf, name, d, ws_pkg)

    authors = []
    for a in fld("authors") or []:
        if not isinstance(a, str):
            continue
        a = re.sub(r"<[^>]*>?", " ", a)                 # "Name <mail>" and unterminated "Name <mail"
        a = " ".join(re.sub(r"\S+@\S+", " ", a).split())  # bare addresses
        if a:
            authors.append(a)
    authors = authors[:4]

    def url(v):
        return v if isinstance(v, str) and v.strip() else None

    feats = (mf or {}).get("features") or {}
    referenced = set()
    for vals in feats.values():
        for e in vals if isinstance(vals, list) else []:
            if isinstance(e, str) and e.startswith("dep:"):
                referenced.add(e[4:])
    optional = []
    for _, t in ew.dep_tables(mf or {}):
        for k, v in t.items():
            if isinstance(v, dict) and v.get("optional") and k not in optional:
                optional.append(k)
    names = [f for f in feats if f != "default"]
    names += [k for k in optional if k not in feats and k not in referenced]
    default = feats.get("default")
    pkg = (mf or {}).get("package", {})
    build = pkg.get("build")
    lib = (mf or {}).get("lib") or {}
    return {
        "authors": authors,
        "repository": url(fld("repository")),
        "homepage": url(fld("homepage")),
        "documentation": url(fld("documentation")),
        "edition": fld("edition") if isinstance(fld("edition"), str) else ("2015" if mf else None),  # Cargo's default
        "rust_version": fld("rust-version") if isinstance(fld("rust-version"), str) else None,
        "categories": [c for c in (fld("categories") or []) if isinstance(c, str)],
        "keywords": [c for c in (fld("keywords") or []) if isinstance(c, str)],
        "features": {"default": [x for x in default if isinstance(x, str)] if isinstance(default, list) else [],
                     "all": names},
        "build_set": isinstance(build, str) and bool(build),
        "build_off": build is False,
        "proc_macro": bool(lib.get("proc-macro") or lib.get("proc_macro")),
    }


# --------------------------------------------------------------------------- worker

def analyze(job):
    """Scan one crate directory: write its detail file, and (when asked) compute the trust fields."""
    pid, d, kind, cap, want_trust = job["id"], Path(job["dir"]), job["kind"], job["cap"], job["trust"]
    res = {"id": pid, "warn": []}
    mf = ew.load_toml(d / "Cargo.toml")
    ws_pkg = job.get("ws_pkg") or {}
    if mf is None:
        res["warn"].append(f"{pid}: no readable Cargo.toml in {d}")
    lp = ((mf or {}).get("lib") or {}).get("path") or "src/lib.rs"
    root = (d / lp).parent
    if kind != "yours" and not root.is_dir():
        root = d / "src"
    total, mods = scan_items_detail(root, root == d)
    listed = cap_detail(mods, cap)
    obj = {"id": pid, "modules": listed}
    if sum(len(m["items"]) for m in listed) < total:
        obj["truncated"] = True
    data = json.dumps(obj, ensure_ascii=False, separators=(",", ":"))
    (PKG_DIR / file_name(pid)).write_text(data, encoding="utf-8")
    res.update(items=total, bytes=len(data.encode("utf-8")), truncated=bool(obj.get("truncated")))
    if want_trust:
        f = manifest_fields(mf, d, ws_pkg if kind == "yours" else None)
        t = trust_scan(d, mf, root)
        f["build_rs"] = bool((t.pop("build_rs") or f["build_set"]) and not f["build_off"])
        f.pop("build_set")
        f.pop("build_off")
        res["trust"] = {**f, **t}
    return res


# --------------------------------------------------------------------------- licences

def parse_license(expr: str):
    s = expr.strip()
    if s.lower().startswith("see ") or not s:
        return {"terms": [s] if s else [], "op": None}
    norm = re.sub(r"\s*/\s*", " OR ", s)                    # legacy "MIT/Apache-2.0"
    toks = re.findall(r"\(|\)|[^\s()]+", norm)
    terms, i = [], 0
    ops_any, ops0, depth = set(), set(), 0
    wrapped = bool(toks) and toks[0] == "("
    for k, tk in enumerate(toks):                            # does one pair of parens wrap everything?
        if tk == "(":
            depth += 1
        elif tk == ")":
            depth -= 1
            if depth == 0 and k < len(toks) - 1:
                wrapped = False
    depth = 0
    lead = 1 if wrapped else 0
    while i < len(toks):
        tk = toks[i]
        up = tk.upper()
        if tk == "(":
            depth += 1
        elif tk == ")":
            depth -= 1
        elif up in ("AND", "OR"):
            ops_any.add(up)
            if depth == lead:
                ops0.add(up)
        else:
            term = tk
            if i + 2 < len(toks) and toks[i + 1].upper() == "WITH":      # "Apache-2.0 WITH LLVM-exception"
                term = f"{tk} WITH {toks[i + 2]}"
                i += 2
            if term not in terms:
                terms.append(term)
        i += 1
    op = "OR" if "OR" in ops0 else "AND" if "AND" in ops0 else None
    if op is None and len(ops_any) == 1:
        op = next(iter(ops_any))
    out = {"terms": terms, "op": op}
    if len(ops_any) == 2:
        out["mixed"] = True
    return out


# --------------------------------------------------------------------------- main

def main():
    t0 = time.time()
    OUT.mkdir(parents=True, exist_ok=True)
    PKG_DIR.mkdir(parents=True, exist_ok=True)
    for f in PKG_DIR.glob("*.json"):
        f.unlink()

    world = json.loads((OUT / "world.json").read_text(encoding="utf-8"))
    cands = json.loads((OUT / "candidates.json").read_text(encoding="utf-8"))
    root_manifest = ew.load_toml(ROOT / "Cargo.toml")
    with open(ROOT / "Cargo.lock", "rb") as fh:
        lock_src = {f'{p["name"]}@{p["version"]}': p.get("source") for p in ew.tomllib.load(fh)["package"]}
    ws_pkg = (root_manifest.get("workspace") or {}).get("package", {})

    # ---- on-disk registry versions per crate name
    dir_re = re.compile(r"^(?P<name>.+?)-(?P<ver>\d+\.\d+\.\d+(?:[-+].*)?)$")
    disk: dict[str, list[str]] = defaultdict(list)
    for dn in os.listdir(REG_SRC):
        m = dir_re.match(dn)
        if m and ew.sv(ew.strip_build(m.group("ver"))) is not None:
            disk[m.group("name")].append(m.group("ver"))
    for v in disk.values():
        v.sort(key=vkey)

    # ---- vendored path crates and git checkouts (same lookup rules as extract_world)
    local_pkg_dirs = {}
    for tp in glob_paths(ROOT / "vendor" / "*" / "Cargo.toml"):
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
        for tp in glob_paths(ew.GIT_CHECKOUTS / pat):
            mf = ew.load_toml(Path(tp))
            if mf and "package" in mf:
                git_index[mf["package"]["name"]].append(Path(tp).parent)

    # ---- the package list: world.json + candidates + extras
    pk: dict[str, dict] = {}
    warns: list[str] = []
    for pid, r in world["packages"].items():
        k, name = r["kind"], r["name"]
        d = None
        if k == "yours":
            d = ROOT / r["dir"]
        elif k == "registry":
            d = REG_SRC / f'{name}-{r["version"]}'
        elif k == "path":
            d = (local_pkg_dirs.get(name) or [None])[0]
        elif k == "git":
            cands_g = git_index.get(name, [])
            sha = (lock_src.get(pid) or "").rsplit("#", 1)[-1][:7]
            pick = [x for x in cands_g if sha and sha in str(x)] or cands_g
            d = pick[0] if pick else None
        pk[pid] = {"id": pid, "name": name, "version": r["version"], "kind": k, "depth": r["depth"],
                   "license": r.get("license"), "dir": d if d and Path(d).is_dir() else None, "src": "world"}
        if pk[pid]["dir"] is None:
            warns.append(f"no local source dir for {pid} ({k})")
    for q in cands["queries"].values():
        for r in q:
            if r["id"] not in pk:
                pk[r["id"]] = {"id": r["id"], "name": r["name"], "version": r["version"], "kind": "registry",
                               "depth": None, "license": r.get("license"), "src": "candidate",
                               "dir": REG_SRC / f'{r["name"]}-{r["version"]}'}
    cand_rec = {r["id"]: r for q in cands["queries"].values() for r in q}
    cand_rec.update(cands["extra"])
    for pid, r in cands["extra"].items():
        if pid not in pk:
            pk[pid] = {"id": pid, "name": r["name"], "version": r["version"], "kind": "registry",
                       "depth": 2, "license": r.get("license"), "src": "extra",
                       "dir": REG_SRC / f'{r["name"]}-{r["version"]}'}
    for v in pk.values():
        if v["dir"] is not None and not Path(v["dir"]).is_dir():
            warns.append(f"no local source dir for {v['id']}")
            v["dir"] = None

    def cap_for(depth):
        return DETAIL_HARD_CAP if depth is None or depth < 2 else DETAIL_CAP_DEEP

    def items_of(v):
        if v["src"] == "world":
            return world["packages"][v["id"]]["items"]
        return (cand_rec.get(v["id"]) or {}).get("items", 0)

    jobs = []
    multi_names = sorted({v["name"] for v in pk.values() if v["kind"] == "registry" and len(disk.get(v["name"], [])) >= 2})
    set_cap = {}     # crate name -> one cap for every version file of that crate, so client-side diffs compare like with like
    for name in multi_names:
        anchor = [v for v in pk.values() if v["name"] == name and v["kind"] == "registry"]
        if max(items_of(v) for v in anchor) <= SET_UNCAPPED_MAX:
            set_cap[name] = None
        else:
            set_cap[name] = cap_for(min((v["depth"] for v in anchor if v["depth"] is not None), default=None))
    for pid, v in pk.items():
        if v["dir"] is not None:
            cap = set_cap[v["name"]] if v["name"] in set_cap and v["kind"] == "registry" else cap_for(v["depth"])
            jobs.append({"id": pid, "dir": str(v["dir"]), "kind": v["kind"], "cap": cap,
                         "trust": True, "ws_pkg": ws_pkg})
    # ---- extra on-disk versions of multi-version crates
    local_versions = {}
    n_lock_jobs = len(jobs)
    for name in multi_names:
        vers = disk[name]
        anchor = [v for v in pk.values() if v["name"] == name and v["kind"] == "registry"]
        cap = set_cap[name]
        for ver in vers:
            pid = f"{name}@{ver}"
            if pid not in pk:
                jobs.append({"id": pid, "dir": str(REG_SRC / f"{name}-{ver}"), "kind": "registry", "cap": cap,
                             "trust": False})
        for v in anchor:
            local_versions[v["id"]] = vers
    print(f"{len(pk)} packages ({sum(1 for v in pk.values() if v['src']=='world')} world, "
          f"{sum(1 for v in pk.values() if v['src']=='candidate')} candidates, "
          f"{sum(1 for v in pk.values() if v['src']=='extra')} extras); {len(multi_names)} multi-version crates; "
          f"{n_lock_jobs} package scans + {len(jobs) - n_lock_jobs} extra version files")

    # ---- scan (slow part) over a few processes
    t1 = time.time()
    results = {}
    with ProcessPoolExecutor(max_workers=WORKERS) as ex:
        futs = {j["id"]: ex.submit(analyze, j) for j in jobs}
        for pid, f in futs.items():
            results[pid] = f.result()
    print(f"scanned {len(jobs)} crate dirs in {time.time() - t1:.1f}s")
    for r in results.values():
        warns.extend(r["warn"])

    # ---- assemble trust.json
    def build(readme_cap, ex_cap):
        trust = {}
        for pid, v in pk.items():
            rel, first, last, n, rv = releases_of(v["name"])
            res = results.get(pid)
            t = (res or {}).get("trust")
            rec = {"releases": rel, "first": first, "last": last, "n": n}
            if t is None:
                rec.update(authors=[], repository=None, homepage=None, documentation=None, edition=None,
                           rust_version=rv.get(v["version"]), categories=[], keywords=[],
                           features={"default": [], "all": []}, build_rs=None, proc_macro=None, sloc=None,
                           **{"unsafe": None}, forbid_unsafe=None, caps=None, readme=None, tests=None, examples=None)
            else:
                caps = {}
                for c in CAPS:
                    caps[c] = t["caps"][c][:ex_cap]
                    caps[c + "_n"] = t["caps"][c + "_n"]
                rec.update(authors=t["authors"], repository=t["repository"], homepage=t["homepage"],
                           documentation=t["documentation"], edition=t["edition"],
                           rust_version=t["rust_version"] or rv.get(v["version"]),
                           categories=t["categories"], keywords=t["keywords"], features=t["features"],
                           build_rs=t["build_rs"], proc_macro=t["proc_macro"], sloc=t["sloc"],
                           **{"unsafe": t["unsafe"]}, forbid_unsafe=t["forbid_unsafe"], caps=caps,
                           readme=assemble_readme(t["readme_paras"], readme_cap), tests=t["tests"],
                           examples=t["examples"])
            if pid in local_versions:
                rec["local_versions"] = local_versions[pid]
            trust[pid] = rec
        return trust

    def dump(trust, path):
        lines = ["{"]
        items = list(trust.items())
        for j, (pid, rec) in enumerate(items):
            lines.append(f"  {json.dumps(pid)}: {json.dumps(rec, ensure_ascii=False, separators=(',', ':'))}"
                         + ("," if j < len(items) - 1 else ""))
        lines.append("}")
        data = "\n".join(lines) + "\n"
        Path(path).write_text(data, encoding="utf-8")
        return len(data.encode("utf-8"))

    trust = build(README_MAX, CAP_EXAMPLES)
    size = dump(trust, OUT / "trust.json")
    trimmed = False
    if size > TRUST_BUDGET:
        trust = build(README_MAX_TRIM, CAP_EXAMPLES_TRIM)
        size = dump(trust, OUT / "trust.json")
        trimmed = True
    print(f"trust.json: {size / 1e6:.2f} MB, {len(trust)} records" + (" (trimmed: readme 450, caps examples 2)" if trimmed else ""))

    # ---- licences
    lic_count = Counter(v["license"] for v in pk.values() if v["license"])
    lic = {}
    for expr, n in lic_count.most_common():
        lic[expr] = {**parse_license(expr), "count": n}
        lic[expr] = {k: lic[expr][k] for k in ("terms", "op", "count") + (("mixed",) if "mixed" in lic[expr] else ())}
    (OUT / "licenses.json").write_text(json.dumps(lic, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")

    # ---- report
    report(pk, trust, results, lic, lic_count, size, warns, local_versions, multi_names)
    print(f"done in {time.time() - t0:.1f}s")


def glob_paths(pattern: Path):
    import glob
    return glob.glob(str(pattern))


def report(pk, trust, results, lic, lic_count, size, warns, local_versions, multi_names):
    T = list(trust.values())
    have = [t for t in T if t["sloc"] is not None]
    print("\n== counts (of %d records, %d scanned from source) ==" % (len(T), len(have)))
    print("with release dates :", sum(1 for t in T if t["first"]))
    print("with README        :", sum(1 for t in T if t["readme"]))
    print("with build_rs      :", sum(1 for t in T if t["build_rs"]))
    print("with proc_macro    :", sum(1 for t in T if t["proc_macro"]))
    print("with unsafe > 0    :", sum(1 for t in T if t["unsafe"]))
    print("with forbid_unsafe :", sum(1 for t in T if t["forbid_unsafe"]))
    for c in CAPS:
        print(f"cap {c:8s}       :", sum(1 for t in have if t["caps"][c + "_n"]))
    spdx = Counter()
    for expr, n in lic_count.items():
        for term in lic[expr]["terms"]:
            spdx[term] += n
    print("\n== licence expressions (top 15 of %d) ==" % len(lic_count))
    for expr, n in lic_count.most_common(15):
        print(f"{n:5d}  {expr}")
    print("no license field   :", sum(1 for v in pk.values() if not v["license"]))
    print("\n== distinct SPDX ids (%d) ==" % len(spdx))
    print(", ".join(f"{k} {n}" for k, n in spdx.most_common()))
    pkg_bytes = sum(f.stat().st_size for f in PKG_DIR.glob("*.json"))
    print(f"\ntrust.json {size / 1e6:.2f} MB; data/pkg {pkg_bytes / 1e6:.2f} MB in {len(list(PKG_DIR.glob('*.json')))} files")
    print(f"packages with local_versions: {len(local_versions)} across {len(multi_names)} crate names")
    for w in warns:
        print("WARN:", w)


if __name__ == "__main__":
    main()
