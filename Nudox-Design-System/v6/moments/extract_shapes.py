#!/usr/bin/env python3
"""extract_shapes.py: the symbol lab's data, across five languages, from sources on this machine.

For each symbol it writes data/shapes/<id>.json:
  - its declaration, first doc sentence and source file:line, read from the package's own source;
  - its shape in one language-neutral model (inputs, output, threads for generics, seams for
    nothing / failure / later / many, and knobs: options that change the shape), where every fact
    carries HOW we know it (typed | said | found | seen) and the evidence line it was read from.
    The extractor asserts every evidence string is really on the line it cites, so nothing is invented;
  - every call site in the local corpus (the Rust registry sources plus this repository; the Go module
    cache; the Python stdlib and site-packages; two node_modules trees), each classified by what the
    caller does before (left) and after (right): the "ways" it is used.

Corpora (all local, nothing is fetched):
  rust   ~/.cargo/registry/src/index.crates.io-*/ (newest version of each crate) + this repository (yours)
  go     ~/go/pkg/mod (newest version of each module)
  python the 3.14 stdlib (+ site-packages: pip and its vendored libraries, meson, PIL, wheel)
  ts/js  ~/Projects/gpui-ce3/.opencode/node_modules, ~/Downloads/icon/node_modules
"""
import json, os, re, sys, glob, collections

HOME = os.path.expanduser("~")
REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
OUT = os.path.join(os.path.dirname(__file__), "data", "shapes")
REG = glob.glob(f"{HOME}/.cargo/registry/src/index.crates.io-*/")[0]
GOMOD = f"{HOME}/go/pkg/mod"
PYSTD = "/opt/homebrew/opt/python@3.14/Frameworks/Python.framework/Versions/3.14/lib/python3.14"
PYSITE = "/opt/homebrew/lib/python3.14/site-packages"
NODE = [f"{HOME}/Projects/gpui-ce3/.opencode/node_modules", f"{HOME}/Downloads/icon/node_modules"]

def vkey(v):
    return [int(x) if x.isdigit() else -1 for x in re.split(r"[.+-]", v)]

# ---------------------------------------------------------------- corpora: (package label, yours?, path)
def rust_roots():
    newest = {}
    for d in os.listdir(REG):
        m = re.match(r"^(.*)-(\d+\.\d+\.\d+.*)$", d)
        if not m: continue
        n, v = m.groups()
        if n not in newest or vkey(v) > vkey(newest[n][0]): newest[n] = (v, d)
    roots = [(n, False, os.path.join(REG, d)) for n, (v, d) in sorted(newest.items())]
    for top in ["crates", "apps", "server", "frontends", "tools", "tests", "workspace", "extensions"]:
        p = os.path.join(REPO, top)
        if os.path.isdir(p): roots.append((None, True, p))
    return roots

def go_roots():
    newest = {}
    for dirpath, dirs, files in os.walk(GOMOD):
        if "/cache" in dirpath: dirs[:] = []; continue
        base = os.path.basename(dirpath)
        if "@" in base:
            mod = os.path.relpath(dirpath, GOMOD).split("@")[0]
            v = base.split("@")[1]
            if mod not in newest or vkey(v.lstrip("v")) > vkey(newest[mod][0].lstrip("v")): newest[mod] = (v, dirpath)
            dirs[:] = []
    return [(m, False, p) for m, (v, p) in sorted(newest.items())]

def py_roots():
    roots = []
    for d in sorted(os.listdir(PYSTD)):
        p = os.path.join(PYSTD, d)
        if d in ("site-packages", "__pycache__"): continue
        if os.path.isdir(p) or d.endswith(".py"): roots.append(("stdlib/" + d.replace(".py", ""), False, p))
    for d in ["pip", "mesonbuild", "PIL", "wheel"]:
        roots.append((d, False, os.path.join(PYSITE, d)))
    return roots

def node_roots():
    roots, seen = [], set()
    for N in NODE:
        for d in sorted(os.listdir(N)):
            if d.startswith("."): continue
            ps = [os.path.join(N, d, s) for s in os.listdir(os.path.join(N, d))] if d.startswith("@") else [os.path.join(N, d)]
            for p in ps:
                name = os.path.relpath(p, N)
                if name in seen: continue
                seen.add(name); roots.append((name, False, p))
    return roots

def files(root, exts, skip=()):
    if os.path.isfile(root):
        yield root; return
    for dirpath, dirs, fs in os.walk(root):
        dirs[:] = [d for d in dirs if d not in ("target", ".git", "node_modules", "__pycache__", ".local", "vendor", "testdata") and not d.startswith(".")
                   and not (d == "turso" and dirpath.startswith(REPO))]   # server/index/turso is a vendored checkout, not yours
        if any(s in dirpath for s in skip): continue
        for f in fs:
            if f.endswith(exts) and not f.endswith((".d.ts", ".min.js", ".map")): yield os.path.join(dirpath, f)

_crate_of = {}
def crate_of(path):
    d = os.path.dirname(path)
    while d.startswith(REPO) and d != REPO:
        if d in _crate_of: return _crate_of[d]
        ct = os.path.join(d, "Cargo.toml")
        if os.path.exists(ct):
            m = re.search(r'^name\s*=\s*"([^"]+)"', open(ct).read(), re.M)
            _crate_of[d] = m.group(1) if m else os.path.basename(d)
            return _crate_of[d]
        d = os.path.dirname(d)
    return os.path.relpath(path, REPO).split("/")[0]

# ---------------------------------------------------------------- lexical call-site scanning
def close_paren(text, i):
    """i is at '('; return the index after its matching ')', skipping strings. -1 if it runs away."""
    depth, j, n = 0, i, len(text)
    while j < n and j < i + 4000:
        c = text[j]
        if c in "\"'`":
            q = c; j += 1
            while j < n and text[j] != q:
                if text[j] == "\\": j += 1
                if q != "`" and text[j] == "\n": break
                j += 1
        elif c in "([{": depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0: return j + 1
        j += 1
    return -1

def split_args(s):
    out, depth, cur, j = [], 0, "", 0
    while j < len(s):
        c = s[j]
        if c in "\"'`":
            q = c; k = j + 1
            while k < len(s) and s[k] != q:
                if s[k] == "\\": k += 1
                k += 1
            cur += s[j:k + 1]; j = k + 1; continue
        if c in "([{<" and not (c == "<" and depth == 0 and not re.match(r"\w", s[j - 1:j] or " ")): depth += 1
        if c in ")]}>" and depth > 0 and not (c == ">" and s[j - 1:j] == "="): depth -= 1
        if c == "," and depth == 0: out.append(cur.strip()); cur = ""
        else: cur += c
        j += 1
    if cur.strip(): out.append(cur.strip())
    return out

