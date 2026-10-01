#!/usr/bin/env python3
"""extract_page6.py: the symbol page's model, one JSON per symbol (data/page6/<id>.json).

The model is language-neutral and is the contract the native page is built from:

  head       kind, path, name, summary, source {file, line}, lang, version
  docs       the full doc comment as blocks: {k: "p"|"h"|"code"|"list", t}
  sig        (callables) recv {how: reads|changes|consumes, type}, ins [{name, type, opt, dflt, rest, doc}],
             out {type, fails {type, when, kinds[]} | none {when} | later | many}, generics [{name, role, bounds[], fills[]}]
  shape      (types) {kind: "one of"|"holds"|"opaque"|"alias", cases|fields [{name, type, doc, loops}], hidden}
  groups     (types) methods by what they do with it: makes | reads | changes | uses up, each {name, sig, out, doc, yours}
  implements (types) [{name, how: derive|impl, of}]
  required / provided / implementors   (traits)
  uses       places in YOUR workspace: {pkg, dir, file, line, text, tag, fill?}; tags are the page's verbs
  siblings   the module's other names, with the word that differs
  history    the item's signature across the releases on disk
  facts      how each fact is known when the language doesn't declare it: typed | docs | code

A type is {text, word, gen?, link?}: text as written, word in plain words, gen when it is a type parameter.
Sources: the registry sources of this repository's Cargo.lock, data/sym (extract_sym.py), data/sym_uses.json
(your crates' lines), data/shapes (extract_shapes.py: call sites, the Serialize crowd), the Python stdlib and
node_modules for the two non-Rust pages. Nothing is fetched.
"""
import json, os, re, glob, ast

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
REG = glob.glob(os.path.expanduser("~/.cargo/registry/src/index.crates.io-*/"))[0]
OUT = os.path.join(HERE, "data", "page6")
WORLD = json.load(open(os.path.join(HERE, "data", "world.json")))
MEMBERS = {k: v for k, v in WORLD["members_info"].items()}
USES = json.load(open(os.path.join(HERE, "data", "sym_uses.json")))

def sym(pid): return json.load(open(os.path.join(HERE, "data", "sym", pid + ".json")))["items"]
def L(v): return ast.literal_eval(v) if isinstance(v, str) else v

# ---------------------------------------------------------------- docs: a doc comment as blocks
def doc_lines(lines, at, prefix="///"):
    """The doc comment above line `at` (1-based), skipping attributes."""
    i, out = at - 2, []
    while i >= 0:
        s = lines[i].strip()
        if s.startswith("#[") or s.startswith("#!["): i -= 1; continue
        if not s.startswith("//") and (s.endswith(")]") or s == ")]"):   # the tail of a multi-line attribute: climb to its #[
            while i >= 0 and not lines[i].strip().startswith("#["): i -= 1
            i -= 1; continue
        if s.startswith(prefix): out.insert(0, s[len(prefix):][1:] if s[len(prefix):].startswith(" ") else s[len(prefix):]); i -= 1; continue
        break
    return out

def blocks(md_lines, lang="rust"):
    out, para, code, fence = [], [], None, None
    def flush():
        nonlocal para
        if para: out.append({"k": "p", "t": " ".join(x.strip() for x in para)}); para = []
    for raw in md_lines:
        s = raw.rstrip()
        if code is not None:
            if s.strip().startswith("```"): out.append({"k": "code", "t": "\n".join(code).strip("\n"), "lang": fence or lang}); code = None; continue
            if (fence or lang) == "rust" and (s.strip() == "#" or s.strip().startswith("# ")): continue   # rustdoc's hidden lines
            code.append(s); continue
        if s.strip().startswith("```"):
            flush(); f = s.strip()[3:].strip(); fence = None if not f or f in ("rust", "no_run", "ignore", "should_panic", "compile_fail", "edition2021") else f.split(",")[0]; code = []; continue
        if re.match(r"^#{1,4}\s", s): flush(); out.append({"k": "h", "t": s.lstrip("#").strip()}); continue
        if not s.strip(): flush(); continue
        if re.match(r"^\s*\[[^\]]+\]:\s*\S+", s): continue   # a reference-link definition
        if re.match(r"^\s*[-*]\s", s): flush(); out.append({"k": "li", "t": re.sub(r"^\s*[-*]\s", "", s)}); continue
        para.append(s)
    flush()
    # rustdoc link syntax: [`Name`](path) and [`Name`] -> `Name`
    for b in out:
        if b["k"] in ("p", "li", "h"): b["t"] = re.sub(r"\[(`[^`]+`)\]\([^)]*\)", r"\1", re.sub(r"\[(`[^`]+`)\](?!\()", r"\1", b["t"]))
    return out

