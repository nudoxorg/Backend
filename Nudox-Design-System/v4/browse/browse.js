// FACET v4 · Browse — find a package for a need, judge it, compare by capability, see your tree, adopt.
// Everything shown comes from data.json (build_data.py). Unknown stays unknown.
//
// URL (headless stills are reproducible):
//   ?q=parse toml            find (words · shape `text -> Value` · `like serde_json` · a pasted line of code)
//   ?view=tree | ?compare=toml,toml_edit,basic-toml   (&preview=basic-toml opens your code rewritten)
//   ?hover=<package>         select a row / column and open its card     ?sel=<n> select the n-th row
//   ?add=<package>&t=<ms>    adopt, frozen at t ms                         ?to=<q>&t=<ms> FLIP re-rank frozen
//   ?alt=1 (⌥ held) ?cmd=1 (⌘ held) ?w=<px> window width ?zoom=2 (200 % text) ?why=<crate> (tree: the path)
import { ecosystemMark, licenseMark, versionComb, packageGem, openMark } from "../marks/marks.js";

const P = new URLSearchParams(location.search);
const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const fmt = (n) => (n == null ? "—" : Number(n).toLocaleString("en"));
const plural = (n, one, many = one + "s") => `${fmt(n)} ${n === 1 ? one : many}`;
const WORDS = ["no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve"];
const word = (n) => (n >= 0 && n < WORDS.length ? WORDS[n] : fmt(n));
const list = (xs, conj = "and") => (xs.length <= 1 ? xs.join("") : xs.slice(0, -1).join(", ") + ` ${conj} ` + xs.at(-1));
const cap = (s) => (s ? s[0].toUpperCase() + s.slice(1) : s);
const ident = (n) => n.replace(/-/g, "_");
const short = (v) => String(v || "").replace(/\+.*$/, "");
const lastSeg = (p) => p.split("::").at(-1);
const tail2 = (p) => p.split("::").slice(p.split("::").length > 2 ? -2 : -1).join("::");
const SPRING = "cubic-bezier(.2,.9,.25,1.18)", GLIDE = "cubic-bezier(.22,1,.36,1)", BOUNCE = "cubic-bezier(.34,1.56,.64,1)", DROP = "cubic-bezier(.5,0,.9,.6)", SNAP = "cubic-bezier(.3,0,0,1)";
const REDUCED = matchMedia("(prefers-reduced-motion: reduce)").matches || P.get("reduced") === "1";

let D, ICONS = {}, GROUND = "";
const S = {
  view: P.get("view") || (P.get("compare") ? "compare" : "find"),
  q: P.get("q") ?? "", sel: P.has("sel") ? +P.get("sel") : null, hover: P.get("hover"),
  compare: (P.get("compare") || "").split(",").filter(Boolean), preview: P.get("preview"),
  alt: P.get("alt") === "1", cmd: P.get("cmd") === "1", added: [], hand: (P.get("hand") || "").split(",").filter(Boolean),
  open: new Set(), whyOpen: P.get("why"), results: null,
};

// ------------------------------------------------------------------ house glyphs
function inner(svg) { return svg.slice(svg.indexOf(">") + 1, svg.lastIndexOf("</svg>")); }
function ico(name, cls = "s14") { const s = ICONS["ui/" + name]; return s ? `<svg class="ico ${cls}" viewBox="0 0 24 24" aria-hidden="true">${inner(s)}</svg>` : ""; }
function kind(name, size = "sm") {
  const s = ICONS["kind/" + name];
  const fam = { module: "ns", package: "ns", struct: "ty", enum: "ty", type: "ty", trait: "co", function: "ca", method: "ca", macro: "ca", constant: "va", variant: "va" }[name] || "ns";
  return s ? `<span class="k ${fam} ${size}"><svg viewBox="0 0 24 24">${inner(s)}</svg></span>` : "";
}
const FACETS = ["24,2 35,13 24,10", "24,10 35,13 38,24", "35,13 46,24 38,24", "46,24 35,35 38,24", "38,24 35,35 24,38", "35,35 24,46 24,38",
  "24,46 13,35 24,38", "24,38 13,35 10,24", "13,35 2,24 10,24", "2,24 13,13 10,24", "10,24 13,13 24,10", "13,13 24,2 24,10"];
const LIT = [0.4, 0.3, 0.34, 0.14, 0.08, 0.11, 0.22, 0.2, 0.3, 0.56, 0.5, 0.66];
function gem(kindName = "package", px = 28, cls = "") {
  const g = ICONS["kind/" + kindName] ? inner(ICONS["kind/" + kindName]) : "";
  return `<svg class="gem ns ${cls}" width="${px}" height="${px}" viewBox="0 0 48 48">` + FACETS.map((p, i) => `<polygon class="fc" style="--t:${LIT[i]};--i:${i}" points="${p}"></polygon>`).join("")
    + `<path d="M24 2 46 24 24 46 2 24z" fill="none" stroke="currentColor" stroke-width="1"></path><path class="tb" d="M24 10 38 24 24 38 10 24z" stroke="currentColor" stroke-width=".7" stroke-opacity=".55"></path>`
    + (px >= 22 ? `<g transform="translate(17.4 17.4) scale(.55)" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="square" stroke-linejoin="miter">${g.replace('class="f"', 'class="f" fill="currentColor" stroke="none" opacity=".5"')}</g>` : "") + `</svg>`;
}
const ECO = { rust: "crates", npm: "npm", pypi: "pypi", go: "go", java: "maven", csharp: "nuget", cpp: "cpp" };
function emark(p, opts = {}) {
  const eco = ECO[p.eco] || "crates";
  if (p.stdlib && p.eco === "rust") return ecosystemMark("crates", { word: false, id: opts.id, say: "The standard library: it ships with Rust, nothing to add.", install: "use std::…;" });
  return ecosystemMark(eco, { word: false, pkg: p.name, id: opts.id, ...(opts.m || {}) });
}
const keysHtml = (...ks) => `<span class="keys">${ks.map((k) => `<kbd class="kbd">${esc(k)}</kbd>`).join("")}</span>`;

// ------------------------------------------------------------------ data helpers
const pk = (n) => D.packages[n] || null;
const directOf = (n) => D.direct.find((d) => d.name === n);
function tree(p) {
  if (!p) return {};
  if (p.stdlib) return { std: true };
  const added = S.added.includes(p.name);
  const direct = p.tree.direct || added;
  const inLock = (p.tree.pins || []).length > 0;
  return { direct, inLock, added, pins: p.tree.pins || [], uses: p.tree.uses, usesBy: p.tree.usesBy || {} };
}
function addsOf(p) { return (p.cost.adds || []).map((x) => x.split(" ")[0]); }
function advisory(p) {
  const a = p.advisory;
  if (!a) return null;
  return a.affecting && a.affecting.length ? a.affecting[0] : null;
}
function deprecated(p) { return /deprecated/.test(p.latest || "") || /deprecated/i.test(p.desc || ""); }
// the one quiet reason a row exists
function reason(p) {
  const t = tree(p);
  if (t.std) return { t: "in the standard library", m: true };
  if (t.added) return { t: "yours · just added", m: true };
  const adv = advisory(p);
  if (adv && (adv.informational === "unmaintained" || !t.inLock)) return { t: adv.informational === "unmaintained" ? "unmaintained" : "known advisory", w: true };
  if (deprecated(p)) return { t: "deprecated", w: true };
  if (t.direct) return { t: t.uses ? `yours · ${plural(t.uses, "place")}` : "yours", m: true };
  if (t.inLock) return { t: "already in your tree", m: true };
  const n = addsOf(p).length;
  return { t: n === 0 ? "adds only itself" : `adds ${plural(n + (p.cost.addsSelf ? 1 : 0), "crate")}` };
}
function famOf(n) { return pk(n)?.family; }
function incumbentFor(fam) {
  for (const n of D.families[fam] || []) if (D.incumbents[n] && tree(pk(n)).direct && !S.added.includes(n)) return n;
  return null;
}
function history(p) { return (p.versions || []).filter((x) => !x.pre).map((x) => ({ v: x.v, at: x.at, yanked: x.y })); }
function pinOf(p) { const t = tree(p); return p.tree?.yourPin || (t.pins?.length ? t.pins.at(-1) : null); }

