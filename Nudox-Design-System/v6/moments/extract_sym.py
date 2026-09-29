#!/usr/bin/env python3
"""What the symbol page needs beyond a first line, read from the same local sources as the other
extractors (no cargo, no network, no git mutation). Run it after extract_trust.py.

  data/sym/<file>.json   one per registry/yours package (file rule as data/pkg/): the insides of its
                         public items, keyed "<module>::<name>" with the module as data/pkg writes it:
                           S    a fn's whole signature on one line (where clause included)
                           der  derives written above a type
                           var  an enum's variants [name, payload]
                           fld  a struct's pub fields [name, type]
                           sup  a trait's supertraits (text after the colon)
                           req  a trait's required members [name, signature]; prov = provided ones
                           impl traits implemented for a type in this package [trait, its args]
                           by   types in this package that implement a trait
                           m    a type's pub inherent methods {n, s, d, r} (r: & | &mut | self | "")
  data/sym_uses.json     per (crate of yours > dependency): per symbol of the dependency, how often the
                         crate names it, which of its members it reaches, and real lines:
                           {"<crate id>><dep id>": {"<Name>": {"n": 12, "m": {"as_str": 5}, "q": {"de": 1, "": 11},
                              "l": [[file, line, text]], "ml": {"as_str": [[file, line, text]]}, "i": [[Type, file, line]], "ni": 3 }}}
                         `l` up to ten lines per crate (three per file), `ml` up to three per member, `q` the module
                         path each use spelled out, `i`/`ni` your types that implement or derive it (traits).

APPROXIMATE, like world.json's `uses`: a path scan, not a compiler. A symbol is named when a path
`dep::…::Name` (or a `use dep::{…}` group) reaches it; the member is the segment after it. Bare uses
after an import are not counted, except derives/impls of an imported trait, which land in `i`.
"""
from __future__ import annotations

import json
import os
import re
import sys
import time
from collections import Counter, defaultdict
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import extract_world as ew
import extract_trust as et

OUT = ew.OUT
SYM_DIR = OUT / "sym"
REG_SRC = ew.REG_SRC
ROOT = ew.ROOT
WORKERS = 4
CAP_M, CAP_VAR, CAP_FLD, CAP_BY, CAP_SIG = 80, 60, 40, 40, 260
LINES_CAP = 10

WS = re.compile(r"\s+")
_FN = re.compile(r'(?m)^[ \t]*(?P<vis>pub(?:[ \t]*\([^)]*\))?[ \t]+)?(?:(?:default|const|async|unsafe|safe|extern[ \t]+"[^"]*"|extern)[ \t]+)*fn[ \t]+(?:r#)?(?P<n>[A-Za-z_]\w*)')
_IMPL = re.compile(r"(?m)^[ \t]*(?:unsafe[ \t]+)?impl(?=[ \t<\n])")
_TYPE_ASSOC = re.compile(r"(?m)^[ \t]*(?:type|const)[ \t]+(?P<n>[A-Za-z_]\w*)")
_TEST_MOD = re.compile(r"cfg\(\s*test\s*\)\s*\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")


def one(s: str, cap: int = CAP_SIG) -> str:
    s = WS.sub(" ", s).strip()
    s = s.replace("( ", "(").replace(" )", ")").replace("< ", "<").replace(" >", ">").replace(" ,", ",")
    return s if len(s) <= cap else s[:cap - 1].rstrip() + "…"


def close_of(code: str, i: int, o: str = "{", c: str = "}") -> int:
    """Index of the bracket closing the one at i (or the end)."""
    d = 0
    for j in range(i, len(code)):
        ch = code[j]
        if ch == o:
            d += 1
        elif ch == c:
            d -= 1
            if d == 0:
                return j
    return len(code) - 1


def head_end(code: str, i: int) -> int:
    """From an item keyword, the index of the `{` or `;` that ends its header (outside () and [])."""
    d = 0
    j = i
    n = len(code)
    while j < n:
        ch = code[j]
        if ch in "([":
            d += 1
        elif ch in ")]":
            d -= 1
        elif d == 0 and ch in "{;":
            return j
        j += 1
    return n - 1