def scan(roots, exts, rx, prefilter, keep_file=None, skip=()):
    sites = []
    r = re.compile(rx)
    for label, yours, root in roots:
        for f in files(root, exts, skip):
            try: text = open(f, errors="ignore").read()
            except Exception: continue
            if prefilter not in text: continue
            if keep_file and not keep_file(text): continue
            lines_at = [0] + [m.end() for m in re.finditer("\n", text)]
            for m in r.finditer(text):
                p = text.find("(", m.end() - 1)
                if p < 0: continue
                end = close_paren(text, p)
                if end < 0: continue
                ls = text.rfind("\n", 0, m.start()) + 1
                le = text.find("\n", end); le = len(text) if le < 0 else le
                line_no = text.count("\n", 0, m.start()) + 1
                pre = text[ls:m.start()]
                # a Rust/JS chain that starts on the previous line: `let x: T =\n    serde_json::from_str(`
                if not pre.strip():
                    prev = text[max(0, ls - 300):ls].rstrip("\n").split("\n")[-1]
                    if prev.rstrip().endswith(("=", "(", ",", "return", "=>", "?", "Ok(", "Some(")): pre = prev.strip() + " "
                post = text[end:le]
                # continuation lines: `.map_err(...)` or `?` on the next lines
                k = le
                for _ in range(4):
                    nl = text.find("\n", k + 1); nl = len(text) if nl < 0 else nl
                    nxt = text[k + 1:nl]
                    if re.match(r"^\s*[.?]", nxt) and not re.match(r"^\s*\.\.\.", nxt): post += " " + nxt.strip(); k = nl
                    else: break
                after = text[k + 1:k + 1 + 900].split("\n")[:6]
                before = text[max(0, ls - 3000):ls].split("\n")[-40:]
                pkg = label if label else crate_of(f)
                # inside a string literal (an odd number of quotes before it on the line), it's a mention, not a call
                if (pre.count('"') - pre.count('\\"')) % 2 == 1: continue
                if re.match(r"^\s*(//|#|\*|/\*|--|>>>|\.\.\.)", pre) or re.match(r"^\s*(//|#|\*)", text[ls:m.start()]): continue
                rel = os.path.relpath(f, root)
                ctx = "code"
                if re.search(r"(^|/)(tests?|testing|testdata|__tests__|spec)(/|$)|_test\.go$|(^|/)test_[^/]*\.py$|_tests?\.(rs|py)$|\.(test|spec)\.[jt]s$", rel): ctx = "test"
                elif re.search(r"(^|/)(examples?|demo|samples?)(/|$)|example_test\.go$", rel): ctx = "example"
                elif re.search(r"(^|/)benches?(/|$)|_bench", rel): ctx = "bench"
                elif f.endswith(".rs"):
                    cfg = text.rfind("#[cfg(test)]", 0, m.start())
                    if cfg >= 0 or re.search(r"#\[(tokio::)?test\]", "\n".join(before[-30:])): ctx = "test"
                elif f.endswith(".go") and re.search(r"^func (Test|Benchmark)\w*\(", "\n".join(before), re.M): ctx = "test"
                sites.append({
                    "ctx": ctx,
                    "pkg": pkg, "yours": yours, "file": os.path.relpath(f, root) if not yours else os.path.relpath(f, REPO),
                    "line": line_no, "pre": pre, "call": text[m.start():end], "args": text[p + 1:end - 1], "post": post,
                    "after": after, "before": before, "src": text[ls:le][:400],
                })
    return sites

def squash(s, n=90):
    s = re.sub(r"\s+", " ", s).strip()
    return s if len(s) <= n else s[:n - 1] + "…"

def collapse_call(s):
    """.map_err(|e| Error::Parse(e))?; -> ['.map_err(…)', '?', ';']"""
    toks, j = [], 0
    s = s.strip()
    while j < len(s) and len(toks) < 4:
        c = s[j]
        if c.isspace(): j += 1; continue
        m = re.match(r"\.\s*(\w+)\s*(::<[^>]*>)?\s*\(", s[j:])
        if m:
            p = j + m.end() - 1; e = close_paren(s, p)
            inner = s[p + 1:e - 1].strip() if e > 0 else ""
            toks.append(f".{m.group(1)}({'' if not inner else '…'})"); j = e if e > 0 else len(s); continue
        m = re.match(r"\.\s*(\w+)", s[j:])
        if m: toks.append("." + m.group(1)); j += m.end(); continue
        if c in "?;),]}": toks.append(c); j += 1; continue
        m = re.match(r"(\w+|==|!=|<=|>=|&&|\|\||[^\w\s])", s[j:])
        toks.append(m.group(1)); j += m.end()
    return toks

# ---------------------------------------------------------------- evidence: every fact cites a real line
class Src:
    def __init__(self, base, rel):
        self.base, self.rel = base, rel
        self.lines = open(os.path.join(base, rel), errors="ignore").read().split("\n")
    def at(self, needle, after=0):
        for i in range(after, len(self.lines)):
            if needle in self.lines[i]: return {"f": self.rel, "l": i + 1, "t": self.lines[i].strip()[:220]}
        raise SystemExit(f"evidence not found in {self.rel}: {needle!r}")
    def line_of(self, needle, after=0): return self.at(needle, after)["l"]
    def doc_before(self, line, prefix):
        out, i = [], line - 2
        while i >= 0 and self.lines[i].strip().startswith(prefix): out.insert(0, self.lines[i].strip()[len(prefix):].strip()); i -= 1
        return out

def lede(text):
    text = re.sub(r"\s+", " ", " ".join(t for t in text if t and not t.startswith("#"))).strip()
    m = re.match(r"(.+?[.!?])(\s|$)", text)
    return (m.group(1) if m else text)[:220]