def summary(bs):
    for b in bs:
        if b["k"] == "p":
            m = re.match(r"(.+?[.!?])(\s|$)", b["t"]); return (m.group(1) if m else b["t"])
    return ""

# ---------------------------------------------------------------- types in plain words
PRIM = {"str": "text", "String": "text", "bool": "yes or no", "char": "a character", "f32": "a number", "f64": "a number",
        "u8": "a byte", "usize": "a count", "u64": "a count", "u32": "a count", "i64": "an integer", "i32": "an integer", "isize": "an integer",
        "Duration": "a duration", "Path": "a path", "PathBuf": "a path", "()": "nothing", "Number": "a number"}
def split_top(s, sep=","):
    out, d, cur = [], 0, ""
    for c in s:
        if c in "<([": d += 1
        if c in ">)]": d -= 1
        if c == sep and d == 0: out.append(cur.strip()); cur = ""
        else: cur += c
    if cur.strip(): out.append(cur.strip())
    return out

def tref(t, gens=(), local=()):
    """A type as written -> {text, word, gen?, link?, shape?}."""
    t0 = t.strip()
    s = re.sub(r"'\w+\s*", "", t0).replace("& ", "&").strip()
    s = re.sub(r"^(crate|self|super)::", "", s)
    borrow = ""
    if s.startswith("&mut "): borrow, s = "changes", s[5:].strip()
    elif s.startswith("&"): borrow, s = "reads", s[1:].strip()
    base = s.split("<")[0].split("::")[-1]
    inner = s[s.index("<") + 1:s.rindex(">")] if "<" in s and s.endswith(">") else ""
    args = split_top(inner) if inner else []
    r = {"text": t0}
    if borrow: r["borrow"] = borrow
    if base in gens: r.update(word=base, gen=base); return r
    if base == "Self": r.update(word="this type", link="Self"); return r
    if base == "[u8]" or s == "[u8]": r["word"] = "bytes"; return r
    if s.startswith("[") and s.endswith("]"): r.update(word="a list of " + tref(s[1:-1], gens, local)["word"], of=tref(s[1:-1], gens, local)); return r
    if base in ("Vec", "VecDeque", "HashSet", "BTreeSet", "IndexSet") and args: a = tref(args[0], gens, local); r.update(word="a list of " + a["word"], of=a); return r
    if base in ("HashMap", "BTreeMap", "Map", "IndexMap") and len(args) >= 2: k, v = tref(args[0], gens, local), tref(args[1], gens, local); r.update(word=f"a map of {k['word']} to {v['word']}", key=k, of=v); return r
    if base in ("Box", "Rc", "Arc", "Cow") and args: a = tref(args[-1], gens, local); r.update(word=a["word"], of=a); return r
    if base == "Option" and args: a = tref(args[0], gens, local); r.update(word=a["word"], of=a, shape="maybe"); return r
    if base == "Result": a = tref(args[0], gens, local) if args else {"word": "?"}; r.update(word=a["word"], of=a, shape="result", err=(tref(args[1], gens, local) if len(args) > 1 else None)); return r
    if s.startswith("impl "): r.update(word="any " + s[5:].split("<")[0].split("::")[-1]); return r
    if s.startswith("dyn "): r.update(word="any " + s[4:].split("<")[0].split("::")[-1]); return r
    if base in PRIM: r["word"] = PRIM[base]; return r
    r.update(word=base, link=base); return r

def parse_fn(S):
    """pub fn name<gens>(params) -> ret where ... -> dict"""
    S = re.sub(r"\s+", " ", S)
    m = re.match(r".*?\bfn\s+(\w+)\s*(<.*?>)?\s*\((.*)\)\s*(->\s*(.*?))?\s*(where\s+(.*))?[,{]?\s*$", S)
    if not m: return None
    name, g, params, _, ret, _, where = m.groups()
    gens, bounds = [], {}
    if g:
        for x in split_top(g[1:-1]):
            if x.startswith("'"): continue
            n = x.split(":")[0].strip(); gens.append(n)
            if ":" in x: bounds.setdefault(n, []).extend(b.strip() for b in split_top(x.split(":", 1)[1], "+"))
    if where:
        for w in split_top(where.rstrip(",")):
            if ":" in w: n = w.split(":")[0].strip(); bounds.setdefault(n, []).extend(b.strip() for b in split_top(w.split(":", 1)[1], "+") if not b.strip().startswith("'"))
    ps, recv = [], None
    for p in split_top(params):
        if re.match(r"^&?\s*('\w+\s+)?(mut\s+)?self$", p): recv = "changes" if "mut" in p else "reads" if p.startswith("&") else "uses up"; continue
        if ":" not in p: continue
        n, t = p.split(":", 1); ps.append((n.strip().replace("mut ", ""), t.strip()))
    return {"name": name, "gens": gens, "bounds": bounds, "params": ps, "ret": (ret or "").strip(), "recv": recv, "async": " async " in f" {S} "}