def split_top(s: str, sep: str = ","):
    """Split at depth 0 of <>, (), [], {} (an arrow's '>' is not a bracket)."""
    out, cur, d, i = [], [], 0, 0
    while i < len(s):
        ch = s[i]
        if ch == "-" and i + 1 < len(s) and s[i + 1] == ">":
            cur.append("->"); i += 2; continue
        if ch in "<([{":
            d += 1
        elif ch in ">)]}":
            d -= 1
        if ch == sep and d == 0:
            out.append("".join(cur).strip()); cur = []
        else:
            cur.append(ch)
        i += 1
    if "".join(cur).strip():
        out.append("".join(cur).strip())
    return out


def strip_generics(s: str) -> str:
    """Drop a leading <…> (balanced)."""
    s = s.lstrip()
    if not s.startswith("<"):
        return s
    d = 0
    for j, ch in enumerate(s):
        if ch == "-" and j + 1 < len(s) and s[j + 1] == ">":
            continue
        if ch == "<":
            d += 1
        elif ch == ">" and (j == 0 or s[j - 1] != "-"):
            d -= 1
            if d == 0:
                return s[j + 1:]
    return s


def path_name(t: str):
    """(last segment name, generic args) of a type path like `&'a mut foo::Bar<X, Y>`."""
    t = re.sub(r"^&\s*('\w+\s+)?(mut\s+)?", "", t.strip())
    t = re.sub(r"^(dyn|impl)\s+", "", t)
    t = t.lstrip("!").strip()
    m = re.match(r"((?:\w+::)*)(\w+)\s*(<.*>)?", t)
    if not m:
        return None, ""
    args = m.group(3) or ""
    return m.group(2), (args[1:-1].strip() if args else "")


def fn_sig(text: str, code: str, st: int):
    e = head_end(code, st)
    return one(text[st:e]), (code[e] if e < len(code) else ";")


def receiver(sig: str) -> str:
    m = re.search(r"\((.*)", sig)
    if not m:
        return ""
    first = split_top(m.group(1).rsplit(")", 1)[0] if ")" in m.group(1) else m.group(1))
    p = first[0] if first else ""
    if re.match(r"^&\s*('\w+\s+)?mut\s+self\b", p) or re.match(r"^self\s*:\s*.*&\s*('\w+\s+)?mut\s+Self", p):
        return "&mut"
    if re.match(r"^&\s*('\w+\s+)?self\b", p) or re.match(r"^self\s*:\s*&", p):
        return "&"
    if re.match(r"^(mut\s+)?self\b", p):
        return "self"
    return ""


def derives_above(lines, li):
    out = []
    i = li - 1
    while i >= 0 and li - i <= 30:
        s = lines[i].strip()
        if s.startswith("///") or s.startswith("//") or s == "":
            i -= 1
            continue
        if s.startswith("#[") or s.endswith(")]") or s.endswith(",") or s.endswith("("):
            for m in re.finditer(r"derive\s*\(([^)]*)\)", s):
                out += [x.strip().split("::")[-1] for x in m.group(1).split(",") if x.strip()]
            i -= 1
            continue
        break
    return list(dict.fromkeys(out))


def test_ranges(code: str):
    rs = []
    for m in _TEST_MOD.finditer(code):
        b = m.end() - 1
        rs.append((b, close_of(code, b)))
    return rs


def inside(rs, i):
    return any(a <= i <= b for a, b in rs)


def at_depth(code: str, positions, base: int, want: int):
    """Filter sorted positions to those at brace depth `want` relative to `base`."""
    out, d, j = [], 0, base
    for p in positions:
        while j < p:
            ch = code[j]
            if ch == "{":
                d += 1
            elif ch == "}":
                d -= 1
            j += 1
        if d == want:
            out.append(p)
    return out