# ================================================================ Rust · serde_json::from_str
def rust_from_str():
    base = os.path.join(REG, "serde_json-1.0.151")
    de = Src(base, "src/de.rs"); err = Src(base, "src/error.rs")
    L = de.line_of("pub fn from_str<'a, T>(s: &'a str) -> Result<T>")
    doc = de.doc_before(L, "///")
    shape = {
        "ins": [{"n": "s", "t": "&'a str", "w": "text", "how": "typed", "ev": de.at("pub fn from_str<'a, T>(s: &'a str)")}],
        "out": {"w": "T", "thread": "T", "how": "typed", "ev": de.at("pub fn from_str<'a, T>(s: &'a str) -> Result<T>")},
        "threads": [{"k": "T", "role": "choose", "needs": "Deserialize", "says": "you choose what it reads into; it must be readable (Deserialize)",
                     "how": "typed", "ev": de.at("T: de::Deserialize<'a>", L)}],
        "seams": {
            "fail": {"how": "typed", "route": "returned", "w": "Error", "ev": de.at("-> Result<T>", L - 1),
                     "said": de.at("This conversion can fail if the structure of the input does not match the"),
                     "kinds": [
                         {"n": "Syntax", "w": "not valid JSON", "ev": err.at("    Syntax,")},
                         {"n": "Data", "w": "valid JSON, wrong shape for T", "ev": err.at("    Data,")},
                         {"n": "Eof", "w": "the text ended early", "ev": err.at("    Eof,")},
                         {"n": "Io", "w": "reading bytes failed (not from text)", "ev": err.at("    Io,")},
                     ]},
        },
        "knobs": [],
    }
    fam = {"axes": [["text", "bytes", "a reader", "a Value"], ["reads into your type"]], "cells": [
        {"n": "from_str", "at": [0, 0], "ins": "text", "ev": de.at("pub fn from_str<'a, T>")},
        {"n": "from_slice", "at": [1, 0], "ins": "bytes", "ev": de.at("pub fn from_slice<'a, T>")},
        {"n": "from_reader", "at": [2, 0], "ins": "a reader", "ev": de.at("pub fn from_reader<R, T>")},
        {"n": "from_value", "at": [3, 0], "ins": "a Value", "ev": Src(base, "src/value/mod.rs").at("pub fn from_value<T>")},
    ], "says": "the same reading, from four kinds of input"}

    imp = re.compile(r"use\s+serde_json::(\{[^}]*\bfrom_str\b[^}]*\}|from_str\b)")
    def keep(text): return True
    rx = r"(?<![\w:])(?:serde_json::from_str|from_str)\s*(?:::\s*<[^()]*?>\s*)?\("
    sites = scan(rust_roots(), (".rs",), rx, "from_str", keep)
    out = []
    for s in sites:
        text_ok = s["call"].startswith("serde_json::")
        if not text_ok:
            # bare from_str only counts in files that import serde_json's
            head = "\n".join(s["before"])
            if not imp.search(open_file_head(s)): continue
            if re.search(r"(fn|impl|::)\s*from_str\s*$", s["pre"]) or re.search(r"\w$", s["pre"].rstrip()) and not re.search(r"(=|\(|return|,|\{|=>|!|&|\|)\s*$", s["pre"]): continue
            if re.search(r"\.\s*$", s["pre"]): continue
        t = None
        m = re.match(r"(?:serde_json::)?from_str\s*::\s*<(.+?)>\s*\($", s["call"].split("(")[0] + "(")
        if m: t = m.group(1).strip()
        if not t:
            m = re.search(r"let\s+(?:mut\s+)?[\w(), ]+?\s*:\s*(.+?)\s*=\s*$", s["pre"])
            if m: t = m.group(1).strip()
        if t and ("_" == t): t = None
        post = collapse_call(s["post"])
        pre = s["pre"].strip()
        way = rust_handling(pre, post)
        if way == "other" and post and post[0] == ";":
            v = re.search(r"let\s+(?:mut\s+)?(\w+)\s*(?::[^=]+)?=\s*$", pre)
            if v:
                v = v.group(1); nx = " ".join(a.strip() for a in s["after"][:6])
                if re.search(rf"\b{v}\s*\?", nx): way = "propagate"
                elif re.search(rf"\b{v}\.(unwrap|expect)\(|\b{v}\.err\(\)\.unwrap", nx): way = "crash"
                elif re.search(rf"\b{v}\.(is_err|is_ok|unwrap_err|expect_err)\(|assert\w*!\([^;]*\b{v}\b", nx): way = "test"
                elif re.search(rf"\bmatch\s+&?{v}\b|if let (Ok|Err)\([^)]*\)\s*=\s*&?{v}\b|let (Ok|Err)\([^)]*\)\s*=\s*{v}\b", nx): way = "handle"
                elif re.search(rf"\b{v}\.map_err\(", nx): way = "convert"
                elif re.search(rf"\b{v}\.ok\(\)", nx): way = "discard"
                elif re.search(rf"\b{v}\.(context|with_context)\(", nx): way = "wrap"
        if t and t.startswith("$"): t = "(a macro's type)"
        out.append(site_record(s, left=rust_left(pre), right=post, fill=t or "inferred", way=way))
    return {"id": "rs-serde_json-from_str", "lang": "rust", "pkg": "serde_json", "version": "1.0.151", "module": "serde_json",
            "name": "from_str", "kind": "function", "decl": clean_decl(de, L, "{"), "doc": lede(doc), "file": f"src/de.rs:{L}",
            "shape": shape, "family": fam, "sites": out}

_head_cache = {}
def open_file_head(s):
    return "\n".join(s["before"]) + "\n" + s["src"]

def rust_left(pre):
    p = pre.rstrip()
    if re.search(r"\blet\s+(mut\s+)?\w+\s*:\s*.+=$", p): return ["let x: T ="]
    if re.search(r"\blet\s+(Ok|Some)\(", p): return ["let Ok(x) ="]
    if re.search(r"\bif let (Ok|Err)\(", p): return ["if let Ok(…) ="]
    if re.search(r"\blet\s+(mut\s+)?[\w_]+\s*=$", p): return ["let x ="]
    if re.search(r"\blet\s+\(", p): return ["let (…) ="]
    if re.search(r"\bmatch\s*$", p): return ["match"]
    if re.search(r"\breturn\s*$", p): return ["return"]
    if re.search(r"\bOk\($", p): return ["Ok("]
    if re.search(r"\bSome\($", p): return ["Some("]
    if re.search(r"assert\w*!\($|assert\w*!\(.*,\s*$", p): return ["assert…("]
    if re.search(r"\|[^|]*\|\s*$", p): return ["|…| closure"]
    if re.search(r"=>\s*$", p): return ["=> arm"]
    if re.search(r"\w+\s*:\s*$", p): return ["field:"]
    if re.search(r"[=!]=\s*$|\(\s*$|,\s*$", p): return ["(as an argument)"]
    if re.search(r"=\s*$", p): return ["x ="]
    if not p: return ["(statement)"]
    return ["(other)"]

def rust_handling(pre, post):
    first = post[0] if post else ""
    if first == "?": return "propagate"
    if first.startswith((".context(", ".with_context(", ".wrap_err(", ".wrap_err_with(", ".change_context(", ".attach")): return "wrap"
    if first.startswith(".map_err("): return "convert"
    if first in (".unwrap()",) or first.startswith(".expect("): return "crash"
    if first.startswith(".unwrap_or") : return "fallback"
    if first == ".ok()": return "discard"
    if first.startswith((".is_err(", ".is_ok(", ".unwrap_err(", ".expect_err(")): return "test"
    if first.startswith((".map(", ".and_then(", ".or_else(", ".map_or")): return "chain"
    if re.search(r"\b(match|if let|let (Ok|Err)\()", pre): return "handle"
    if re.search(r"assert", pre): return "test"
    if re.search(r"(return|Ok\(|=>)\s*$", pre) or first in (")", "}", ";") and not pre.strip().startswith("let") and re.search(r"(return|^)\s*$", pre): return "pass"
    if first in (")", ",", "}") and not pre.strip(): return "pass"
    return "other"

def site_record(s, left, right, fill=None, way=None, extra=None):
    r = {"p": s["pkg"], "y": s["yours"], "x": s["ctx"], "f": s["file"], "l": s["line"], "left": left, "right": right, "way": way,
         "pre": squash(s["pre"], 70), "call": squash(s["call"], 110), "post": squash(s["post"], 60)}
    if fill: r["fill"] = fill
    if extra: r.update(extra)
    return r

def clean_decl(src, line, stop):
    out = []
    for i in range(line - 1, min(line + 12, len(src.lines))):
        t = src.lines[i]
        if stop in t: out.append(t.split(stop)[0].rstrip()); break
        out.append(t.rstrip())
    return "\n".join(out).strip()