BOUND_WORD = {"Deserialize": "can be read by serde (any format)", "Serialize": "can be written by serde (any format)", "Index": "an index: a position or a key",
              "Ord": "can be ordered", "Hash": "can be hashed", "Clone": "can be copied", "Debug": "prints for debugging", "Display": "prints", "Send": "can move between threads",
              "Sync": "can be shared between threads", "IntoIterator": "can be looped over", "AsRef": "can be read as", "Into": "turns into", "Read": "a reader (io::Read)", "Write": "a writer (io::Write)",
              "Future": "a future", "IntoFuture": "anything you can await", "Fn": "a function", "FnMut": "a function", "FnOnce": "a function"}

def sig_model(S, crate_err=None, self_name=None):
    f = parse_fn(S)
    if not f: return None
    gens = f["gens"]
    ins = [{"name": n, "type": tref(t, gens)} for n, t in f["params"]]
    rt = tref(f["ret"], gens) if f["ret"] else {"text": "()", "word": "nothing"}
    out = {"type": rt.get("of", rt) if rt.get("shape") in ("result", "maybe") else rt}
    if rt.get("shape") == "result": out["fails"] = {"type": rt.get("err") or crate_err or {"text": "Error", "word": "Error", "link": "Error"}}
    if rt.get("shape") == "maybe": out["none"] = {}
    if f["async"]: out["later"] = True
    if self_name:
        for x in [out["type"]] + [i["type"] for i in ins]:
            if x.get("link") == "Self": x["word"] = self_name
    generics = []
    for g in gens:
        bs = f["bounds"].get(g, [])
        where_in = [i["name"] for i in ins if re.search(rf"\b{g}\b", i["type"]["text"])]
        in_out = bool(re.search(rf"\b{g}\b", f["ret"]))
        role = "choose" if in_out and not where_in else "through" if in_out and where_in else "needs"
        generics.append({"name": g, "role": role, "bounds": [{"text": b, "word": BOUND_WORD.get(b.split("<")[0].split("::")[-1], b.split("<")[0].split("::")[-1]), "name": b.split("<")[0].split("::")[-1]} for b in bs if not b.startswith("?")], "in": where_in, "out": in_out})
    m = {"ins": ins, "out": out, "generics": generics}
    if f["recv"]: m["recv"] = {"how": f["recv"]}
    return m

# ---------------------------------------------------------------- your workspace: lines from sym_uses, with the page's verbs as tags
def member_dir(pid):
    n = pid.split("@")[0]
    return (MEMBERS.get(n) or {}).get("dir", "")

def uses_of(pkg_id, name, tag_of):
    out = []
    for k, v in USES.items():
        a, b = k.split(">")
        if b != pkg_id or name not in v: continue
        rec = v[name]; d = member_dir(a)
        seen = set()
        for m, ls in (rec.get("ml") or {}).items():
            for l in ls: out.append({"pkg": a.split("@")[0].replace("backend-", ""), "dir": d, "file": l[0], "line": l[1], "text": l[2], "tag": tag_of(m, l[2]), "member": m}); seen.add((l[0], l[1]))
        for l in rec.get("l") or []:
            if (l[0], l[1]) in seen: continue
            out.append({"pkg": a.split("@")[0].replace("backend-", ""), "dir": d, "file": l[0], "line": l[1], "text": l[2], "tag": tag_of(None, l[2])})
    return out


def scan_members(uses, names, tag_of, skip=()):
    """Method calls and field reads by name, in the workspace files that import the type. Approximate:
    matched by name, not resolved; each one says so (approx)."""
    files = {(u["dir"], u["file"], u["pkg"]) for u in uses if u["tag"] == "imports" and u["dir"]}
    rx = re.compile(r"\.(%s)\b(\s*\()?" % "|".join(re.escape(n) for n in names if n not in skip))
    out, seen = [], {(u["dir"], u["file"], u["line"]) for u in uses}
    for d, f, pkg in files:
        path = os.path.join(REPO, d, f)
        if not os.path.exists(path): continue
        for i, line in enumerate(open(path, errors="ignore")):
            m = rx.search(line)
            if not m or (d, f, i + 1) in seen or line.strip().startswith("//"): continue
            out.append({"pkg": pkg, "dir": d, "file": f, "line": i + 1, "text": line.strip()[:200], "tag": tag_of(m.group(1)), "member": m.group(1), "approx": True})
    return out