def scan_package(job):
    """Every public type's insides, every trait's members and implementors, every fn's whole signature."""
    pid, root, skip = job["id"], Path(job["root"]), job["skip"]
    items: dict[str, dict] = {}
    impls = []   # (module, trait name or None, trait args, type name, methods)
    files = []
    for dp, dns, fns in os.walk(root):
        dpp = Path(dp)
        dns[:] = sorted(d for d in dns if not d.startswith(".") and d.lower() not in ew._TESTISH_DIR
                        and not (skip and (dpp / d / "Cargo.toml").exists()))
        for fn in fns:
            if fn.endswith(".rs") and fn != "build.rs":
                p = dpp / fn
                if not ew._is_test_file(p):
                    files.append(p)
    for p in sorted(files):
        try:
            text = p.read_bytes().decode("utf-8", "replace")
        except OSError:
            continue
        if "pub" not in text and "impl" not in text:
            continue
        code = ew.mask_rust(text)
        rel = p.relative_to(root)
        mod = ew.module_name(rel)
        nl = et._newlines(text)
        lines = text.split("\n")
        tr = test_ranges(code)
        # ---- module-scope declarations (same walk and rule as the detail scan)
        stack, bad, boundary = [], 0, -1
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
                            kind = "test" if ("cfg(test)" in raw_hdr.replace(" ", "") or mm.group(1) in ("tests", "test")) else "mod"
                        elif ew._EXTERN_HDR.search(hdr):
                            kind = "extern"
                        else:
                            kind = "other"
                    stack.append(kind)
                    if kind in ("other", "test"):
                        bad += 1
                elif c == "}":
                    if stack and stack.pop() in ("other", "test"):
                        bad -= 1
                boundary = m.start()
                continue
            if bad:
                continue
            kw, name = m.group("kw"), m.group("name")
            if not name or kw in ("mod", "macro_rules!", "const", "static"):
                continue
            key = f"{mod}::{name}"
            if key in items:
                continue
            st = m.start("kw")
            li = et._line_of(nl, m.start("item"))
            rec = {"k": kw, "file": str(rel), "line": li + 1}
            if kw == "fn":
                rec["S"] = fn_sig(text, code, m.start("item"))[0]
            elif kw in ("struct", "enum", "union"):
                rec["der"] = derives_above(lines, li)
                e = head_end(code, st)
                if e < len(code) and code[e] == "{":
                    body = ew._ATTR.sub(" ", code[e + 1:close_of(code, e)])
                    parts = split_top(body)
                    if kw == "enum":
                        rec["var"] = []
                        for v in parts[:CAP_VAR]:
                            vm = re.match(r"(\w+)\s*(.*)", v, re.S)
                            if not vm:
                                continue
                            rest = vm.group(2).strip()
                            pay = ""
                            if rest.startswith("("):
                                pay = one(rest[1:close_of(rest, 0, "(", ")")], 80)
                            elif rest.startswith("{"):
                                inner = split_top(rest[1:close_of(rest, 0)])
                                pay = "{" + ", ".join(x.split(":")[0].strip() for x in inner if ":" in x)[:80] + "}"
                            rec["var"].append([vm.group(1), pay])
                    else:
                        rec["fld"] = []
                        for f in parts:
                            fm = re.match(r"pub\s+(\w+)\s*:\s*(.*)", f, re.S)
                            if fm:
                                rec["fld"].append([fm.group(1), one(fm.group(2), 80)])
                        rec["fld"] = rec["fld"][:CAP_FLD]
                elif e < len(code) and code[e] == ";":
                    hdr = code[st:e]
                    if "(" in hdr:   # tuple struct
                        inner = hdr[hdr.index("("):]
                        inner = inner[1:close_of(inner, 0, "(", ")")]
                        rec["fld"] = [[str(i), one(re.sub(r"^pub(\([^)]*\))?\s+", "", x), 80)] for i, x in enumerate(split_top(inner)) if x.startswith("pub")][:CAP_FLD]
                        rec["tuple"] = True
            elif kw == "trait":
                e = head_end(code, st)
                hdr = one(code[st:e])
                sm = re.match(r"trait\s+\w+\s*(<.*?>)?\s*:\s*(.+?)(\s+where\b.*)?$", strip_generics_after(hdr))
                if sm:
                    rec["sup"] = sm.group(2).strip()
                if e < len(code) and code[e] == "{":
                    b1 = close_of(code, e)
                    body_pos = [x.start() for x in _FN.finditer(code, e + 1, b1)]
                    body_pos = at_depth(code, body_pos, e + 1, 0)
                    req, prov = [], []
                    for bp in body_pos:
                        mm = _FN.match(code, bp)
                        sig, end = fn_sig(text, code, bp)
                        (req if end == ";" else prov).append([mm.group("n"), sig])
                    ta = [x.start() for x in _TYPE_ASSOC.finditer(code, e + 1, b1)]
                    for tp in at_depth(code, ta, e + 1, 0):
                        tm = _TYPE_ASSOC.match(code, tp)
                        seg = code[tp:head_end(code, tp)]
                        (prov if "=" in seg else req).append([tm.group("n"), one(text[tp:head_end(code, tp)])])
                    rec["req"], rec["prov"] = req[:CAP_M], prov[:CAP_M]
            elif kw == "type":
                pass
            items[key] = rec
        # ---- impl blocks anywhere outside tests
        for im in _IMPL.finditer(code):
            if inside(tr, im.start()):
                continue
            e = head_end(code, im.start())
            if e >= len(code) or code[e] != "{":
                continue
            hdr = one(code[im.start():e])
            hdr = re.sub(r"^(unsafe\s+)?impl\s*", "", hdr)
            gen = hdr[1:len(hdr) - len(strip_generics(hdr)) - 1] if hdr.startswith("<") else ""
            hdr = strip_generics(hdr).strip()
            hw = re.split(r"\s+where\b", hdr, maxsplit=1)
            hdr = hw[0]
            # the impl's own bounds: which traits its generic parameters must do
            bounds = {}
            for g in split_top(gen) + split_top(hw[1] if len(hw) > 1 else ""):
                gm = re.match(r"(\w+)\s*(?::\s*(.+))?$", g.strip(), re.S)
                if not gm or g.strip().startswith("'"):
                    continue
                bs = bounds.setdefault(gm.group(1), [])
                for b in split_top(gm.group(2) or "", "+"):
                    b = re.sub(r"<.*", "", b.strip().lstrip("?")).split("::")[-1]
                    if b and not b.startswith("'") and b not in ("Sized", "Send", "Sync", "Unpin") and b not in bs:
                        bs.append(b)
            # the trait half and the type half meet at " for " outside any brackets
            joined, pos, dd = hdr, -1, 0
            for j, ch in enumerate(joined):
                if ch == "-" and j + 1 < len(joined) and joined[j + 1] == ">":
                    continue
                if ch in "<([":
                    dd += 1
                elif ch in ">)]" and not (ch == ">" and j > 0 and joined[j - 1] == "-"):
                    dd -= 1
                elif dd == 0 and joined.startswith(" for ", j):
                    pos = j
                    break
            if pos >= 0:
                tname, targs = path_name(joined[:pos])
                yname, _ = path_name(joined[pos + 5:])
            else:
                tname, targs = None, ""
                yname, _ = path_name(joined)
            if not yname:
                continue
            b1 = close_of(code, e)
            meths = []
            if tname is None:
                ps = at_depth(code, [x.start() for x in _FN.finditer(code, e + 1, b1)], e + 1, 0)
                for bp in ps:
                    mm = _FN.match(code, bp)
                    if not (mm.group("vis") or "").strip().startswith("pub") or "(" in (mm.group("vis") or ""):
                        continue
                    sig, _ = fn_sig(text, code, bp)
                    li = et._line_of(nl, bp)
                    meths.append({"n": mm.group("n"), "s": sig, "d": et._doc_of(lines, li), "r": receiver(sig)})
            impls.append((mod, tname, one(targs, 60), yname, meths, bounds))
    # ---- attach impls to their types (same module first, then a unique name)
    by_name = defaultdict(list)
    for k, r in items.items():
        by_name[k.split("::")[-1]].append(k)
    for mod, tname, targs, yname, meths, bounds in impls:
        # a blanket impl (`impl<R: A> B for R`) gives every A a B; any other impl that bounds on A asks for it
        if yname in bounds and tname:
            for b in bounds[yname]:
                for k in by_name.get(b) or []:
                    if items[k]["k"] == "trait":
                        ext = items[k].setdefault("ext", [])
                        if tname not in ext and tname != b:   # `impl<T: A> A for &mut T` is A forwarding, not a gift
                            ext.append(tname)
            continue
        for b in {x for v in bounds.values() for x in v}:
            for k in by_name.get(b) or []:
                if items[k]["k"] == "trait":
                    need = items[k].setdefault("need", [])
                    pr = [tname or "", yname]
                    if tname != b and pr not in need and len(need) < CAP_BY:   # a wrapper implementing A is an implementor
                        need.append(pr)
        keys = by_name.get(yname) or []
        pick = [k for k in keys if k.rsplit("::", 1)[0] == mod] or (keys if len(keys) == 1 else [k for k in keys if k.split("::")[0] == mod.split("::")[0]])
        for k in pick:
            r = items[k]
            if tname is None:
                have = {x["n"] for x in r.setdefault("m", [])}
                for x in meths:   # cfg twins write one method twice; keep the first
                    if x["n"] not in have:
                        have.add(x["n"])
                        r["m"].append(x)
            else:
                pr = [tname, targs]
                if pr not in r.setdefault("impl", []):
                    r["impl"].append(pr)
        if tname:
            for k in by_name.get(tname) or []:
                if items[k]["k"] == "trait":
                    lst = items[k].setdefault("by", [])
                    if yname not in lst and yname != tname and len(yname) > 1:   # a lone `T` is a blanket impl, not a type
                        lst.append(yname)
    for r in items.values():
        if "m" in r:
            r["m"] = r["m"][:CAP_M]
        if "impl" in r:
            r["impl"] = r["impl"][:CAP_M]
        if "by" in r:
            r["by"] = r["by"][:CAP_BY]
    # only what the board can open: the names its detail file lists
    try:
        det = json.loads((et.PKG_DIR / et.file_name(pid)).read_text(encoding="utf-8"))
        keep = {f'{m["path"]}::{it["n"]}' for m in det["modules"] for it in m["items"]}
        items = {k: v for k, v in items.items() if k in keep}
    except OSError:
        pass
    data = json.dumps({"id": pid, "items": items}, ensure_ascii=False, separators=(",", ":"))
    (SYM_DIR / et.file_name(pid)).write_text(data, encoding="utf-8")
    return pid, len(items), len(data)