// ------------------------------------------------------------------ the query, read
const STOP = new Set("a an the into to of for with my in and or some on by it this that i want need how do can me our your from as".split(" "));
const SYN = {
  parse: ["from_str", "from_slice", "parse", "from_reader", "decode", "deserialize", "load", "loads"],
  read: ["from_str", "from_slice", "from_reader", "read", "read_to_string", "load", "parse"],
  load: ["from_str", "load", "from_reader", "read_to_string"], decode: ["from_slice", "decode", "from_str", "decode_from_slice"],
  deserialize: ["from_str", "from_slice", "deserialize"],
  write: ["to_string", "to_vec", "to_writer", "write", "serialize", "to_string_pretty", "encode"], serialize: ["to_string", "to_vec", "serialize"],
  encode: ["to_vec", "encode", "to_string", "encode_to_vec"], print: ["to_string", "to_string_pretty", "Display"], dump: ["to_string", "to_writer"],
  get: ["get", "request", "call", "send"], fetch: ["get", "request", "send"], request: ["request", "get", "send", "call"], download: ["get", "call"],
  post: ["post", "send_json", "json"], send: ["send", "send_json", "call"],
  match: ["is_match", "find", "captures", "new"], find: ["find", "find_iter", "is_match"], search: ["find", "search", "is_match"], replace: ["replace", "replace_all"], compile: ["new", "compile"],
  spawn: ["spawn", "block_on", "run"], run: ["block_on", "run", "spawn"], block: ["block_on"], wait: ["block_on", "sleep", "join"],
  sleep: ["sleep", "after", "Timer"], timeout: ["timeout", "timeout_global"], timer: ["Timer", "after", "sleep"],
  error: ["Error", "Report", "Whatever", "Diagnostic"], fail: ["Error", "bail", "ensure"], context: ["context", "with_context", "Context", "wrap_err"], report: ["Report", "Diagnostic"],
  args: ["Parser", "Command", "Arguments", "from_env", "Arg"], arguments: ["Parser", "Command", "Arguments", "Arg"], flags: ["Arg", "contains", "Parser"],
  channel: ["channel", "unbounded", "bounded", "mpsc"], file: ["read_to_string", "File", "read", "write"], thread: ["spawn", "scope"],
  vec: ["SmallVec", "ArrayVec", "TinyVec"], stack: ["ArrayVec", "SmallVec", "TinyVec"], inline: ["SmallVec", "TinyVec", "ArrayVec"],
  env: ["var", "vars", "args"], time: ["Instant", "Duration", "now", "elapsed"],
};
const NOUN = {
  toml: ["toml"], json: ["json"], yaml: ["yaml"], xml: ["xml"], cbor: ["cbor"], msgpack: ["msgpack", "messagepack", "rmp"], ron: ["ron"],
  binary: ["binary", "bincode", "postcard", "compact"], regex: ["regex", "regular"], http: ["http", "https", "client"], url: ["http", "client"],
  cli: ["cli", "argument", "command-line", "arguments", "args"], errors: ["error", "error-handling"], async: ["async", "asynchronous", "runtime", "executor"],
  vector: ["vector", "vec", "stack", "small", "array"], parser: ["parser", "combinator", "parsing", "grammar", "peg"], grammar: ["peg", "grammar", "parser"],
  lookaround: ["look-around", "lookaround", "lookahead", "look-ahead"], backreferences: ["backreference", "backreferences"],
};
const NOUN_ALIAS = { lookahead: "lookaround", "look-around": "lookaround", backreference: "backreferences", yml: "yaml", regexp: "regex", pattern: "regex", web: "http", arg: "cli", command: "cli", error: "errors", runtime: "async", vec: "vector", combinator: "parser" };
const SHAPE_WORD = (w) => {
  const x = w.trim().toLowerCase().replace(/^&\s*(mut\s+)?/, "");
  if (["text", "str", "string", "&str", "strings"].includes(x)) return "text";
  if (["bytes", "[u8]", "&[u8]", "vec<u8>", "binary"].includes(x)) return "bytes";
  if (["t", "type", "struct", "your type", "a type", "any type", "yours"].includes(x)) return "your type";
  if (["path", "pathbuf", "file"].includes(x)) return "path";
  if (["reader", "read", "io"].includes(x)) return "reader";
  if (["number", "int", "integer", "usize", "i32", "u32", "i64", "u64", "f64"].includes(x)) return "number";
  if (["nothing", "()", "unit"].includes(x)) return "nothing";
  return x;
};
function readQuery(raw) {
  const q = (raw || "").trim();
  if (!q) return { mode: "empty" };
  if (/^[a-z_][\w]*::[\w:]+/i.test(q) && /[(!{<]/.test(q)) return { mode: "code", q };
  const like = q.match(/^like\s+(\S+)/i);
  if (like) return { mode: "like", q, x: like[1] };
  const sh = q.split(/\s*(?:->|→)\s*/);
  if (sh.length === 2) {
    const norm = (s) => {
      let fails = false, maybe = false; let x = s.trim().toLowerCase();
      if (/\s+or fails$/.test(x)) { fails = true; x = x.replace(/\s+or fails$/, ""); }
      if (/^maybe\s+/.test(x)) { maybe = true; x = x.replace(/^maybe\s+/, ""); }
      const disp = s.trim().replace(/\s+or fails$/i, "").replace(/^maybe\s+/i, "");
      return { base: SHAPE_WORD(x), fails, maybe, disp: ["text", "bytes", "your type", "path", "reader", "number", "nothing"].includes(SHAPE_WORD(x)) ? SHAPE_WORD(x) : disp };
    };
    return { mode: "shape", q, a: norm(sh[0]), b: norm(sh[1]) };
  }
  return { mode: "words", q, toks: q.toLowerCase().split(/[^a-z0-9_+#.-]+/).filter((t) => t && !STOP.has(t)) };
}

// ------------------------------------------------------------------ the index
let IDX = null;
function splitName(n) { return n.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase().split(/[_\W]+/).filter(Boolean); }
function buildIndex() {
  IDX = [];
  for (const [n, p] of Object.entries(D.packages)) {
    const text = [p.name, ...(p.keywords || []), ...(p.categories || []), p.desc || "", p.api?.crateDoc || ""].join(" ").replace(/https?:\/\/\S+/g, " ").toLowerCase();
    const items = (p.api?.items || []).map((it) => {
      const nm = lastSeg(it.p);
      const owner = it.p.split("::").at(-2);
      return { ...it, nm, owner, toks: splitName(nm), doc: (it.d || "").toLowerCase(), depth: it.p.split("::").length, pkg: n };
    });
    IDX.push({ n, p, text, items, names: new Set(items.filter((i) => i.depth <= 3 && !["variant"].includes(i.k)).map((i) => i.nm)) });
  }
}
function pkgScore(e, toks) {
  let s = 0;
  for (const t of toks) {
    const nn = NOUN[NOUN_ALIAS[t] || t] || [t];
    let best = 0;
    for (const x of nn) {
      if (e.p.name.toLowerCase().includes(x)) best = Math.max(best, 5);
      if ((e.p.keywords || []).some((k) => k.toLowerCase() === x)) best = Math.max(best, 4);
      if ((e.p.categories || []).some((c) => c.toLowerCase().includes(x))) best = Math.max(best, 2);
      if (new RegExp(`\\b${x.replace(/[.+#-]/g, "\\$&")}\\b`).test(e.text)) best = Math.max(best, 2);
    }
    s += best;
  }
  return s;
}
function nounsMissing(e, toks) {
  // a noun in the query ("toml") that the package doesn't speak excludes it ("parse toml" never answers str::parse)
  return toks.some((t) => {
    const key = NOUN_ALIAS[t] || t;
    if (!NOUN[key]) return false;
    return !NOUN[key].some((x) => e.p.name.toLowerCase().includes(x) || (e.p.keywords || []).some((k) => k.toLowerCase().includes(x)) || new RegExp(`\\b${x}\\b`).test(e.text));
  });
}
function itemScore(it, toks) {
  let s = 0, hit = 0;
  for (const t of toks) {
    const syn = SYN[t] || [];
    let b = 0;
    if (it.nm.toLowerCase() === t) b = SYN[t] ? 5.5 : 6;
    if (syn.includes(it.nm)) b = Math.max(b, 6 - syn.indexOf(it.nm) * 0.35);
    const isNoun = !!NOUN[NOUN_ALIAS[t] || t];
    if (it.toks.includes(t)) b = Math.max(b, 3);
    if (syn.some((x) => it.toks.includes(x.toLowerCase()))) b = Math.max(b, 2);
    if (it.owner && (it.owner.toLowerCase() === t || syn.includes(it.owner))) b = Math.max(b, 2);
    if (!isNoun && it.doc && new RegExp(`\\b${t.replace(/[.+#-]/g, "\\$&")}`).test(it.doc)) b += 0.5;
    if (SYN[t] && b >= 5 && ["function", "method", "macro", "derive"].includes(it.k)) b += 1;   // a verb wants something you can call
    if (b >= 2) hit++;
    s += b;
  }
  if (hit > 1) s *= 1 + 0.4 * (hit - 1);
  if (s > 0) {
    if (it.depth <= 2) s += ["function", "derive", "macro"].includes(it.k) ? 1.5 : 0.5;
    if (it.k === "function" || it.k === "derive") s += 0.6;
    if (it.k === "variant") s -= 2;
    if (it.dep) s -= 3;
  }
  return s;
}
function search(raw) {
  const r = readQuery(raw);
  if (r.mode === "empty") return { r, rows: [] };
  if (r.mode === "words") return words(r);
  if (r.mode === "shape") return shape(r);
  if (r.mode === "like") return like(r);
  return code(r);
}
function words(r) {
  const toks = r.toks;
  const rows = [];
  for (const e of IDX) {
    if (nounsMissing(e, toks)) continue;
    const ps = pkgScore(e, toks);
    let best = null, bs = 0;
    for (const it of e.items) {
      const s = itemScore(it, toks);
      if (s > bs || (s === bs && best && it.p.length < best.p.length)) { bs = s; best = it; }
    }
    const tot = ps + bs * 0.8;
    if (ps < 2 && bs < 6.5) continue;
    if (!best) best = e.items.find((i) => i.depth <= 2 && ["struct", "enum", "trait", "function", "derive"].includes(i.k)) || e.items[0];
    rows.push({ n: e.n, it: best, score: tot + (e.p.stdlib ? 0.5 : 0), hl: toks });
  }
  rows.sort((a, b) => b.score - a.score);
  const top = rows[0]?.score || 0;
  return finish(r, rows.filter((x) => x.score >= top * 0.6).slice(0, 8));
}
function shape(r) {
  const rows = [];
  const want = r.b.base;
  for (const e of IDX) {
    let best = null, bs = 0;
    for (const it of e.items) {
      if (!it.sh) continue;
      const ins = it.sh.in.map((x) => x.toLowerCase());
      let out = it.sh.out.toLowerCase(), fails = false, maybe = false;
      if (/ or fails$/.test(out)) { fails = true; out = out.replace(/ or fails$/, ""); }
      if (/^maybe /.test(out)) { maybe = true; out = out.replace(/^maybe /, ""); }
      // a generic `T: Deserialize` can be the Value you asked for, when this package's Value implements Deserialize
      const deser = out === "your type" && want !== "your type" && (e.p.api?.deser || []).find((t) => t.toLowerCase() === want);
      if (out !== want && !deser) continue;
      if (!ins.some((x) => x === r.a.base)) continue;
      let s = 4 + (r.b.fails === fails ? 0.6 : 0) + (r.b.maybe === maybe ? 0.8 : -1) - 0.6 * (ins.length - 1) - (deser ? 0.4 : 0);
      if (it.depth <= 2) s += 1; if (it.k === "function") s += 0.5; if (it.dep) s -= 3;
      if (ins[0] === r.a.base) s += 0.5;
      if (s > bs) { bs = s; best = deser ? { ...it, turbofish: deser, sh: { in: it.sh.in, out: it.sh.out.replace("your type", deser) } } : it; }
    }
    if (best) rows.push({ n: e.n, it: best, score: bs + (e.p.stdlib ? 0.3 : 0), shape: true });
  }
  rows.sort((a, b) => b.score - a.score);
  return finish(r, rows.slice(0, 8));
}
const NOISE = new Set("new default fmt clone from into eq hash cmp partial_cmp drop deref deref_mut as_ref borrow Error Result Ok Err iter len is_empty get get_mut serialize deserialize visit_str build builder with_capacity capacity push pop insert remove clear unicode Split Iter IntoIter Builder Config Options is msg".split(" "));
function like(r) {
  const x = r.x.replace(/-/g, "_").toLowerCase();
  const me = IDX.find((e) => ident(e.n).toLowerCase() === x);
  if (!me) return { r, rows: [], missing: r.x };
  const A = new Set([...me.names].filter((n) => !NOISE.has(n)));
  const rows = [];
  for (const e of IDX) {
    if (e === me || e.p.stdlib) continue;
    const B = new Set([...e.names].filter((n) => !NOISE.has(n)));
    const shared = [...A].filter((n) => B.has(n));
    const kw = (me.p.keywords || []).filter((k) => (e.p.keywords || []).includes(k)).length;
    const ct = (me.p.categories || []).filter((k) => (e.p.categories || []).includes(k)).length;
    const s = shared.length / Math.sqrt(Math.max(1, A.size) * Math.max(1, B.size)) * 4 + kw * 0.25 + ct * 0.2 + (e.p.family === me.p.family ? 0.6 : 0);
    if (!shared.length && s < 0.6) continue;
    // characteristic shared items: functions first, then types
    const rank = (n) => { const it = e.items.find((i) => i.nm === n && i.depth <= 3); return it ? ({ function: 0, enum: 1, struct: 1, macro: 2, trait: 2 }[it.k] ?? 3) : 5; };
    shared.sort((a, b) => rank(a) - rank(b) || a.length - b.length);
    const it = e.items.filter((i) => i.nm === shared[0]).sort((a, b) => a.depth - b.depth)[0] || e.items[0];
    rows.push({ n: e.n, it, score: s, shared });
  }
  rows.sort((a, b) => b.score - a.score);
  const top = rows[0]?.score || 0;
  return finish(r, rows.filter((x) => x.score >= top * 0.35).slice(0, 7), me.n);
}
function code(r) {
  const m = r.q.match(/([A-Za-z_]\w*(?:::[A-Za-z_]\w*)+)/);
  const path = m[1];
  const crate = path.split("::")[0];
  const me = IDX.find((e) => ident(e.n) === crate);
  if (!me) return { r, rows: [], missing: crate };
  const segs = path.split("::");
  const it0 = me.items.find((i) => i.p === path) || me.items.filter((i) => i.nm === segs.at(-1)).sort((a, b) => a.depth - b.depth)[0];
  if (!it0) return { r, rows: [], missing: path };
  const sh0 = JSON.stringify(it0.sh || null);
  const rows = [{ n: me.n, it: it0, score: 99, snippet: r.q, change: 0, self: true }];
  for (const e of IDX) {
    if (e === me || e.p.stdlib) continue;
    let best = null, bc = 9;
    for (const it of e.items) {
      if (!["function", "method", "macro"].includes(it.k)) continue;
      const sameName = it.nm === it0.nm, sameShape = JSON.stringify(it.sh || null) === sh0 && sh0 !== "null";
      const c = sameName && sameShape ? 1 : sameShape && (e.p.family === me.p.family) ? 2 : sameName && e.p.family === me.p.family ? 3 : 9;
      if (c < bc || (c === bc && best && it.depth < best.depth)) { bc = c; best = it; }
    }
    if (best && bc < 9) {
      const snippet = r.q.replace(path, best.p);
      rows.push({ n: e.n, it: best, score: 10 - bc, snippet, change: bc, from: path });
    }
  }
  rows.sort((a, b) => b.score - a.score || (a.it.p.length - b.it.p.length));
  return finish(r, rows.slice(0, 8), me.n);
}
function finish(r, rows, anchor) {
  // "you need no new package": std or something you already hold answers first
  let verdict = null;
  if (rows.length && (r.mode === "words" || r.mode === "shape")) {
    const top = rows[0].score;
    const std = rows.find((x) => pk(x.n).stdlib && x.score >= top * 0.72);
    const mine = rows.find((x) => tree(pk(x.n)).direct && x.score >= top * 0.72);
    const pick = std || mine;
    if (pick) { verdict = { kind: std ? "std" : "yours", row: pick }; rows = [pick, ...rows.filter((x) => x !== pick)]; }
  }
  const fam = rows.find((x) => !pk(x.n).stdlib)?.n;
  const named = r.mode === "like" || (r.mode === "words" && r.toks.some((t) => NOUN[NOUN_ALIAS[t] || t]));
  const cousins = named && fam ? (D.cousins[famOf(fam)] || []) : [];
  return { r, rows, verdict, cousins, anchor };
}

// ------------------------------------------------------------------ small renderers
function hlItem(it, toks) {
  const segs = it.p.split("::");
  const shown = segs.length > 2 ? segs.slice(-2) : segs.slice(-1);
  const nm = shown.at(-1);
  let nmH = esc(nm);
  if (toks?.length) {
    const hit = toks.map((t) => [t, ...(SYN[t] || [])]).flat().find((t) => nm.toLowerCase() === t.toLowerCase() || nm.toLowerCase().includes(t.toLowerCase()));
    if (hit) { const i = nm.toLowerCase().indexOf(hit.toLowerCase()); nmH = esc(nm.slice(0, i)) + `<u>${esc(nm.slice(i, i + hit.length))}</u>` + esc(nm.slice(i + hit.length)); }
  }
  const own = shown.length > 1 ? `<span class="own">${esc(shown[0])}::</span>` : "";
  const call = (it.turbofish ? esc("::<" + it.turbofish + ">") : "") + (["function", "method"].includes(it.k) ? "(…)" : it.k === "macro" ? "!" : "");
  return own + (it.k === "derive" ? `<span class="own">#[derive(</span>${nmH}<span class="own">)]</span>` : nmH + `<span class="own">${call}</span>`);
}
function shapeWords(sh) {
  if (!sh) return "";
  const ins = sh.in.length ? sh.in.join(", ") : "";
  return `(${esc(ins)}) → <b>${esc(sh.out)}</b>`;
}
function sigHtml(s) { return esc(s || "").replace(/(-&gt;|where|&lt;|&gt;)/g, '<span class="p">$1</span>'); }
function seam(stage) {
  return `<span class="seam4">${[0, 1, 2, 3].map((i) => `<i class="${i < stage ? "done" : i === stage ? "now" : ""}"></i>`).join("")}</span>`;
}

// ------------------------------------------------------------------ frame
function titlebar() {
  let k = ico("search", "s14"), nm = "find", path = "a package for what you need";
  if (S.view === "compare") { k = kind("package"); nm = "compare"; path = S.compare.join(" · "); }
  if (S.view === "tree") { k = kind("package"); nm = "backend"; path = "your tree"; }
  return `<header class="titlebar v2"><span class="lights live"><i></i><i></i><i></i></span>
    <button class="ibtn on" aria-label="Toggle shelf"><svg class="ico" viewBox="0 0 24 24"><path class="f" d="M7 4h2v16H3V8z"></path><path d="M7 4h14v12l-4 4H3V8z"></path><path d="M9 4v16"></path></svg></button>
    <div class="thread"><button class="here askf" data-go="find">${k}<span class="nm">${esc(nm)}</span><span class="path">${esc(path)}</span></button></div>
    <button class="ibtn" aria-label="Library" data-go="tree">${ico("layers", "s16")}</button>
    <button class="ibtn" aria-label="Inbox">${ico("inbox", "s16")}</button></header>`;
}
const FAM_ROLE = { toml: "formats", formats: "formats", http: "network", async: "concurrency", errors: "errors", inline: "memory", regex: "other", cli: "other", parsers: "formats" };
function roleOf(n) {
  const d = directOf(n);
  if (d) return d.role;
  return FAM_ROLE[famOf(n)] || "other";
}
function shelf(w) {
  if (w < 640) return "";
  if (w < 900) {
    return `<nav class="ckspine"><span class="sp on" data-go="tree" title="Library">${kind("package")}</span><span class="sp">${ico("search", "s14")}</span><span class="sp">${ico("layers", "s14")}</span></nav>`;
  }
  const openRole = S.shelfRole || "formats";
  const rows = D.roles.filter((r) => r.direct.length).map((r) => {
    const names = [...r.direct, ...S.added.filter((a) => roleOf(a) === r.id && !r.direct.includes(a))];
    const open = r.id === openRole;
    const head = `<div class="lib-role${open ? " open" : ""}" data-role="${r.id}"><svg class="tw" viewBox="0 0 10 10"><path d="M3 1.5 7 5 3 8.5" fill="none" stroke="currentColor" stroke-width="1.4"/></svg><span class="n">${esc(r.label)}</span><span class="c">${names.length}</span></div>`;
    if (!open) return head;
    const kids = names.map((n) => {
      const p = pk(n) || { name: n, eco: "rust" };
      const isNew = S.added.includes(n);
      return `<div class="lib-pk${(S.hover === n || S.selName === n) && !isNew ? " cur" : ""}${isNew ? " new" : ""}" data-pk="${esc(n)}"><span class="st">${ecosystemMark("crates", { word: false, px: 12 })}</span><span class="n">${esc(n)}</span>${isNew ? `<span class="seam">${seam(-1)}</span>` : ""}</div>`;
    }).join("");
    return head + `<div class="lib-kids" data-role-kids="${r.id}">${kids}</div>`;
  }).join("");
  const pr = D.project;
  return `<aside class="cshelf"><div class="cup"><svg class="ico s12" viewBox="0 0 24 24" style="transform:rotate(180deg)"><path d="m9 6 6 6-6 6"></path></svg>backend</div>
    <div class="cbook">${gem("package", 28)}<div class="col" style="gap:1px;min-width:0"><span class="bn">Library</span><span class="bv">${pr.direct + S.added.length} direct · <span class="odo-in">${fmt(pr.inTree + S.added.reduce((a, n) => a + addCount(pk(n)), 0))}</span></span></div></div>
    <div class="cfilter">${ico("filter", "s12")}<span>Filter</span></div><div class="crows">${rows}</div></aside>`;
}
function addCount(p) { return p ? addsOf(p).length + (p.cost.addsSelf ? 1 : 0) : 0; }
function status() {
  const addr = S.view === "find" ? `nudox://find?q=${S.q}` : S.view === "compare" ? `nudox://compare/${S.compare.join(",")}` : "nudox://backend/tree";
  const n = D.project.inTree + S.added.reduce((a, x) => a + addCount(pk(x)), 0);
  return `<footer class="cstatus"><span class="addr">${esc(addr)}</span><span class="grow"></span><span class="tc" data-odo><b class="odo-n">${fmt(n)}</b> in your tree</span></footer>`;
}

// ------------------------------------------------------------------ find
function findView(w) {
  const res = S.results = search(S.q);
  const read = readLine(res);
  const hints = res.r.mode === "empty" ? `<div class="hints">
      <div class="hint" data-q="parse toml"><span class="q">parse toml</span><span class="w">a need, in words</span></div>
      <div class="hint" data-q="text -> Value"><span class="q">text → Value</span><span class="w">a shape: what you have → what you need</span></div>
      <div class="hint" data-q="like serde_json"><span class="q">like serde_json</span><span class="w">a package you know, to find its kin</span></div>
      <div class="hint" data-q="serde_json::from_str::<Config>(&s)?"><span class="q">serde_json::from_str::&lt;Config&gt;(&amp;s)?</span><span class="w">or paste a line of code</span></div></div>` : "";
  const hero = `<div class="bq"><div class="qrow">${ico("search", "s18")}<input id="qin" spellcheck="false" autocomplete="off" placeholder="What do you need?" value="${esc(S.q)}"></div><div class="read">${read}</div></div>`;
  const body = res.r.mode === "empty" ? hints : resultsHtml(res, w);
  const wide = w >= 1100;
  return `<div class="bf">${hero}<div class="fgrid"><div class="rs" id="rs">${body}</div>${wide ? `<div class="jcol" id="jcol"></div>` : ""}</div></div>`;
}
function readLine(res) {
  const r = res.r;
  if (r.mode === "empty") return "";
  if (r.mode === "shape") return `read as a shape: you have <span class="shw">${esc(r.a.disp)}</span>, you need <span class="shw">${esc((r.b.maybe ? "maybe " : "") + r.b.disp + (r.b.fails ? " or fails" : ""))}</span>`;
  if (r.mode === "like") return res.missing ? `${esc(res.missing)} isn't indexed on this machine` : `read as kin of <code>${esc(res.anchor)}</code>: shared items, not shared keywords`;
  if (r.mode === "code") return res.missing ? `<code>${esc(res.missing)}</code> isn't indexed on this machine` : `read as code: the same call in other packages, least change first`;
  return res.rows.length ? "" : "nothing answers that yet — try a shape, like <code>text -> Value</code>";
}
function rowHtml(x, i, res) {
  const p = pk(x.n);
  const rs = res.r.mode === "like" ? { t: x.shared.length ? `shares ${list(x.shared.slice(0, 2))}${x.shared.length > 2 ? ` +${x.shared.length - 2}` : ""}` : "the same job, by its categories" }
    : res.r.mode === "code" ? (x.self ? reason(p) : { t: x.change === 1 ? (x.it.p.split("::").length === x.from.split("::").length ? "changes one word" : "changes the path") : x.change === 2 ? "renames the call" : "same name, other shape" })
    : reason(p);
  const t = tree(p);
  let item;
  if (res.r.mode === "code") item = `<span class="code">${codeDiff(x)}</span>`;
  else item = `<span class="it">${hlItem(x.it, x.hl)}${x.shape ? `<span class="shw">${shapeWords(x.it.sh)}</span>` : ""}</span>`;
  const sel = S.selName === x.n ? " sel" : "";
  return `<div class="r${t.direct || t.std ? " yours" : ""}${sel}" data-n="${esc(x.n)}" data-i="${i}">${emark(p, { id: "r-" + x.n })}<span class="pn">${esc(p.stdlib ? "std" : x.n)}</span>${item}`
    + `<span class="why${rs.m ? " m" : ""}"${rs.w ? ' style="color:var(--amber)"' : ""}>${esc(rs.t)}</span>${keysHtml("A", "C", "↵")}</div>`;
}
function codeDiff(x) {
  if (!x.from) return esc(x.snippet);
  const i = x.snippet.indexOf(x.it.p);
  const a = x.from.split("::"), b = x.it.p.split("::");
  const segs = b.map((seg, k) => { const j = a.length - (b.length - k); return j >= 0 && a[j] === seg ? esc(seg) : `<ins>${esc(seg)}</ins>`; }).join("::");
  return esc(x.snippet.slice(0, i)) + segs + esc(x.snippet.slice(i + x.it.p.length));
}
function resultsHtml(res, w) {
  if (!res.rows.length) return "";
  let html = "";
  let rows = res.rows;
  if (res.verdict) {
    const v = res.verdict, p = pk(v.row.n), it = v.row.it;
    const call = `<code>${esc(tail2(it.p))}${it.turbofish ? esc(`::<${it.turbofish}>`) : ""}</code>`;
    const say = v.kind === "std"
      ? `<b>No package needed.</b> The standard library does this: ${call}${it.sh ? ` takes ${esc(it.sh.in.join(", ") || "nothing")} and gives ${esc(it.sh.out)}` : ""}.`
      : (() => { const u = D.incumbents[p.name]?.uses.find((x) => x.path === it.p || lastSeg(x.path) === lastSeg(it.p) && x.path.split("::").length === 2); return u
        ? `<b>You already have this.</b> ${call} from <span class="m">${esc(p.name)}</span> — your code calls it in ${plural(u.count, "place")}.`
        : `<b>You already have this.</b> ${call} from <span class="m">${esc(p.name)}</span>, which your code uses in ${plural(tree(p).uses || 0, "place")}.`; })();
    html += `<div class="verdict"><div class="say">${say}</div>${rowHtml(v.row, 0, res)}</div>`;
    rows = rows.slice(1);
    if (rows.length) html += `<div class="gh">or a package</div>`;
  }
  html += rows.map((x, i) => rowHtml(x, i + (res.verdict ? 1 : 0), res)).join("");
  if (res.cousins?.length) {
    const cs = [...res.cousins].sort((a, b) => (b.stdlib ? 1 : 0) - (a.stdlib ? 1 : 0));
    const shown = S.allCousins ? cs : cs.slice(0, 3);
    html += `<div class="gh">the same idea elsewhere</div>` + shown.map((c) => {
      const cm = ecosystemMark(ECO[c.eco] || c.eco, { word: false, pkg: c.name, say: c.say, id: "c-" + c.name });
      return `<div class="r cousin" data-cousin="${esc(c.name)}">${cm}<span class="pn">${esc(c.name)}</span><span class="it">${esc(c.item)}</span><span class="why">${esc(c.stdlib ? "in the standard library" : c.say)}</span></div>`;
    }).join("") + (cs.length > shown.length ? `<div class="fold-cousins" data-all-cousins>and ${word(cs.length - shown.length)} more, in ${esc(list([...new Set(cs.slice(shown.length).map((c) => ({ npm: "npm", pypi: "PyPI", go: "Go", java: "Java", csharp: ".NET", cpp: "C++" })[c.eco]))]))}</div>` : "");
  }
  const lit = res.rows.length;
  if (res.r.mode !== "code") html += `<div class="foot"><a href="../Graph.html?find=${encodeURIComponent(S.q)}">${plural(lit, "answer")} lit in the graph ›</a></div>`;
  return html;
}

// ------------------------------------------------------------------ judge
function judgeHtml(n, it) {
  const p = pk(n);
  if (!p) return "";
  const t = tree(p);
  const hist = history(p);
  const pin = pinOf(p);
  const also = (p.tree.pins || []).filter((v) => v !== pin).map((v) => ({ v, via: (D.transitive.find((x) => x.n === n && x.v === v)?.why || []).slice(1, -1) }));
  let care, cls = "";
  const adv = advisory(p);
  if (t.std) { care = `<b>No package.</b> It ships with Rust.`; cls = " m"; }
  else if (t.added) { care = `<b>Yours now.</b> Added to backend just now.`; cls = " m"; }
  else if (t.direct) {
    const by = Object.keys(t.usesBy || {});
    care = `<b>Yours</b> · ${t.uses ? plural(t.uses, "place") : "used"}${by.length ? ` in ${esc(list(by.slice(0, 3)))}${by.length > 3 ? ` +${by.length - 3}` : ""}` : ""}`; cls = " m";
  } else if (t.inLock) {
    const via = p.tree.pulledBy || [];
    care = `<b>Already in your tree</b>${via.length ? `, through ${esc(list(via.slice(0, 2)))}` : ""}. Adopting it adds nothing.`; cls = " m";
  } else {
    const a = addsOf(p);
    care = a.length ? `<b>Adds ${plural(a.length + (p.cost.addsSelf ? 1 : 0), "crate")}</b> you don't have: <code>${esc(n)}</code>, ${a.slice(0, 4).map((x) => `<code>${esc(x)}</code>`).join(", ")}${a.length > 4 ? ` <span class="dim">+${a.length - 4}</span>` : ""}`
      : `<b>Adds only itself.</b> <span class="dim">Everything it needs is already in your tree.</span>`;
    if (p.cost.second?.length) care += ` <span class="dim">· a second ${esc(p.cost.second.map((x) => x.split(" ")[0]).join(", "))}</span>`;
  }
  const warn = adv ? `<div class="care w"><b>${esc(cap(adv.informational || "advisory"))}</b> · ${esc(adv.id)} · ${esc(adv.title)}</div>` : deprecated(p) ? `<div class="care w"><b>Deprecated</b> · its own version says so: <code>${esc(p.latest)}</code></div>` : "";
  const say = it?.d || p.desc || "";
  const sig = it ? `<div class="jsig">${sigHtml(it.s)}</div>` : "";
  // switching from what you use (adoption preview in one line)
  let sw = "";
  const inc = !t.std && incumbentFor(p.family);
  if (inc && inc !== n) {
    const I = D.incumbents[inc];
    const cov = I.uses.filter((u) => u.cells[n]).length;
    const places = I.uses.filter((u) => u.cells[n]).reduce((a, u) => a + u.count, 0);
    sw = `<div class="sw">Switching from <b>${esc(inc)}</b>: ${cov === I.uses.length ? "an equivalent for everything you use" : `covers ${word(cov)} of the ${word(I.uses.length)} things you use (${plural(places, "place")} of ${fmt(I.total)})`} · <a data-compare="${esc([inc, n].join(","))}">see your code ›</a></div>`;
  }
  // ⌥: churn, size, needs, dependents, advisories, downloads
  const st = p.stability || {}, cl = st.claimed, me = st.measured;
  const f = [];
  if (cl) {
    const lb = cl.lastBreaking ? ` · last <b>${esc(short(cl.lastBreaking.v))}</b>` : "";
    f.push(["churn", `${cl.breakingPerYear ? `<b>${cl.breakingPerYear}</b> breaking releases a year` : "<b>no</b> breaking release in three years"}${lb} <span class="dim">· by version numbers</span>`]);
  }
  if (me) f.push(["measured", `<span class="notch"></span>${me.slips ? `<b>${me.slips}</b> broke API without a major bump` : "every break came with a major bump"} <span class="dim">· ${plural(me.pairs.length, "real API diff")}</span>`]);
  else if (!t.std) f.push(["measured", `<span class="dim">API diffs not measured yet</span>`]);
  if (p.api?.size != null) f.push(["API", `<b>${fmt(p.api.size)}</b> public items${p.srcVersion && p.srcVersion !== p.latest ? ` <span class="dim">· read at ${esc(short(p.srcVersion))}</span>` : ""}`]);
  if (!t.std) f.push(["needs Rust", p.msrv ? `<b>${esc(p.msrv)}</b>` : `<span class="dim">not declared</span>`]);
  if (p.revdeps) f.push(["depended on", `by <b>${fmt(p.revdeps.count)}</b> of ${fmt(p.revdeps.of)} crates on this machine`]);
  if (!t.std) f.push(["advisories", p.advisory ? (p.advisory.affecting.length ? `<b>${p.advisory.affecting.length}</b> affect ${esc(short(p.advisory.version))}` : `none affect ${esc(short(p.advisory.version))} <span class="dim">· RustSec, ${esc(p.advisory.checked)}</span>`) : `<span class="dim">not measured</span>`]);
  if (p.downloads) f.push(["downloads", `<b>${(p.downloads.recent / 1e6).toFixed(p.downloads.recent > 1e7 ? 0 : 1)}M</b> in 90 days <span class="dim">· crates.io, ${esc(p.downloads.fetched)}</span>`]);
  const xr = `<div class="xr">${f.map(([k, v]) => `<div class="f"><span class="xk">${k}</span><span>${v}</span></div>`).join("")}</div>`;
  // ⌥: two of your own lines, rewritten
  let pv = "";
  if (inc && inc !== n) {
    const sites = D.incumbents[inc].sites.filter((s) => s.rw[n]).slice(0, 2);
    pv = `<div class="pv apv">${sites.map((s) => pairHtml(s, n, inc)).join("")}</div>`;
  }
  const dd = S.view === "tree" ? directOf(n) : null;
  const roleLine = dd ? `<div class="sw">In <b>${esc(D.roles.find((r) => r.id === dd.role)?.label || dd.role)}</b> because ${esc(dd.roleWhy)}</div>` : "";
  const comb = hist.length ? `<div class="jcomb">${versionComb(hist, pin, { name: n, width: 340, id: "j-ver", also })}</div>` : "";
  const lic = p.license?.spdx !== undefined ? licenseMark(p.license.spdx, D.project.license, { project: "backend", word: false, id: "j-lic" }) : "";
  const action = t.direct || t.std ? `<span class="open">open ${esc(p.stdlib ? "std" : n)} ›</span>` : `<span class="jadd" data-add="${esc(n)}"><i class="plus"></i>add to backend</span>`;
  return `<div class="jwrap"><div class="judge" data-judge="${esc(n)}"><div class="jh">${t.std ? gem("module", 30) : `<span class="jgem">${gem("package", 30)}</span>`}<div class="col" style="gap:2px;min-width:0"><span class="nm">${esc(p.stdlib ? "std" : n)}</span>`
    + `<span class="marks">${emark(p, { id: "j-eco" })}${lic}</span></div></div>`
    + `<div class="care${cls}">${care}</div>${warn}${comb}<div class="say">${esc(say)}</div>${sig}${sw}${roleLine}${xr}${pv}`
    + `<div class="jf">${action}<span class="grow"></span><span class="kf">${t.direct || t.std ? "" : `${keysHtml("A")}<span>add</span>`}${keysHtml("C")}<span>compare</span>${keysHtml("⌥")}<span>more</span></span></div></div></div>`;
}
function placeJudge() {
  let col = $("#jcol");
  if (col && getComputedStyle(col).display === "none") col = null;
  const res = S.results;
  if (!res || !res.rows.length) { if (col) col.innerHTML = ""; $$(".jin").forEach((e) => e.remove()); return; }
  const selRow = $(`.r[data-n="${CSS.escape(S.selName || "")}"]`) || $(".r[data-n]");
  if (!selRow) return;
  const n = selRow.dataset.n;
  const x = res.rows.find((r) => r.n === n);
  const html = judgeHtml(n, x?.it);
  $$(".r.sel").forEach((r) => r.classList.remove("sel"));
  selRow.classList.add("sel");
  if (col) {
    col.innerHTML = `<div class="jcard">${html}</div>`;
    const top = selRow.offsetTop - 8;
    const card = $(".jcard", col);
    const maxTop = Math.max(0, top);
    card.style.transform = `translateY(${maxTop}px)`;
  } else {
    $$(".jin").forEach((e) => e.remove());
    selRow.insertAdjacentHTML("afterend", `<div class="jin">${html}</div>`);
  }
}

// ------------------------------------------------------------------ compare
function compareView(w) {
  const set = S.compare.filter((n) => pk(n)).slice(0, 4);
  const fam = famOf(set[0]);
  const inc = set.find((n) => D.incumbents[n] && tree(pk(n)).direct) || null;
  const cols = inc ? [inc, ...set.filter((n) => n !== inc)] : set;
  const others = cols.filter((n) => n !== inc);
  const I = inc ? D.incumbents[inc] : null;
  // the verdict
  let lede = "", sub = "";
  if (I) {
    let first = true;
    const parts = others.map((o) => {
      const cov = I.uses.filter((u) => u.cells[o]);
      const places = cov.reduce((a, u) => a + u.count, 0);
      const diff = cov.filter((u) => u.cells[o].same === false).map((u) => lastSeg(u.path));
      const miss = I.uses.filter((u) => !u.cells[o]).map((u) => tail2(u.path));
      if (cov.length === I.uses.length) { first = false; } if (cov.length === I.uses.length) return `<b>${esc(o)}</b> has an equivalent for ${I.uses.length === 1 ? "the one thing" : `all ${word(I.uses.length)} things`} your code does with ${esc(inc)}${diff.length ? `; ${list(diff.map((d) => `<code>${esc(d)}</code>`))} ${diff.length === 1 ? "reads" : "read"} differently` : ""}.`;
      if (!cov.length) { first = false; return `<b>${esc(o)}</b> has no equivalent for anything your code does with ${esc(inc)}.`; }
      const what = first ? ` things your code does with ${esc(inc)}` : ""; first = false;
      if (miss.length <= 2) return `<b>${esc(o)}</b> covers ${word(cov.length)} of the ${word(I.uses.length)}${what}; ${list(miss.map((d) => `<code>${esc(d)}</code>`))} ${miss.length === 1 ? "has" : "have"} no equivalent.`;
      return `<b>${esc(o)}</b> covers ${word(cov.length)} of the ${word(I.uses.length)}${what} — ${fmt(places)} of your ${fmt(I.total)} places.`;
    });
    const none = others.filter((o) => !I.uses.some((u) => u.cells[o]));
    if (none.length > 1) {
      const only = I.uses.length === 1 ? `<code>${esc(I.uses[0].k === "derive" ? `#[derive(${lastSeg(I.uses[0].path)})]` : tail2(I.uses[0].path))}</code>, which your code uses in ${plural(I.total, "place")}` : `anything your code does with ${esc(inc)}`;
      const rest = parts.filter((_, i) => !none.includes(others[i]));
      lede = `None of ${list(none.map((o) => `<b>${esc(o)}</b>`), "or")} has an equivalent for ${only}; they do the job another way, below.` + (rest.length ? " " + rest.join(" ") : "");
    } else lede = parts.join(" ");
    const free = others.filter((o) => tree(pk(o)).inLock || tree(pk(o)).direct), paid = others.filter((o) => !free.includes(o));
    const costs = [...(free.length ? [`nothing for ${list(free.map((o) => `<b>${esc(o)}</b>`))}, already in your tree`] : []),
      ...paid.map((o) => `${word(addCount(pk(o)))} ${addCount(pk(o)) === 1 ? "crate" : "crates"} for <b>${esc(o)}</b>`)];
    sub = `You use <b>${esc(inc)}</b> in ${plural(I.total, "place")}. Moving costs ${costs.join("; ")}.`;
  } else {
    const caps = (D.compare[fam]?.caps || []).filter((c) => cols.some((n) => c.cells[n]));
    const counts = cols.map((n) => [n, caps.filter((c) => c.cells[n]).length]).sort((a, b) => b[1] - a[1]);
    lede = counts.map(([n, k], i) => `<b>${esc(n)}</b> ${i === 0 ? `does ${word(k)} of the ${word(caps.length)} things` : `does ${word(k)}`}`).join(", ") + ".";
    const costs = cols.map((n) => { const t = tree(pk(n)); return t.direct ? `<b>${esc(n)}</b> is yours` : t.inLock ? `<b>${esc(n)}</b> is already in your tree` : `<b>${esc(n)}</b> adds ${plural(addCount(pk(n)), "crate")}`; });
    sub = `In this project: ${list(costs)}.`;
  }
  const readerW = w - (w < 640 ? 0 : w < 900 ? 42 : Math.min(264, Math.max(220, w * 0.18)));
  const slot = cols.length < 4 && readerW >= 900;
  const N = cols.length + (slot ? 1 : 0);
  let g = `<div class="corner"></div>`;
  for (const n of cols) {
    const p = pk(n), t = tree(p);
    const yo = n === inc ? `<span class="yo m">yours · ${plural(I.total, "place")}</span>` : t.direct ? `<span class="yo m">yours</span>` : t.inLock ? `<span class="yo m">already in your tree</span>` : `<span class="yo">adds ${plural(addCount(p), "crate")}</span>`;
    g += `<div class="c hd${n === inc ? " inc" : ""}${S.hover === n ? " hov" : ""}" data-col="${esc(n)}"><span class="t">${emark(p, { id: "h-" + n })}<span class="pn">${esc(n)}</span><span class="x" data-drop="${esc(n)}">×</span></span>${yo}`
      + `${versionComb(history(p), pinOf(p), { name: n, width: 220, id: "cv-" + n, latest: false })}</div>`;
  }
  if (slot) g += `<div class="c ghost" data-addcol><span>+ ${cols.length === 3 ? "a fourth" : "another"}</span></div>`;
  const cell = (c, n, isInc) => {
    if (!c) return `<div class="c cell none" data-col="${esc(n)}"><span class="nm">none</span></div>`;
    if (c.fact) return `<div class="c cell cf" data-col="${esc(n)}"><span class="nm">${c.fact}</span></div>`;
    const segs = c.path.split("::");
    const nm = segs.length > 2 ? `<span class="own">${esc(segs.slice(1, -1).join("::"))}::</span>${esc(segs.at(-1))}` : esc(segs.at(-1));
    return `<div class="c cell${isInc ? " inc" : ""}${c.same === false ? " diff" : ""}" data-col="${esc(n)}" title="${esc(c.sig || "")}"><span class="nm">${nm}</span><span class="sg">${esc(c.sig || "")}${c.same === false ? ` — reads differently: ${esc(shapeWords(c.sh).replace(/<\/?b>/g, ""))}` : ""}</span></div>`;
  };
  let ri = 0;
  const row = (label, cells) => { ri++; return `<div class="c lab" data-row="${ri}">${label}</div>${cells}${slot ? `<div class="c" data-row="${ri}"></div>` : ""}`; };
  if (I) {
    g += `<div class="band"><b>What your code does with ${esc(inc)} today</b><span>${plural(I.uses.length, "thing")} · ${plural(I.total, "place")}</span></div>`;
    const shown = I.uses.slice(0, S.moreUses ? 24 : 9);
    for (const u of shown) {
      const own = pk(inc).api.items.find((it) => it.p === u.path);
      g += row(`<code title="${esc(own?.s || u.sig || "")}">${esc(tail2(u.path))}</code>`,
        cols.map((n) => n === inc ? `<div class="c cell cf inc" data-col="${esc(n)}"><span class="nm"><span class="dim">${plural(u.count, "place")}${u.files > 1 ? `<span class="fl"> · ${u.files} files</span>` : ""}</span></span><span class="sg">${esc(own?.s || u.sig || "")}</span></div>` : cell(u.cells[n], n)).join(""));
    }
    if (I.uses.length > shown.length) g += `<div class="fold-more" data-more-uses>and ${plural(I.uses.length - shown.length, "more thing")} your code does</div>`;
  }
  const caps = (D.compare[fam]?.caps || []).filter((c) => cols.some((n) => c.cells[n]));
  if (caps.length) {
    g += `<div class="band"><b>${I ? "What else it can do" : "What you can do"}</b><span>${I ? "found in each package's API" : "rows are capabilities; each cell is the item that does it"}</span></div>`;
    for (const c of caps) g += row(esc(c.label), cols.map((n) => cell(c.cells[n], n, n === inc)).join(""));
  }
  // what it costs you
  g += `<div class="band"><b>What it costs you</b><span>this project, this machine</span></div>`;
  const fact = (html) => ({ fact: html });
  g += row("adds to your tree", cols.map((n) => {
    const p = pk(n), t = tree(p), a = addsOf(p);
    return cell(fact(t.direct ? `<span class="m">nothing · yours</span>` : t.inLock ? `<span class="m">nothing · already here</span>` : a.length ? `${plural(a.length + 1, "crate")} <span class="dim">· ${esc(list([n, ...a].slice(0, 3)))}${a.length > 2 ? "…" : ""}</span>` : `only itself`), n);
  }).join(""));
  g += row("license", cols.map((n) => `<div class="c cell cf lic" data-col="${esc(n)}"><span class="nm">${licenseMark(pk(n).license.spdx, D.project.license, { project: "backend", id: "cl-" + n })}</span></div>`).join(""));
  g += row("breaking releases", cols.map((n) => {
    const s = pk(n).stability, cl = s.claimed, me = s.measured;
    return cell(fact(cl ? `${cl.breakingPerYear ? `${cl.breakingPerYear} a year` : "none in three years"}${me ? ` <span class="dim">· ${me.slips ? me.slips + " slipped" : "0 slips"}, measured</span>` : ` <span class="dim">· claimed</span>`}` : `<span class="dim">not measured</span>`), n);
  }).join(""));
  g += row("public API", cols.map((n) => cell(fact(plural(pk(n).api.size, "item")), n)).join(""));
  g += row("needs Rust", cols.map((n) => cell(fact(pk(n).msrv ? esc(pk(n).msrv) : `<span class="dim">not declared</span>`), n)).join(""));
  g += row("known advisories", cols.map((n) => {
    const a = pk(n).advisory;
    return cell(fact(!a ? `<span class="dim">not checked</span>` : a.affecting.length ? `<span style="color:var(--amber)">${esc(a.affecting[0].informational || a.affecting[0].id)}</span>` : a.known ? `none now <span class="dim">· ${word(a.known)} fixed</span>` : `none, ever`), n);
  }).join(""));
  if (S.alt) g += row("downloads, 90 days", cols.map((n) => cell(fact(pk(n).downloads ? `${(pk(n).downloads.recent / 1e6).toFixed(1)}M` : `<span class="dim">not fetched</span>`), n)).join(""));
  const ledger = `<div class="ledger2" id="ledger" style="--n:${N}">${g}</div>`;
  // adoption preview
  let apv = "";
  if (I && others.length) {
    const cand = S.preview && others.includes(S.preview) ? S.preview : others[0];
    const sites = I.sites;
    const changed = sites.filter((s) => s.rw[cand] && !s.rw[cand].miss.length && s.rw[cand].t !== s.t);
    const missing = sites.filter((s) => s.rw[cand] && s.rw[cand].miss.length);
    const files = new Set(sites.map((s) => s.f)).size;
    const show = [...changed.slice(0, 3), ...missing.slice(0, 3)].sort((a, b) => a.f.localeCompare(b.f) || a.l - b.l);
    let body = "", lastF = "";
    for (const s of show.slice(0, S.moreLines ? 40 : 6)) {
      if (s.f !== lastF) { body += `<div class="file">${esc(s.f)}</div>`; lastF = s.f; }
      body += pairHtml(s, cand, inc);
    }
    apv = `<section class="apv" id="apv"><h2>Your code, with <span class="pick">${others.map((o) => `<span class="${o === cand ? "on" : ""}" data-preview="${esc(o)}">${esc(o)}</span>`).join("")}</span></h2>
      <div class="sum"><b>${fmt(changed.length)}</b> of your ${fmt(sites.length)} lines change only in name${missing.length ? ` · <b>${fmt(missing.length)}</b> have no equivalent` : ""} · across ${plural(files, "file")} <span style="color:var(--ink4)">· matched by name; review before you switch</span></div>
      <div>${body}</div>${show.length > 6 || sites.length > show.length ? `<div class="more" data-more-lines>and ${plural(sites.length - Math.min(6, show.length), "more line")}</div>` : ""}</section>`;
  }
  return `<div class="bf"><div class="cv"><p class="lede">${lede}</p><div class="sub">${sub}</div></div>${ledger}${apv}</div>`;
}
function pairHtml(s, cand, inc) {
  const rw = s.rw[cand];
  const no = `<span class="pno">${s.l}</span>`;
  if (!rw) return "";
  if (rw.miss.length) {
    const m = rw.miss[0];
    const sp = [m, tail2(m), lastSeg(m)].find((x) => s.t.includes(x)) || lastSeg(m);
    const i = s.t.indexOf(sp);
    const before = i >= 0 ? esc(s.t.slice(0, i)) + `<del>${esc(sp)}</del>` + esc(s.t.slice(i + sp.length)) : esc(s.t);
    return `<div class="pair"><div class="pl">${no}<span class="pb">${before}</span></div><div class="pl"><span class="pno"></span><span class="pgap">no equivalent for <code>${esc(tail2(m))}</code> in ${esc(cand)}</span></div></div>`;
  }
  // mark what changed
  let a = 0; while (a < s.t.length && s.t[a] === rw.t[a]) a++;
  let b = 0; while (b < s.t.length - a && b < rw.t.length - a && s.t[s.t.length - 1 - b] === rw.t[rw.t.length - 1 - b]) b++;
  while (a > 0 && /\w/.test(s.t[a - 1])) a--;                                     // widen to whole identifiers
  while (b > 0 && /\w/.test(s.t[s.t.length - b]) && /\w/.test(rw.t[rw.t.length - b])) b--;
  const before = esc(s.t.slice(0, a)) + `<del>${esc(s.t.slice(a, s.t.length - b))}</del>` + esc(s.t.slice(s.t.length - b));
  const after = esc(rw.t.slice(0, a)) + `<ins>${esc(rw.t.slice(a, rw.t.length - b))}</ins>` + esc(rw.t.slice(rw.t.length - b));
  return `<div class="pair"><div class="pl">${no}<span class="pb">${before}</span></div><div class="pl"><span class="pno"></span><span class="pa">${after}</span></div></div>`;
}

// ------------------------------------------------------------------ your tree
function treeView(w) {
  const pr = D.project;
  const n = pr.inTree + S.added.reduce((a, x) => a + addCount(pk(x)), 0);
  const h = D.health;
  const dupes = D.duplicates;
  const warn = h.affecting.map((x) => `<span class="w">${esc(x.n)} ${esc(short(x.v))} is ${esc(x.informational || "affected")} ›</span>`).join("");
  const roles = D.roles.filter((r) => r.direct.length).map((r) => {
    const names = [...S.added.filter((a) => roleOf(a) === r.id && !r.direct.includes(a)), ...r.direct];
    const rows = names.map((nm) => treeRow(nm)).join("");
    const trans = r.transitive ? `<div class="tmore" data-trans="${r.id}">and ${plural(r.transitive, "crate")} that ${r.transitive === 1 ? "comes" : "come"} with them</div>` : "";
    const open = S.open.has(r.id) ? transRows(r.id) : "";
    return `<section class="role"><h3><span class="lbl">${esc(r.label)}</span><span class="for">${r.for.length ? "for " + esc(list(r.for.slice(0, 3))) : ""}</span></h3>${rows}${trans}${open}</section>`;
  }).join("");
  const shared = D.roles.find((r) => r.id === "shared");
  const twice = dupes.slice(0, 10);
  const twiceHtml = `<section class="role twice" id="twice"><h3>Here twice<span class="for">${plural(dupes.length, "crate")} compile at more than one version</span></h3>${twice.map((d) => {
    const hov = S.whyOpen === d.n;
    const up = d.upgrade;
    let move = "";
    if (hov && up) {
      const it = up.items[0];
      const strip = (x) => (x || "").replace(/'\w+\s*,?\s*|&'\w+\s*/g, "").replace(/\s+/g, "");
      const onlyLt = it && strip(it.before) === strip(it.after);
      move = `<div class="whyline move">Moving yours to <b>${esc(short(up.to))}</b> drops a copy · ${up.touched ? `${plural(up.touched, "of your " + up.of + " uses", "of your " + up.of + " uses")} touch <code>${esc(lastSeg(it.path))}</code>${onlyLt ? ", whose signature only gained a lifetime" : ", whose signature changed"}` : `none of your ${up.of} uses change`}</div>`;
    }
    return `<div class="tr${hov ? " hov" : ""}" data-dupe="${esc(d.n)}">${ecosystemMark("crates", { word: false })}<span class="pn">${esc(d.n)}</span><span class="vs">${d.versions.map((v) => v.yours ? `<b class="yv">${esc(short(v.v))}</b>` : `<b>${esc(short(v.v))}</b>`).join(" · ")}</span></div>`
      + (hov ? d.versions.map((v) => whyLine(v.why)).join("") + move : "");
  }).join("")}<div class="tmore">and ${fmt(dupes.length - twice.length)} more, deeper in the tree</div></section>`;
  return `<div class="bf"><div><div class="th">${gem("package", 44)}<div><div class="nm">backend</div>
      <div class="tlede">Your ${fmt(pr.members)} packages lean on <b>${fmt(pr.direct)}</b> others directly, and <b class="odo-n">${fmt(n)}</b> in all.</div></div></div></div>
    <div class="tfacts">${warn}<a data-jump="twice">${plural(dupes.length, "crate")} here twice ›</a><span>${fmt(shared?.transitive || 0)} come with more than one role</span><span>advisories checked for ${fmt(h.checked)} of ${fmt(h.of)}</span></div>
    <div class="roles">${roles}${twiceHtml}</div></div>`;
}
function treeRow(nm) {
  const d = directOf(nm);
  const p = pk(nm);
  const isNew = S.added.includes(nm);
  let why = "", wcls = "";
  if (isNew) why = "just added";
  else if (d) {
    const adv = d.advisory?.affecting?.[0];
    if (adv && adv.informational !== "unsound") { why = adv.informational || "advisory"; wcls = " w"; }
    else if (d.pins.length > 1) why = `twice · ${d.pins.map(short).join(" · ")}`;
    else if (d.kinds.length === 1 && d.kinds[0] === "dev") why = "tests only";
    else why = d.uses ? plural(d.uses, "place") : "declared, not named";
  }
  return `<div class="tr${S.hover === nm ? " sel" : ""}${isNew ? " new" : ""}" data-tree="${esc(nm)}">${ecosystemMark("crates", { word: false, pkg: nm, id: "t-" + nm })}<span class="pn">${esc(nm)}</span><span class="why${wcls}">${esc(why)}</span>${isNew ? `<span class="seam">${seam(-1)}</span>` : ""}</div>`
    ;
}
function transRows(role) {
  const xs = D.transitive.filter((t) => t.role === role && !t.direct).slice(0, 14);
  return xs.map((t) => `<div class="tr t" data-why="${esc(t.n)}">${ecosystemMark("crates", { word: false })}<span class="pn">${esc(t.n)}</span><span class="why">${esc(short(t.v))}</span></div>` + (S.whyOpen === t.n ? whyLine(t.why) : "")).join("");
}
function whyLine(why) {
  if (!why) return `<div class="whyline"><span>not reached from your packages</span></div>`;
  return `<div class="whyline">${why.map((x, i) => `<span class="${i === 0 ? "you" : i === why.length - 1 ? "it" : ""}">${esc(x.replace(/\+[^ ]*$/, ""))}</span>`).join("<i>→</i>")}</div>`;
}

// ------------------------------------------------------------------ render
let WIN_W = 0;
let LAST_W = 0;
function render(opts = {}) {
  const keepScroll = $("#reader")?.scrollTop || 0;
  const zoom = +(P.get("zoom") || 1);
  const W = Math.round((+(P.get("w") || 0) || innerWidth) / zoom);
  const H = Math.round(innerHeight / zoom);
  WIN_W = W;
  document.documentElement.style.setProperty("--zoom", zoom);
  const w = W;
  if (S.view === "find" && S.hover && !S.selName) S.selName = S.hover;
  if (S.view !== "find" && S.hover && !S.shelfRole) S.shelfRole = roleOf(S.hover);
  const reader = S.view === "find" ? findView(w) : S.view === "compare" ? compareView(w) : treeView(w);
  const cls = ["nx", "v4", "bw", S.alt ? "x-alt" : "", S.cmd ? "cmd" : ""].filter(Boolean).join(" ");
  const WW = P.get("w") ? `${W}px` : `calc(100vw / ${zoom})`, HH = `calc(100vh / ${zoom})`;
  $("#app").innerHTML = `<div class="${cls}" style="width:${WW};height:${HH};${zoom !== 1 ? `zoom:${zoom};` : ""}"><div class="win calm" style="width:100%;height:100%">${GROUND}${titlebar()}`
    + `<div class="cbody">${shelf(w)}<main class="reader creader" id="reader">${reader}</main>${handHtml()}</div>${status()}</div></div>`;
  if (S.view === "find") {
    if (S.sel != null && S.results?.rows[S.sel]) S.selName = S.results.rows[S.sel].n;
    if (!S.selName && S.results?.rows.length) S.selName = S.results.rows[0].n;
    placeJudge();
  }
  if (S.view === "compare" && S.hover) { $$(`[data-col="${CSS.escape(S.hover)}"]`).forEach((e) => e.classList.add("col-hov")); }
  if (S.view === "compare" && P.get("row")) openRow(+P.get("row"));
  if (S.view === "tree" && (S.hover || S.whyOpen) && !P.get("scroll") && !S.treeScrolled) {
    S.treeScrolled = true;
    const r = $(`.tr[data-tree="${CSS.escape(S.hover || "")}"]`) || $(`.tr[data-dupe="${CSS.escape(S.whyOpen || "")}"]`) || $(`.tr[data-why="${CSS.escape(S.whyOpen || "")}"]`);
    if (r) $("#reader").scrollTop = Math.max(0, r.offsetTop - $("#reader").clientHeight * 0.3);
  }
  if (S.view === "tree" && S.hover) placeTreeCard(S.hover);
  const el = P.get("scroll") && !S.userScrolled ? document.getElementById(P.get("scroll")) : null;
  $("#reader").scrollTop = el ? el.offsetTop - 24 : keepScroll;
  $("#reader").addEventListener("wheel", () => { S.userScrolled = true; }, { once: true });
  const ph = P.get("hovermark");
  if (ph) openMark(ph);
  wire();
}
function handHtml() {
  if (S.hand.length < 1 || S.view !== "find") return "";
  return `<div class="handwrap" style="position:absolute;left:50%;bottom:14px;transform:translateX(-50%);z-index:30"><div class="hand" style="position:static;transform:none">${S.hand.map((n) => `<span class="hc">${ecosystemMark("crates", { word: false })}${esc(n)}</span>`).join("")}<span class="go" data-compare="${esc(S.hand.join(","))}">compare ${word(S.hand.length)} ›</span></div></div>`;
}
function openRow(i) { $$(`#ledger [data-row="${i}"]`).forEach((e) => e.classList.add("row-hov")); $$(`#ledger .cell`).forEach(() => {}); const lab = $(`#ledger .lab[data-row="${i}"]`); if (!lab) return; let el = lab.nextElementSibling; while (el && !el.classList.contains("lab") && !el.classList.contains("band")) { el.classList.add("row-open", "row-hov"); el = el.nextElementSibling; } }
function placeTreeCard(n) {
  const row = $(`.tr[data-tree="${CSS.escape(n)}"]`);
  if (!row || !pk(n)) return;
  const reader = $("#reader");
  const rr = row.getBoundingClientRect(), fr = reader.getBoundingClientRect();
  const html = judgeHtml(n, null);
  const left = Math.min(rr.right - fr.left + 16, fr.width - 396);
  const el = document.createElement("div");
  el.className = "tcard";
  el.innerHTML = html;
  el.style.left = (left < rr.right - fr.left - 40 ? Math.max(8, rr.left - fr.left) : left) + "px";
  el.style.top = (rr.bottom - fr.top + reader.scrollTop + 6) + "px";
  if (left >= rr.right - fr.left - 40) el.style.top = (rr.top - fr.top + reader.scrollTop - 10) + "px";
  reader.appendChild(el);
}

// ------------------------------------------------------------------ interaction
function wire() {
  const qin = $("#qin");
  if (qin) {
    qin.addEventListener("input", () => { retype(qin.value); });
    qin.addEventListener("keydown", (e) => {
      const rows = $$(".r[data-n]");
      const i = rows.findIndex((r) => r.dataset.n === S.selName);
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const j = Math.max(0, Math.min(rows.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)));
        if (rows[j]) { S.selName = rows[j].dataset.n; placeJudge(); }
      }
    });
    if (!P.has("still")) { qin.focus(); qin.setSelectionRange(qin.value.length, qin.value.length); }
  }
  $$(".hint").forEach((h) => h.addEventListener("click", () => { S.q = h.dataset.q; S.selName = null; render(); }));
  $$(".r[data-n]").forEach((r) => r.addEventListener("pointerenter", () => { if (S.selName !== r.dataset.n) { S.selName = r.dataset.n; placeJudge(); } }));
  $$("[data-go]").forEach((b) => b.addEventListener("click", () => { S.view = b.dataset.go; S.hover = null; render(); }));
  $$(".lib-role").forEach((b) => b.addEventListener("click", () => { S.shelfRole = S.shelfRole === b.dataset.role ? "__none" : b.dataset.role; render(); }));
  document.onclick = (e) => {
    const c = e.target.closest("[data-compare]");
    if (c) { S.compare = c.dataset.compare.split(","); S.view = "compare"; S.preview = S.compare[1]; S.hover = null; render(); $("#apv")?.scrollIntoView({ block: "start" }); return; }
    const a = e.target.closest("[data-add]");
    if (a) { adopt(a.dataset.add); return; }
    const pv = e.target.closest("[data-preview]");
    if (pv) { S.preview = pv.dataset.preview; render(); $("#apv")?.scrollIntoView({ block: "start" }); return; }
    const dr = e.target.closest("[data-drop]");
    if (dr) { dropColumn(dr.dataset.drop); return; }
    const t = e.target.closest("[data-trans]");
    if (t) { S.open.has(t.dataset.trans) ? S.open.delete(t.dataset.trans) : S.open.add(t.dataset.trans); render(); return; }
    const mu = e.target.closest("[data-more-uses]"); if (mu) { S.moreUses = true; render(); return; }
    const ml = e.target.closest("[data-more-lines]"); if (ml) { S.moreLines = true; render(); return; }
    const ac = e.target.closest("[data-all-cousins]"); if (ac) { S.allCousins = true; render(); return; }
    const j = e.target.closest("[data-jump]"); if (j) { $("#" + j.dataset.jump)?.scrollIntoView({ behavior: REDUCED ? "auto" : "smooth" }); return; }
    const r = e.target.closest(".r[data-n]");
    if (r && (e.metaKey || e.ctrlKey)) { toHand(r.dataset.n); return; }
  };
  $$(".tr[data-why],.tr[data-dupe]").forEach((r) => r.addEventListener("pointerenter", () => { const n = r.dataset.why || r.dataset.dupe; if (S.whyOpen !== n) { S.whyOpen = n; const y = $("#reader").scrollTop; render(); $("#reader").scrollTop = y; } }));
  $$(".tr[data-tree]").forEach((r) => {
    r.addEventListener("pointerenter", () => { $$(".tcard").forEach((c) => c.remove()); $$(".tr.sel").forEach((x) => x.classList.remove("sel")); r.classList.add("sel"); S.hover = r.dataset.tree; placeTreeCard(r.dataset.tree); });
  });
  // ledger: rows unfold their signatures, columns lift
  const lg = $("#ledger");
  if (lg) {
    lg.addEventListener("pointerover", (e) => {
      const c = e.target.closest(".c");
      $$(".row-hov,.row-open,.col-hov", lg).forEach((x) => x.classList.remove("row-hov", "row-open", "col-hov"));
      $$(".hd.hov", lg).forEach((x) => x.classList.remove("hov"));
      if (!c) return;
      const col = c.dataset.col;
      if (col) { $$(`[data-col="${CSS.escape(col)}"]`, lg).forEach((x) => x.classList.add("col-hov")); $(`.hd[data-col="${CSS.escape(col)}"]`, lg)?.classList.add("hov"); }
      let lab = c.classList.contains("lab") ? c : null;
      if (!lab && !c.classList.contains("hd")) { let el = c; while (el && !el.classList.contains("lab") && !el.classList.contains("band")) el = el.previousElementSibling; if (el?.classList.contains("lab")) lab = el; }
      if (lab) { lab.classList.add("row-hov"); let el = lab.nextElementSibling; while (el && !el.classList.contains("lab") && !el.classList.contains("band") && !el.classList.contains("fold-more")) { el.classList.add("row-hov", "row-open"); el = el.nextElementSibling; } }
    });
  }
}
addEventListener("keydown", (e) => {
  if (e.key === "Alt") { S.alt = true; $(".nx")?.classList.add("x-alt"); }
  if (e.key === "Meta") { S.cmd = true; $(".nx")?.classList.add("cmd"); }
  if (document.activeElement?.id === "qin" && !(e.metaKey || e.ctrlKey)) return;
  if ((e.key === "a" || e.key === "A") && S.selName) adopt(S.selName);
  if ((e.key === "c" || e.key === "C") && S.selName) toHand(S.selName);
});
addEventListener("keyup", (e) => {
  if (e.key === "Alt") { S.alt = P.get("alt") === "1"; $(".nx")?.classList.toggle("x-alt", S.alt); }
  if (e.key === "Meta") { S.cmd = P.get("cmd") === "1"; $(".nx")?.classList.toggle("cmd", S.cmd); }
});
addEventListener("resize", () => { if (P.has("t")) return; if (innerWidth + "x" + innerHeight !== LAST_W) { LAST_W = innerWidth + "x" + innerHeight; render(); } });
function toHand(n) { if (!S.hand.includes(n)) S.hand = [...S.hand, n].slice(-4); render(); }

// ------------------------------------------------------------------ motion: FLIP re-rank (rows travel; new rows rise 4 px in rank order)
function retype(q, t = null) {
  const before = new Map($$(".r[data-n]").map((r) => [r.dataset.n, r.getBoundingClientRect()]));
  S.q = q; S.selName = null;
  if ($("#qin") && $("#qin").value !== q) $("#qin").value = q;
  const res = S.results = search(q);
  $("#rs").innerHTML = res.r.mode === "empty" ? "" : resultsHtml(res, WIN_W);
  $(".bq .read").innerHTML = readLine(res);
  const addr = $(".cstatus .addr"); if (addr) addr.textContent = `nudox://find?q=${q}`;
  if (!S.selName && res.rows.length) S.selName = res.rows[0].n;
  placeJudge();
  $$(".r[data-n]").forEach((r) => r.addEventListener("pointerenter", () => { if (S.selName !== r.dataset.n) { S.selName = r.dataset.n; placeJudge(); } }));
  const anims = [];
  let k = 0;
  for (const r of $$(".r[data-n]")) {
    const a = before.get(r.dataset.n), b = r.getBoundingClientRect();
    if (REDUCED) continue;
    if (a) {
      const dy = a.top - b.top;
      if (Math.abs(dy) > 0.5) anims.push(r.animate([{ transform: `translateY(${dy}px)` }, { transform: "none" }], { duration: 240, easing: SPRING }));
    } else {
      anims.push(r.animate([{ transform: "translateY(4px)", clipPath: "inset(0 0 100% 0)" }, { transform: "none", clipPath: "inset(0 0 0 0)" }], { duration: 200, delay: 18 * k++, easing: GLIDE, fill: "backwards" }));
    }
  }
  if (t != null) anims.forEach((a) => { a.pause(); a.currentTime = t; });
  return anims;
}
function dropColumn(n) {
  const lg = $("#ledger");
  const before = new Map($$(".hd", lg).map((h) => [h.dataset.col, h.getBoundingClientRect()]));
  S.compare = S.compare.filter((x) => x !== n);
  render();
  if (REDUCED) return;
  $$("#ledger .hd").forEach((h) => { const a = before.get(h.dataset.col); if (!a) return; const b = h.getBoundingClientRect(); const dx = a.left - b.left; if (dx) h.animate([{ transform: `translateX(${dx}px)` }, { transform: "none" }], { duration: 240, easing: SPRING }); });
}

// ------------------------------------------------------------------ motion: adopt — lift, arc, drop, make room, tick, index
const T = { lift: [0, 140], fly: [140, 640], room: [380, 660], drop: [620, 940], name: [660, 860], odo: [700, 1100], seam: [940, 3340], settle: [3340, 3640] };
const SEAM_WORDS = ["fetching", "unpacking", "reading", "sealing"];
function adopt(n, frozenT = null) {
  const p = pk(n);
  if (!p || S.added.includes(n) || tree(p).direct) return;
  const src = $(".judge .jgem svg") || $(`.r[data-n="${CSS.escape(n)}"] .mk-stone`);
  const srcR = src?.getBoundingClientRect();
  $(".judge .jadd")?.classList.add("press");
  const oldN = D.project.inTree + S.added.reduce((a, x) => a + addCount(pk(x)), 0);
  S.added.push(n);
  S.shelfRole = roleOf(n);
  const newN = oldN + addCount(p);
  const keepSel = S.selName;
  render();
  S.selName = keepSel; placeJudge();
  const addBtn = $(".judge .jadd") || $(".judge .open");
  // destination: the new shelf row, else the spine, else the status count
  let dst = $(`.lib-pk.new[data-pk="${CSS.escape(n)}"]`) || $(`.tr.new[data-tree="${CSS.escape(n)}"]`);
  const target = dst ? $(".st", dst) || $(".mk", dst) : $(".ckspine .sp.on") || $(".cstatus .tc");
  const anims = [];
  // the list makes room: the new row grows from zero height and pushes its neighbours
  if (dst && !REDUCED) {
    const h = dst.getBoundingClientRect().height;
    anims.push(dst.animate([{ height: "0px", paddingTop: 0, paddingBottom: 0 }, { height: h + "px" }], { duration: T.room[1] - T.room[0], delay: T.room[0], easing: SPRING, fill: "backwards" }));
    const nm = $(".n,.pn", dst);
    if (nm) anims.push(nm.animate([{ clipPath: "inset(0 100% 0 0)", transform: "translateX(-6px)" }, { clipPath: "inset(0 0 0 0)", transform: "none" }], { duration: T.name[1] - T.name[0], delay: T.name[0], easing: GLIDE, fill: "backwards" }));
    const mk = $(".st,.mk", dst);
    if (mk) anims.push(mk.animate([{ visibility: "hidden", offset: 0 }, { visibility: "hidden", offset: 0.999 }, { visibility: "visible", offset: 1 }], { duration: T.drop[0], fill: "backwards" }));
    if (mk) anims.push(mk.animate([{ transform: "translateY(-10px) scale(.7,1.3)" }, { transform: "translateY(0) scale(1.25,.72)", offset: 0.35 }, { transform: "translateY(-5px) scale(.92,1.1)", offset: 0.6 }, { transform: "translateY(0) scale(1.05,.95)", offset: 0.8 }, { transform: "none" }], { duration: T.drop[1] - T.drop[0], delay: T.drop[0], easing: "linear", fill: "backwards" }));
  }
  // the gem lifts off the card, arcs to its place, and is gone when the row's mark lands
  if (srcR && target && !REDUCED) {
    const tr = target.getBoundingClientRect();
    const fly = document.createElement("div");
    fly.className = "flyer";
    fly.innerHTML = gem("package", 30);
    const z = +(P.get("zoom") || 1);
    fly.style.left = srcR.left * z + "px"; fly.style.top = srcR.top * z + "px"; fly.style.width = fly.style.height = srcR.width * z + "px";
    document.body.appendChild(fly);
    const x0 = 0, y0 = 0, x1 = (tr.left + tr.width / 2 - (srcR.left + srcR.width / 2)) * z, y1 = (tr.top + tr.height / 2 - (srcR.top + srcR.height / 2)) * z;
    const cx = (x0 + x1) / 2, cy = Math.min(y0, y1) - 120 * z;
    const s1 = Math.max(0.35, (tr.width || 14) / srcR.width);
    const frames = [];
    const total = T.fly[1];
    frames.push({ transform: "translate(0,0) scale(1)", offset: 0 });
    frames.push({ transform: "translate(0,-8px) scale(1.12) rotate(-8deg)", offset: T.lift[1] / total });
    for (let i = 1; i <= 12; i++) {
      const u = i / 12, ease = u * u * (3 - 2 * u);
      const x = (1 - ease) ** 2 * x0 + 2 * (1 - ease) * ease * cx + ease ** 2 * x1;
      const y = (1 - ease) ** 2 * (y0 - 8) + 2 * (1 - ease) * ease * cy + ease ** 2 * y1;
      const s = u < 0.45 ? 1.12 + 0.5 * (u / 0.45) : u < 0.8 ? 1.62 - 0.6 * ((u - 0.45) / 0.35) : 1.02 + (s1 - 1.02) * ((u - 0.8) / 0.2);
      frames.push({ transform: `translate(${x.toFixed(1)}px,${y.toFixed(1)}px) scale(${s.toFixed(3)}) rotate(${(-8 + 68 * ease).toFixed(1)}deg)`, offset: (T.lift[1] + (T.fly[1] - T.lift[1]) * u) / total });
    }
    anims.push(fly.animate(frames, { duration: total, easing: "linear", fill: "forwards" }));
    anims.push(fly.animate([{ visibility: "visible" }, { visibility: "hidden" }], { duration: 1, delay: T.drop[0], fill: "forwards" }));
  }
  // no row to land in (narrow windows): the spine icon or the status count catches it and bounces
  if (!dst && target && !REDUCED) anims.push(target.animate([{ transform: "none" }, { transform: "translateY(3px) scale(1.25,.8)", offset: 0.3 }, { transform: "translateY(-4px) scale(.92,1.1)", offset: 0.6 }, { transform: "none" }], { duration: 320, delay: T.fly[1] - 20, easing: "linear", fill: "backwards" }));
  // the tree count rolls like an odometer
  $$(".odo-n,.odo-in").forEach((el) => anims.push(...odometer(el, oldN, newN)));
  // slim, complete indexing: four stages under the new row, and the card says which one
  const seamEls = $$(".seam4", dst || document.createElement("div"));
  const setStage = (t) => {
    const k = t < T.seam[0] ? -1 : Math.min(4, Math.floor((t - T.seam[0]) / ((T.seam[1] - T.seam[0]) / 4)));
    seamEls.forEach((s) => { [...s.children].forEach((c, i) => { c.className = i < k ? "done" : i === k ? "now" : ""; }); s.style.opacity = t < T.drop[1] || t > T.settle[1] ? 0 : 1; s.style.transform = t > T.settle[0] ? `scaleX(${Math.max(0, 1 - (t - T.settle[0]) / (T.settle[1] - T.settle[0]))})` : "none"; s.style.transformOrigin = "left"; });
    if (addBtn && addBtn.classList.contains("open")) addBtn.innerHTML = k < 0 ? "adding…" : k < 4 ? `added · ${SEAM_WORDS[k]}${k === 2 && p.api?.size ? ` <span style="color:var(--ink4)">${fmt(p.api.size)} public items</span>` : ""} <span style="color:var(--ink4)">${k + 1} of 4</span>` : `added · indexed`;
    const g = $(".judge .jgem svg");
    if (g) g.classList.toggle("lighting", k >= 0 && k < 4);
  };
  if (frozenT != null) {
    anims.forEach((a) => { a.pause(); a.currentTime = frozenT; });
    setStage(frozenT);
    return;
  }
  const t0 = performance.now();
  const tick = () => { const t = performance.now() - t0; setStage(t); if (t < T.settle[1] + 50) requestAnimationFrame(tick); else { $$(".flyer").forEach((f) => f.remove()); } };
  requestAnimationFrame(tick);
}
// the ask field in the titlebar becomes the page's hero: its box travels and grows; the results rise under it in rank order
function flyIn(t = null) {
  const from = $(".titlebar .here"), to = $(".bq .qrow");
  if (!from || !to || REDUCED) return;
  const a = from.getBoundingClientRect(), b = to.getBoundingClientRect();
  const sx = a.width / b.width, sy = a.height / b.height;
  const anims = [to.animate([{ transform: `translate(${a.left - b.left}px,${a.top - b.top}px) scale(${sx},${sy})`, transformOrigin: "0 0" }, { transform: "none", transformOrigin: "0 0" }], { duration: 320, easing: GLIDE, fill: "backwards" })];
  const ghost = document.createElement("div");
  ghost.className = "flyghost";
  ghost.style.cssText = `left:${a.left}px;top:${a.top}px;width:${a.width}px;height:${a.height}px`;
  document.body.appendChild(ghost);
  anims.push(ghost.animate([{ transform: "none" }, { transform: `translate(${b.left - a.left}px,${b.top - a.top}px) scale(${b.width / a.width},${b.height / a.height})` }], { duration: 320, easing: GLIDE, fill: "forwards" }));
  anims.push(ghost.animate([{ visibility: "visible" }, { visibility: "hidden" }], { duration: 1, delay: 320, fill: "forwards" }));
  $$(".r, .verdict .say, .gh, .hint, .fold-cousins, .foot").forEach((r, i) => anims.push(r.animate([{ transform: "translateY(4px)", clipPath: "inset(0 0 100% 0)" }, { transform: "none", clipPath: "inset(0 0 0 0)" }], { duration: 200, delay: 220 + 22 * i, easing: GLIDE, fill: "backwards" })));
  const card = $(".jcard .judge") || $(".jin .judge");
  if (card) anims.push(card.animate([{ clipPath: "polygon(0 0,100% 0,100% 0,0 0)" }, { clipPath: "polygon(0 0,100% 0,100% 100%,0 100%)" }], { duration: 240, delay: 300, easing: GLIDE, fill: "backwards" }));
  if (t != null) anims.forEach((x) => { x.pause(); x.currentTime = t; });
}
function odometer(el, from, to) {
  const a = fmt(from), b = fmt(to);
  if (a === b || REDUCED) { el.textContent = b; return []; }
  el.innerHTML = `<span class="odo">${[...b].map((ch, i) => {
    const old = a.length === b.length ? a[i] : "";
    if (ch === old || !/\d/.test(ch)) return esc(ch);
    return `<span class="dg" data-from="${esc(old)}"><span>${esc(old || "0")}</span><span>${esc(ch)}</span></span>`;
  }).join("")}</span>`;
  return $$(".dg", el).map((d, i, all) => d.animate([{ transform: "translateY(0)" }, { transform: "translateY(-50%)" }], { duration: T.odo[1] - T.odo[0], delay: T.odo[0] + (all.length - 1 - i) * 60, easing: SPRING, fill: "both" }));
}

// ------------------------------------------------------------------ boot
async function boot() {
  const [data, icons, ground] = await Promise.all([
    fetch("data.json", { cache: "reload" }).then((r) => r.json()),
    fetch("../../spec/icons.json").then((r) => r.json()),
    fetch("../tpl-ground.html").then((r) => r.text()).catch(() => ""),
  ]);
  D = data; GROUND = ground;
  globalThis.MARKS_NOW = D.generated + "T12:00:00Z";
  for (const i of icons) ICONS[i.group + "/" + i.name] = i.svg;
  buildIndex();
  if (S.view === "compare" && !S.compare.length) S.compare = ["toml", "toml_edit", "basic-toml"];
  LAST_W = innerWidth + "x" + innerHeight;
  render();
  // reproducible motion stills
  const t = P.has("t") ? +P.get("t") : null;
  const motion = () => {
    if (t != null) render();                    // the viewport has settled; lay out again before freezing
    if (P.get("to") != null) retype(P.get("to"), t);
    if (P.get("add")) adopt(P.get("add"), t);
    if (P.get("fly")) flyIn(t);
    document.body.dataset.ready = "1";
  };
  if (t != null) setTimeout(motion, 400); else motion();
}
boot();