# ================================================================ serde_json::from_str
def from_str():
    base = os.path.join(REG, "serde_json-1.0.151"); src = open(os.path.join(base, "src/de.rs")).read().split("\n")
    I = sym("serde_json@1.0.151")["de::from_str"]; line = int(I["line"])
    bs = blocks(doc_lines(src, line))
    err = sym("serde_json@1.0.151")["error::Category"]
    esrc = open(os.path.join(base, "src/error.rs")).read().split("\n")
    kinds = []
    for n, _ in L(err["var"]):
        at = next(i for i, l in enumerate(esrc) if re.match(rf"^\s+{n},\s*$", l)) + 1
        kinds.append({"name": n, "doc": summary(blocks(doc_lines(esrc, at)))})
    sg = sig_model(I["S"])
    errors = next((b for i, b in enumerate(bs) if b["k"] == "p" and i > 0 and bs[i - 1]["k"] == "h" and bs[i - 1]["t"] == "Errors"), None)
    sg["out"]["fails"] = {"type": {"text": "serde_json::Error", "word": "Error", "link": "Error"}, "when": errors["t"] if errors else "", "short": "if it isn't valid JSON, or doesn't fit T", "kinds": kinds, "kinds_from": "Error::classify() → Category"}
    sg["ins"][0]["doc"] = "the JSON"
    # what callers in your workspace choose for T, with where
    sh = json.load(open(os.path.join(HERE, "data", "shapes", "rs-serde_json-from_str.json")))
    uses, fills = [], {}
    for s in sh["sites"]:
        if not s["y"]: continue
        pkg = s["p"].replace("backend-", ""); d = member_dir(s["p"] + "@")
        d = next((v["dir"] for k, v in MEMBERS.items() if k == s["p"]), "")
        rel = os.path.relpath(os.path.join(REPO, s["f"]), os.path.join(REPO, d)) if d and s["f"].startswith(d) else s["f"]
        u = {"pkg": pkg, "dir": d, "file": rel, "line": s["l"], "text": (s["pre"] + " " + s["call"] + s["post"]).strip(), "tag": "calls", "ctx": s["x"]}
        if s.get("fill") and s["fill"] != "inferred": u["fill"] = s["fill"].replace("serde_json::", ""); fills.setdefault(u["fill"], []).append(pkg)
        uses.append(u)
    g = sg["generics"][0]
    g["fills"] = [{"type": t, "pkgs": sorted(set(p)), "n": len(p)} for t, p in sorted(fills.items(), key=lambda x: -len(x[1]))]
    g["says"] = "You choose it: whatever you read the JSON into."
    all_fills = {}
    for s in sh["sites"]:
        if s.get("fill") and s["fill"] not in ("inferred", "(a macro's type)"): all_fills.setdefault(s["fill"].replace("serde_json::", ""), set()).add(s["p"])
    g["elsewhere"] = [{"type": t, "crates": len(c)} for t, c in sorted(all_fills.items(), key=lambda x: -len(x[1]))[:8]]
    sib = [{"name": "from_slice", "differs": "from bytes", "kind": "fn", "fails": True}, {"name": "from_reader", "differs": "from a reader", "kind": "fn", "fails": True}, {"name": "from_value", "differs": "from a Value", "kind": "fn", "fails": True}, {"name": "to_string", "differs": "the other way", "kind": "fn", "fails": True}]
    hist = history("serde_json", "src/de.rs", r"pub fn from_str<")
    return {"id": "rs-from_str", "lang": "rust", "pkg": "serde_json", "version": "1.0.151", "path": ["serde_json"], "name": "from_str", "kind": "function",
            "summary": summary(bs), "docs": bs, "source": {"file": "src/de.rs", "line": line, "root": f"~/.cargo/registry/src/…/serde_json-1.0.151"},
            "decl": re.sub(r"\s+", " ", I["S"]), "sig": sg, "uses": uses, "siblings": sib, "history": hist}

def history(crate, rel, pat):
    out = []
    for d in sorted(glob.glob(os.path.join(REG, f"{crate}-[0-9]*")), key=lambda p: [int(x) if x.isdigit() else 0 for x in re.split(r"[.-]", p.split(crate + "-")[-1])]):
        f = os.path.join(d, rel)
        if not os.path.exists(f): continue
        t = open(f).read(); i = t.find(pat)
        sig = re.sub(r"\s+", " ", t[i:t.find("{", i)]).strip() if i >= 0 else None
        out.append({"v": d.split(crate + "-")[-1], "sig": sig})
    same = len({h["sig"] for h in out}) == 1
    return {"releases": out, "same": same}