def strip_generics_after(hdr: str) -> str:
    """`trait Name<…>: Sup` → `trait Name: Sup` (drop the trait's own generics so the colon is found)."""
    m = re.match(r"(trait\s+\w+)\s*(<.*)", hdr)
    if not m:
        return hdr
    return m.group(1) + " " + strip_generics(m.group(2))


# --------------------------------------------------------------------------- your crates → symbols
SKIP_DIRS = {"target", "node_modules", "fixtures", "fixture", "testdata", "corpus", "snapshots"}
SEG = re.compile(r"(?:r#)?([A-Za-z_][A-Za-z0-9_]*)")
WS0 = re.compile(r"\s*")
AS_ALIAS = re.compile(r"as\s+(?:r#)?[A-Za-z_][A-Za-z0-9_]*|as\s+_")


def parse_entry(s, i, prefix, out):
    """A `use`/path tail into flat paths (same reading as extract_world's uses scan)."""
    i = WS0.match(s, i).end()
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
            i = WS0.match(s, i).end()
            if i >= len(s):
                return i
            if s[i] == "}":
                return i + 1
            if s[i] == ",":
                i += 1
                continue
            j = parse_entry(s, i, prefix, out)
            j = WS0.match(s, j).end()
            am = AS_ALIAS.match(s, j)
            if am:
                j = WS0.match(s, am.end()).end()
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
    j = WS0.match(s, i).end()
    if s.startswith("::", j):
        k = WS0.match(s, j + 2).end()
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


