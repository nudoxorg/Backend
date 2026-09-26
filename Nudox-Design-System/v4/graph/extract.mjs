// extract.mjs — a real symbol graph from Rust sources, for the graph-view prototype.
//
// Not a compiler: a token-level item parser plus import-aware name resolution.
// Good enough to show the true shape of a codebase (items, members, derives,
// impls, field/param/return types, calls). Output: world.json next to this file.
//
//   node extract.mjs            # the backend workspace + serde_core + serde_json
import fs from "node:fs";
import path from "node:path";

const REPO = path.resolve(path.dirname(new URL(import.meta.url).pathname), "../../..");
const REG = fs.readdirSync(path.join(process.env.HOME, ".cargo/registry/src"))
  .map((d) => path.join(process.env.HOME, ".cargo/registry/src", d))[0];

// ---------------------------------------------------------------- crates
const CRATE_DIRS = [
  ...["crates", "frontends", "extensions", "apps"].flatMap((g) =>
    fs.readdirSync(path.join(REPO, g)).map((d) => path.join(REPO, g, d))),
  path.join(REG, "serde_core-1.0.229"), path.join(REG, "serde_json-1.0.151"),
].filter((d) => fs.existsSync(path.join(d, "Cargo.toml")) && !d.endsWith("/turso"));

function readCrate(dir) {
  const toml = fs.readFileSync(path.join(dir, "Cargo.toml"), "utf8");
  const name = /^\s*name\s*=\s*"([^"]+)"/m.exec(toml)[1];
  const lib = /\[lib\][^[]*?path\s*=\s*"([^"]+)"/s.exec(toml);
  let root = lib ? path.join(dir, lib[1]) : path.join(dir, "src/lib.rs");
  if (!fs.existsSync(root)) root = path.join(dir, "src/main.rs");
  const deps = [...toml.matchAll(/^\s*([a-z0-9_-]+)\s*=\s*\{[^}\n]*path\s*=/gm)].map((m) => m[1]);
  const ver = /^\s*version\s*=\s*"([^"]+)"/m.exec(toml);
  const yours = dir.includes("/apps/");
  const external = dir.startsWith(REG);
  return { dir, name, root, deps, version: ver ? ver[1] : "0.1.0", yours, external };
}

function walk(dir, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (e.name.startsWith(".") || ["target", "tests", "benches", "examples", "fixtures", "node_modules"].includes(e.name)) continue;
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else if (e.name.endsWith(".rs") && e.name !== "tests.rs" && e.name !== "build.rs") out.push(p);
  }
  return out;
}

// ---------------------------------------------------------------- lexer
const KW = new Set(("as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while union macro_rules yield box").split(" "));
const PRIM = new Set(("u8 u16 u32 u64 u128 usize i8 i16 i32 i64 i128 isize f32 f64 bool char str String Vec Option Result Box Rc Arc Cow HashMap HashSet BTreeMap BTreeSet VecDeque Some None Ok Err PhantomData Fn FnMut FnOnce Iterator IntoIterator Default Clone Copy Debug Display PartialEq Eq PartialOrd Ord Hash Send Sync Sized Unpin From Into TryFrom TryInto AsRef AsMut Borrow Deref DerefMut Drop ToString ToOwned Any Error Formatter Ordering Path PathBuf Duration Instant Mutex RwLock RefCell Cell Pin Future Poll Context Range Write Read BufRead Hasher Self").split(" "));
const STD_TRAITS = new Set("Clone Copy Debug Display PartialEq Eq PartialOrd Ord Hash Default Send Sync From Into Iterator Deref Drop AsRef Error ToString FromStr".split(" "));