# ================================================================ serde_json::Value
def value():
    base = os.path.join(REG, "serde_json-1.0.151"); src = open(os.path.join(base, "src/value/mod.rs")).read().split("\n")
    S = sym("serde_json@1.0.151"); I = S["value::Value"]; line = int(I["line"])
    bs = blocks(doc_lines(src, line))
    cases = []
    for n, pay in L(I["var"]):
        at = next(i for i, l in enumerate(src[line:], line) if re.match(rf"^\s+{n}\b", l)) + 1
        d = blocks(doc_lines(src, at))
        t = tref(pay) if pay else None
        cases.append({"name": n, "type": t, "doc": summary(d), "more": next((b["t"] for b in d[1:] if b["k"] == "p"), ""), "loops": bool(pay and re.search(r"\bValue\b", pay))})
    ms = L(I["m"])
    verb = {"&": "reads", "&mut": "changes", "self": "uses up", "": "makes"}
    groups = {"makes": [], "reads": [], "changes": [], "uses up": []}
    U = uses_of("serde_json@1.0.151", "Value", lambda m, t: tag_value(m, t, ms))
    rk = {x["n"]: x["r"] for x in ms}
    U += scan_members(U, [x["n"] for x in ms], lambda n: {"&": "reads", "&mut": "changes", "self": "uses up"}.get(rk.get(n, "&"), "reads"), skip=("get", "get_mut", "take", "len", "is_empty", "iter"))
    used = {}
    for u in U:
        if u.get("member"): used[u["member"]] = used.get(u["member"], 0) + 1
    for m in ms:
        sg = sig_model(m["s"], {"text": "Error", "word": "Error", "link": "Error"}, "Value")
        groups[verb.get(m["r"], "makes")].append({"name": m["n"], "doc": m["d"], "sig": sg, "yours": used.get(m["n"], 0)})
    froms = []
    for tr, arg in L(I["impl"]):
        if tr == "From" and "$" not in arg:
            t = tref(arg); w = t["word"] if t["word"] not in ("a number", "a count", "an integer") else "a number"
            if w not in froms: froms.append(w)
    if froms: groups["makes"].insert(0, {"name": "from", "doc": "Value::from(x) or x.into(), from any of these.", "froms": froms, "trait": True})
    for tr, arg in L(I["impl"]):
        if tr == "FromStr": groups["makes"].append({"name": "parse", "doc": "Read one from JSON text (str::parse).", "from": {"text": "&str", "word": "text"}, "fails": True, "trait": True})
    for g in groups.values(): g.sort(key=lambda x: -x.get("yours", 0))
    impls, seen = [], set()
    for n in L(I["der"]): impls.append({"name": n, "how": "derive"}); seen.add(n)
    for tr, arg in L(I["impl"]):
        if tr in ("From",) or "$" in arg or tr in seen: continue
        seen.add(tr); impls.append({"name": tr, "how": "impl"})
    sib = [{"name": "Map", "differs": "what an Object holds", "kind": "struct"}, {"name": "Number", "differs": "what a Number holds", "kind": "struct"}, {"name": "json!", "differs": "write one as JSON", "kind": "macro"}, {"name": "to_value", "differs": "turn a T into one", "kind": "fn", "fails": True}, {"name": "from_value", "differs": "turn one into a T", "kind": "fn", "fails": True}]
    return {"id": "rs-Value", "lang": "rust", "pkg": "serde_json", "version": "1.0.151", "path": ["serde_json", "value"], "name": "Value", "kind": "enum",
            "summary": summary(bs), "docs": bs, "source": {"file": "src/value/mod.rs", "line": line}, "decl": "pub enum Value",
            "shape": {"kind": "one of", "cases": cases}, "groups": [{"verb": k, "items": v} for k, v in groups.items() if v], "implements": impls,
            "uses": U, "siblings": sib, "history": history("serde_json", "src/value/mod.rs", "pub enum Value")}

def tag_type(t, name):
    """What a line that names a type does with it, from the line itself."""
    n = rf"(serde_json::)?{name}\b"
    if re.match(r"^\s*(pub(\(\w+\))?\s+)?use\b", t): return "imports"
    if re.search(rf"->\s*[^{{;]*{n}", t): return "makes"
    if re.search(rf"let\s+(mut\s+)?\w+\s*:\s*[^=]*{n}[^=]*=", t): return "makes"
    if re.search(rf"\b\w+\s*:\s*&mut\s+{n}", t): return "changes"
    if re.search(rf"\b\w+\s*:\s*&\s*{n}", t): return "reads"
    if re.search(rf"^\s*(pub(\(\w+\))?\s+)?\w+\s*:\s*[^=]*{n}[^=]*,?\s*$", t) and not t.strip().startswith("let"): return "holds"
    if re.search(rf"<[^>]*{n}[^>]*>", t): return "holds"
    return "names"