# ================================================================ Python · re.match
def py_re_match():
    s = Src(PYSTD, "re/__init__.py")
    L = s.line_of("def match(pattern, string, flags=0):")
    shape = {
        "ins": [
            {"n": "pattern", "w": "a pattern (text or bytes)", "how": "said", "ev": s.at("def match(pattern, string, flags=0):"),
             "seen": None},
            {"n": "string", "w": "text or bytes", "how": "seen", "ev": s.at("def match(pattern, string, flags=0):")},
            {"n": "flags", "w": "flags", "opt": True, "dflt": "0", "how": "found", "ev": s.at("def match(pattern, string, flags=0):")},
        ],
        "out": {"w": "a Match", "how": "said", "ev": s.at("Try to apply the pattern at the start of the string, returning")},
        "threads": [{"k": "AnyStr", "role": "through", "says": "text in, text out; bytes in, bytes out", "how": "said",
                     "ev": s.at("those found in Perl.  It supports both 8-bit and Unicode strings; both")}],
        "seams": {
            "none": {"how": "said", "w": "None when it doesn't match", "ev": s.at("a Match object, or None if no match was found.")},
            "fail": {"how": "found", "route": "raised", "w": "re.error (PatternError)", "when": "the pattern itself is bad",
                     "ev": Src(PYSTD, "re/_constants.py").at("class PatternError(Exception):"), "kinds": []},
        },
        "knobs": [],
        "untyped": "The stdlib ships no type hints for re; typeshed (not on this machine) would say AnyStr.",
    }
    fam = {"axes": [["at the start", "anywhere", "the whole string"], ["one"]], "says": "the same test, looking in three places", "cells": [
        {"n": "match", "at": [0, 0], "ev": s.at("def match(pattern, string, flags=0):")},
        {"n": "search", "at": [1, 0], "ev": s.at("def search(pattern, string, flags=0):")},
        {"n": "fullmatch", "at": [2, 0], "ev": s.at("def fullmatch(pattern, string, flags=0):")},
    ]}
    sites = scan(py_roots(), (".py",), r"(?<![\w.])re\.match\s*\(", "re.match")
    out = []
    for x in sites:
        pre = x["pre"].rstrip(); post = collapse_call(x["post"])
        left, way = py_left(pre), None
        first = post[0] if post else ""
        if first.startswith((".group(", ".groups(", ".start(", ".end(", ".span(", ".groupdict(", ".expand(")) or first in (".string", ".pos", ".lastindex", ".re"):
            way = "unchecked"
        elif left[0] in ("if", "if not", "elif", "while", "not", "and / or", "if … and / or", "… if (filter)", "bool(…)", "ternary"):
            way = "cond"
        elif left[0] == "assert":
            way = "test"
        elif left[0] == "return":
            way = "pass"
        elif left[0] == "x =":
            var = re.search(r"(\w+)\s*=\s*$", pre)
            v = var.group(1) if var else None
            nxt = " ".join(a.strip() for a in x["after"][:5])
            if v and re.search(rf"\bif\s+(not\s+)?{v}\b|\b{v}\s+is\s+(not\s+)?None\b|\bassert\s+{v}\b|\bif\s+{v}\s*:|\b{v}\s+and\b|\bwhile\s+{v}\b|\bor\s+not\s+{v}\b", nxt): way = "checked"
            elif v and re.search(rf"\b{v}\.(group|groups|start|end|span|groupdict)\(", nxt): way = "unchecked"
            else: way = "other"
        else:
            way = "other"
        out.append(site_record(x, left=left, right=post, way=way))
    return {"id": "py-re-match", "lang": "python", "pkg": "re", "version": "3.14 stdlib", "module": "re", "name": "match", "kind": "function",
            "decl": "def match(pattern, string, flags=0):", "doc": "Try to apply the pattern at the start of the string, returning a Match object, or None if no match was found.",
            "file": f"re/__init__.py:{L}", "shape": shape, "family": fam, "sites": out}

def py_left(p):
    p = p.rstrip()
    if re.search(r"\bif\s+not$|\bif$|\bif\s*\($", p): return ["if"] if not p.endswith("not") else ["if not"]
    if re.search(r"\belif(\s+not)?$", p): return ["elif"]
    if re.search(r"\bwhile(\s+not)?$", p): return ["while"]
    if re.search(r"\bfor\b.*\bif(\s+not)?$", p): return ["… if (filter)"]
    if re.search(r"\bif\b.*\b(and|or)(\s+not)?$", p): return ["if … and / or"]
    if re.search(r"\bassert\w*\(?(\s*not)?$", p): return ["assert"]
    if re.search(r"\breturn$", p): return ["return"]
    if re.search(r"\b(and|or)(\s+not)?$", p): return ["and / or"]
    if re.search(r"\bnot$", p): return ["not"]
    if re.search(r"\bbool\($", p): return ["bool(…)"]
    if re.search(r"\belse$|\bif\b.*\belse$", p): return ["ternary"]
    if re.search(r"(?<![=!<>])=$", p) and not re.search(r"\(\s*\w+\s*=$", p): return ["x ="]
    if re.search(r"[(,]$|\w+=$", p): return ["(as an argument)"]
    if not p.strip(): return ["(statement)"]
    return ["(other)"]