function lex(src) {
  const t = []; // {k:'id'|'p'|'doc'|'lt'|'lit', v, line}
  let i = 0, line = 1; const n = src.length;
  const push = (k, v) => t.push({ k, v, line });
  while (i < n) {
    const c = src[i];
    if (c === "\n") { line++; i++; continue; }
    if (c === " " || c === "\t" || c === "\r") { i++; continue; }
    if (c === "/" && src[i + 1] === "/") {
      let j = src.indexOf("\n", i); if (j < 0) j = n;
      const body = src.slice(i, j);
      if (body.startsWith("///") && !body.startsWith("////")) push("doc", body.slice(3).trim());
      i = j; continue;
    }
    if (c === "/" && src[i + 1] === "*") {
      let d = 1, j = i + 2;
      while (j < n && d) { if (src[j] === "\n") line++; if (src[j] === "/" && src[j + 1] === "*") { d++; j += 2; } else if (src[j] === "*" && src[j + 1] === "/") { d--; j += 2; } else j++; }
      i = j; continue;
    }
    if ((c === "r" || c === "b") && /^(br|r|b)?#*"/.test(src.slice(i, i + 4)) && (c === "r" || src[i + 1] === "r")) {
      let j = i; while (src[j] !== "#" && src[j] !== '"') j++;
      let h = 0; while (src[j] === "#") { h++; j++; }
      const close = '"' + "#".repeat(h); const e = src.indexOf(close, j + 1);
      for (let k = i; k < e; k++) if (src[k] === "\n") line++;
      push("lit", "str"); i = e + close.length; continue;
    }
    if (c === '"' || (c === "b" && src[i + 1] === '"')) {
      let j = c === "b" ? i + 2 : i + 1;
      while (j < n && src[j] !== '"') { if (src[j] === "\\") j++; if (src[j] === "\n") line++; j++; }
      push("lit", "str"); i = j + 1; continue;
    }
    if (c === "'" || (c === "b" && src[i + 1] === "'")) {
      const s = c === "b" ? i + 1 : i;
      if (src[s + 1] === "\\") { let j = s + 2; while (src[j] !== "'") j++; push("lit", "chr"); i = j + 1; continue; }
      const cp = src.codePointAt(s + 1); const w = cp > 0xffff ? 2 : 1;
      if (src[s + 1 + w] === "'") { push("lit", "chr"); i = s + 2 + w; continue; }
      let j = s + 1; while (j < n && /[A-Za-z0-9_]/.test(src[j])) j++;
      push("lt", src.slice(s, j)); i = j; continue;
    }
    if (/[A-Za-z_]/.test(c)) {
      let j = i + 1; while (j < n && /[A-Za-z0-9_]/.test(src[j])) j++;
      let v = src.slice(i, j);
      if (v === "r" && src[j] === "#" && /[A-Za-z_]/.test(src[j + 1] || "")) { let k = j + 1; while (/[A-Za-z0-9_]/.test(src[k])) k++; v = src.slice(j + 1, k); j = k; }
      push("id", v); i = j; continue;
    }
    if (/[0-9]/.test(c)) { let j = i + 1; while (j < n && /[A-Za-z0-9_.]/.test(src[j]) && !(src[j] === "." && src[j + 1] === ".")) j++; push("lit", "num"); i = j; continue; }
    const two = src.slice(i, i + 2);
    if (two === "::" || two === "->" || two === "=>") { push("p", two); i += 2; continue; }
    push("p", c); i++;
  }
  // bracket matching
  const st = [];
  for (let k = 0; k < t.length; k++) {
    const v = t[k].k === "p" ? t[k].v : "";
    if (v === "{" || v === "(" || v === "[") st.push(k);
    else if (v === "}" || v === ")" || v === "]") { const o = st.pop(); if (o !== undefined) { t[o].m = k; t[k].m = o; } }
  }
  return t;
}

// ---------------------------------------------------------------- parser
const nodes = []; // {id,pkg,mod,kind,name,parent,vis,file,line,end,doc,sig,recv,via,derives,attrs}
const pending = []; // unresolved refs: {from, path:[..], kind, ctx}
const impls = []; // {selfPath, traitPath, ctx, members:[ids], line}
const modules = new Map(); // key pkg::path -> {id, pkg, path, file, imports:Map, globs:[], items:Map}

function modKey(pkg, mpath) { return pkg + "::" + mpath.join("::"); }
function getMod(pkg, mpath, file) {
  const k = modKey(pkg, mpath);
  if (!modules.has(k)) modules.set(k, { id: modules.size, key: k, pkg, path: mpath, file, imports: new Map(), globs: [], items: new Map() });
  return modules.get(k);
}

function skipAngles(t, i) { // t[i] is '<'
  let d = 0;
  for (; i < t.length; i++) {
    const v = t[i].k === "p" ? t[i].v : "";
    if (v === "<") d++; else if (v === ">") { d--; if (d === 0) return i + 1; }
    else if (v === "(" || v === "[") i = t[i].m ?? i;
    else if (v === "{" || v === ";") return i;
  }
  return i;
}

// collect type paths (like a::b::C<..>) from token range [a,b)
function paths(t, a, b) {
  const out = [];
  for (let i = a; i < b; i++) {
    if (t[i].k !== "id" || KW.has(t[i].v) && t[i].v !== "Self" && t[i].v !== "crate" && t[i].v !== "super" && t[i].v !== "self") continue;
    if (i > a && t[i - 1].k === "p" && t[i - 1].v === "::") continue; // continuation handled by head
    if (i > a && t[i - 1].k === "p" && t[i - 1].v === ".") { // method call .name(
      if (t[i + 1] && t[i + 1].v === "(") out.push({ seg: [t[i].v], method: true, line: t[i].line });
      continue;
    }
    const seg = [t[i].v]; let j = i + 1;
    while (j + 1 < b && t[j].v === "::") {
      if (t[j + 1].v === "<") { j = skipAngles(t, j + 1); continue; }
      if (t[j + 1].k !== "id") break;
      seg.push(t[j + 1].v); j += 2;
    }
    const call = t[j] && t[j].v === "(";
    const macro = t[j] && t[j].v === "!";
    if (macro) continue;
    out.push({ seg, call, line: t[i].line });
    i = j - 1;
  }
  return out;
}