def tag_value(m, t, ms):
    r = {x["n"]: x["r"] for x in ms}
    if m: return {"&": "reads", "&mut": "changes", "self": "uses up"}.get(r.get(m, "&"), "makes")
    if re.search(r"(match\b|=>|if let|matches!\()[^;]*Value::(Null|Bool|Number|String|Array|Object)\b|Value::(Null|Bool|Number|String|Array|Object)\b[^;]*=>", t): return "matches"
    if re.search(r"Value::(Null|Bool|Number|String|Array|Object)\b\s*\(|Value::(Null)\b|Value::from\(|json!\(|to_value\(", t): return "makes"
    if re.search(r"from_(str|slice|reader)\s*(::<[^>]*Value[^>]*>)?\(|:\s*(serde_json::)?Value\s*=\s*serde_json::from", t): return "makes"
    return tag_type(t, "Value")

# ================================================================ serde_json::Value::as_str (a method: maybe nothing)
def as_str():
    base = os.path.join(REG, "serde_json-1.0.151"); src = open(os.path.join(base, "src/value/mod.rs")).read().split("\n")
    S = sym("serde_json@1.0.151"); ms = L(S["value::Value"]["m"]); m = next(x for x in ms if x["n"] == "as_str")
    at = next(i for i, l in enumerate(src) if "pub fn as_str(&self)" in l) + 1
    bs = blocks(doc_lines(src, at))
    sg = sig_model(m["s"], None, "Value")
    sg["recv"] = {"how": "reads", "type": {"text": "&Value", "word": "Value", "link": "Value"}}
    sg["out"]["none"] = {"when": "if it isn't a String"}
    U = [u for u in uses_of("serde_json@1.0.151", "Value", lambda mm, t: "calls") if u.get("member") == "as_str"]
    sib = [{"name": n, "differs": d, "kind": "method", "none": True} for n, d in [("as_i64", "a number"), ("as_bool", "yes or no"), ("as_array", "a list"), ("as_object", "a map"), ("is_string", "just asks")]]
    return {"id": "rs-as_str", "lang": "rust", "pkg": "serde_json", "version": "1.0.151", "path": ["serde_json", "Value"], "name": "as_str", "kind": "method",
            "summary": summary(bs), "docs": bs, "source": {"file": "src/value/mod.rs", "line": at}, "decl": m["s"], "sig": sg, "uses": U, "siblings": sib,
            "history": history("serde_json", "src/value/mod.rs", "pub fn as_str(&self)")}

# ================================================================ allocation_counter::AllocationInfo (a struct with its fields out)
def alloc_info():
    base = os.path.join(REG, "allocation-counter-0.8.1"); src = open(os.path.join(base, "src/lib.rs")).read().split("\n")
    line = next(i for i, l in enumerate(src) if l.startswith("pub struct AllocationInfo")) + 1
    bs = blocks(doc_lines(src, line))
    fields, i = [], line
    while not src[i].startswith("}"):
        mm = re.match(r"^\s+pub\s+(\w+):\s*([^,]+),", src[i])
        if mm: d = blocks(doc_lines(src, i + 1)); fields.append({"name": mm.group(1), "type": tref(mm.group(2)), "doc": summary(d), "more": " ".join(b["t"] for b in d[1:] if b["k"] == "p")[:400]})
        i += 1
    U = uses_of("allocation-counter@0.8.1", "AllocationInfo", lambda m, t: tag_type(t, "AllocationInfo"))
    U += scan_members(U, [f["name"] for f in fields], lambda n: "reads")
    for f in fields: f["yours"] = sum(1 for u in U if u.get("member") == f["name"])
    der = re.findall(r"\w+", next(l for l in src[line - 3:line] if "derive" in l).split("derive(")[1])
    return {"id": "rs-AllocationInfo", "lang": "rust", "pkg": "allocation-counter", "version": "0.8.1", "path": ["allocation_counter"], "name": "AllocationInfo", "kind": "struct",
            "summary": summary(bs), "docs": bs, "source": {"file": "src/lib.rs", "line": line}, "decl": "pub struct AllocationInfo",
            "shape": {"kind": "holds", "fields": fields, "hidden": 0}, "groups": [{"verb": "makes", "items": [{"name": "measure", "doc": "Run a closure and count its allocations.", "sig": {"ins": [{"name": "run", "type": {"text": "F", "word": "a function"}}], "out": {"type": {"text": "AllocationInfo", "word": "AllocationInfo"}}}, "yours": 0}, {"name": "default", "doc": "All zeros.", "trait": True}]}],
            "implements": [{"name": d, "how": "derive"} for d in der], "uses": U, "siblings": [{"name": "measure", "differs": "the function that fills one", "kind": "fn"}],
            "history": {"releases": [{"v": "0.8.1"}], "same": True}}