# ================================================================ Python · subprocess.run
def py_subprocess_run():
    s = Src(PYSTD, "subprocess.py")
    L = s.line_of("def run(*popenargs,")
    shape = {
        "ins": [
            {"n": "*popenargs", "w": "the program and its arguments", "rest": True, "how": "said", "ev": s.at("def run(*popenargs,")},
            {"n": "input", "w": "text or bytes to send", "opt": True, "dflt": "None", "kw": True, "how": "found", "ev": s.at("input=None, capture_output=False, timeout=None, check=False, **kwargs):")},
            {"n": "capture_output", "w": "yes or no", "opt": True, "dflt": "False", "kw": True, "how": "found", "ev": s.at("input=None, capture_output=False, timeout=None, check=False, **kwargs):")},
            {"n": "timeout", "w": "seconds", "opt": True, "dflt": "None", "kw": True, "how": "found", "ev": s.at("input=None, capture_output=False, timeout=None, check=False, **kwargs):")},
            {"n": "check", "w": "yes or no", "opt": True, "dflt": "False", "kw": True, "how": "found", "ev": s.at("input=None, capture_output=False, timeout=None, check=False, **kwargs):")},
            {"n": "**kwargs", "w": "anything Popen takes", "rest": True, "kw": True, "how": "said", "ev": s.at("The other arguments are the same as for the Popen constructor.")},
        ],
        "out": {"w": "a CompletedProcess", "how": "said", "ev": s.at("Run command with arguments and return a CompletedProcess instance.")},
        "threads": [],
        "seams": {
            "fail": {"how": "said", "route": "raised", "w": "CalledProcessError", "when": "check=True and the exit code isn't 0",
                     "ev": s.at("If check is True and the exit code was non-zero, it raises a"),
                     "kinds": [
                         {"n": "CalledProcessError", "w": "exited non-zero (only with check=True)", "ev": s.at("raise CalledProcessError(retcode, process.args,", L)},
                         {"n": "TimeoutExpired", "w": "took longer than timeout", "ev": s.at("a TimeoutExpired exception will be raised.")},
                         {"n": "ValueError", "w": "input and stdin both given", "ev": s.at("raise ValueError('stdin and input arguments may not both be used.')", L)},
                         {"n": "FileNotFoundError", "w": "the program isn't there (from Popen)", "how": "seen", "ev": s.at("with Popen(*popenargs, **kwargs) as process:", L)},
                     ]},
        },
        "knobs": [
            {"n": "check", "on": "True", "changes": "fail", "w": "a non-zero exit raises instead of returning", "ev": s.at("if check and retcode:", L)},
            {"n": "capture_output", "on": "True", "changes": "out", "w": "stdout and stderr are kept (else None)", "ev": s.at("if capture_output:", L)},
            {"n": "text", "on": "True", "changes": "out", "w": "stdout is text, not bytes", "how": "said", "ev": s.at("text: If true, decode stdin, stdout and stderr using the given encoding")},
        ],
    }
    sites = scan(py_roots(), (".py",), r"(?<![\w.])subprocess\.run\s*\(", "subprocess.run")
    out = []
    for x in sites:
        pre = x["pre"].rstrip(); post = collapse_call(x["post"])
        args = split_args(x["args"])
        kws = sorted({re.match(r"(\w+)\s*=", a).group(1) for a in args if re.match(r"\w+\s*=[^=]", a)})
        chk = any(re.match(r"check\s*=\s*True", a) for a in args)
        nxt = " ".join(a.strip() for a in x["after"][:4])
        var = re.search(r"(\w+)\s*=\s*$", pre)
        v = var.group(1) if var else None
        if chk: way = "raises (check=True)"
        elif v and re.search(rf"\b{v}\.returncode\b|\b{v}\.check_returncode\(", nxt) or ".returncode" in post: way = "reads returncode"
        elif post and post[0] == ".check_returncode()": way = "reads returncode"
        elif not pre.strip() or pre.strip().endswith(":"): way = "ignores the exit"
        else: way = "other"
        out.append(site_record(x, left=py_left(pre), right=post or [")"], way=way, extra={"kw": kws}))
    return {"id": "py-subprocess-run", "lang": "python", "pkg": "subprocess", "version": "3.14 stdlib", "module": "subprocess", "name": "run", "kind": "function",
            "decl": "def run(*popenargs,\n        input=None, capture_output=False, timeout=None, check=False, **kwargs):",
            "doc": "Run command with arguments and return a CompletedProcess instance.", "file": f"subprocess.py:{L}", "shape": shape, "sites": out}

# ================================================================ Go · yaml.Unmarshal
def go_yaml_unmarshal():
    base = f"{GOMOD}/gopkg.in/yaml.v3@v3.0.1"
    s = Src(base, "yaml.go")
    L = s.line_of("func Unmarshal(in []byte, out interface{}) (err error) {")
    shape = {
        "ins": [
            {"n": "in", "t": "[]byte", "w": "bytes", "how": "typed", "ev": s.at("func Unmarshal(in []byte, out interface{}) (err error) {")},
            {"n": "out", "t": "interface{}", "w": "a pointer it fills", "how": "said", "fillsIn": True, "ev": s.at("// and assigns decoded values into the out value.")},
        ],
        "out": {"w": "nothing (it fills out)", "how": "typed", "ev": s.at("func Unmarshal(in []byte, out interface{}) (err error) {")},
        "threads": [{"k": "out", "role": "choose", "says": "you choose what it fills by the pointer you pass; nothing checks it until it runs", "how": "said",
                     "ev": s.at("// Maps and pointers (to a struct, string, int, etc) are accepted as out")}],
        "seams": {
            "fail": {"how": "typed", "route": "returned", "w": "error", "ev": s.at("func Unmarshal(in []byte, out interface{}) (err error) {"),
                     "partial": s.at("// mismatches, decoding continues partially until the end of the YAML"),
                     "kinds": [
                         {"n": "*yaml.TypeError", "w": "some values didn't fit; the rest were filled", "ev": s.at("// content, and a *yaml.TypeError is returned with details for all")},
                         {"n": "syntax", "w": "not valid YAML", "how": "found", "ev": s.at("func handleErr(err *error) {")},
                     ]},
        },
        "knobs": [],
    }
    rx = r"(?<![\w.])yaml\.Unmarshal\s*\("
    sites = scan(go_roots(), (".go",), rx, "yaml.Unmarshal")
    out = []
    for x in sites:
        pre = x["pre"].rstrip()
        args = split_args(x["args"])
        fill = None
        if len(args) >= 2:
            a = args[1].strip()
            m = re.match(r"&(\w+)$", a)
            if m:
                v = m.group(1)
                for b in reversed(x["before"]):
                    mm = re.search(rf"\bvar\s+{v}\s+([\w.\[\]*]+(\{{\}})?)", b) or re.search(rf"\b{v}\s*:=\s*&?([\w.\[\]*]+)\{{", b) or re.search(rf"\b{v}\s*:=\s*make\(([^,)]+)", b)
                    if mm: fill = mm.group(1); break
                fill = fill or "a variable"
            elif re.match(r"&\w+\{", a): fill = re.match(r"&(\w+)\{", a).group(1)
            elif re.match(r"\w+$", a): fill = "a pointer variable"
            else: fill = "an expression"
        nxt = [a.strip() for a in x["after"][:4]]
        post = x["post"].strip()
        left = go_left(pre)
        way = "other"
        body = " ".join(nxt)
        if left[0] in ("_ =",): way = "discard"
        elif left[0] == "(statement)" and not post.startswith(";"): way = "discard"
        elif left[0] in ("require.NoError(", "assert.NoError(", "c.Assert("): way = "test"
        elif left[0] == "return": way = "pass"
        elif re.search(r"\b(c|s)\.Assert\(\s*err\b|\b(require|assert)\.(NoError|Nil|Error|ErrorContains|EqualError)\(\s*\w+,\s*err\b|\bassert\.(NoError|Nil)\(\s*err", body): way = "test"
        elif re.search(r"err\s*!=\s*nil", post + " " + body):
            act = body.split("err != nil", 1)[-1] if "err != nil" in body else body
            if re.search(r"\bt\.(Fatal|Fatalf|Error|Errorf|Skip)|\bc\.(Fatal|Assert)|\bs\.(Fatal|FailNow)", act): way = "test"
            elif re.search(r"\bpanic\(|log\.Fatal", act): way = "crash"
            elif re.search(r"return\s+.*(fmt\.Errorf|errors\.Wrap|Wrapf|\bnewError)", act): way = "wrap"
            elif re.search(r"return\b", act): way = "propagate"
            elif re.search(r"\bcontinue\b|\bbreak\b", act): way = "skip"
            else: way = "handle"
        elif left[0] in ("if err :=",): way = "handle"
        out.append(site_record(x, left=left, right=[post[:20] or "↵"], fill=fill, way=way))
    return {"id": "go-yaml-unmarshal", "lang": "go", "pkg": "gopkg.in/yaml.v3", "version": "v3.0.1", "module": "yaml", "name": "Unmarshal", "kind": "function",
            "decl": "func Unmarshal(in []byte, out interface{}) (err error)", "doc": "Unmarshal decodes the first document found within the in byte slice and assigns decoded values into the out value.",
            "file": f"yaml.go:{L}", "shape": shape, "sites": out}