def yours_uses(W):
    P = W["packages"]
    names_of = {}
    for pid, p in P.items():
        names_of[pid] = {it["n"] for m in p["modules"] for it in m["items"]}
    out = {}
    for pid, p in P.items():
        if p["kind"] != "yours":
            continue
        cdir = ROOT / W["members_info"][p["name"]]["dir"]
        deps = [P[d] for d in p["deps"] if d in P and P[d]["kind"] != "yours"]
        if not deps:
            continue
        ident = {}
        for d in deps:
            idn = d["name"].replace("-", "_")
            if d["kind"] == "registry":
                mf = ew.load_toml(REG_SRC / f'{d["name"]}-{d["version"]}' / "Cargo.toml")
                ln = ((mf or {}).get("lib") or {}).get("name")
                if ln:
                    idn = ln.replace("-", "_")
            ident.setdefault(idn, d)
        alt = "|".join(sorted((re.escape(k) for k in ident), key=len, reverse=True))
        pre = re.compile(r"(?<![A-Za-z0-9_])(" + alt + r")(?=\s*::)")
        files = []
        for dp, dns, fns in os.walk(cdir):
            dpp = Path(dp)
            dns[:] = sorted(x for x in dns if not x.startswith(".") and x not in SKIP_DIRS and not (dpp != cdir and (dpp / x / "Cargo.toml").exists()))
            files += [dpp / f for f in fns if f.endswith(".rs")]
        acc = defaultdict(lambda: defaultdict(lambda: {"n": 0, "m": Counter(), "q": Counter(), "l": [], "i": [], "ml": defaultdict(list)}))
        for f in sorted(files):
            try:
                text = f.read_bytes().decode("utf-8", "replace")
            except OSError:
                continue
            code = ew.mask_rust(text)
            nl = et._newlines(text)
            lines = text.split("\n")
            rel = str(f.relative_to(cdir))
            imported = {}   # local name -> (dep, item name)
            for m in pre.finditer(code):
                st = m.start()
                if st > 0 and code[st - 1] == ".":
                    continue
                if code[max(0, st - 2):st] == "::":
                    b = code[st - 3] if st >= 3 else " "
                    if b.isalnum() or b in "_>)":
                        continue
                d = ident[m.group(1)]
                j = WS0.match(code, m.end()).end() + 2
                paths = []
                parse_entry(code, j, [], paths)
                is_use = bool(re.search(r"\buse\s+(::)?\s*$", code[max(0, st - 12):st]))
                li = et._line_of(nl, st)
                for path in paths:
                    sym = next((k for k, s in enumerate(path) if s in names_of[d["id"]]), None)
                    if sym is None:
                        continue
                    nm = path[sym]
                    if is_use:
                        imported[nm] = (d, nm)
                    a = acc[d["id"]][nm]
                    a["n"] += 1
                    # the module path the use spells out (`de` in toml::de::Error): tells same-named items apart
                    a["q"]["::".join(path[:sym])] += 1
                    line = lines[li].strip() if li < len(lines) else ""
                    ev = [rel, li + 1, line[:140]]
                    if sym + 1 < len(path):
                        a["m"][path[sym + 1]] += 1
                        ml = a["ml"][path[sym + 1]]
                        if len(ml) < 3 and ev not in ml:
                            ml.append(ev)
                    # up to ten lines per crate, spread over files (the call-site stack fans these out)
                    if len(a["l"]) < LINES_CAP and not line.startswith("//") and ev not in a["l"] and sum(x[0] == rel for x in a["l"]) < 3:
                        a["l"].append(ev)
            # derives and impls of imported (or path-named) traits
            for dm in re.finditer(r"#\[derive\s*\(([^)]*)\)\]", text):
                names = [x.strip() for x in dm.group(1).split(",") if x.strip()]
                after = code[dm.end():dm.end() + 400]
                tm = re.search(r"\b(?:struct|enum|union)\s+(\w+)", after)
                if not tm:
                    continue
                for nmx in names:
                    segs = nmx.split("::")
                    hit = None
                    if len(segs) > 1 and segs[0] in ident:
                        dd = ident[segs[0]]
                        if segs[-1] in names_of[dd["id"]]:
                            hit = (dd, segs[-1])
                    elif nmx in imported:
                        hit = imported[nmx]
                    if hit:
                        a = acc[hit[0]["id"]][hit[1]]
                        if len(a["i"]) < 6:
                            a["i"].append([tm.group(1), rel, et._line_of(nl, dm.start()) + 1])
                        a["ni"] = a.get("ni", 0) + 1
            for im in re.finditer(r"(?m)^[ \t]*(?:unsafe[ \t]+)?impl\b[^{;]*?\bfor\s+&?(?:'\w+\s+)?(?:mut\s+)?(\w+)", code):
                hdr = strip_generics(re.sub(r"^\s*(unsafe\s+)?impl", "", im.group(0)))
                tpath = hdr.split(" for ")[0].strip()
                segs = re.split(r"::", re.sub(r"<.*", "", tpath).strip().lstrip("!"))
                hit = None
                if len(segs) > 1 and segs[0] in ident and segs[-1] in names_of[ident[segs[0]]["id"]]:
                    hit = (ident[segs[0]], segs[-1])
                elif len(segs) == 1 and segs[0] in imported:
                    hit = imported[segs[0]]
                if hit:
                    a = acc[hit[0]["id"]][hit[1]]
                    if len(a["i"]) < 6:
                        a["i"].append([im.group(1), rel, et._line_of(nl, im.start()) + 1])
                    a["ni"] = a.get("ni", 0) + 1
        for did, syms in acc.items():
            rec = {}
            for nm, a in syms.items():
                r = {"n": a["n"], "l": a["l"]}
                if any(a["q"]):
                    r["q"] = dict(a["q"])
                if a["m"]:
                    r["m"] = dict(a["m"].most_common(12))
                    r["ml"] = {k: a["ml"][k] for k in r["m"] if a["ml"].get(k)}
                if a["i"]:
                    r["i"] = a["i"]
                    r["ni"] = a.get("ni", len(a["i"]))
                rec[nm] = r
            out[f"{pid}>{did}"] = rec
    return out