# ================================================================ serde::Serialize (a trait: who writes it in your workspace)
def serialize():
    base = glob.glob(os.path.join(REG, "serde_core-1.0.2*"))
    base = base[-1] if base else os.path.join(REG, "serde-1.0.229")
    f = next(p for p in glob.glob(os.path.join(base, "src", "**", "*.rs"), recursive=True) if "pub trait Serialize" in open(p).read())
    src = open(f).read().split("\n"); line = next(i for i, l in enumerate(src) if l.startswith("pub trait Serialize")) + 1
    bs = blocks(doc_lines(src, line))
    crowd = json.load(open(os.path.join(HERE, "data", "shapes", "rs-serde-serialize.json")))["crowd"]
    U = []
    for n, dv, hd, yours, names in crowd:
        if not yours: continue
        d = MEMBERS.get(n, {}).get("dir", "")
        for nm, how, rel, ln in names:
            U.append({"pkg": n.replace("backend-", ""), "dir": d, "file": os.path.relpath(os.path.join(REPO, rel), os.path.join(REPO, d)) if d and rel.startswith(d) else rel, "line": ln, "text": f"{'#[derive(… Serialize …)]' if how == 'derive' else 'impl Serialize for'} {nm}", "tag": "derives" if how == "derive" else "implements", "type": nm})
    for k, v in USES.items():
        a, b = k.split(">")
        if not b.startswith("serde@") or "Serialize" not in v: continue
        for l in v["Serialize"].get("l") or []:
            if re.search(r":\s*[^;]*\bSerialize\b|impl\s+Serialize\b|where\b|\+\s*Serialize", l[2]) and "derive" not in l[2]:
                U.append({"pkg": a.split("@")[0].replace("backend-", ""), "dir": member_dir(a), "file": l[0], "line": l[1], "text": l[2], "tag": "asks for"})
    total = sum(r[1] + r[2] for r in crowd)
    return {"id": "rs-Serialize", "lang": "rust", "pkg": "serde", "version": "1.0.229", "path": ["serde"], "name": "Serialize", "kind": "trait",
            "summary": summary(bs), "docs": bs[:6], "source": {"file": os.path.relpath(f, base), "line": line}, "decl": "pub trait Serialize",
            "required": [{"name": "serialize", "sig": sig_model("fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: Serializer,"), "doc": "Serialize this value into the given Serde serializer."}],
            "provided": [], "implementors": {"total": total, "crates": len(crowd), "derived": round(100 * sum(r[1] for r in crowd) / total)},
            "uses": U, "siblings": [{"name": "Deserialize", "differs": "the other way", "kind": "trait"}, {"name": "Serializer", "differs": "a format writes this", "kind": "trait"}],
            "history": {"releases": [], "same": True}}