def go_left(p):
    p = p.rstrip()
    if re.search(r"\bif\s+err\s*:?=\s*$", p): return ["if err :="]
    if re.search(r"\berr\s*:=\s*$", p) or re.search(r"\w+,\s*err\s*:=\s*$", p): return ["err :="]
    if re.search(r"\berr\s*=\s*$", p): return ["err ="]
    if re.search(r"\b_\s*=\s*$", p): return ["_ ="]
    if re.search(r"require\.NoError\(\s*\w+,\s*$", p): return ["require.NoError("]
    if re.search(r"assert\.NoError\(\s*\w+,\s*$", p): return ["assert.NoError("]
    if re.search(r"\bc\.Assert\(\s*$", p): return ["c.Assert("]
    if re.search(r"\breturn\s*$", p): return ["return"]
    if not p.strip(): return ["(statement)"]
    if re.search(r"[(,]\s*$", p): return ["(as an argument)"]
    return ["(other)"]

# ================================================================ Go · slices.BinarySearch (generics with a capability, and "found")
def go_binary_search():
    base = glob.glob(f"{GOMOD}/golang.org/x/exp@*")[0]
    s = Src(base, "slices/sort.go")
    L = s.line_of("func BinarySearch[S ~[]E, E cmp.Ordered](x S, target E) (int, bool) {")
    doc = s.doc_before(L, "//")
    shape = {
        "ins": [
            {"n": "x", "t": "S", "w": "a sorted list of E", "thread": "E", "how": "typed", "ev": s.at("func BinarySearch[S ~[]E")},
            {"n": "target", "t": "E", "w": "one E", "thread": "E", "how": "typed", "ev": s.at("(x S, target E) (int, bool)")},
        ],
        "out": {"w": "a position", "how": "typed", "ev": s.at("(x S, target E) (int, bool)")},
        "threads": [
            {"k": "E", "role": "needs", "needs": "cmp.Ordered", "says": "anything that orders with < (numbers, text)", "how": "typed", "ev": s.at("E cmp.Ordered](x S")},
            {"k": "S", "role": "through", "says": "any list type whose elements are E (~[]E)", "how": "typed", "ev": s.at("[S ~[]E,")},
        ],
        "seams": {
            "none": {"how": "typed", "w": "found? (a bool): where it would go if it isn't there", "ev": s.at("(int, bool) {"), "said": s.at("// sort order; it also returns a bool saying whether the target is really found")},
        },
        "knobs": [],
    }
    fam = {"axes": [["ordered by <", "ordered by your function"], ["one"]], "says": "the same search, ordered two ways", "cells": [
        {"n": "BinarySearch", "at": [0, 0], "ev": s.at("func BinarySearch[S ~[]E, E cmp.Ordered]")},
        {"n": "BinarySearchFunc", "at": [1, 0], "ev": s.at("func BinarySearchFunc[")},
    ]}
    return {"id": "go-slices-binarysearch", "lang": "go", "pkg": "golang.org/x/exp", "version": os.path.basename(base).split("@")[1], "module": "slices",
            "name": "BinarySearch", "kind": "function", "decl": "func BinarySearch[S ~[]E, E cmp.Ordered](x S, target E) (int, bool)",
            "doc": lede(doc), "file": f"slices/sort.go:{L}", "shape": shape, "family": fam, "sites": []}

# ================================================================ Go · testify assert.Equal (highly used, many ways)
def go_assert_equal():
    base = f"{GOMOD}/github.com/stretchr/testify@v1.9.0"
    s = Src(base, "assert/assertions.go")
    L = s.line_of("func Equal(t TestingT, expected, actual interface{}, msgAndArgs ...interface{}) bool {")
    doc = s.doc_before(L, "//")
    shape = {
        "ins": [
            {"n": "t", "t": "TestingT", "w": "the test", "how": "typed", "ev": s.at("func Equal(t TestingT")},
            {"n": "expected", "t": "interface{}", "w": "what it should be", "how": "typed", "ev": s.at("expected, actual interface{}", L - 1)},
            {"n": "actual", "t": "interface{}", "w": "what it is", "how": "typed", "ev": s.at("expected, actual interface{}", L - 1)},
            {"n": "msgAndArgs", "t": "...interface{}", "w": "a message", "rest": True, "opt": True, "how": "typed", "ev": s.at("msgAndArgs ...interface{}) bool {", L - 1)},
        ],
        "out": {"w": "equal? (yes or no)", "how": "typed", "ev": s.at("msgAndArgs ...interface{}) bool {", L - 1)},
        "threads": [],
        "seams": {"fail": {"how": "found", "route": "reports", "w": "marks the test failed, keeps going", "ev": s.at("return Fail(t, fmt.Sprintf(\"Not equal:", L),
                           "kinds": [{"n": "Fail", "w": "not equal: the test is marked failed", "ev": s.at("return Fail(t, fmt.Sprintf(\"Not equal:", L)},
                                     {"n": "Fail", "w": "a function was compared: always fails", "ev": s.at("if err := validateEqualArgs(expected, actual); err != nil {", L)}]}},
        "knobs": [],
    }
    rx = r"(?<![\w.])assert\.Equal\s*\("
    sites = scan(go_roots(), (".go",), rx, "assert.Equal")
    out = []
    lit = re.compile(r'^(-?\d[\w.]*|"(?:[^"\\]|\\.)*"|`[^`]*`|true|false|nil|\'.\')$')
    for x in sites:
        args = split_args(x["args"])
        if len(args) < 3: continue
        e, a = args[1], args[2]
        shape_of = lambda v: "a literal" if lit.match(v) else "len(…)" if v.startswith("len(") else "err" if re.match(r"^err\b", v) else ".Error()" if v.endswith(".Error()") else "a value it builds" if re.match(r"^(&?[\w.]+\{|\[\]|map\[|[\w.]+\()", v) else "a variable"
        es, as_ = shape_of(e), shape_of(a)
        way = "reversed?" if as_ == "a literal" and es != "a literal" else "expected first" if es == "a literal" else "two values"
        left = go_left(x["pre"].rstrip())
        if re.search(r"\bif\s+!\s*$", x["pre"]): left = ["if !"]
        msg = "with a message" if len(args) > 3 else "no message"
        out.append(site_record(x, left=left, right=[msg], way=way, extra={"e": squash(e, 40), "a": squash(a, 40), "es": es, "as": as_}))
    return {"id": "go-testify-assert-equal", "lang": "go", "pkg": "github.com/stretchr/testify", "version": "v1.9.0", "module": "assert", "name": "Equal", "kind": "function",
            "decl": "func Equal(t TestingT, expected, actual interface{}, msgAndArgs ...interface{}) bool", "doc": lede(doc), "file": f"assert/assertions.go:{L}",
            "shape": shape, "sites": out}