def main():
    t0 = time.time()
    SYM_DIR.mkdir(parents=True, exist_ok=True)
    W = json.loads((OUT / "world.json").read_text(encoding="utf-8"))
    T = json.loads((OUT / "trust.json").read_text(encoding="utf-8"))
    jobs = []
    seen = set()
    for pid, p in W["packages"].items():
        if p["kind"] == "yours":
            d = ROOT / W["members_info"][p["name"]]["dir"]
        elif p["kind"] == "registry":
            d = REG_SRC / f'{p["name"]}-{p["version"]}'
        else:
            continue
        vers = [pid]
        # other releases on disk too, so a symbol's history can compare insides
        for v in (T.get(pid) or {}).get("local_versions") or []:
            vers.append(f'{p["name"]}@{v}')
        for vid in vers:
            if vid in seen:
                continue
            seen.add(vid)
            dd = d if vid == pid else REG_SRC / f'{p["name"]}-{vid.split("@", 1)[1]}'
            mf = ew.load_toml(dd / "Cargo.toml")
            lp = ((mf or {}).get("lib") or {}).get("path") or "src/lib.rs"
            root = (dd / lp).parent
            if not root.is_dir():
                root = dd / "src"
            if root.is_dir():
                jobs.append({"id": vid, "root": str(root), "skip": root == dd})
    if "--uses-only" in sys.argv:
        jobs = []
    print(len(jobs), "package scans")
    n = b = 0
    with ProcessPoolExecutor(max_workers=WORKERS) as ex:
        for pid, k, size in ex.map(scan_package, jobs, chunksize=8):
            n += k
            b += size
    print(f"sym: {n} items, {b / 1e6:.1f} MB in {time.time() - t0:.0f}s")
    U = yours_uses(W)
    (OUT / "sym_uses.json").write_text(json.dumps(U, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
    print(f"sym_uses: {len(U)} edges, {(OUT / 'sym_uses.json').stat().st_size / 1e6:.2f} MB in {time.time() - t0:.0f}s")
    for k in [k for k in U if k.endswith(">toml@0.8.23")]:
        print(k, {n: (v["n"], v.get("m")) for n, v in U[k].items()})


if __name__ == "__main__":
    main()