# ================================================================ Python · re.match and JavaScript · which.sync (no declared types)
def re_match():
    sh = json.load(open(os.path.join(HERE, "data", "shapes", "py-re-match.json")))
    S = "/opt/homebrew/opt/python@3.14/Frameworks/Python.framework/Versions/3.14/lib/python3.14/re/__init__.py"
    src = open(S).read().split("\n"); line = next(i for i, l in enumerate(src) if l.startswith("def match(")) + 1
    sg = {"ins": [{"name": "pattern", "type": {"text": "", "word": "a pattern", "known": "docs"}, "doc": "a regular expression, text or compiled"},
                  {"name": "string", "type": {"text": "", "word": "text or bytes", "known": "docs", "gen": "AnyStr"}, "doc": "what to look at"},
                  {"name": "flags", "type": {"text": "", "word": "flags", "known": "code"}, "opt": True, "dflt": "0"}],
          "out": {"type": {"text": "", "word": "a Match", "link": "Match", "known": "docs"}, "none": {"when": "if the pattern doesn't match at the start", "known": "docs"},
                  "fails": {"type": {"text": "re.error", "word": "re.error", "known": "code"}, "when": "if the pattern itself is invalid", "kinds": []}},
          "generics": [{"name": "AnyStr", "role": "through", "bounds": [{"name": "str | bytes", "word": "text or bytes"}], "says": "Text in, text out; bytes in, bytes out.", "known": "docs"}]}
    uses = [{"pkg": s["p"].replace("stdlib/", ""), "dir": "", "file": s["f"], "line": s["l"], "text": (s["pre"] + " " + s["call"] + s["post"]).strip(), "tag": "calls", "ctx": s["x"]} for s in sh["sites"] if s["x"] != "test"][:60]
    return {"id": "py-re.match", "lang": "python", "pkg": "re", "version": "3.14 stdlib", "path": ["re"], "name": "match", "kind": "function",
            "summary": "Try to apply the pattern at the start of the string, returning a Match object, or None if no match was found.",
            "docs": [{"k": "p", "t": "Try to apply the pattern at the start of the string, returning a Match object, or None if no match was found."}],
            "source": {"file": "re/__init__.py", "line": line}, "decl": src[line - 1].strip(), "sig": sg, "uses": uses, "workspace": "the Python 3.14 stdlib and site-packages (no Python in your workspace)",
            "siblings": [{"name": "search", "differs": "anywhere in the string", "kind": "fn", "none": True}, {"name": "fullmatch", "differs": "the whole string", "kind": "fn", "none": True}, {"name": "compile", "differs": "keep the pattern", "kind": "fn"}],
            "facts": "The stdlib ships no type hints for re. Its types come from its docstring (docs) and its code.", "history": {"releases": [], "same": True}}

def which_sync():
    N = os.path.expanduser("~/Projects/gpui-ce3/.opencode/node_modules/which/which.js")
    src = open(N).read().split("\n"); line = next(i for i, l in enumerate(src) if l.startswith("const whichSync")) + 1
    sg = {"ins": [{"name": "cmd", "type": {"text": "", "word": "a program name", "known": "docs"}},
                  {"name": "opt", "type": {"text": "", "word": "options", "known": "code"}, "opt": True, "fields": [
                      {"name": "nothrow", "word": "yes or no", "doc": "give null instead of throwing", "changes": "fails → none"},
                      {"name": "all", "word": "yes or no", "doc": "give every match, a list", "changes": "one → many"},
                      {"name": "path", "word": "text", "doc": "search this instead of $PATH"},
                      {"name": "pathExt", "word": "text", "doc": "extensions to try (Windows)"}]}],
          "out": {"type": {"text": "", "word": "a path", "known": "code"},
                  "fails": {"type": {"text": "Error", "word": "Error (code ENOENT)", "known": "code"}, "when": "if it isn't on PATH, unless nothrow", "kinds": []},
                  "none": {"when": "only with nothrow", "known": "code"}},
          "generics": []}
    sh = json.load(open(os.path.join(HERE, "data", "shapes", "js-which.json")))
    uses = [{"pkg": s["p"], "dir": "", "file": s["f"], "line": s["l"], "text": (s["pre"] + " " + s["call"] + s["post"]).strip(), "tag": "calls"} for s in sh["sites"]]
    return {"id": "js-which.sync", "lang": "javascript", "pkg": "which", "version": "2.0.2", "path": ["which"], "name": "which.sync", "kind": "function",
            "summary": "Find the first instance of an executable in the PATH.", "docs": [{"k": "p", "t": "Find the first instance of an executable in the PATH."}, {"k": "p", "t": "Like the unix `which` utility. Finds the first instance of a specified executable in the PATH environment variable. Does not cache the results, so `hash -r` is not needed when the PATH changes."}],
            "source": {"file": "which.js", "line": line}, "decl": src[line - 1].strip(), "sig": sg, "uses": uses, "workspace": "two node_modules trees (no JavaScript in your workspace)",
            "siblings": [{"name": "which", "differs": "later: a Promise, or a callback", "kind": "fn", "later": True, "fails": True}],
            "facts": "which ships no types: no .d.ts, no JSDoc. Every type here is read from its code or its README.", "history": {"releases": [], "same": True}}

def main():
    os.makedirs(OUT, exist_ok=True)
    for f in [from_str, value, as_str, alloc_info, serialize, re_match, which_sync]:
        d = f(); json.dump(d, open(os.path.join(OUT, d["id"] + ".json"), "w"), indent=0, separators=(",", ":"))
        tags = {}
        for u in d.get("uses", []): tags[u["tag"]] = tags.get(u["tag"], 0) + 1
        print(f"{d['id']:20} docs {len(d['docs']):2}  uses {len(d.get('uses', [])):4} {tags}")

if __name__ == "__main__":
    main()