# ================================================================ TypeScript · Effect.map (threads: A turns into B; E and R pass through)
def ts_effect_map():
    base = f"{NODE[0]}/effect"
    s = Src(base, "dist/Effect.d.ts")
    L = s.line_of("export declare const map: {")
    shape = {
        "ins": [
            {"n": "self", "t": "Effect<A, E, R>", "w": "an effect", "thread": "A E R", "how": "typed", "ev": s.at("<A, E, R, B>(self: Effect<A, E, R>, f: (a: A) => B): Effect<B, E, R>;", L)},
            {"n": "f", "t": "(a: A) => B", "w": "a function from A to B", "fn": {"in": "A", "out": "B"}, "how": "typed", "ev": s.at("<A, E, R, B>(self: Effect<A, E, R>, f: (a: A) => B): Effect<B, E, R>;", L)},
        ],
        "out": {"w": "an effect of B", "thread": "B E R", "how": "typed", "ev": s.at("<A, E, R, B>(self: Effect<A, E, R>, f: (a: A) => B): Effect<B, E, R>;", L)},
        "threads": [
            {"k": "A", "role": "turns", "into": "B", "says": "its value goes through your function", "how": "typed", "ev": s.at("f: (a: A) => B", L)},
            {"k": "B", "role": "turns", "from": "A", "says": "what your function gives becomes its value", "how": "typed", "ev": s.at("): Effect<B, E, R>;", L)},
            {"k": "E", "role": "through", "says": "its failure passes through untouched", "how": "typed", "ev": s.at("without changing the original effect's")},
            {"k": "R", "role": "through", "says": "what it needs to run passes through untouched", "how": "typed", "ev": s.at("typed error or context requirements.")},
        ],
        "seams": {
            "fail": {"how": "typed", "route": "carried", "w": "E, inside the effect", "ev": s.at("): Effect<B, E, R>;", L),
                     "kinds": [], "note": "It can't fail by itself; failure travels in the effect's E."},
            "later": {"how": "typed", "w": "an Effect: nothing runs until you run it", "ev": s.at("It's important to note that effects are immutable, meaning that the original")},
        },
        "knobs": [],
        "forms": [
            {"n": "data-last", "w": "Effect.map(f) inside pipe", "ev": s.at("<A, B>(f: (a: A) => B): <E, R>(self: Effect<A, E, R>) => Effect<B, E, R>;", L)},
            {"n": "data-first", "w": "Effect.map(effect, f)", "ev": s.at("<A, E, R, B>(self: Effect<A, E, R>, f: (a: A) => B): Effect<B, E, R>;", L)},
        ],
    }
    fam = {"axes": [["its value", "its failure", "both"], ["with a plain function", "with an effect"]], "says": "map touches one channel with a plain function", "cells": [
        {"n": "map", "at": [0, 0], "ev": s.at("export declare const map: {")},
        {"n": "mapError", "at": [1, 0], "ev": s.at("export declare const mapError: {")},
        {"n": "mapBoth", "at": [2, 0], "ev": s.at("export declare const mapBoth: {")},
        {"n": "flatMap", "at": [0, 1], "ev": s.at("export declare const flatMap: {")},
    ]}
    sites = scan(node_roots(), (".js", ".mjs", ".cjs", ".ts"), r"(?<![\w$])Effect\.map\s*\(", "Effect.map", skip=("/dist",))
    out = []
    for x in sites:
        if "/dist/cjs/" in x["file"] or x["file"].endswith(".d.ts"): continue
        args = split_args(x["args"])
        form = "data-first" if len(args) >= 2 else "data-last"
        f = args[-1] if args else ""
        if re.match(r"^\(\s*\)\s*=>|^_\w*\s*=>|^\(_\w*\)\s*=>", f): fshape = "() => … (ignores it)"
        elif re.match(r"^\(?\s*\{", f): fshape = "({…}) => (takes it apart)"
        elif re.match(r"^\(?\s*\[", f): fshape = "([…]) => (takes it apart)"
        elif re.match(r"^\(?\s*\w+\s*\)?\s*=>", f): fshape = "x => … (transforms it)"
        elif re.match(r"^(async\s+)?function", f): fshape = "function"
        elif re.match(r"^const\w+$|^constVoid|^constTrue|^constFalse|^constNull|^constUndefined", f): fshape = "a constant"
        elif re.match(r"^[\w$.]+$", f): fshape = "a named function"
        else: fshape = "other"
        pre = x["pre"].rstrip()
        left = ["=> (arrow body)"] if re.search(r"=>\s*$", pre) else ["pipe(…, "] if re.search(r"pipe\([^()]*,\s*$|,\s*$", pre) else [".pipe("] if re.search(r"\.pipe\(\s*$", pre) else ["return"] if re.search(r"\breturn\s*$", pre) else ["x ="] if re.search(r"=\s*$", pre) else ["(as an argument)"] if re.search(r"\(\s*$", pre) else ["(other)"]
        out.append(site_record(x, left=left, right=[fshape], way=form))
    return {"id": "ts-effect-map", "lang": "typescript", "pkg": "effect", "version": json.load(open(f"{base}/package.json"))["version"], "module": "Effect", "name": "map",
            "kind": "function", "decl": "\n".join(l.rstrip() for l in s.lines[L - 1:L] + [s.lines[i] for i in range(L, L + 200) if s.lines[i].lstrip().startswith("<")][:2] + ["};"]),
            "doc": "Transforms the value inside an effect by applying a function to it.", "file": f"dist/Effect.d.ts:{L}", "shape": shape, "family": fam, "sites": out}

# ================================================================ TypeScript · zod parse and its family (throws vs returns a result, now vs later)
def ts_zod_parse():
    base = f"{NODE[0]}/zod"
    s = Src(base, "v4/classic/schemas.d.ts"); p = Src(base, "v4/classic/parse.d.ts")
    L = s.line_of("    parse(data: unknown, params?: core.ParseContext<core.$ZodIssue>): core.output<this>;")
    shape = {
        "recv": {"w": "a schema", "how": "typed", "ev": s.at("parse(data: unknown")},
        "ins": [
            {"n": "data", "t": "unknown", "w": "anything", "how": "typed", "ev": s.at("parse(data: unknown")},
            {"n": "params", "t": "ParseContext", "w": "options", "opt": True, "how": "typed", "ev": s.at("params?: core.ParseContext<core.$ZodIssue>): core.output<this>;")},
        ],
        "out": {"w": "what the schema describes", "thread": "this", "how": "typed", "ev": s.at("): core.output<this>;")},
        "threads": [{"k": "output<this>", "role": "sealed", "says": "the schema decides the type; you never write it", "how": "typed", "ev": s.at("): core.output<this>;")}],
        "seams": {"fail": {"how": "said", "route": "thrown", "w": "ZodError", "ev": p.at("error: ZodError<T>;"), "note": "TypeScript can't say a function throws; only its sibling safeParse puts the error in the type.", "kinds": []}},
        "knobs": [],
    }
    fam = {"axes": [["throws", "returns success or error"], ["now", "later (a Promise)"]], "says": "one reading, two ways to fail, two times", "cells": [
        {"n": "parse", "at": [0, 0], "ev": s.at("    parse(data: unknown")},
        {"n": "safeParse", "at": [1, 0], "ev": s.at("    safeParse(data: unknown")},
        {"n": "parseAsync", "at": [0, 1], "ev": s.at("    parseAsync(data: unknown")},
        {"n": "safeParseAsync", "at": [1, 1], "ev": s.at("    safeParseAsync(data: unknown")},
    ], "result": {"ok": p.at("success: true;"), "err": p.at("success: false;")}}
    return {"id": "ts-zod-parse", "lang": "typescript", "pkg": "zod", "version": json.load(open(f"{base}/package.json"))["version"], "module": "ZodType",
            "name": "parse", "kind": "method", "decl": s.lines[L - 1].strip(), "doc": "Reads unknown data against the schema and gives it back typed.", "file": f"v4/classic/schemas.d.ts:{L}",
            "shape": shape, "family": fam, "sites": []}