function firstSentence(docs) {
  const s = docs.join(" ").replace(/`/g, "").replace(/\[([^\]]+)\]\([^)]*\)/g, "$1").replace(/\[([^\]]+)\]/g, "$1").trim();
  if (!s || s.startsWith("#")) return "";
  const m = /^(.+?[.!?])(\s|$)/.exec(s);
  return (m ? m[1] : s).slice(0, 200);
}

function tokText(t, a, b) {
  let s = "";
  for (let i = a; i < b; i++) {
    const v = t[i].k === "lit" ? (t[i].v === "str" ? '"…"' : t[i].v === "chr" ? "'…'" : "0") : t[i].v;
    const prev = s.slice(-1);
    const tight = /[(\[<.:&]$/.test(s) || /^[)\]>,.:;?]/.test(v) || v === "(" && /[A-Za-z0-9_>]$/.test(prev) || v === "<" && /[A-Za-z0-9_]$/.test(prev) || v === "::" || prev === "!" || v === "!" || v === "'" ;
    s += (s && !tight ? " " : "") + v;
  }
  return s.replace(/\s+/g, " ").replace(/& '/g, "&'").replace(/ :: /g, "::").replace(/-> /g, "-> ").trim();
}

function addNode(n) { n.id = nodes.length; nodes.push(n); return n.id; }

function parseItems(t, a, b, ctx) {
  let docs = [], attrs = [];
  let i = a;
  while (i < b) {
    const tk = t[i];
    if (tk.k === "doc") { docs.push(tk.v); i++; continue; }
    if (tk.v === "#" && (t[i + 1]?.v === "[" || (t[i + 1]?.v === "!" && t[i + 2]?.v === "["))) {
      const o = t[i + 1].v === "[" ? i + 1 : i + 2;
      if (o === i + 1) attrs.push(t.slice(o + 1, t[o].m));
      i = t[o].m + 1; continue;
    }
    const cfgTest = attrs.some((x) => x[0]?.v === "cfg" && x.some((y) => y.v === "test"));
    const derives = attrs.filter((x) => x[0]?.v === "derive").flatMap((x) => x.filter((y) => y.k === "id").slice(1).map((y) => y.v));
    const nonExhaustive = attrs.some((x) => x[0]?.v === "non_exhaustive");
    const mustUse = attrs.some((x) => x[0]?.v === "must_use");
    const deprecated = attrs.some((x) => x[0]?.v === "deprecated");
    const d = docs; docs = []; attrs = [];
    let vis = "";
    if (tk.v === "pub") { vis = "pub"; i++; if (t[i]?.v === "(") { vis = "pub(" + tokText(t, i + 1, t[i].m) + ")"; i = t[i].m + 1; } }
    const quals = [];
    while (["async", "unsafe", "default", "extern"].includes(t[i]?.v) || (t[i]?.v === "const" && t[i + 1]?.v === "fn")) {
      if (t[i].v === "extern" && t[i + 1]?.v === "crate") break;
      quals.push(t[i].v); i++;
      if (t[i - 1].v === "extern" && t[i]?.k === "lit") i++;
    }
    const kw = t[i]?.v; const start = t[i]?.line;
    if (i >= b) break;
    if (kw === "mod") {
      const name = t[i + 1].v; i += 2;
      if (t[i]?.v === "{") { if (!cfgTest && name !== "tests") parseItems(t, i + 1, t[i].m, { ...ctx, mod: getMod(ctx.pkg, [...ctx.mod.path, name], ctx.file) }); i = t[i].m + 1; }
      else i++;
      continue;
    }
    if (cfgTest) { // skip the whole item
      while (i < b && t[i].v !== ";" && t[i].v !== "{") { if (t[i].v === "(" || t[i].v === "[") i = t[i].m; i++; }
      if (t[i]?.v === "{") i = t[i].m; i++; continue;
    }
    if (kw === "use") {
      let j = i + 1; while (j < b && t[j].v !== ";") { if (t[j].v === "{") j = t[j].m; j++; }
      parseUse(t, i + 1, j, [], ctx.mod, vis);
      i = j + 1; continue;
    }
    if (kw === "extern" && t[i + 1]?.v === "crate") { while (i < b && t[i].v !== ";") i++; i++; continue; }
    if (kw === "struct" || kw === "union" || kw === "enum" || kw === "trait") {
      const name = t[i + 1].v; let j = i + 2;
      if (t[j]?.v === "<") j = skipAngles(t, j);
      const headStart = i;
      let supers = [];
      if (kw === "trait" && t[j]?.v === ":") { const s = j; while (j < b && t[j].v !== "{" && t[j].v !== "where") j++; supers = paths(t, s + 1, j); }
      let whereAt = -1;
      while (j < b && t[j].v !== "{" && t[j].v !== ";" && t[j].v !== "(") { if (t[j].v === "where") whereAt = j; j++; }
      const id = addNode({ pkg: ctx.pkg, mod: ctx.mod.id, kind: kw === "union" ? "struct" : kw, name, parent: -1, vis, file: ctx.file, line: start,
        doc: firstSentence(d), derives, nonExhaustive, mustUse, deprecated, sig: tokText(t, headStart, whereAt > 0 ? whereAt : j) });
      ctx.mod.items.set(name, id);
      for (const p of supers) pending.push({ from: id, p, rel: "is", ctx });
      for (const dv of derives) pending.push({ from: id, p: { seg: [dv] }, rel: "derives", ctx });
      if (t[j]?.v === "(") { // tuple struct
        const fields = splitTop(t, j + 1, t[j].m);
        fields.forEach(([fa, fb], k) => {
          let s = fa; while (t[s]?.v === "pub" || t[s]?.v === "#") { if (t[s].v === "#") s = t[s + 1].m + 1; else { s++; if (t[s]?.v === "(") s = t[s].m + 1; } }
          const fid = addNode({ pkg: ctx.pkg, mod: ctx.mod.id, kind: "field", name: String(k), parent: id, vis: t[fa]?.v === "pub" ? "pub" : "", file: ctx.file, line: t[fa]?.line, doc: "", ty: tokText(t, s, fb) });
          for (const p of paths(t, s, fb)) { pending.push({ from: id, p, rel: "has", ctx }); pending.push({ from: fid, p, rel: "type", ctx }); }
        });
        j = t[j].m + 1; while (j < b && t[j].v !== ";") j++;
        nodes[id].end = t[j]?.line; i = j + 1; continue;
      }
      if (t[j]?.v === ";") { nodes[id].end = t[j].line; i = j + 1; continue; }
      const o = j, c = t[j].m; nodes[id].end = t[c].line;
      if (kw === "trait") parseItems(t, o + 1, c, { ...ctx, traitOf: id });
      else if (kw === "enum") parseVariants(t, o + 1, c, id, ctx);
      else parseFields(t, o + 1, c, id, ctx);
      i = c + 1; continue;
    }
    if (kw === "impl") {
      let j = i + 1; if (t[j]?.v === "<") j = skipAngles(t, j);
      const s = j; let forAt = -1, whereAt = -1, dd = 0;
      while (j < b && !(t[j].v === "{" && dd === 0)) {
        const v = t[j].v;
        if (v === "<") dd++; else if (v === ">") dd--; else if (v === "(" || v === "[") j = t[j].m;
        else if (v === "for" && dd === 0 && forAt < 0) forAt = j; else if (v === "where" && dd === 0) whereAt = j;
        j++;
      }
      const tyEnd = whereAt > 0 ? whereAt : j;
      const traitP = forAt > 0 ? paths(t, s, forAt)[0] : null;
      const selfPs = paths(t, forAt > 0 ? forAt + 1 : s, tyEnd).filter((p) => !p.method);
      const neg = t[s]?.v === "!";
      const imp = { self: selfPs[0], trait: traitP, neg, ctx, members: [], line: t[i].line, head: tokText(t, i, tyEnd), generic: t[i + 1]?.v === "<" };
      impls.push(imp);
      if (t[j]?.v === "{") { parseItems(t, j + 1, t[j].m, { ...ctx, impl: imp }); i = t[j].m + 1; } else i = j + 1;
      continue;
    }
    if (kw === "fn") {
      const name = t[i + 1].v; let j = i + 2; let gen = "";
      if (t[j]?.v === "<") { const g0 = j; j = skipAngles(t, j); gen = tokText(t, g0 + 1, j - 1); }
      const po = j; if (t[po]?.v !== "(") { i++; continue; }
      const pc = t[po].m; j = pc + 1;
      let retA = -1, whereAt = -1;
      if (t[j]?.v === "->") retA = j + 1;
      while (j < b && t[j].v !== "{" && t[j].v !== ";") { if (t[j].v === "where") whereAt = j; if (t[j].v === "(" || t[j].v === "[") j = t[j].m; j++; }
      const retB = whereAt > 0 ? whereAt : j;
      const params = splitTop(t, po + 1, pc);
      let recv = "";
      if (params.length) {
        const txt = tokText(t, params[0][0], params[0][1]);
        if (/^(mut )?self\b/.test(txt)) recv = "consumes"; else if (/^&\s*('\w+ )?mut self/.test(txt)) recv = "changes"; else if (/^&\s*('\w+ )?self/.test(txt)) recv = "reads";
        else if (/^self\s*:/.test(txt)) recv = /&\s*mut/.test(txt) ? "changes" : /&/.test(txt) ? "reads" : "consumes";
      }
      const owner = ctx.impl || ctx.traitOf !== undefined;
      const kind = owner ? "method" : "function";
      const id = addNode({ pkg: ctx.pkg, mod: ctx.mod.id, kind, name, parent: ctx.traitOf ?? -1, vis: vis || (ctx.impl?.trait || ctx.traitOf !== undefined ? "pub" : ""), file: ctx.file, line: start,
        doc: firstSentence(d), recv, quals, mustUse, deprecated, sig: tokText(t, i - quals.length - (vis ? 1 : 0), retB),
        params: params.map(([pa, pb]) => tokText(t, pa, pb)), ret: retA > 0 ? tokText(t, retA, retB) : "", required: t[j]?.v === ";" && ctx.traitOf !== undefined,
        gen, wh: whereAt > 0 ? tokText(t, whereAt + 1, j) : "" });
      if (ctx.impl) ctx.impl.members.push(id);
      else if (ctx.traitOf === undefined) ctx.mod.items.set(name, id);
      for (const [pa, pb] of params) for (const p of paths(t, pa, pb)) pending.push({ from: id, p, rel: "takes", ctx });
      if (retA > 0) for (const p of paths(t, retA, retB)) pending.push({ from: id, p, rel: "gives", ctx });
      if (t[j]?.v === "{") {
        const c = t[j].m; nodes[id].end = t[c].line;
        for (const p of paths(t, j + 1, c)) pending.push({ from: id, p, rel: p.call || p.method ? "calls" : "uses", ctx });
        i = c + 1;
      } else { nodes[id].end = t[j]?.line; i = j + 1; }
      continue;
    }
    if (kw === "type" || kw === "const" || kw === "static") {
      const name = t[i + 1]?.v === "mut" ? t[i + 2].v : t[i + 1].v; let j = i + 1;
      while (j < b && t[j].v !== ";") { if (t[j].v === "{" || t[j].v === "(" || t[j].v === "[") j = t[j].m; j++; }
      if (name !== "_") {
        const kind = kw === "type" ? "type" : "constant";
        const id = addNode({ pkg: ctx.pkg, mod: ctx.mod.id, kind, name, parent: ctx.traitOf ?? -1, vis, file: ctx.file, line: start, end: t[j]?.line, doc: firstSentence(d), sig: tokText(t, i, Math.min(j, i + 40)) });
        if (ctx.impl) ctx.impl.members.push(id); else if (ctx.traitOf === undefined) ctx.mod.items.set(name, id);
        for (const p of paths(t, i + 2, j)) pending.push({ from: id, p, rel: kind === "type" ? "is" : "uses", ctx });
      }
      i = j + 1; continue;
    }
    if (kw === "macro_rules" && t[i + 1]?.v === "!") {
      const name = t[i + 2].v; const o = i + 3;
      const id = addNode({ pkg: ctx.pkg, mod: ctx.mod.id, kind: "macro", name, parent: -1, vis: "pub", file: ctx.file, line: start, end: t[t[o].m]?.line, doc: firstSentence(d), sig: `macro_rules! ${name}` });
      ctx.mod.items.set(name, id);
      i = t[o].m + 1; if (t[i]?.v === ";") i++; continue;
    }
    // item-level macro call or anything else
    if (t[i].k === "id" && t[i + 1]?.v === "!") { let j = i + 2; if (t[j]?.k === "id") j++; if (t[j]?.m) j = t[j].m; i = j + 1; if (t[i]?.v === ";") i++; continue; }
    i++;
  }
}

function splitTop(t, a, b) {
  const out = []; let s = a, d = 0;
  for (let i = a; i < b; i++) {
    const v = t[i].v;
    if (v === "<") d++; else if (v === ">") d--; else if (v === "(" || v === "[" || v === "{") i = t[i].m;
    else if (v === "," && d <= 0) { if (i > s) out.push([s, i]); s = i + 1; }
  }
  if (b > s) out.push([s, b]);
  return out;
}

function parseFields(t, a, b, owner, ctx) {
  for (const [fa, fb] of splitTop(t, a, b)) {
    let s = fa, docs = [];
    while (s < fb && (t[s].k === "doc" || t[s].v === "#")) { if (t[s].k === "doc") { docs.push(t[s].v); s++; } else s = t[s + 1].m + 1; }
    let vis = ""; if (t[s]?.v === "pub") { vis = "pub"; s++; if (t[s]?.v === "(") s = t[s].m + 1; }
    if (t[s]?.k !== "id" || t[s + 1]?.v !== ":") continue;
    const fid = addNode({ pkg: ctx.pkg, mod: ctx.mod.id, kind: "field", name: t[s].v, parent: owner, vis, file: ctx.file, line: t[s].line, doc: firstSentence(docs), ty: tokText(t, s + 2, fb) });
    for (const p of paths(t, s + 2, fb)) { pending.push({ from: owner, p, rel: "has", ctx }); pending.push({ from: fid, p, rel: "type", ctx }); }
  }
}

function parseVariants(t, a, b, owner, ctx) {
  for (const [va, vb] of splitTop(t, a, b)) {
    let s = va, docs = [];
    while (s < vb && (t[s].k === "doc" || t[s].v === "#")) { if (t[s].k === "doc") { docs.push(t[s].v); s++; } else s = t[s + 1].m + 1; }
    if (t[s]?.k !== "id") continue;
    let payload = "", shape = "unit";
    if (t[s + 1]?.v === "(" || t[s + 1]?.v === "{") {
      shape = t[s + 1].v === "(" ? "tuple" : "record";
      payload = tokText(t, s + 2, t[s + 1].m);
      if (shape === "record") payload = splitTop(t, s + 2, t[s + 1].m).map(([x, y]) => { let q = x; while (t[q]?.k === "doc" || t[q]?.v === "#") q = t[q].k === "doc" ? q + 1 : t[q + 1].m + 1; return tokText(t, q, y); }).join(", ");
    }
    const vid = addNode({ pkg: ctx.pkg, mod: ctx.mod.id, kind: "variant", name: t[s].v, parent: owner, vis: "pub", file: ctx.file, line: t[s].line, doc: firstSentence(docs), ty: payload, shape });
    if (t[s + 1]?.m) for (const p of paths(t, s + 2, t[s + 1].m)) { pending.push({ from: owner, p, rel: "has", ctx }); pending.push({ from: vid, p, rel: "type", ctx }); }
  }
}

function parseUse(t, a, b, prefix, mod, vis) {
  let seg = [...prefix];
  for (let i = a; i < b; i++) {
    const v = t[i].v;
    if (v === "::") continue;
    if (v === "{") {
      for (const [x, y] of splitTop(t, i + 1, t[i].m)) parseUse(t, x, y, seg, mod, vis);
      return;
    }
    if (v === "*") { mod.globs.push(seg); return; }
    if (v === "as") { const alias = t[i + 1].v; if (alias !== "_") mod.imports.set(alias, seg); return; }
    if (t[i].k === "id") seg = [...seg, v];
  }
  if (seg.length) { const last = seg[seg.length - 1]; mod.imports.set(last === "self" ? seg[seg.length - 2] : last, last === "self" ? seg.slice(0, -1) : seg); if (vis === "pub") (mod.reexports ||= []).push(seg); }
}

// ---------------------------------------------------------------- run
const crates = CRATE_DIRS.map(readCrate);
const byCrateIdent = new Map(crates.map((c) => [c.name.replace(/-/g, "_"), c.name]));
byCrateIdent.set("serde", "serde_core");
let lines = 0;
for (const c of crates) {
  const base = path.dirname(c.root);
  const files = walk(base).filter((f) => !f.includes("/bin/") || f === c.root);
  for (const f of files) {
    const rel = path.relative(base, f).replace(/\.rs$/, "").split(path.sep);
    let mp = rel;
    if (["lib", "main"].includes(rel[rel.length - 1]) && rel.length === 1) mp = [];
    else if (rel[rel.length - 1] === "mod") mp = rel.slice(0, -1);
    if (mp.some((s) => s === "tests" || s === "benches")) continue;
    const src = fs.readFileSync(f, "utf8"); lines += src.split("\n").length;
    const t = lex(src);
    parseItems(t, 0, t.length, { pkg: c.name, mod: getMod(c.name, mp, path.relative(REPO, f)), file: path.relative(c.external ? REG : REPO, f) });
  }
}

// ---- resolution
const pkgDeps = new Map(crates.map((c) => [c.name, new Set([...c.deps, "serde_core", c.name])]));
for (const c of crates) if (c.name === "serde_json") pkgDeps.get(c.name).add("serde_core");
const byName = new Map();
for (const n of nodes) if (n.parent === -1 && n.kind !== "method") { if (!byName.has(n.name)) byName.set(n.name, []); byName.get(n.name).push(n.id); }
const methodsByName = new Map();

function lookupInMod(pkg, mpath, name, depth = 0) {
  const m = modules.get(modKey(pkg, mpath));
  if (!m) return -1;
  if (m.items.has(name)) return m.items.get(name);
  if (depth < 4 && m.imports.has(name)) { const r = resolveAbs(m.imports.get(name), m, depth + 1); if (r >= 0) return r; }
  if (depth < 4) for (const g of m.globs) { const gm = modPathOf(g, m); if (gm) { const r = lookupInMod(gm.pkg, gm.path, name, depth + 1); if (r >= 0) return r; } }
  return -1;
}
function modPathOf(seg, m) { // a module path (all segments are modules)
  let pkg = m.pkg, p = [...m.path], s = [...seg];
  if (s[0] === "crate") { p = []; s.shift(); }
  else if (s[0] === "self") s.shift();
  else if (s[0] === "super") { while (s[0] === "super") { p.pop(); s.shift(); } }
  else if (byCrateIdent.has(s[0])) { pkg = byCrateIdent.get(s[0]); p = []; s.shift(); }
  else if (!modules.has(modKey(pkg, [...p, s[0]]))) { if (!modules.has(modKey(pkg, [s[0]]))) return null; p = []; }
  return { pkg, path: [...p, ...s] };
}
function resolveAbs(seg, m, depth = 0) {
  if (seg.length === 1) return depth ? lookupInMod(m.pkg, m.path, seg[0], depth) : -1;
  const head = modPathOf(seg.slice(0, -1), m); const name = seg[seg.length - 1];
  if (head) {
    const r = lookupInMod(head.pkg, head.path, name, depth + 1); if (r >= 0) return r;
    // Type::member
    const tp = modPathOf(seg.slice(0, -2), m);
    if (tp && seg.length >= 2) { const ty = lookupInMod(tp.pkg, tp.path, seg[seg.length - 2], depth + 1); if (ty >= 0) return memberOf(ty, name); }
    // re-exported elsewhere in that package: unique name
    return uniqueIn(name, (n) => n.pkg === head.pkg);
  }
  return -1;
}
const members = new Map(); // parent -> Map(name -> id)
function memberOf(ty, name) { return members.get(ty)?.get(name) ?? -1; }
function uniqueIn(name, pred) { const c = (byName.get(name) || []).filter((id) => pred(nodes[id])); return c.length === 1 ? c[0] : -1; }

function resolve(p, ctx, from) {
  const seg = p.seg; const m = ctx.mod;
  if (p.method) { const c = methodsByName.get(seg[0]) || []; const inDeps = c.filter((id) => pkgDeps.get(ctx.pkg).has(nodes[id].pkg)); return inDeps.length === 1 ? inDeps[0] : -1; }
  if (seg[0] === "Self" || seg[0] === "self") {
    const owner = ctx.impl?.selfId ?? ctx.traitOf ?? (nodes[from].parent >= 0 ? nodes[from].parent : -1);
    if (owner < 0) return -1; return seg.length > 1 ? memberOf(owner, seg[1]) : owner;
  }
  if (PRIM.has(seg[0]) && seg.length === 1 && !m.items.has(seg[0]) && !m.imports.has(seg[0])) return STD_TRAITS.has(seg[0]) ? stdNode(seg[0]) : -1;
  if (seg.length === 1) {
    const r = lookupInMod(m.pkg, m.path, seg[0]); if (r >= 0) return r;
    // fall back: unique in this package, then unique in deps
    const own = uniqueIn(seg[0], (n) => n.pkg === m.pkg); if (own >= 0) return own;
    return uniqueIn(seg[0], (n) => pkgDeps.get(m.pkg).has(n.pkg));
  }
  let s = seg;
  if (m.imports.has(s[0])) s = [...m.imports.get(s[0]), ...s.slice(1)];
  if (["std", "core", "alloc", "fmt", "cmp", "hash", "ops", "convert", "error", "str", "iter", "clone", "marker", "default", "string"].includes(s[0]) && STD_TRAITS.has(s[s.length - 1])) return stdNode(s[s.length - 1]);
  const r = resolveAbs(s, m); if (r >= 0) return r;
  // Type::method where Type resolves
  const ty = resolve({ seg: s.slice(0, -1) }, ctx, from);
  if (ty >= 0) return memberOf(ty, s[s.length - 1]);
  return -1;
}
const stdIds = new Map();
function stdNode(name) {
  if (!stdIds.has(name)) {
    const mod = getMod("std", [stdModOf(name)], "");
    const id = addNode({ pkg: "std", mod: mod.id, kind: "trait", name, parent: -1, vis: "pub", file: "", line: 0, doc: STD_DOC[name] || "", sig: `pub trait ${name}` });
    mod.items.set(name, id); stdIds.set(name, id);
  }
  return stdIds.get(name);
}
function stdModOf(n) { return ({ Clone: "clone", Copy: "marker", Send: "marker", Sync: "marker", Debug: "fmt", Display: "fmt", PartialEq: "cmp", Eq: "cmp", PartialOrd: "cmp", Ord: "cmp", Hash: "hash", Default: "default", From: "convert", Into: "convert", AsRef: "convert", Iterator: "iter", Deref: "ops", Drop: "ops", Error: "error", ToString: "string", FromStr: "str" })[n] || "prelude"; }
const STD_DOC = { Clone: "Make an explicit copy of a value.", Copy: "Copied bit for bit; moving never gives it away.", Debug: "Print for programmers.", Display: "Print for people.", PartialEq: "Compare with ==.", Eq: "Equality is total.", PartialOrd: "Compare with < and >.", Ord: "A total order.", Hash: "Feed into a hasher.", Default: "Has a sensible empty value.", From: "Build from another type.", Error: "A value that describes a failure.", Iterator: "Yields a sequence of values.", Deref: "Behaves like a reference to another type.", Drop: "Runs code when it goes away.", FromStr: "Parse from a string.", ToString: "Turn into a String.", Send: "Can move to another thread.", Sync: "Can be shared between threads.", AsRef: "Cheaply borrow as another type.", Into: "Convert into another type." };

// impls: attach members to their self type
for (const imp of impls) {
  if (!imp.self) continue;
  const id = resolve(imp.self, imp.ctx, -1);
  imp.selfId = id >= 0 && nodes[id].parent === -1 && ["struct", "enum", "trait", "type"].includes(nodes[id].kind) ? id : -1;
  if (imp.trait) imp.traitId = resolve(imp.trait, imp.ctx, -1);
  for (const mid of imp.members) {
    if (imp.selfId >= 0) { nodes[mid].parent = imp.selfId; if (imp.trait) nodes[mid].via = imp.trait.seg[imp.trait.seg.length - 1]; }
    else { nodes[mid].orphan = true; }
  }
}
for (const n of nodes) if (n.parent >= 0) {
  if (!members.has(n.parent)) members.set(n.parent, new Map());
  if (!members.get(n.parent).has(n.name)) members.get(n.parent).set(n.name, n.id);
  if (n.kind === "method") { if (!methodsByName.has(n.name)) methodsByName.set(n.name, []); methodsByName.get(n.name).push(n.id); }
}
// orphan impl members (impl for a foreign type) become free functions of their module
for (const n of nodes) if (n.orphan) { n.kind = "function"; n.orphan = 1; }

const REL = ["has", "takes", "gives", "is", "derives", "calls", "uses", "type", "impl"];
const edges = new Map();
function addEdge(a, b, rel) {
  if (a < 0 || b < 0 || a === b) return;
  const k = a * 4194304 + b; const bit = 1 << REL.indexOf(rel);
  edges.set(k, (edges.get(k) || 0) | bit);
}
for (const imp of impls) if (imp.selfId >= 0 && imp.traitId >= 0 && !imp.neg) {
  addEdge(imp.selfId, imp.traitId, "impl");
  (nodes[imp.selfId].impls ||= []).push({ trait: imp.traitId, line: imp.line, members: imp.members.map((m) => nodes[m].name), generic: imp.generic });
} else if (imp.trait && imp.selfId >= 0) (nodes[imp.selfId].implsExt ||= []).push(imp.trait.seg.join("::"));
let res = 0, unres = 0;
for (const r of pending) {
  const to = resolve(r.p, r.ctx.impl ? { ...r.ctx } : r.ctx, r.from);
  if (to >= 0) { res++; addEdge(r.from, to, r.rel); } else unres++;
}

// ---- output
const pkgs = [...new Set(nodes.map((n) => n.pkg))];
const pkgIndex = new Map(pkgs.map((p, i) => [p, i]));
const modList = [...modules.values()].filter((m) => nodes.some((n) => n.mod === m.id));
const modIndex = new Map(modList.map((m, i) => [m.id, i]));
const out = {
  packages: pkgs.map((p) => { const c = crates.find((x) => x.name === p); return { name: p, version: c ? c.version : "1.93.0", yours: !!c?.yours, external: !c || c.external, deps: c ? [...pkgDeps.get(p)].filter((d) => d !== p && pkgIndex.has(d)).map((d) => pkgIndex.get(d)) : [] }; }),
  modules: modList.map((m) => ({ pkg: pkgIndex.get(m.pkg), path: m.path.join("::"), file: m.file })),
  nodes: nodes.map((n) => {
    const o = { k: n.kind, n: n.name, p: pkgIndex.get(n.pkg), m: modIndex.get(n.mod), u: n.parent, l: n.line };
    if (n.end) o.e = n.end;
    if (n.vis) o.v = n.vis;
    if (n.doc) o.d = n.doc;
    if (n.sig) o.s = n.sig;
    if (n.file) o.f = n.file;
    for (const k of ["gen", "wh", "orphan", "recv", "via", "derives", "params", "ret", "ty", "shape", "quals", "impls", "implsExt", "required", "nonExhaustive", "mustUse", "deprecated"]) {
      const v = n[k]; if (v === undefined || v === false || v === "" || (Array.isArray(v) && !v.length)) continue; o[k] = v;
    }
    return o;
  }),
  rel: REL,
  edges: [...edges.entries()].map(([k, bits]) => [Math.floor(k / 4194304), k % 4194304, bits]),
};
fs.writeFileSync(path.join(path.dirname(new URL(import.meta.url).pathname), "world.json"), JSON.stringify(out));
const kinds = {}; for (const n of nodes) kinds[n.kind] = (kinds[n.kind] || 0) + 1;
console.log(JSON.stringify({ crates: crates.length, lines, modules: modList.length, nodes: nodes.length, kinds, edges: edges.size, resolved: res, unresolved: unres, impls: impls.length }));