# ================================================================ JavaScript (no types at all) · which / which.sync
def js_which():
    base = f"{NODE[0]}/which"
    s = Src(base, "which.js"); rd = Src(base, "README.md")
    L = s.line_of("const whichSync = (cmd, opt) => {")
    shape = {
        "ins": [
            {"n": "cmd", "w": "a program name", "how": "said", "ev": rd.at("which.sync")},
            {"n": "opt", "w": "options", "opt": True, "how": "found", "ev": s.at("opt = opt || {}", L)},
        ],
        "out": {"w": "a path", "how": "found", "ev": s.at("return cur", L)},
        "threads": [],
        "seams": {
            "fail": {"how": "found", "route": "thrown", "w": "Error with code ENOENT", "ev": s.at("throw getNotFoundError(cmd)", L),
                     "kinds": [{"n": "ENOENT", "w": "not found: <cmd>", "ev": s.at("Object.assign(new Error(`not found: ${cmd}`), { code: 'ENOENT' })")}]},
        },
        "knobs": [
            {"n": "nothrow", "on": "true", "changes": "fail→none", "w": "gives null instead of throwing", "ev": s.at("if (opt.nothrow)", L)},
            {"n": "all", "on": "true", "changes": "one→many", "w": "gives every match, a list", "ev": s.at("if (opt.all && found.length)", L)},
        ],
        "untyped": "which ships no types: no .d.ts, no JSDoc. Every fact here is read from its code (which.js) or its README.",
    }
    fam = {"axes": [["throws", "gives null (nothrow)"], ["now (which.sync)", "later (which: a Promise or a callback)"]], "says": "one lookup; an option and a sibling change how it fails and when", "cells": [
        {"n": "which.sync", "at": [0, 0], "ev": s.at("const whichSync = (cmd, opt) => {")},
        {"n": "which.sync …nothrow", "at": [1, 0], "ev": s.at("if (opt.nothrow)")},
        {"n": "which", "at": [0, 1], "ev": s.at(": reject(getNotFoundError(cmd))")},
    ]}
    sites = scan(node_roots(), (".js", ".mjs", ".cjs"), r"(?<![\w$.])which(\.sync)?\s*\(", "which", keep_file=lambda t: re.search(r"require\(['\"]which['\"]\)|from ['\"]which['\"]", t) is not None)
    out = []
    for x in sites:
        nothrow = "nothrow" in x["args"]
        pre = x["pre"]
        tried = any(re.search(r"\btry\s*\{", b) for b in x["before"][-8:])
        way = "nothrow → null" if nothrow else "try / catch" if tried else ".catch(…)" if ".catch(" in x["post"] else "other"
        out.append(site_record(x, left=["(statement)"], right=[x["post"].strip()[:20] or "↵"], way=way))
    return {"id": "js-which", "lang": "javascript", "pkg": "which", "version": json.load(open(f"{base}/package.json"))["version"], "module": "which", "name": "which.sync",
            "kind": "function", "decl": "const whichSync = (cmd, opt) => {", "doc": "Find the first instance of an executable in the PATH.", "file": f"which.js:{L}", "shape": shape, "family": fam, "sites": out}

# ================================================================ Rust · serde::Serialize (a trait with thousands of implementors)
def rust_serialize_crowd():
    by = collections.defaultdict(lambda: {"derive": 0, "hand": 0, "yours": False, "names": []})
    rh = re.compile(r"impl\b[^{;]{0,200}?\bSerialize\s+for\s+([\w:]+)")
    decl = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(struct|enum|union)\s+(\w+)")
    for label, yours, root in rust_roots():
        for f in files(root, (".rs",)):
            try: t = open(f, errors="ignore").read()
            except Exception: continue
            if "Serialize" not in t: continue
            k = label or crate_of(f)
            L = t.split("\n")
            rel = os.path.relpath(f, root if not yours else REPO)
            for i, line in enumerate(L):
                if "derive(" in line and re.search(r"derive\([^)]*(?<![\w:])(serde::)?Serialize\b", line):
                    for j in range(i + 1, min(i + 40, len(L))):
                        m = decl.match(L[j])
                        if m:
                            by[k]["derive"] += 1; by[k]["yours"] = yours
                            if len(by[k]["names"]) < 40: by[k]["names"].append([m.group(2), "derive", rel, j + 1])
                            break
                        if not re.match(r"^\s*(#|//|$)", L[j]): break
                if "Serialize" in line and line.lstrip().startswith("impl"):
                    m = rh.search(" ".join(L[i:i + 3]))
                    if m:
                        by[k]["hand"] += 1; by[k]["yours"] = yours
                        if len(by[k]["names"]) < 40: by[k]["names"].append([m.group(1).split("::")[-1].split("<")[0], "hand", rel, i + 1])
    rows = sorted(([k, v["derive"], v["hand"], v["yours"], v["names"]] for k, v in by.items() if v["derive"] + v["hand"]), key=lambda r: -(r[1] + r[2]))
    return {"id": "rs-serde-serialize", "lang": "rust", "pkg": "serde", "name": "Serialize", "kind": "trait", "version": "1.0.228",
            "doc": "A data structure that can be serialized into any data format supported by Serde.", "crowd": rows}

def main():
    os.makedirs(OUT, exist_ok=True)
    only = set(sys.argv[1:])
    jobs = [rust_from_str, py_re_match, py_subprocess_run, go_yaml_unmarshal, go_binary_search, go_assert_equal, ts_effect_map, ts_zod_parse, js_which, rust_serialize_crowd]
    index = []
    for j in jobs:
        if only and j.__name__ not in only: continue
        d = j()
        json.dump(d, open(os.path.join(OUT, d["id"] + ".json"), "w"), separators=(",", ":"))
        n = len(d.get("sites", [])) or len(d.get("crowd", []))
        ways = collections.Counter(s["way"] for s in d.get("sites", []))
        print(f"{d['id']:28} {n:6}  {dict(ways.most_common(8))}")
    ids = sorted(f[:-5] for f in os.listdir(OUT) if f.endswith(".json") and f != "index.json")
    json.dump(ids, open(os.path.join(OUT, "index.json"), "w"))

if __name__ == "__main__":
    main()
