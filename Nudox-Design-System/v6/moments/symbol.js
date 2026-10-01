// The symbol page, and the moves between symbols. One page per kind, in the drawn page's idiom:
// left is where a thing comes from, right is where it goes, the spine runs down the middle.
//   fn / method  a pipe: what it takes enters from the left, what it gives leaves right, failure drops below
//   type         what makes one on the left, what it does on the right, what it holds hangs below
//   trait        a socket: what you write are notches, implementors plug in from the left
// Then the heart, the relations back to us: a reach bar you can scrub (which members your code reaches,
// or which crates), and each crate's call sites as a deck that fans out. Whose code uses it when yours
// doesn't; what its own file reaches in other packages; its history across the releases on disk.
// Relations are read from signatures (a name of this package inside another's signature) and a path
// scan of your crates (extract_sym.py), not from a compiler: real, approximate, and the page says so.
import { gem } from "./paint.js";
import { icon, read, tagHtml } from "./badges.js";
import { detail, folio3 } from "./folio3.js";
import { kindOf, NOW, ago } from "./charts.js";
import { CARRY, play, lerp, clamp, wait } from "./motion.js";
import { fmt, plural } from "./world.js";

const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const fileOf = (id) => id.replace(/[/+]/g, "_");
const J = new Map();
const json = (u) => J.get(u) || J.set(u, fetch(u, { cache: "reload" }).then((r) => (r.ok ? r.json() : null)).catch(() => null)).get(u);
const insides = async (id) => ((await json(`data/sym/${fileOf(id)}.json`)) || {}).items || {};
const USES = async () => (await json("data/sym_uses.json")) || {};
const VIA = async () => (await json("data/via.json")) || {};
const short = (n) => n.replace(/^backend-/, "");
const K = (pid, mod, name) => `${pid}|${mod}|${name}`;
const unK = (k) => { const [pid, mod, name] = k.split("|"); return { pid, mod, name }; };

const HUE = { type: "var(--k-ty)", callable: "var(--k-ca)", contract: "var(--k-co)", value: "var(--k-va)" };
const WORD = { fn: "function", method: "method", macro: "macro", trait: "trait", alias: "alias", struct: "struct", enum: "enum", union: "union", const: "constant", static: "static", item: "item" };
const RECV = { "&": "reads it", "&mut": "changes it", self: "consumes it" };
const SP = 300; // the spine, from the page's content edge

// ---------------------------------------------------------------- reading signatures
function closeAt(s, i, o, c) {
  let d = 0;
  for (let j = i; j < s.length; j++) {
    const ch = s[j];
    if (ch === "-" && s[j + 1] === ">") { j++; continue; }
    if (ch === o) d++;
    else if (ch === c) { d--; if (!d) return j; }
  }
  return s.length;
}
function splitTop(s, sep = ",") {
  const out = []; let d = 0, cur = "";
  for (let j = 0; j < s.length; j++) {
    const ch = s[j];
    if (ch === "-" && s[j + 1] === ">") { cur += "->"; j++; continue; }
    if ("<([{".includes(ch)) d++; else if (">)]}".includes(ch)) d--;
    if (ch === sep && d === 0) { if (cur.trim()) out.push(cur.trim()); cur = ""; } else cur += ch;
  }
  if (cur.trim()) out.push(cur.trim());
  return out;
}
// A fn's signature into its generics, parameters, receiver and return.
export function parseFn(S) {
  let s = (S || "").replace(/\s*\{\s*$/, "").replace(/…$/, "");
  const at = s.search(/\bfn\s+/); if (at < 0) return null;
  s = s.slice(at).replace(/^fn\s+(r#)?\w+\s*/, "");
  let gen = "";
  if (s[0] === "<") { const e = closeAt(s, 0, "<", ">"); gen = s.slice(1, e); s = s.slice(e + 1).trim(); }
  const gens = new Map(), params = []; let recv = "", ret = "";
  for (const g of splitTop(gen)) { if (g.startsWith("'")) continue; const m = g.replace(/^const\s+/, "").match(/^(\w+)\s*(?::([\s\S]*))?/); if (m) gens.set(m[1], (m[2] || "").split("=")[0].trim()); }
  if (s[0] !== "(") return { gens, params, ret, recv, partial: true };
  const pe = closeAt(s, 0, "(", ")");
  let rest = s.slice(pe + 1);
  const wi = rest.search(/\bwhere\b/);
  const where = wi >= 0 ? rest.slice(wi + 5) : "";
  if (wi >= 0) rest = rest.slice(0, wi);
  ret = ((rest.match(/->\s*([\s\S]+)$/) || [])[1] || "").trim().replace(/[,;]\s*$/, "");
  for (const w of splitTop(where)) { const m = w.match(/^(\w+)\s*:\s*([\s\S]+)$/); if (m) gens.set(m[1], [gens.get(m[1]), m[2]].filter(Boolean).join(" + ")); }
  for (const p of splitTop(s.slice(1, pe))) {
    if (/^(&\s*('\w+\s+)?(mut\s+)?)?(mut\s+)?self\b/.test(p)) { recv = /&/.test(p) ? (/mut/.test(p) ? "&mut" : "&") : /Pin<\s*&\s*mut/.test(p) ? "&mut" : "self"; if (/^self\s*:/.test(p)) recv = /&\s*('\w+\s+)?mut|Pin<\s*&\s*mut/.test(p) ? "&mut" : /&/.test(p) ? "&" : "self"; continue; }
    const m = p.match(/^(?:mut\s+)?(\w+|_)\s*:\s*([\s\S]+)$/); if (m) params.push({ name: m[1], type: m[2].trim() });
  }
  return { gens, params, ret, recv, partial: pe >= s.length };
}
const NOISE = /^(\?Sized|Sized|Send|Sync|Unpin|Copy|Clone|Debug|Display|'\w+|'static|Eq|Hash|Ord|PartialEq|PartialOrd)$/;
function boundOf(b) {
  const parts = splitTop(b || "", "+").map((x) => x.trim()).filter((x) => x && !NOISE.test(x));
  if (!parts.length) return null;
  let t = parts[0].replace(/^(dyn|impl)\s+/, "").replace(/<[\s\S]*$/, "").split("::").pop();
  if (t === "DeserializeOwned") t = "Deserialize";
  if (/^Fn(Mut|Once)?$/.test(t)) return "fn";
  return t;
}
const PRIM = { str: "text", String: "text", OsStr: "OS text", OsString: "OS text", Path: "path", PathBuf: "path", bool: "yes or no", char: "a character", f32: "float", f64: "float", Duration: "a duration", Instant: "an instant", SystemTime: "a time" };
const INT = /^[iu](8|16|32|64|128|size)$/;
const WRAP = new Set(["Box", "Rc", "Arc", "Pin", "Cow", "ManuallyDrop", "RefCell", "Cell", "NonNull", "Weak", "MaybeUninit"]);
// A Rust type into words: `&[u8]` is "bytes", `Option<&str>` is "maybe text", a generic is "any <its bound>",
// and a name of this package becomes a token you can hover and follow.
function tw(t, cx, d = 0) {
  t = (t || "").trim().replace(/,$/, "");
  if (!t || d > 5) return [];
  let m;
  if ((m = t.match(/^&\s*(?:'\w+\s+)?(?:mut\s+)?([\s\S]+)$/))) return tw(m[1], cx, d + 1);
  if ((m = t.match(/^\*(?:const|mut)\s+([\s\S]+)$/))) return [{ w: "pointer to" }, ...tw(m[1], cx, d + 1)];
  if ((m = t.match(/^(?:dyn|impl)\s+([\s\S]+)$/))) return anyOf(m[1], cx);
  if (t === "()") return [{ p: "nothing" }];
  if (t === "!") return [{ p: "never returns" }];
  if (t === "Self") return cx.self ? [cx.ref(cx.self)] : [{ p: "itself" }];
  if (t[0] === "(") return splitTop(t.slice(1, closeAt(t, 0, "(", ")"))).flatMap((x, i) => [...(i ? [{ w: "and" }] : []), ...tw(x, cx, d + 1)]);
  if (t[0] === "[") { const [el] = splitTop(t.slice(1, closeAt(t, 0, "[", "]")), ";"); return /^u8$/.test((el || "").trim()) ? [{ p: "bytes" }] : [{ w: "list of" }, ...tw(el, cx, d + 1)]; }
  m = t.match(/^((?:\w+::)*)(\w+)\s*(<[\s\S]*>)?/);
  if (!m) return [{ x: t.slice(0, 24) }];
  const name = m[2], qual = m[1], args = m[3] ? splitTop(m[3].slice(1, -1)).filter((a) => !a.startsWith("'")) : [];
  if (cx.gens && cx.gens.has(name) && !qual) return anyOf(cx.gens.get(name), cx);
  if (PRIM[name]) return [{ p: PRIM[name] }];
  if (INT.test(name)) return [{ p: "integer" }];
  if (WRAP.has(name) && args.length) return tw(args[args.length - 1], cx, d + 1);
  if (/^(Vec|VecDeque|SmallVec|LinkedList)$/.test(name) && args.length) return /^u8$/.test(args[0]) ? [{ p: "bytes" }] : [{ w: "list of" }, ...tw(args[0], cx, d + 1)];
  if (name === "Option" && args.length) return [{ w: "maybe" }, ...tw(args[0], cx, d + 1)];
  if (/^(HashMap|BTreeMap|IndexMap)$/.test(name) && args.length >= 2) return [{ w: "map of" }, ...tw(args[0], cx, d + 1), { w: "to" }, ...tw(args[1], cx, d + 1)];
  if (/^(HashSet|BTreeSet|IndexSet)$/.test(name) && args.length) return [{ w: "set of" }, ...tw(args[0], cx, d + 1)];
  if (/^Fn(Mut|Once)?$/.test(name)) return [{ p: "a function" }];
  if (name === "PhantomData") return [{ p: "nothing" }];
  if (name === "Poll" && args.length) return [{ w: "ready or pending" }, ...tw(args[0], cx, d + 1)];
  if (name === "Result" && args.length) return [...tw(args[0], cx, d + 1), { w: "or fails" }];
  const r = cx.ref(name, qual);
  if (!r && /^[A-Z]\d?$/.test(name)) return [{ g: "any" }];   // an impl's own generic (V, S, T2)
  const head = r ? [r] : [{ x: name }];
  if (!args.length || name === "Context") return head;
  const rest = args.filter((a) => !/^\w$/.test(a) || !r).slice(0, 2);
  return rest.length ? [...head, { w: "of" }, ...rest.flatMap((a, i) => [...(i ? [{ w: "to" }] : []), ...tw(a, cx, d + 1)])] : head;
}
function anyOf(b, cx) { const t = boundOf(b); if (!t) return [{ g: "any type" }]; if (t === "fn") return [{ p: "a function" }]; const r = cx.ref(t); return r ? [{ w: "any" }, r] : [{ g: `any ${t}` }]; }
function resultOf(ret, cx) {
  const m = ret.match(/^((?:\w+::)*)Result\s*<([\s\S]*)>$/);
  if (!m) return null;
  const [ok, er] = splitTop(m[2]);
  const err = er ? tw(er, cx) : m[1].startsWith("io::") ? [{ x: "io::Error" }] : (() => { const r = cx.ref("Error"); return r ? [r] : [{ p: "an error" }]; })();
  return { ok: tw(ok, cx), err };
}
const namesIn = (ws) => ws.filter((p) => p.key).map((p) => p.key);

// ---------------------------------------------------------------- sigils: icons that carry facts
// fn: a plate; one prong per input on the left, an arrow out if it gives something, a coral drop if it
// can fail, a stem from above if it is a method (its receiver), dashed if async, an amber corner if unsafe.
// type: a diamond; an enum stacks one bar per case, a struct one tick per field, an alias doubles its outline.
// trait: a socket; one notch per member you must write, one tab per member you get.
// Mint dots along the lower right: one per crate of yours that names it.
export function facts(item, X, extra = {}) {
  const r = read(item); const k = extra.kind || r.kind;
  const F = { fam: item.f, kind: k, yours: extra.yours || 0 };
  if (k === "fn" || k === "method") {
    const f = parseFn((X && X.S) || item.s) || { params: [], ret: "" };
    F.nIn = f.params.length; F.gives = !!f.ret && f.ret !== "()" && f.ret !== "Self" ? true : !!f.ret;
    F.fails = /Result\b/.test(f.ret); F.recv = f.recv || (extra.kind === "method" ? "&" : ""); F.async = /\basync\b/.test(item.s || ""); F.unsafe = /\bunsafe\b/.test((item.s || "").split(/\bfn\b/)[0]);
  } else if (k === "enum") F.nVar = X && X.var ? X.var.length : 3;
  else if (k === "struct" || k === "union") { F.nFld = X && X.fld ? X.fld.length : 0; F.marker = /;\s*$/.test(item.s || ""); }
  else if (k === "trait") { F.nReq = X && X.req ? X.req.length : 1; F.nProv = X && X.prov ? X.prov.length : 0; }
  return F;
}
export function sigil(F, size = 64, cls = "") {
  const sw = size >= 48 ? 2 : size >= 24 ? 3 : 4.6;
  let s = "";
  const k = F.kind;
  if (k === "fn" || k === "method" || k === "macro") {
    s += `<path class="pl${F.async ? " dash" : ""}" d="M24 19H47V39L41 45H17V25Z"/>`;
    if (k === "macro") s += `<path class="ln" d="M32 24V35M32 39V40.5"/>`;
    const n = Math.min(F.nIn || 0, 5);
    for (let i = 0; i < n; i++) { const y = n === 1 ? 32 : 21 + (22 * i) / (n - 1); s += `<path class="ln" d="M2 ${y.toFixed(1)}H8L17 ${(32 + (y - 32) * 0.3).toFixed(1)}"/>`; }
    if (F.gives) s += `<path class="ln" d="M47 32H60M55 27l5 5-5 5"/>`;
    if (F.fails) s += `<path class="co" d="M29 45V57H38"/><path class="cof" d="M38 53h6v6h-6z"/>`;
    if (F.recv) s += `<path class="ln" d="M32 2V19M26 2H38"/>`;
    if (F.unsafe) s += `<path class="amf" d="M47 19V29L37 19Z"/>`;
  } else if (k === "trait") {
    const req = Math.min(F.nReq || 0, 4), prov = Math.min(F.nProv || 0, 4);
    let d = "M18 8H50V50L44 56H18";
    for (let i = req - 1; i >= 0; i--) { const y = 16 + ((40 - 8) * (i + 0.5)) / Math.max(req, 1); d += `V${(y + 5).toFixed(1)}L26 ${y.toFixed(1)}L18 ${(y - 5).toFixed(1)}`; }
    s += `<path class="pl" d="${d}V8Z"/>`;
    for (let i = 0; i < prov; i++) { const y = 16 + (32 * (i + 0.5)) / Math.max(prov, 1); s += `<path class="ln" d="M50 ${(y - 3).toFixed(1)}H57V${(y + 3).toFixed(1)}H50"/>`; }
  } else if (F.fam === "type") {
    s += `<path class="pl" d="M32 3 61 32 32 61 3 32Z"/>`;
    if (k === "alias") s += `<path class="ln" d="M32 13 51 32 32 51 13 32Z"/>`;
    else if (k === "enum") { const n = Math.min(F.nVar || 0, 6); for (let i = 0; i < n; i++) { const y = 32 + (i - (n - 1) / 2) * 5.2; const w = 13 - Math.abs(y - 32) * 0.55; s += `<path class="ln" d="M${(32 - w).toFixed(1)} ${y.toFixed(1)}H${(32 + w).toFixed(1)}"/>`; } }
    else if (!F.marker) { const n = Math.min(F.nFld || 0, 5); for (let i = 0; i < n; i++) { const x = 32 + (i - (n - 1) / 2) * 6; s += `<path class="ln" d="M${x.toFixed(1)} 27V37"/>`; } if (!n) s += `<path class="ln" d="M24 32H40"/>`; }
  } else {
    s += `<circle class="pl" cx="32" cy="32" r="13"/><circle class="ln" cx="32" cy="32" r="21"/>`;
  }
  const y = Math.min(F.yours || 0, 6);
  for (let i = 0; i < y; i++) { const a = (Math.PI / 180) * (20 + i * 11); s += `<circle class="sy-yd" cx="${(32 + 31 * Math.cos(a)).toFixed(1)}" cy="${(32 + 31 * Math.sin(a)).toFixed(1)}" r="${size >= 40 ? 2.4 : 3.6}"/>`; }
  return `<svg class="sg ${F.fam} ${cls}" viewBox="0 0 64 64" width="${size}" height="${size}" style="--sw:${sw}">${s}</svg>`;
}

// ---------------------------------------------------------------- the model: one symbol, read
async function model(W, P, sname, mpath) {
  const D = await detail(P.id); if (!D) return null;
  const X = await insides(P.id), U = await USES();
  const byName = new Map();
  for (const m of D.modules) for (const it of m.items) (byName.get(it.n) || byName.set(it.n, []).get(it.n)).push({ it, mod: m.path, priv: !!m.private });
  const pick = (n, mod = null, qual = "") => {
    const all = byName.get(n); if (!all) return null;
    const q = (qual || "").replace(/^(crate|self|super)::/, "").replace(/::$/, "");
    return (q && all.find((e) => e.mod === q || e.mod.endsWith(q) || q.endsWith(e.mod))) || all.find((e) => e.mod === mod) || all.find((e) => !e.priv) || all[0];
  };
  let owner = null, item, mod, kind;
  if (sname.includes("::")) {
    const [on, mn] = sname.split("::");
    owner = pick(on, mpath); if (!owner) return null;
    const xm = ((X[`${owner.mod}::${on}`] || {}).m || []).find((x) => x.n === mn); if (!xm) return null;
    item = { n: mn, f: "callable", s: xm.s, d: xm.d }; mod = owner.mod; kind = "method";
  } else {
    const e = pick(sname, mpath); if (!e) return null;
    item = e.it; mod = e.mod; kind = read(item).kind;
  }
  const key = K(P.id, mod, sname);
  const xOf = (e) => X[`${e.mod}::${e.it.n}`] || null;
  const XI = owner ? null : X[`${mod}::${item.n}`] || null;
  const self = owner ? owner.it.n : item.n;
  const usedCount = (n) => P.used && P.used.get(n) || 0;
  const refOf = (n, qual = "") => {
    const e = pick(n, mod, qual); if (!e) return null;
    const k = K(P.id, e.mod, n);
    return { n, key: k, fam: e.it.f, kw: `${WORD[read(e.it).kind] || e.it.f}${e.mod !== mod ? ` · ${e.mod}` : ""}`, doc: e.it.d, cur: k === key, F: facts(e.it, xOf(e)) };
  };
  const mref = (on, omod, m) => { const k = K(P.id, omod, `${on}::${m.n}`); return { n: m.n, key: k, fam: "callable", kw: `method of ${on} · ${RECV[m.r] || "makes one"}`, doc: m.d, cur: k === key, F: facts({ n: m.n, f: "callable", s: m.s }, null, { kind: "method" }) }; };
  const cx = (s) => { const f = parseFn(s) || { gens: new Map() }; return { gens: f.gens, self, ref: (n, q) => (n === "Self" ? refOf(self) : refOf(n, q)) }; };
  const words = (t, s) => tw(t, cx(s));
  const re = new RegExp(`\\b${self}\\b`);
  const M = { W, P, D, X, U, key, mod, item, owner, sname, kind, fam: item.f, r: read(item), XI, refOf };
  M.sig = owner ? item.s : (XI && XI.S) || item.s;
  const yoursUses = P.dependentsP.filter((d) => d.kind === "yours").map((y) => ({ y, u: (U[`${y.id}>${P.id}`] || {}) }));
  // Two items can share a name (toml has de::Error and ser::Error); then only uses that spell out the
  // module count, and only lines that say `ser::Error` are shown.
  const topName = owner ? owner.it.n : item.n, topMod = owner ? owner.mod : mod;
  const twin = (byName.get(topName) || []).length > 1;
  const leaf = topMod.split("::").pop();
  const says = (t) => new RegExp(`\\b${leaf}::${topName}\\b`).test(t);
  const nameUse = (u) => {
    let rec = u[topName]; if (!rec) return null;
    if (twin) { const n = Object.entries(rec.q || {}).filter(([q]) => q && (q === topMod || q.endsWith(leaf) || topMod.endsWith(q))).reduce((a, [, c]) => a + c, 0); if (!n) return null; rec = { ...rec, n, l: rec.l.filter((l) => says(l[2])), m: {}, ml: {} }; }
    if (!owner) return rec;
    const n = rec.m && rec.m[item.n]; return n ? { n, l: (rec.ml && rec.ml[item.n]) || [] } : null;
  };
  M.F = facts(item, XI, { kind, yours: yoursUses.filter((x) => nameUse(x.u)).length });

  // ---- the pipe (fn, method)
  if (kind === "fn" || kind === "method") {
    const f = parseFn(M.sig) || { params: [], ret: "", recv: "", gens: new Map() };
    const c = cx(M.sig);
    const res = resultOf(f.ret, c);
    M.fn = {
      params: f.params.map((p) => ({ name: p.name, words: tw(p.type, c), mut: /^&\s*('\w+\s+)?mut\b/.test(p.type) })),
      out: res ? res.ok : f.ret ? tw(f.ret, c) : [], fail: res ? res.err : null, recv: f.recv, async: /\basync\b/.test(M.sig.split(/\bfn\b/)[0]),
    };
    if (owner) M.ownerRef = refOf(owner.it.n);
  }
  // ---- the record (struct, enum, union, alias)
  if (item.f === "type") {
    const T = XI || {};
    M.makers = []; M.takers = []; M.held = []; M.does = { reads: [], changes: [], consumes: [] };
    for (const m of T.m || []) {
      const f = parseFn(m.s); if (!f) continue;
      if (!m.r) { if (/\bSelf\b/.test(f.ret) || re.test(f.ret)) M.makers.push({ via: mref(self, mod, m), ins: f.params.flatMap((p, i) => [...(i ? [{ w: "," }] : []), ...words(p.type, m.s)]), fails: /Result\b/.test(f.ret) }); }
      else M.does[{ "&": "reads", "&mut": "changes", self: "consumes" }[m.r]].push({ ...mref(self, mod, m), gives: resultOf(f.ret, cx(m.s)) ? resultOf(f.ret, cx(m.s)).ok : words(f.ret, m.s) });
    }
    const TR = { From: "from", TryFrom: "try_from", FromStr: "parse", Default: "default", Deserialize: "deserialize", FromIterator: "collect" };
    for (const [tr, args] of T.impl || []) {
      if (!TR[tr] || /\$/.test(args)) continue;   // `From<$T>` is a macro's template, not a type
      const ins = tr === "FromStr" ? [{ p: "text" }] : tr === "Default" ? [{ p: "nothing" }] : tr === "Deserialize" ? [{ g: "any format" }] : tr === "FromIterator" ? [{ w: "many" }, ...words(args, "")] : words(args, "");
      if (M.makers.some((x) => x.trait === TR[tr] && JSON.stringify(x.ins) === JSON.stringify(ins))) continue;
      M.makers.push({ trait: TR[tr], by: tr, ins, fails: tr === "TryFrom" || tr === "FromStr" });
    }
    // other names of the package that give one or take one, read from their signatures
    const groups = new Map();
    for (const m of D.modules) for (const it of m.items) {
      if (it.n === self) continue;
      const e = { it, mod: m.path }, xe = xOf(e);
      if (it.f === "callable" && read(it).kind === "fn") {
        const S = (xe && xe.S) || it.s, f = parseFn(S); if (!f) continue;
        if (re.test(f.ret)) M.makers.push({ via: refOf(it.n, m.path), ins: f.params.flatMap((p, i) => [...(i ? [{ w: "," }] : []), ...words(p.type, S)]), fails: /Result\b/.test(f.ret) });
        else if (f.params.some((p) => re.test(p.type))) M.takers.push({ via: refOf(it.n, m.path), how: f.params.some((p) => re.test(p.type) && /&\s*mut/.test(p.type)) ? "changes it" : "takes it" });
      } else if (it.f === "type" && re.test(`${it.s} ${JSON.stringify((xe && (xe.var || xe.fld)) || "")}`)) {
        const al = it.s.match(/=\s*([^;]+);?$/);
        M.held.push({ via: refOf(it.n, m.path), words: al ? words(al[1], "") : [] });
      }
      if (it.f === "type" && xe && xe.m) for (const mm of xe.m) {
        const f = parseFn(mm.s); if (!f) continue;
        const gives = re.test(f.ret), takes = f.params.some((p) => re.test(p.type));
        if (!gives && !takes) continue;
        const g = groups.get(it.n) || groups.set(it.n, { owner: refOf(it.n, m.path), gives: [], takes: [] }).get(it.n);
        (gives ? g.gives : g.takes).push(mref(it.n, m.path, mm));
      }
    }
    for (const g of groups.values()) { if (g.gives.length) M.makers.push({ owner: g.owner, meths: g.gives }); if (g.takes.length) M.takers.push({ owner: g.owner, meths: g.takes }); }
    M.members = T.var ? { kind: "one of", rows: T.var.map(([n, pay]) => ({ n, words: pay ? (pay.startsWith("{") ? [{ x: pay }] : splitTop(pay).flatMap((x, i) => [...(i ? [{ w: "and" }] : []), ...words(x, "")])) : [{ w: "just the name" }] })) }
      : T.fld && T.fld.length ? { kind: T.tuple ? "wraps" : "holds", rows: T.fld.map(([n, t]) => ({ n: T.tuple ? "" : n, words: words(t, "") })) } : null;
    const al = kind === "alias" ? (item.s.match(/=\s*([^;]+);?$/) || [])[1] : null;
    if (al) M.alias = words(al, "");
    M.can = capabilities(T);
  }
  // ---- the socket (trait)
  if (kind === "trait") {
    const T = XI || {};
    M.sock = {
      req: (T.req || []).map(([n, s]) => ({ n, s, words: (() => { const f = parseFn(s); return f ? (f.ret ? words(resultOf(f.ret, cx(s)) ? f.ret.replace(/^.*?Result\s*<([^,>]+)[\s\S]*$/, "$1") : f.ret, s) : [{ p: "nothing" }]) : [{ w: "a type" }]; })(), fails: /Result\b/.test(s), assoc: !/\bfn\b/.test(s) })),
      prov: (T.prov || []).map(([n, s]) => ({ n, s, assoc: !/\bfn\b/.test(s) })),
      sup: T.sup ? splitTop(T.sup, "+").map((x) => x.trim()).filter((x) => !NOISE.test(x)).map((x) => refOf(x.replace(/<.*$/, "").split("::").pop()) || { x: x.replace(/<.*$/, "").split("::").pop() }) : [],
      by: (T.by || []).map((n) => refOf(n) || { x: n, fam: "type" }),
      ext: (T.ext || []).map((n) => refOf(n) || { x: n, fam: "contract" }),
      need: (T.need || []).map(([t, y]) => ({ ...(refOf(y) || { x: y, fam: "type" }), how: t })),
      needed: [],
    };
    for (const m of D.modules) for (const it of m.items) {
      if (it.n === self) continue;
      const xe = X[`${m.path}::${it.n}`];
      if (it.f === "callable" && read(it).kind === "fn" && re.test((xe && xe.S) || it.s)) M.sock.needed.push({ via: refOf(it.n, m.path) });
      if (xe && xe.m) { const ms = xe.m.filter((mm) => re.test(mm.s)); if (ms.length) M.sock.needed.push({ owner: refOf(it.n, m.path), meths: ms.map((mm) => mref(it.n, m.path, mm)) }); }
    }
  }
  if (kind === "const" || kind === "static") { const m = item.s.match(/:\s*([^=;]+)/); M.valueType = m ? words(m[1], "") : []; }

  // ---- relations back to us
  M.yours = [];
  M.notUsing = [];
  for (const { y, u } of yoursUses) {
    const rec = nameUse(u);
    if (rec) {
      const full = owner ? null : rec;
      M.yours.push({ y, n: rec.n, lines: rec.l || [], members: (full && full.m) || {}, ml: (full && full.ml) || {}, impl: (full && full.i) || [], ni: (full && full.ni) || 0 });
    } else {
      const names = Object.keys(u).filter((n) => n !== topName);   // a same-named item elsewhere is not a sibling
      if (!names.length) continue;
      const here = names.filter((n) => (byName.get(n) || []).some((e) => e.mod === mod));
      M.notUsing.push({ y, names: (here.length ? here : names).slice(0, 3), scope: here.length ? "this module" : "this package" });
    }
  }
  M.yours.sort((a, b) => b.n - a.n);
  M.total = M.yours.reduce((s, r) => s + r.n, 0);
  // who else uses it: registry dependents, with their own lines where the scan kept them
  const V = await VIA();
  const top = owner ? owner.it.n : item.n;
  M.others = [];
  for (const d of P.dependentsP.filter((q) => q.kind !== "yours")) {
    const ls = ((W.LINES[`${d.id}>${P.id}`] || {})[top] || []).filter((l) => (!owner || l[2].includes(item.n)) && (!twin || says(l[2])));
    const named = (V[`${d.id}>${P.id}`] || []).includes(top) && !twin;
    if (ls.length || (named && !owner)) M.others.push({ d, lines: ls });
  }
  M.others.sort((a, b) => b.lines.length - a.lines.length);
  // beneath: what its own file names in the packages it rests on (lines the scan kept)
  const file = (XI && XI.file) || (owner && (X[`${owner.mod}::${owner.it.n}`] || {}).file);
  M.file = file;
  M.beneath = [];
  if (file) for (const d of P.depsP) {
    const ex = W.LINES[`${P.id}>${d.id}`] || {};
    for (const [nm, ls] of Object.entries(ex)) for (const l of ls) if (l[0] === "src/" + file || l[0].endsWith("/" + file)) M.beneath.push({ d, n: nm, line: l });
  }
  if (M.beneath.length) {
    const dets = new Map(await Promise.all([...new Set(M.beneath.map((b) => b.d.id))].map(async (id) => [id, await detail(id)])));
    for (const b of M.beneath) {
      const DD = dets.get(b.d.id); let e = null;
      if (DD) for (const m of DD.modules) { const it = m.items.find((x) => x.n === b.n); if (it && (!e || !m.private)) e = { it, mod: m.path }; }
      b.ref = e ? { n: b.n, key: K(b.d.id, e.mod, b.n), fam: e.it.f, kw: `${WORD[read(e.it).kind] || e.it.f} · ${b.d.name}`, doc: e.it.d, far: true, F: facts(e.it, null) } : { x: b.n };
    }
  }
  // ---- the same name elsewhere: other packages with a public name like it, and who names those
  M.twins = [];
  if (!owner) {
    const tw2 = new Map();
    for (const [k, names] of Object.entries(V)) {
      if (!names.includes(top)) continue;
      const [a, b] = k.split(">");
      const bp = W.WD.byId.get(b), ap = W.WD.byId.get(a);
      if (!bp || !ap || bp.name === P.name) continue;
      (tw2.get(b) || tw2.set(b, { d: bp, users: [] }).get(b)).users.push({ a: ap, lines: (W.LINES[k] || {})[top] || [] });
    }
    M.twins = [...tw2.values()].sort((x, y) => y.users.filter((u) => u.a.kind === "yours").length - x.users.filter((u) => u.a.kind === "yours").length || y.users.length - x.users.length).slice(0, 3);
    for (const t of M.twins) {
      const DD = await detail(t.d.id); let e = null;
      if (DD) for (const m of DD.modules) { const it = m.items.find((x) => x.n === top); if (it && (!e || !m.private)) e = { it, mod: m.path }; }
      t.ref = e ? { n: top, key: K(t.d.id, e.mod, top), fam: e.it.f, kw: `${WORD[read(e.it).kind] || e.it.f} · ${t.d.name}`, doc: e.it.d, far: true, F: facts(e.it, null) } : null;
    }
  }
  // ---- its history across the releases on disk
  M.hist = await history(W, P, M);
  // ---- its siblings: the module's other names (or the module family's, when the module is one name)
  let sib = D.modules.filter((m) => m.path === mod), scope = mod;
  if (sib.reduce((s, m) => s + m.items.length, 0) < 4 && mod.includes("::")) { const fam = mod.split("::")[0]; sib = D.modules.filter((m) => m.path === fam || m.path.startsWith(fam + "::")); scope = fam; }
  M.sibScope = scope;
  M.sibs = sib.flatMap((m) => m.items.map((it) => ({ it, mod: m.path, key: K(P.id, m.path, it.n), F: facts(it, X[`${m.path}::${it.n}`], { yours: yoursUses.filter((x) => x.u[it.n]).length }) }))).slice(0, 40);
  if (owner && !M.sibs.some((s) => s.it.n === owner.it.n)) M.sibs.unshift({ it: owner.it, mod: owner.mod, key: K(P.id, owner.mod, owner.it.n), F: facts(owner.it, xOf(owner)) });
  return M;
}
const CAN = { Clone: ["copy", "clones"], Copy: ["copy", "copies"], PartialEq: ["eq", "compares"], Eq: null, PartialOrd: ["ord", "orders"], Ord: null, Hash: ["hash", "hashes"], Default: ["dflt", "has a default"], Display: ["print", "prints"], Debug: ["debug", "debug-prints"], Serialize: ["ser", "serializes"], Deserialize: ["de", "deserializes"], Iterator: ["iter", "iterates"], Error: ["error", "is an error"], Send: ["send", "crosses threads"], Sync: ["send", "shared across threads"], Index: ["index", "indexes"], IntoIterator: ["iter", "loops"] };
const CAN_TIP = { clones: "Clone: .clone() gives an owned copy.", copies: "Copy: assigning it copies, no move.", compares: "PartialEq: == works on it.", orders: "PartialOrd: < and > work on it.", hashes: "Hash: it can be a map key.", "has a default": "Default: T::default() gives one.", prints: "Display: it prints with {}.", "debug-prints": "Debug: it prints with {:?}.", serializes: "Serialize: any serde format can write it.", deserializes: "Deserialize: any serde format can read one.", iterates: "Iterator: you loop over it.", "is an error": "Error: it can be a ? error.", indexes: "Index: v[i] works on it.", loops: "IntoIterator: for x in it works." };
// One small drawn mark per capability, so a stack of them is still readable at a glance.
const CAPI = {
  copy: `<rect x="1.5" y="3.5" width="6" height="6"/><rect x="4.5" y="1.5" width="6" height="6"/>`,
  eq: `<path d="M2 4.5h8M2 7.5h8"/>`, ord: `<path d="M8.5 2.5 3.5 6l5 3.5"/>`, hash: `<path d="M4.5 1.5 3.5 10.5M8.5 1.5l-1 9M1.5 4.5h9M1.5 7.5h9"/>`,
  dflt: `<circle cx="6" cy="6" r="4"/><circle cx="6" cy="6" r="1" class="f"/>`, print: `<path d="M2 3h8M2 6h8M2 9h5"/>`, debug: `<path d="M4 1.5C2.5 1.5 3 6 1.5 6 3 6 2.5 10.5 4 10.5M8 1.5c1.5 0 1 4.5 2.5 4.5C9 6 9.5 10.5 8 10.5"/>`,
  ser: `<rect x="1.5" y="3" width="6" height="6"/><path d="M5 6h5.5M8.5 4l2 2-2 2"/>`, de: `<rect x="4.5" y="3" width="6" height="6"/><path d="M1 6h6M5 4l2 2-2 2"/>`,
  iter: `<path d="M2 3.5h6M2 6h6M2 8.5h6M9.5 7l1.5 1.5-1.5 1.5"/>`, error: `<rect x="2" y="2" width="8" height="8"/><path d="M4.3 4.3l3.4 3.4M7.7 4.3 4.3 7.7"/>`,
  send: `<path d="M1.5 6h9M7.5 3l3 3-3 3M1.5 3v6"/>`, index: `<path d="M3.5 2H2v8h1.5M8.5 2H10v8H8.5M6 4v4"/>`,
};
const capIcon = (k) => `<svg class="ic cap" viewBox="0 0 12 12" width="12" height="12">${CAPI[k] || ""}</svg>`;
function capabilities(T) {
  const out = [], seen = new Set();
  for (const t of [...(T.der || []), ...(T.impl || []).map(([n]) => n)]) { const c = CAN[t]; if (c && !seen.has(c[1])) { seen.add(c[1]); out.push({ k: c[0], label: c[1], tip: CAN_TIP[c[1]] || t }); } }
  return out;
}

// Where the symbol stands at each release read on disk, by the time-travel rule: before your pin a
// missing name hadn't arrived yet; after it, it's gone; a different first line is a change.
async function history(W, P, M) {
  const T = W.TRUST[P.id]; const locals = (T && T.local_versions) || [P.version];
  const pinAt = locals.indexOf(P.version);
  const norm = (s) => (s || "").replace(/^pub(\([^)]*\))?\s+/, "").replace(/\s+/g, " ").replace(/\s*\{\s*$/, "").trim();
  const noLife = (s) => norm(s).replace(/'\w+\s*,?\s*/g, "").replace(/<\s*>/g, "").replace(/\s+/g, " ");
  const here = norm(M.item.s);
  const out = [];
  for (const [i, v] of locals.entries()) {
    if (v === P.version) { out.push({ v, st: "pin", item: M.item }); continue; }
    const id = `${P.name}@${v}`;
    let found = null, moved = null;
    if (M.owner) {
      const Xv = await insides(id);
      for (const [k, r] of Object.entries(Xv)) if (k.endsWith("::" + M.owner.it.n) && r.m) { const mm = r.m.find((x) => x.n === M.item.n); if (mm) { found = { n: mm.n, f: "callable", s: mm.s, d: mm.d }; break; } }
    } else {
      const Dv = await detail(id);
      if (Dv) for (const m of Dv.modules) { const it = m.items.find((x) => x.n === M.item.n && x.f === M.item.f); if (it) { if (m.path === M.mod) { found = it; moved = null; break; } if (!found) { found = it; moved = m.path; } } }
      if (!Dv) { out.push({ v, st: "unread" }); continue; }
    }
    const before = i < pinAt;
    if (!found) { out.push({ v, st: before ? "absent" : "gone" }); continue; }
    const same = norm(found.s) === here;
    const note = moved ? `lived in ${moved}` : same ? "" : noLife(found.s) === noLife(M.item.s) ? "only its lifetimes differ" : "its first line differs";
    out.push({ v, st: moved || !same ? "chg" : "same", note, item: found, moved });
  }
  return out;
}

// ---------------------------------------------------------------- html: tokens, words, stacks
const kmark = (fam) => `<i class="km3 ${fam}"></i>`;
export function tok(r, cls = "") {
  if (!r) return "";
  if (r.x) return `<span class="sy-x">${esc(r.x)}</span>`;
  return `<span class="sy-tok ${r.fam}${r.cur ? " cur" : ""}${r.far ? " far" : ""} ${cls}" data-key="${esc(r.key)}" tabindex="0">${r.F ? sigil(r.F, 16, "mini") : kmark(r.fam)}<b>${esc(r.n)}</b><span class="sy-more"><em>${esc(r.kw)}</em>${r.doc ? esc(r.doc) : `<i>undocumented</i>`}</span></span>`;
}
export function wordsHtml(ws) {
  return ws.map((p) => (p.key ? tok(p) : p.p ? `<span class="sy-p"><i></i>${esc(p.p)}</span>` : p.g ? `<span class="sy-g"><i></i>${esc(p.g)}</span>` : p.x ? `<span class="sy-x">${esc(p.x)}</span>` : `<span class="sy-w">${esc(p.w)}</span>`)).join("");
}
// A compact group: its members' sigils stacked; hover spreads them and unrolls their names in place,
// a click opens the full list (each with what it gives).
function stack(refs, { label = "", cls = "", gives = false } = {}) {
  if (!refs.length) return "";
  const show = refs.slice(0, 5);
  return `<span class="sy-stack ${cls}" tabindex="0" data-n="${refs.length}">${label ? `<span class="sy-st-l">${esc(label)}</span>` : ""}<span class="sy-st-marks">${show.map((r) => sigil(r.F || { fam: r.fam || "type", kind: r.fam === "contract" ? "trait" : r.fam === "callable" ? "fn" : "struct" }, 16, "mini")).join("")}</span><b class="sy-st-n">${refs.length}</b>
    <span class="sy-st-list">${refs.map((r) => `<span class="sy-st-i">${r.key ? tok(r) : `<span class="sy-x plug">${esc(r.x)}</span>`}${r.how ? `<span class="sy-w">${esc(r.how === "Future" ? "a future" : r.how ? `for ${r.how}` : "")}</span>` : ""}${gives && r.gives && r.gives.length ? `<span class="sy-st-g"><span class="sy-w">gives</span>${wordsHtml(r.gives)}</span>` : ""}</span>`).join("")}</span></span>`;
}
const foldRows = (rows, html) => {
  if (rows.length <= 8) return rows.map(html).join("");
  return rows.slice(0, 7).map(html).join("") + `<div class="sy-row fold" tabindex="0" data-more="${esc(JSON.stringify(rows.length - 7))}"><span class="sy-w">and ${rows.length - 7} more</span></div><div class="sy-folded">${rows.slice(7).map(html).join("")}</div>`;
};

// ---------------------------------------------------------------- html: the page
function heroHtml(M) {
  const where = M.owner ? `${WORD.method} of ${M.owner.it.n}` : `${WORD[M.kind] || M.kind}`;
  const tags = M.r.tags.map(tagHtml).join("");
  const can = M.can && M.can.length ? `<span class="sy-can" tabindex="0"><b>can</b><span class="sy-cn-marks">${M.can.map((c) => capIcon(c.k)).join("")}</span><span class="sy-cn-list">${M.can.map((c) => `<span class="bdg" tabindex="0">${capIcon(c.k)}<b>${esc(c.label)}</b><em>${esc(c.tip)}</em></span>`).join("")}</span></span>` : "";
  const mem = M.members ? `<span class="bdg teal">${icon(M.members.kind === "one of" ? "iter" : "owner")}<b>${M.members.kind} ${M.members.rows.length}</b><em>${M.members.kind === "one of" ? "An enum: a value is exactly one of these cases." : "Its public fields."}</em></span>` : "";
  const sock = M.sock ? `<span class="bdg peri">${icon("marker")}<b>you write ${M.sock.req.length}</b><em>Members an implementor must provide.</em></span>${M.sock.prov.length ? `<span class="bdg">${icon("ctor")}<b>you get ${M.sock.prov.length}</b><em>Members it provides for free.</em></span>` : ""}` : "";
  const you = M.total ? `<span class="bdg mint you-b">${icon("you")}<b>${fmt(M.total)} in ${plural(M.yours.length, "crate")}</b><em>Named by your code: a path scan of your crates.</em></span>` : `<span class="bdg quiet">${icon("you")}<b>not named by your code</b></span>`;
  return `<div class="sy-hero"><div class="sy-hp ${M.fam}">
    <span class="sy-hm">${sigil(M.F, 64, "hero")}</span>
    <div class="sy-ht"><div class="sy-kw sy-u">${esc(where)}<i>·</i><span>${esc(M.P.name)}::${esc(M.mod)}</span></div>
      <h1>${esc(M.item.n)}${M.kind === "macro" ? "!" : ""}</h1>
      <div class="lede sy-u">${M.item.d ? esc(M.item.d) : `<span class="undoc">${icon("undoc")}undocumented</span>`}</div>
      <div class="sy-bdgs sy-u">${mem}${sock}${tags}${can}${you}</div></div>
    <div class="sy-hist sy-u">${histHtml(M)}</div></div></div>`;
}
function histHtml(M) {
  const T = M.W.TRUST[M.P.id], rel = (T && T.releases) || [];
  const dated = rel.filter((r) => r.t);
  if (!dated.length) return "";
  const w = 300, h = 40;
  const t0 = +new Date(dated[0].t), t1 = +NOW, X = (t) => 6 + ((+new Date(t) - t0) / Math.max(1, t1 - t0)) * (w - 12);
  const st = new Map(M.hist.map((x) => [x.v, x]));
  let prev = null, bars = "";
  for (const r of rel) {
    const k = kindOf(prev, r.v); if (!r.y) prev = r.v; if (!r.t) continue;
    const s = st.get(r.v), x = X(r.t), bh = k === "major" ? 24 : k === "minor" ? 14 : 8;
    bars += `<rect class="${k}${r.y ? " yank" : ""}${s ? " read " + s.st : ""}" data-v="${esc(r.v)}" x="${(x - 0.7).toFixed(1)}" y="${h - bh}" width="1.4" height="${bh}"/>`;
    if (s) bars += `<g class="cap ${s.st}" data-v="${esc(r.v)}"><rect x="${(x - 4.5).toFixed(1)}" y="${h - bh - 11}" width="9" height="9"/></g>`;
  }
  const years = [];
  for (let y = new Date(dated[0].t).getFullYear() + 1; y <= NOW.getFullYear(); y += 2) years.push(`<text x="${X(`${y}-01-01`).toFixed(1)}" y="${h + 12}">${y}</text>`);
  const chg = M.hist.filter((x) => x.st === "chg"), read = M.hist.filter((x) => x.st !== "unread");
  const cap = read.length <= 1 ? "only your pin is on disk: other releases not read" : chg.length ? `different at ${chg.length} of ${read.length} releases read` : M.hist.some((x) => x.st === "absent") ? `arrived after ${M.hist.find((x) => x.st === "absent").v}` : `the same at all ${read.length} releases read`;
  return `<div class="sy-hk">Its history <span>${esc(cap)}</span></div><div class="sy-hs" tabindex="0"><svg width="${w}" height="${h + 14}" viewBox="0 0 ${w} ${h + 14}"><line class="base" x1="0" x2="${w}" y1="${h}" y2="${h}"/>${years.join("")}${bars}</svg><div class="sy-hl"></div></div>`;
}

function bandHtml(M) {
  const z = { above: "", left: "", plate: "", below: "", right: "", kind: "" };
  const plate = (inner, cls = "") => `<div class="sy-plate ${M.fam} ${cls}" data-key="${esc(M.key)}">${sigil(M.F, 22, "pm")}<b>${esc(M.item.n)}${M.kind === "macro" ? "!" : ""}</b>${inner || ""}</div>`;
  if (M.fn) {
    const F = M.fn;
    z.kind = "sy-pipe";
    z.above = M.owner ? `<div class="sy-row a"><span class="sy-w">on</span>${tok(M.ownerRef)}<span class="sy-rw ${F.recv === "&mut" ? "am" : ""}">${esc(RECV[F.recv] || "")}</span></div>` : "";
    z.left = F.params.length ? F.params.map((p) => `<div class="sy-row l"><span class="sy-pn">${esc(p.name)}</span>${wordsHtml(p.words)}${p.mut ? `<span class="sy-rw am">written into</span>` : ""}</div>`).join("") : `<div class="sy-row l quiet"><span class="sy-w">takes nothing</span></div>`;
    z.plate = plate();
    z.right = `<div class="sy-row r"><span class="sy-k">gives</span>${F.out.length ? wordsHtml(F.out) : `<span class="sy-p"><i></i>nothing back</span>`}${F.async ? `<span class="sy-rw">when awaited</span>` : ""}</div>`;
    z.below = F.fail ? `<div class="sy-drop"><span class="sy-k co">or fails with</span>${wordsHtml(F.fail)}</div>` : "";
  } else if (M.sock) {
    const S = M.sock;
    z.kind = "sy-socket";
    z.above = S.sup.length ? `<div class="sy-row a"><span class="sy-w">builds on</span>${S.sup.map((r) => tok(r)).join(`<span class="sy-w">and</span>`)}</div>` : "";
    const pkgBy = S.by.filter((r) => r.key), extBy = S.by.filter((r) => !r.key);
    const mine = M.yours.filter((r) => r.impl.length);
    z.left = [
      ...mine.map((r) => `<div class="sy-row l mine"><span class="sy-crate">${gem(12, "var(--mint)")}${esc(short(r.y.name))}</span>${r.impl.slice(0, 3).map((i) => `<span class="sy-x y">${esc(i[0])}</span>`).join("")}<b class="sy-cnt y">${r.ni}</b></div>`),
      S.by.length ? `<div class="sy-row l"><span class="sy-k">done by</span>${stack(S.by, { label: `${M.P.name}'s own` })}</div>` : "",
    ].filter(Boolean).join("") || `<div class="sy-row l quiet"><span class="sy-w">no implementor read in ${esc(M.P.name)}</span></div>`;
    const row = (m, cls) => `<div class="sy-sk-r ${cls}"><svg class="sy-nt" viewBox="0 0 12 16" width="12" height="16"><path d="${cls === "req" ? "M0 1 10 8 0 15" : "M0 3H8V13H0"}"/></svg><b>${esc(m.n)}</b>${m.assoc ? `<span class="sy-w">a type</span>` : m.words ? `<span class="sy-w">gives</span>${wordsHtml(m.words)}` : ""}${m.fails ? `<span class="sy-q">?</span>` : ""}</div>`;
    z.plate = plate(`<div class="sy-sk">${S.req.length ? `<div class="sy-sk-h">you write <b>${S.req.length}</b></div>${S.req.slice(0, 6).map((m) => row(m, "req")).join("")}${S.req.length > 6 ? `<div class="sy-sk-r more">and ${S.req.length - 6} more</div>` : ""}` : `<div class="sy-sk-h">nothing to write: a marker</div>`}${S.prov.length ? `<div class="sy-sk-h">you get <b>${S.prov.length}</b></div>${S.prov.slice(0, 4).map((m) => row(m, "prov")).join("")}${S.prov.length > 4 ? `<div class="sy-sk-r more">and ${S.prov.length - 4} more</div>` : ""}` : ""}</div>`, "sock");
    const rights = [];
    if (S.ext.length) rights.push(`<div class="sy-row r"><span class="sy-k">every one also gets</span>${S.ext.map((r) => (r.key ? tok(r) : `<span class="sy-x plug">${esc(r.x)}</span>`)).join("")}</div>`);
    if (S.need.length) rights.push(`<div class="sy-row r"><span class="sy-k">asked for by</span>${stack(S.need, { label: `${M.P.name}'s own` })}</div>`);
    for (const r of S.needed) rights.push(r.via ? `<div class="sy-row r"><span class="sy-k">asked for by</span>${tok(r.via)}</div>` : `<div class="sy-row r">${tok(r.owner)}${stack(r.meths, { label: "asks in" })}</div>`);
    z.right = rights.length ? rights.join("") : `<div class="sy-row r quiet"><span class="sy-w">nothing in ${esc(M.P.name)} asks for it by name</span></div>`;
  } else if (M.makers) {
    z.kind = "sy-record";
    const mk = (r) => r.owner ? `<div class="sy-row l">${tok(r.owner)}${stack(r.meths, { label: "gives one", gives: false })}</div>`
      : r.trait ? `<div class="sy-row l">${wordsHtml(r.ins)}<span class="sy-mini" tabindex="0"><b>${esc(r.trait)}</b>${r.fails ? `<span class="sy-q">?</span>` : ""}<em>by ${esc(r.by)}</em></span></div>`
      : `<div class="sy-row l">${wordsHtml(r.ins.length ? r.ins : [{ p: "nothing" }])}${tok(r.via)}${r.fails ? `<span class="sy-q">?</span>` : ""}</div>`;
    const makers = M.makers.slice().sort((a, b) => !!a.owner - !!b.owner);
    z.left = makers.length ? foldRows(makers, mk) : `<div class="sy-row l quiet"><span class="sy-w">no maker read in ${esc(M.P.name)}</span></div>`;
    z.plate = plate(M.alias ? `<span class="sy-al"><span class="sy-w">is</span>${wordsHtml(M.alias)}</span>` : "");
    const D = M.does;
    const rights = [];
    if (D.reads.length) rights.push(`<div class="sy-row r">${stack(D.reads, { label: "reads it", gives: true })}</div>`);
    if (D.changes.length) rights.push(`<div class="sy-row r">${stack(D.changes, { label: "changes it", cls: "am", gives: true })}</div>`);
    if (D.consumes.length) rights.push(`<div class="sy-row r">${stack(D.consumes, { label: "consumes it", gives: true })}</div>`);
    for (const t of M.takers) rights.push(t.owner ? `<div class="sy-row r">${tok(t.owner)}${stack(t.meths, { label: "takes one" })}</div>` : `<div class="sy-row r"><span class="sy-k">${esc(t.how)}</span>${tok(t.via)}</div>`);
    for (const h of M.held) rights.push(`<div class="sy-row r"><span class="sy-k">held in</span>${tok(h.via)}${h.words.length ? `<span class="sy-w">=</span>${wordsHtml(h.words)}` : ""}</div>`);
    z.right = rights.length ? rights.join("") : `<div class="sy-row r quiet"><span class="sy-w">no method read</span></div>`;
  } else if (M.kind === "macro") {
    z.kind = "sy-pipe";
    z.left = `<div class="sy-row l"><span class="sy-p"><i></i>your tokens</span></div>`;
    z.plate = plate();
    z.right = `<div class="sy-row r"><span class="sy-k">writes</span><span class="sy-p"><i></i>code, in place</span></div>`;
  } else {
    z.kind = "sy-pipe";
    z.plate = plate();
    z.right = `<div class="sy-row r"><span class="sy-k">is</span>${wordsHtml(M.valueType || [])}</div>`;
  }
  return `<div class="sy-band ${z.kind}" style="--hue:${HUE[M.fam] || "var(--ink2)"}"><svg class="sy-lines"></svg><div class="z above">${z.above}</div><div class="z left">${z.left}</div><div class="z plate">${z.plate}</div><div class="z below">${z.below}</div><div class="z right">${z.right}</div></div>`;
}

// The reach bar: the relation between your code and it, as segments you can scrub. For a type, the
// members your code reaches through it; otherwise the crates that name it. Scrubbing lights the same
// member in the band and brings its lines to the top of every deck.
function reachSegs(M) {
  const by = new Map();
  for (const r of M.yours) for (const [m, n] of Object.entries(r.members || {})) by.set(m, (by.get(m) || 0) + n);
  if (by.size && !M.owner) {
    const segs = [...by.entries()].sort((a, b) => b[1] - a[1]).map(([m, n]) => ({ id: m, n, label: m, kind: "m" }));
    const bare = M.total - segs.reduce((s, x) => s + x.n, 0);
    if (bare > 0) segs.push({ id: "", n: bare, label: `${M.item.n} itself`, kind: "bare" });
    return segs;
  }
  return M.yours.map((r) => ({ id: r.y.name, n: r.n, label: short(r.y.name), kind: "c" }));
}
function deckHtml(r, M, lines, cls = "") {
  if (!lines.length) return `<div class="sy-deck empty"><span class="quiet">named ${plural(r.n || 0, "time")}; no line kept</span></div>`;
  const nm = M.owner ? M.item.n : M.item.n;
  // the line is cropped around the name, so the evidence is always in view
  const crop = (t) => { const i = t.search(new RegExp(`\\b${nm}\\b`)); return i > 38 ? "…" + t.slice(i - 28) : t; };
  const hl = (t) => esc(crop(t)).replace(new RegExp(`\\b(${nm})\\b`, "g"), "<u>$1</u>");
  const mems = (t) => Object.keys(r.members || {}).filter((m) => new RegExp(`(::|\\.)${m}\\b`).test(t)).join(" ");
  const extra = Math.max(0, (r.n || lines.length) - lines.length);
  return `<div class="sy-deck ${cls}" tabindex="0" style="--n:${Math.min(lines.length + (extra ? 1 : 0), 5)}" data-count="${r.n || lines.length}">${lines.map(([f, l, t], i) => `<div class="sy-dk${i > 4 ? " deep" : ""}" style="--i:${i};--j:${i}" data-m="${esc(mems(t))}"><em>${esc(f)}:${l}</em><code>${hl(t)}</code></div>`).join("")}${extra ? `<div class="sy-dk more${lines.length > 4 ? " deep" : ""}" style="--i:${lines.length};--j:${lines.length}"><span>and ${extra} more counted, lines not kept</span></div>` : ""}</div>`;
}
function yoursHtml(M) {
  const segs = reachSegs(M);
  const tot = segs.reduce((s, x) => s + x.n, 0) || 1;
  const scrub = segs.length ? `<div class="sy-reach" tabindex="0"><div class="sy-rk">${segs[0].kind === "m" ? "What your code reaches through it" : "Which crates name it"}<span>scrub</span></div><div class="sy-rb">${segs.map((s) => `<i class="${s.kind}" data-id="${esc(s.id)}" style="flex:${s.n}"><b>${esc(s.label)}</b><em>${s.n}</em></i>`).join("")}</div><div class="sy-rc"></div></div>` : "";
  const max = Math.max(1, ...M.yours.map((r) => r.n));
  const rows = M.yours.map((r) => `<div class="sy-yr" data-crate="${esc(r.y.name)}"><div class="sy-yh"><span class="sy-yc">${gem(12, "var(--mint)")}${esc(short(r.y.name))}</span><span class="sy-yb"><i style="width:${(r.n / max) * 100}%"></i></span><span class="sy-yn">${r.n}</span>${Object.keys(r.members).length ? `<span class="sy-ym">${Object.entries(r.members).slice(0, 4).map(([m, n]) => `<span data-m="${esc(m)}">${esc(m)}<i>${n}</i></span>`).join("")}</span>` : ""}${r.impl.length ? `<span class="sy-ym">${r.impl.slice(0, 3).map((i) => `<span>${esc(i[0])}</span>`).join("")}${r.ni > 3 ? `<span><i>+${r.ni - 3}</i></span>` : ""}</span>` : ""}</div>${deckHtml(r, M, r.lines)}</div>`).join("");
  const nots = M.notUsing.slice(0, 3).map((x) => `<div class="sy-not">${gem(10, "var(--mint)")}<b>${esc(short(x.y.name))}</b> uses ${x.names.map((n) => `<span class="nm">${esc(n)}</span>`).join(", ")} from ${x.scope}, not this</div>`).join("");
  const body = M.yours.length ? scrub + `<div class="sy-yrs">${rows}</div>` + nots : `<div class="sy-none">None of your crates name it${M.notUsing.length ? "." : `, and none name anything of ${esc(M.P.name)} directly.`}</div>` + nots;
  return sec("yours", "Your code and it", M.total ? `${fmt(M.total)}<small>places</small>` : `0<small>places</small>`, M.yours.length ? `named ${plural(M.total, "time")} by ${plural(M.yours.length, "crate")}` : "", body);
}
function othersHtml(M) {
  if (!M.others.length) return "";
  const withLines = M.others.filter((o) => o.lines.length), bare = M.others.filter((o) => !o.lines.length);
  const rows = withLines.slice(0, 4).map((o) => `<div class="sy-yr other" data-pid="${esc(o.d.id)}"><div class="sy-yh"><span class="sy-yc o"><a class="pk" data-pkg="${esc(o.d.id)}">${esc(o.d.name)}</a></span></div>${deckHtml({ n: o.lines.length }, M, o.lines, "o")}</div>`).join("");
  const more = bare.length ? `<div class="sy-not o">${bare.slice(0, 6).map((o) => `<a class="pk" data-pkg="${esc(o.d.id)}">${esc(o.d.name)}</a>`).join(", ")}${bare.length > 6 ? ` and ${bare.length - 6} more` : ""} name it too</div>` : "";
  return sec("others", M.yours.length ? "Other packages that use it" : "Who uses it instead", `${M.others.length}<small>${M.others.length === 1 ? "package" : "packages"}</small>`, "in your library, from their own source", rows + more);
}
function twinsHtml(M) {
  if (!M.twins.length) return "";
  const rows = M.twins.map((t) => {
    const yours = t.users.filter((u) => u.a.kind === "yours"), rest = t.users.filter((u) => u.a.kind !== "yours");
    const ex = t.users.find((u) => u.lines.length);
    return `<div class="sy-bn"><span class="sy-yc o"><a class="pk" data-pkg="${esc(t.d.id)}">${esc(t.d.name)}</a></span><div class="sy-bl"><div class="sy-bi">${t.ref ? tok(t.ref) : `<span class="sy-x">${esc(M.item.n)}</span>`}<span class="sy-tw">named by ${yours.length ? `<b class="y">${plural(yours.length, "crate")} of yours</b>${rest.length ? " and " : ""}` : ""}${rest.length ? `${rest.slice(0, 3).map((u) => `<a class="pk" data-pkg="${esc(u.a.id)}">${esc(u.a.name)}</a>`).join(", ")}${rest.length > 3 ? ` +${rest.length - 3}` : ""}` : ""}</span></div>${ex ? `<div class="sy-bi"><em>${esc(short(ex.a.name))} · ${esc(ex.lines[0][0].replace(/^src\//, ""))}:${ex.lines[0][1]}</em><code>${esc(ex.lines[0][2]).replace(new RegExp(`\\b(${M.item.n})\\b`), "<u>$1</u>")}</code></div>` : ""}</div></div>`;
  }).join("");
  return sec("twins", `Also called ${M.item.n}`, `${M.twins.length}<small>elsewhere</small>`, "the same name in other packages of your library, and who names those", rows);
}
function beneathHtml(M) {
  if (!M.beneath.length) return "";
  const byPkg = new Map();
  for (const b of M.beneath) (byPkg.get(b.d.id) || byPkg.set(b.d.id, { d: b.d, list: [] }).get(b.d.id)).list.push(b);
  const rows = [...byPkg.values()].map((g) => `<div class="sy-bn"><span class="sy-yc o"><a class="pk" data-pkg="${esc(g.d.id)}">${esc(g.d.name)}</a></span><div class="sy-bl">${g.list.slice(0, 3).map((b) => `<div class="sy-bi">${tok(b.ref)}<em>${esc(b.line[0].replace(/^src\//, ""))}:${b.line[1]}</em><code>${esc(b.line[2]).replace(new RegExp(`\\b(${b.n})\\b`), "<u>$1</u>")}</code></div>`).join("")}</div></div>`).join("");
  return sec("beneath", "What its file stands on", `${byPkg.size}<small>${byPkg.size === 1 ? "package" : "packages"}</small>`, `names in ${esc(M.file)} from the packages ${esc(M.P.name)} rests on`, rows);
}
function sec(id, title, stub, sub, body) {
  return `<section class="sy-sec sy-u" data-sec="${id}"><div class="sy-stub">${stub}</div><div class="sy-sh"><h2>${esc(title)}</h2>${sub ? `<span>${sub}</span>` : ""}</div>${body}</section>`;
}
// What it holds hangs from the spine under the band: an enum's cases, a struct's fields, three to a row.
function membersHtml(M) {
  if (!M.members || !M.members.rows.length) return "";
  const rows = M.members.rows;
  const one = (r) => `<div class="sy-mem">${r.n ? `<b class="sy-mn">${esc(r.n)}</b>` : ""}<span class="sy-mw">${wordsHtml(r.words)}</span></div>`;
  const fold = rows.length > 9;
  return `<div class="sy-members sy-u"><div class="sy-mh">${esc(M.members.kind)} <b>${rows.length}</b></div><div class="sy-mg">${(fold ? rows.slice(0, 8) : rows).map(one).join("")}${fold ? `<div class="sy-mem fold" tabindex="0">and ${rows.length - 8} more</div><div class="sy-folded">${rows.slice(8).map(one).join("")}</div>` : ""}</div></div>`;
}
function pageHtml(M) {
  return `${heroHtml(M)}<div class="sy-u sy-bw">${bandHtml(M)}</div>${membersHtml(M)}${yoursHtml(M)}${othersHtml(M)}${beneathHtml(M)}${twinsHtml(M)}
    <div class="sy-foot sy-u">Relations are read from signatures inside ${esc(M.P.name)} (one name inside another's) and from a path scan of your crates, not from a compiler: real, and approximate.</div><svg class="sy-ink"></svg>`;
}

// ---------------------------------------------------------------- ink: the spine and the band's lines
const rel = (r, o) => ({ x: r.left - o.left, y: r.top - o.top, w: r.width, h: r.height, r: r.right - o.left, b: r.bottom - o.top });
function layoutBand(page) {
  const band = page.querySelector(".sy-band"); if (!band) return;
  const Z = (k) => band.querySelector(`.z.${k}`);
  const L = Z("left"), R = Z("right"), Pl = Z("plate"), A = Z("above"), B = Z("below");
  const W = band.clientWidth;
  const px = SP + 26;
  L.style.width = SP - 40 + "px"; L.style.left = "0px";
  Pl.style.left = px + "px"; A.style.left = px + "px"; B.style.left = px + 14 + "px";
  const pw = Pl.offsetWidth, ph = Pl.offsetHeight, bw = B.offsetWidth;
  const rx = px + Math.max(pw, bw) + 64;
  R.style.left = rx + "px"; R.style.width = Math.max(200, W - rx) + "px";
  const lh = L.offsetHeight, rh = R.offsetHeight, ah = A.offsetHeight, bh = B.offsetHeight;
  const mid = Math.max(ah ? ah + 30 + ph / 2 : ph / 2, lh / 2, rh / 2, 20);
  const pt = mid - ph / 2;
  Pl.style.top = pt + "px"; L.style.top = mid - lh / 2 + "px"; R.style.top = mid - rh / 2 + "px";
  A.style.top = pt - 30 - ah + "px"; B.style.top = pt + ph + 22 + "px";
  band.style.height = Math.max(mid + lh / 2, mid + rh / 2, pt + ph + (bh ? 22 + bh : 0)) + 14 + "px";
  inkBand(band);
}
function growBand(band) {
  const o = band.getBoundingClientRect();
  const bottom = Math.max(...[...band.querySelectorAll(":scope > .z")].map((z) => z.getBoundingClientRect().bottom - o.top));
  const h = Math.max(bottom + 14, +(band.dataset.h || 0));
  if (!band.dataset.h) band.dataset.h = band.offsetHeight;
  band.style.height = Math.max(+band.dataset.h, bottom + 14) + "px";
}
function inkBand(band) {
  const o = band.getBoundingClientRect();
  const svg = band.querySelector(".sy-lines");
  svg.setAttribute("width", o.width); svg.setAttribute("height", o.height);
  const P = band.querySelector(".sy-plate"); if (!P) return;
  const p = rel(P.getBoundingClientRect(), o), my = p.y + Math.min(p.h / 2, 22);
  let s = "";
  const cxL = p.x - 16;
  for (const row of band.querySelectorAll(".z.left > .sy-row:not(.fold)")) {
    if (row.closest(".sy-folded") && !row.closest(".sy-folded.on")) continue;
    const r = rel(row.getBoundingClientRect(), o), ax = r.r + 8, ay = r.y + r.h / 2, dy = Math.abs(ay - my);
    const kx = Math.max(ax + 6, cxL - dy);
    s += `<path class="${row.classList.contains("mine") ? "y" : ""}" pathLength="1" d="M${ax} ${ay}H${kx}L${cxL} ${my}H${p.x}"/>`;
  }
  const outX = p.r + 16;
  for (const row of band.querySelectorAll(".z.right > .sy-row:not(.fold)")) {
    if (row.closest(".sy-folded") && !row.closest(".sy-folded.on")) continue;
    const r = rel(row.getBoundingClientRect(), o), bx = r.x - 10, by = r.y + Math.min(r.h / 2, 14), dy = Math.abs(by - my);
    const kx = Math.min(bx - 6, outX + dy);
    s += `<path pathLength="1" data-for="${esc(row.querySelector("[data-key]") ? "" : "")}" d="M${p.r} ${my}H${outX}L${kx} ${by}H${bx}"/><path class="hd" d="M${bx - 5} ${by - 4}L${bx} ${by}L${bx - 5} ${by + 4}"/>`;
  }
  const above = band.querySelector(".z.above > .sy-row");
  if (above) { const a = rel(above.getBoundingClientRect(), o); s += `<path pathLength="1" d="M${p.x + 22} ${a.b + 2}V${p.y}"/>`; }
  const drop = band.querySelector(".z.below > .sy-drop");
  if (drop) { const d = rel(drop.getBoundingClientRect(), o); s += `<path class="co" pathLength="1" d="M${p.x + 22} ${p.b}V${d.y + d.h / 2}H${d.x - 6}"/>`; }
  svg.innerHTML = s;
}
function inkPage(page) {
  const o = page.getBoundingClientRect();
  const svg = page.querySelector(".sy-ink");
  svg.setAttribute("width", o.width); svg.setAttribute("height", page.scrollHeight);
  const hm = page.querySelector(".sy-hm"); if (!hm) return;
  const m = rel(hm.getBoundingClientRect(), o), x = m.x + m.w / 2;
  const secs = [...page.querySelectorAll(".sy-sec .sy-sh")];
  const lastY = secs.length ? rel(secs[secs.length - 1].getBoundingClientRect(), o).y + 8 : m.b + 200;
  let s = `<path class="sy-spine" pathLength="1" d="M${x} ${m.b + 2}V${lastY}"/>`;
  for (const h of secs) { const r = rel(h.getBoundingClientRect(), o), y = r.y + 8, k = h.parentElement.dataset.sec; s += `<path class="sy-mk ${k}" d="M${x} ${y - 5}L${x + 5} ${y}L${x} ${y + 5}L${x - 5} ${y}Z"/><path class="sy-tk" d="M${x + 7} ${y}H${r.x - 10}"/>`; }
  // members: a tine from the spine to the first cell of each row
  const cells = [...page.querySelectorAll(".sy-members .sy-mem")].filter((m) => m.offsetParent);
  const rowsY = new Map();
  for (const c of cells) { const r = rel(c.getBoundingClientRect(), o); const y = Math.round(r.y + r.h / 2); if (!rowsY.has(y)) rowsY.set(y, r.x); }
  for (const [y, cx0] of rowsY) s += `<path class="sy-tine" pathLength="1" d="M${x} ${y}H${cx0 - 6}"/>`;
  svg.innerHTML = s;
}

// ---------------------------------------------------------------- the controller
export function mountSymbols({ WD, TRUST, LINES, reader, jump, side, find }) {
  const W = { WD, TRUST, LINES };
  const S = { root: null, page: null, M: null, trail: [], busy: false, folioHost: null, saved: null, strip: null, at: null };

  function shell() {
    const root = document.createElement("div"); root.className = "sy-root";
    root.innerHTML = `<div class="sy-top"><span class="sy-mod" tabindex="0"></span><div class="sy-strip"><div class="sy-rail"></div><i class="sy-ring"></i></div></div><div class="sy-scroll"></div>`;
    reader.appendChild(root);
    root.addEventListener("click", onClick);
    root.addEventListener("mousemove", onMove);
    root.addEventListener("mouseleave", () => scrubAt(null));
    return root;
  }
  async function mount(M, { hidden = false } = {}) {
    const sc = S.root.querySelector(".sy-scroll");
    const page = document.createElement("div"); page.className = "sy-page" + (hidden ? " entering" : "");
    page.style.setProperty("--sp", SP + "px");
    page.style.setProperty("--hue", HUE[M.fam] || "var(--ink2)");
    page.innerHTML = pageHtml(M);
    sc.appendChild(page);
    await document.fonts.ready;
    layoutBand(page); inkPage(page);
    const band = page.querySelector(".sy-band");
    if (band && window.ResizeObserver) { const ro = new ResizeObserver(() => { growBand(band); inkBand(band); inkPage(page); }); page.querySelectorAll(".z.left, .z.right, .z.below").forEach((z) => ro.observe(z)); page._ro = ro; }
    return page;
  }
  function drawStrip(M, animate) {
    const top = S.root.querySelector(".sy-top");
    const scopeKey = `${M.P.id}|${M.sibScope}`;
    const mod = top.querySelector(".sy-mod");
    mod.innerHTML = `<span class="bk">‹</span>${gem(14, "var(--k-ns)")}<b>${esc(M.P.name)}</b><span class="sep">::</span>${esc(M.sibScope)}<i>${M.sibs.length}</i>`;
    const rail = top.querySelector(".sy-rail");
    const rebuilt = S.strip !== scopeKey;
    if (rebuilt) {
      const ORDER = ["unsafe", "async", "fail", "maybe", "iter", "error", "mutates", "borrow", "marker", "owner", "ctor"];
      rail.innerHTML = M.sibs.map((s) => { const r = read(s.it); const b = ORDER.map((k) => r.tags.find((t) => t.k === k && !(k === "ctor" && /^takes/.test(t.label) && r.tags.some((x) => x.k === "fail")))).find(Boolean); return `<span class="sy-sib ${s.it.f}" data-key="${esc(s.key)}" data-n="${esc(s.it.n)}" tabindex="0">${sigil(s.F, 18, "mini")}<b>${esc(s.it.n)}</b>${b ? `<em class="${b.tone || ""}">${esc(b.label)}</em>` : `<em>${esc(WORD[r.kind] || r.kind)}</em>`}</span>`; }).join("");
      S.strip = scopeKey;
      rail.style.transition = "none"; rail.style.transform = "translateX(0px)";
    }
    const curKey = M.owner ? K(M.P.id, M.owner.mod, M.owner.it.n) : M.key;
    rail.querySelectorAll(".sy-sib").forEach((c) => c.classList.toggle("cur", c.dataset.key === curKey));
    const cur = rail.querySelector(".sy-sib.cur");
    const strip = top.querySelector(".sy-strip"), ring = top.querySelector(".sy-ring");
    const sw = strip.clientWidth, rw = rail.scrollWidth;
    let tx = 0;
    if (cur && rw > sw) tx = clamp(sw / 2 - (cur.offsetLeft + cur.offsetWidth / 2), sw - rw - 8, 0);
    const to = { tx, l: cur ? cur.offsetLeft + tx - 3 : 0, w: cur ? cur.offsetWidth + 6 : 0 };
    const from = animate && S.stripAt && !rebuilt ? S.stripAt : to;
    S.stripAt = to;
    ring.style.opacity = cur ? "1" : "0";
    strip.classList.toggle("lf", tx < 0); strip.classList.toggle("rf", rw + tx > sw + 2);
    // the rail slides and the ring moves on the move's own clock (slide(u), u from 0 to 1)
    slide = (u) => { rail.style.transform = `translateX(${lerp(from.tx, to.tx, u)}px)`; ring.style.left = lerp(from.l, to.l, u) + "px"; ring.style.width = lerp(from.w, to.w, u) + "px"; };
    rail.style.transition = "none"; ring.style.transition = "none";
    slide(animate ? 0 : 1);
  }
  let slide = () => {};
  function drawJump() {
    const parts = [`<span class="c" data-lib="1">Library</span><span class="rel">›</span>`];
    let prevP = null;
    S.trail.forEach((t, i) => {
      const M = t.M, last = i === S.trail.length - 1;
      if (M.P !== prevP) { parts.push(`${i ? `<span class="rel">›</span>` : ""}<span class="c pc" data-pk="${esc(M.P.id)}">${esc(M.P.name)}</span><span class="rel">›</span>`); prevP = M.P; }
      else parts.push(`<span class="rel d">${t.rel || "·"}</span>`);
      if (last) parts.push(`${S.trail.length === 1 ? `<span class="md">${esc(M.mod)}</span><span class="rel">›</span>` : ""}<b>${esc(M.sname)}</b>`);
      else parts.push(`<span class="c" data-i="${i}">${esc(M.sname)}</span>`);
    });
    jump.innerHTML = parts.join(" ");
    jump.onclick = (e) => {
      const c = e.target.closest(".c"); if (!c) return;
      if (c.dataset.lib) { location.href = "?v=browse&hover=" + encodeURIComponent(S.M.P.name); return; }
      if (c.dataset.pk) { back(); return; }
      const i = +c.dataset.i; if (Number.isFinite(i)) hopTo(S.trail[i].M, null, { rewind: i });
    };
  }
  function drawSide(M) {
    const T = TRUST[M.P.id];
    const mods = M.D.modules.filter((m) => m.items.length && (!m.private || m.path === M.mod)).slice(0, 18);
    side.innerHTML = `<div class="scope"><span class="up">‹ ${esc(M.P.name)}</span></div>
      <div class="scope" style="padding-top:4px">${gem(22, "var(--k-ns)")}${esc(M.P.name)} <span class="n">${esc(M.P.version)}</span></div>
      <div class="lens"><span class="on">Symbol</span><span>Contents<i>${fmt(M.D.modules.reduce((s, m) => s + m.items.length, 0))}</i></span></div>
      <div class="hint">${T && T.local_versions && T.local_versions.length > 1 ? `${T.local_versions.length} releases on disk` : "one release on disk"}</div>
      <div class="rows sy-side">${mods.map((m) => `<div class="r ${m.path === M.mod ? "cur" : ""}"><span class="km2 ${m.items[0] ? m.items[0].f : "value"}" style="transform:scale(.8)"></span>${esc(m.path)}<span class="g">${m.items.length}</span></div>${m.path === M.mod ? m.items.slice(0, 14).map((it) => `<div class="r in ${it.n === (M.owner ? M.owner.it.n : M.item.n) ? "on" : ""}" data-key="${esc(K(M.P.id, m.path, it.n))}">${sigil(facts(it, M.X[`${m.path}::${it.n}`]), 14, "mini")}${esc(it.n)}${M.P.used && M.P.used.get(it.n) ? `<span class="g y">${M.P.used.get(it.n)}</span>` : ""}</div>`).join("") : ""}`).join("")}</div>`;
    side.onclick = (e) => { const r = e.target.closest("[data-key]"); if (r) hopKey(r.dataset.key, r); else if (e.target.closest(".up")) back(); };
  }

  // ---------------------------------------------------------------- interactions inside the page
  function onClick(e) {
    const fold = e.target.closest(".sy-row.fold, .sy-mem.fold");
    if (fold) { fold.classList.add("gone"); fold.nextElementSibling.classList.add("on"); return; }
    const st = e.target.closest(".sy-stack");
    if (st && !e.target.closest(".sy-tok")) { st.classList.toggle("open"); return; }
    const dk = e.target.closest(".sy-deck");
    if (dk && !e.target.closest("a")) { dk.classList.toggle("open"); return; }
    const cn = e.target.closest(".sy-can");
    if (cn && !e.target.closest(".bdg")) { cn.classList.toggle("open"); return; }
    if (e.target.closest(".sy-mod")) { back(); return; }
    // a click on the history pins the release being read (Esc, or a click on your pin, returns)
    if (e.target.closest(".sy-hist .sy-hs") && S.histHot) { const h = S.M.hist.find((x) => x.v === S.histHot); S.at = h && h.st !== "pin" && h.item ? h.v : null; travel(S.at ? h : null); return; }
    const pk = e.target.closest("a.pk[data-pkg]");
    if (pk) { e.stopPropagation(); const d = WD.byId.get(pk.dataset.pkg); if (d) location.href = `?v=package&p=${encodeURIComponent(d.id)}`; return; }
    const t = e.target.closest("[data-key]");
    if (t && !t.classList.contains("cur") && t.dataset.key !== S.M.key && !t.classList.contains("sy-plate")) hopKey(t.dataset.key, t);
  }
  function onMove(e) {
    const rb = e.target.closest(".sy-reach .sy-rb");
    if (rb) { const r = rb.getBoundingClientRect(); scrubAt(rb, (e.clientX - r.left) / r.width); return; }
    if (S.scrubbing) scrubAt(null);
    const hs = e.target.closest(".sy-hist .sy-hs");
    if (hs) { const r = hs.getBoundingClientRect(); histAt(hs, e.clientX - r.left); return; }
    if (S.histHot) histAt(null);
  }
  // Scrubbing the reach bar: the segment under the pointer widens (its neighbours give way), the band
  // lights the same member, and every deck brings the lines that reach it to the front.
  function scrubAt(rb, u) {
    const page = S.page; if (!page) return;
    const reach = page.querySelector(".sy-reach");
    if (!rb || !reach) {
      S.scrubbing = false;
      if (reach) { reach.classList.remove("on"); reach.querySelectorAll(".sy-rb i").forEach((i) => { i.style.flexGrow = i.dataset.g || i.style.flexGrow; i.classList.remove("hot"); }); reach.querySelector(".sy-rc").innerHTML = ""; }
      page.querySelectorAll(".lit").forEach((x) => x.classList.remove("lit"));
      page.querySelectorAll(".sy-deck.scrub").forEach((d) => { d.classList.remove("scrub"); d.querySelectorAll(".sy-dk").forEach((c) => { const i = +c.style.getPropertyValue("--i"); c.style.setProperty("--j", i); c.classList.toggle("deep", i > 4); c.classList.remove("lit"); }); });
      page.querySelectorAll(".sy-yr.dim").forEach((d) => d.classList.remove("dim"));
      return;
    }
    S.scrubbing = true;
    const segs = [...rb.children];
    segs.forEach((i) => { if (!i.dataset.g) i.dataset.g = i.style.flexGrow || i.style.flex.split(" ")[0]; });
    const tot = segs.reduce((s, i) => s + +i.dataset.g, 0);
    let acc = 0, hot = segs[segs.length - 1];
    for (const i of segs) { acc += +i.dataset.g / tot; if (u <= acc) { hot = i; break; } }
    // fisheye: the hot one takes a fixed share, the rest keep their proportions
    const share = 0.34;
    segs.forEach((i) => { i.style.flexGrow = i === hot ? String(tot * share / (1 - share) + +i.dataset.g * 0.4) : i.dataset.g; i.classList.toggle("hot", i === hot); });
    reach.classList.add("on");
    const id = hot.dataset.id, M = S.M;
    page.querySelectorAll(".lit").forEach((x) => x.classList.remove("lit"));
    if (hot.classList.contains("m")) {
      // the member: its token in the band, the crates that reach it, their lines
      const k = K(M.P.id, M.mod, `${M.item.n}::${id}`);
      page.querySelectorAll(`.sy-band [data-key="${CSS.escape(k)}"]`).forEach((t) => { t.classList.add("lit"); const st = t.closest(".sy-stack"); if (st) st.classList.add("lit"); });
      const var_ = [...page.querySelectorAll(".sy-mem b")].find((b) => b.textContent === id); if (var_) var_.parentElement.classList.add("lit");
      const who = M.yours.filter((r) => r.members[id]).map((r) => `<span>${esc(short(r.y.name))} <b>${r.members[id]}</b></span>`).join("");
      // what the member gives, read the same way the band reads it
      const does = M.does ? [...M.does.reads, ...M.does.changes, ...M.does.consumes] : [];
      const meth = does.find((x) => x.n === id);
      const verb = meth ? RECV[(M.XI.m.find((x) => x.n === id) || {}).r] : "";
      reach.querySelector(".sy-rc").innerHTML = `<b>${esc(id)}</b>${meth ? `<span class="sy-w">${esc(verb || "")}</span>${meth.gives && meth.gives.length ? `<span class="sy-w">gives</span>${wordsHtml(meth.gives)}` : ""}` : M.members && M.members.rows.some((r) => r.n === id) ? `<span class="sy-w">a case of ${esc(M.item.n)}</span>` : `<span class="sy-w">the name itself: in a type, an import or a path</span>`}<span class="sy-rwho">${who}</span>`;
      page.querySelectorAll(".sy-yr").forEach((row) => { const r = M.yours.find((x) => x.y.name === row.dataset.crate); row.classList.toggle("dim", !!r && !r.members[id] && !!id); const d = row.querySelector(".sy-deck"); if (d) { d.classList.add("scrub"); reorder(d, (c) => (id ? (c.dataset.m || "").split(" ").includes(id) : !c.dataset.m)); } });
    } else {
      const r = M.yours.find((x) => x.y.name === id);
      reach.querySelector(".sy-rc").innerHTML = r ? `<b>${esc(short(id))}</b><span class="sy-w">names it ${plural(r.n, "time")}</span>${r.lines[0] ? `<em>${esc(r.lines[0][0])}:${r.lines[0][1]}</em>` : ""}` : "";
      page.querySelectorAll(".sy-yr").forEach((row) => { row.classList.toggle("dim", row.dataset.crate !== id && !row.classList.contains("other")); const d = row.querySelector(".sy-deck"); if (d) d.classList.toggle("scrub", row.dataset.crate === id); });
    }
  }
  // A deck under a scrub brings the lines that match to its front: they take the first places, lit.
  function reorder(d, hit) {
    const cards = [...d.querySelectorAll(".sy-dk:not(.more)")];
    const more = d.querySelector(".sy-dk.more");
    const order = cards.filter(hit).concat(cards.filter((c) => !hit(c)));
    order.forEach((c, j) => { c.classList.toggle("lit", hit(c)); c.style.setProperty("--j", j); c.classList.toggle("deep", j > 4); });
    if (more) { more.style.setProperty("--j", order.length); more.classList.toggle("deep", order.length > 4); }
    d.style.setProperty("--n", Math.min(5, order.length + (more ? 1 : 0)));
  }
  // Scrubbing its history: the release under the pointer lifts; a release read on disk re-reads the
  // symbol there (its sigil and badges re-drawn), and the page takes the warm paper of the past.
  function histAt(hs, x) {
    const page = S.page; if (!page) return;
    const lab = page.querySelector(".sy-hist .sy-hl");
    if (!hs) { S.histHot = null; if (lab) lab.classList.remove("on"); page.querySelectorAll(".sy-hist rect.hot").forEach((r) => r.classList.remove("hot")); if (!S.at) travel(null); return; }
    const rects = [...hs.querySelectorAll("rect[data-v]")];
    let best = null, bd = 1e9;
    for (const r of rects) { const rx = +r.getAttribute("x"); const d = Math.abs(rx - x) - (r.classList.contains("read") ? 6 : 0); if (d < bd) { bd = d; best = r; } }
    if (!best) return;
    rects.forEach((r) => r.classList.toggle("hot", r === best));
    S.histHot = best.dataset.v;
    const M = S.M, h = M.hist.find((q) => q.v === best.dataset.v);
    const T = TRUST[M.P.id], rr = T.releases.find((q) => q.v === best.dataset.v);
    const word = !h ? "not read on disk" : h.st === "pin" ? "your pin" : h.st === "same" ? "the same" : h.st === "chg" ? (h.note || "different") : h.st === "absent" ? "not here yet" : h.st === "gone" ? "gone" : "not read";
    lab.innerHTML = `<b>${esc(best.dataset.v)}</b><i>${esc(ago(rr && rr.t))}</i><span class="${h ? h.st : "nr"}">${esc(word)}</span>`;
    lab.classList.add("on");
    lab.style.left = clamp(+best.getAttribute("x") - lab.offsetWidth / 2, 0, Math.max(0, hs.clientWidth - lab.offsetWidth)) + "px";
    if (!S.at) travel(h && h.st !== "pin" ? h : null, true);
  }
  function travel(h, live = false) {
    const page = S.page; if (!page) return;
    const hp = page.querySelector(".sy-hp");
    page.classList.toggle("past", !!h);
    const M = S.M;
    const it = h && h.item ? h.item : M.item;
    const F = h && h.item ? facts(it, null, { kind: M.kind, yours: M.F.yours }) : M.F;
    const hm = hp.querySelector(".sy-hm"); hm.innerHTML = sigil(F, 64, "hero");
    const b = hp.querySelector(".sy-bdgs");
    if (!b._pin) b._pin = b.innerHTML;
    if (!h || !h.item) { b.innerHTML = b._pin; b.classList.remove("then"); return; }
    const now = new Set(read(M.item).tags.map((t) => t.label)), then = read(it).tags;
    b.innerHTML = `<span class="bdg amber">${icon("marker")}<b>at ${esc(h.v)}</b><em>${esc(h.note || (h.st === "same" ? "the same first line as your pin" : ""))}</em></span>` + then.map((t) => tagHtml({ ...t, tone: now.has(t.label) ? t.tone : "amber" })).join("") + [...now].filter((l) => !then.some((t) => t.label === l)).map((l) => `<span class="bdg coral">${icon("error")}<b>${esc(l)}</b><em>not yet at ${esc(h.v)}</em></span>`).join("");
    b.classList.add("then");
  }

  // ---------------------------------------------------------------- moves
  // Ghosts: only frames, marks and the one name plate travel; text never flies in crowds.
  // A move plays on the frame loop; with &freeze=<ms> the named move renders that one frame and holds it,
  // so a film's frames are exact rather than whatever a headless clock happened to reach.
  const film = (which, dur, fn) => {
    if (S.freeze != null && S.freezeFor === which) { fn(S.freeze); return new Promise(() => {}); }
    return play(dur, fn);
  };
  function ghosts() {
    const layer = document.createElement("div"); layer.className = "sy-ghosts"; document.body.appendChild(layer);
    const add = (cls, html = "") => { const el = document.createElement("div"); el.className = cls; el.innerHTML = html; layer.appendChild(el); return el; };
    return { layer, add, done: () => layer.remove() };
  }
  const put = (el, r) => { el.style.left = r.x + "px"; el.style.top = r.y + "px"; el.style.width = r.w + "px"; el.style.height = r.h + "px"; };
  const R = (r) => ({ x: r.left, y: r.top, w: r.width, h: r.height });
  const lr = (a, b, u) => ({ x: lerp(a.x, b.x, u), y: lerp(a.y, b.y, u), w: lerp(a.w, b.w, u), h: lerp(a.h, b.h, u) });
  // The name plate: re-set in the title face every frame at the size it is seen at, never scaled.
  function nameAt(el, a, b, sa, sb, u, face) {
    const size = lerp(sa, sb, u);
    el.style.font = `700 ${size.toFixed(2)}px/1.1 "Bricolage Grotesque"`;
    el.style.letterSpacing = `${(-0.03 * size).toFixed(2)}px`;
    el.style.left = lerp(a.x, b.x, u) + "px"; el.style.top = lerp(a.y, b.y, u) + "px";
    if (face) el.style.fontFamily = face;
  }
  function unrollParts(page, ref) {
    return [...page.querySelectorAll(".sy-u")].map((el) => ({ el, dy: Math.max(0, el.getBoundingClientRect().top - ref) }));
  }
  // Once a ghost has landed the real element takes over, so the page under it reads (and a frozen late frame is the page).
  const handoff = (u, gs, hp, hm, h1) => { const land = u > 0.995; gs.forEach((g) => (g.style.opacity = land ? 0 : 1)); hp.classList.toggle("ghosting", !land); hm.style.visibility = land ? "" : "hidden"; h1.style.visibility = land ? "" : "hidden"; };
  const unrollAt = (parts, ms, start, rate = 0.45) => parts.forEach((p) => { const e = clamp((ms - start - p.dy * rate) / 240); p.el.style.opacity = e; p.el.style.clipPath = e < 1 ? `inset(0 0 ${((1 - e) * 100).toFixed(1)}% 0)` : ""; });
  const clearParts = (parts) => parts.forEach((p) => { p.el.style.opacity = ""; p.el.style.clipPath = ""; });

  // Card → symbol: the card's frame grows into the hero plate, its mark into the sigil, its name into the
  // title (re-set each frame); the other cards shrink into the sibling strip; the page unrolls in place.
  async function open(P, name, mpath, cardEl) {
    if (S.busy) return; S.busy = true;
    const M = await model(W, P, name, mpath); if (!M) { S.busy = false; return; }
    if (cardEl) S.folioHost = cardEl.closest(".f3") ? cardEl.closest(".f3").parentElement : S.folioHost;
    S.saved = S.saved || { side: side.innerHTML, jump: jump.innerHTML, sideClick: side.onclick, jumpClick: jump.onclick };
    if (!S.root) S.root = shell();
    const root = S.root;
    S.trail = [{ M }]; S.M = M; S.at = null;
    const page = S.page = await mount(M, { hidden: !!cardEl });
    drawStrip(M, false); drawJump(); drawSide(M);
    if (!cardEl) { S.busy = false; return; }
    const hp = page.querySelector(".sy-hp"), hm = page.querySelector(".sy-hm"), h1 = page.querySelector(".sy-hero h1");
    const cards = [...cardEl.parentElement.querySelectorAll(".card3")];
    const cr = R(cardEl.getBoundingClientRect()), cm = R(cardEl.querySelector(".km3").getBoundingClientRect()), cn = R(cardEl.querySelector(".c3t b").getBoundingClientRect());
    const hr = R(hp.getBoundingClientRect()), mr = R(hm.getBoundingClientRect()), tr = R(h1.getBoundingClientRect());
    const G = ghosts();
    const frame = G.add(`sy-gf ${M.fam}`), mark = G.add("sy-gm", sigil(M.F, 64, "hero")), nm = G.add("sy-gn", esc(M.item.n));
    // the card's own words stay where they were and fade; only its name travels
    frame.innerHTML = `<div class="sy-gtext own">${cardEl.innerHTML}</div>`;
    const chips = new Map([...root.querySelectorAll(".sy-sib")].map((c) => [c.dataset.n, c]));
    const others = cards.filter((c) => c !== cardEl).map((c, i) => {
      const chip = chips.get(c.dataset.it); if (!chip) return null;
      const f = G.add("sy-gf sib"), mk = G.add("sy-gm", chip.querySelector("svg").outerHTML);
      f.innerHTML = `<div class="sy-gtext">${c.innerHTML}</div>`;
      return { c, chip, f, mk, a: R(c.getBoundingClientRect()), am: R(c.querySelector(".km3").getBoundingClientRect()), b: R(chip.getBoundingClientRect()), bm: R(chip.querySelector("svg").getBoundingClientRect()), delay: 70 + i * 16 };
    }).filter(Boolean);
    cards.forEach((c) => (c.style.visibility = "hidden"));
    hp.classList.add("ghosting"); hm.style.visibility = "hidden"; h1.style.visibility = "hidden";
    chips.forEach((c) => (c.style.opacity = "0"));
    root.querySelector(".sy-mod").style.opacity = "0";
    const ring = root.querySelector(".sy-ring"); ring.style.opacity = "0";
    const parts = unrollParts(page, hr.y);
    parts.forEach((p) => (p.el.style.opacity = "0"));
    page.classList.remove("entering");
    root.classList.add("arriving");
    await film("open", 1050, (ms) => {
      root.style.setProperty("--bg", clamp(ms / 240).toFixed(3));
      const u = CARRY(ms);
      put(frame, lr(cr, hr, u));
      put(mark, lr(cm, mr, u)); mark.style.setProperty("--detail", clamp((u - 0.25) / 0.6).toFixed(3));
      nameAt(nm, cn, tr, 14, 40, u);
      handoff(u, [frame, mark, nm], hp, hm, h1);
      frame.firstChild.style.opacity = 1 - clamp(ms / 60);
      const curChip = root.querySelector(".sy-sib.cur"); if (curChip) curChip.style.opacity = clamp((ms - 480) / 180);
      for (const o of others) {
        const v = CARRY(ms - o.delay);
        put(o.f, lr(o.a, o.b, v)); put(o.mk, lr(o.am, o.bm, v));
        o.f.firstChild.style.opacity = 1 - clamp(ms / 70);
        if (v > 0.97) { o.chip.style.opacity = clamp((ms - o.delay - 330) / 160); o.f.style.opacity = 1 - clamp((ms - o.delay - 330) / 160); o.mk.style.opacity = o.f.style.opacity; }
      }
      root.querySelector(".sy-mod").style.opacity = clamp((ms - 380) / 200);
      ring.style.opacity = clamp((ms - 620) / 200);
      unrollAt(parts, ms, 240);
      page.style.setProperty("--draw", (1 - clamp((ms - 300) / 520)).toFixed(3));
    });
    chips.forEach((c) => (c.style.opacity = ""));
    hp.classList.remove("ghosting"); hm.style.visibility = ""; h1.style.visibility = "";
    clearParts(parts); root.classList.remove("arriving"); root.style.removeProperty("--bg"); page.style.removeProperty("--draw");
    G.done();
    S.busy = false;
  }

  // Symbol → symbol: the clicked token's name grows into the title; the symbol you leave shrinks into
  // the place its relation to the new one gives it (took it as input? now it takes it: it lands right);
  // the spine re-forms; the sibling strip slides so the new one is ringed.
  async function hopKey(key, fromEl) {
    const { pid, mod, name } = unK(key);
    const P = WD.byId.get(pid); if (!P) return;
    const M = await model(W, P, name, mod); if (!M) return;
    return hopTo(M, fromEl, { rel: fromEl && fromEl.closest(".sy-strip, .sy-side") ? "·" : "→" });
  }
  async function hopTo(M, fromEl, { rel = "→", rewind = null } = {}) {
    if (S.busy) return; S.busy = true;
    S.at = null;
    const old = S.page, Mold = S.M, root = S.root, sc = root.querySelector(".sy-scroll");
    const oh = old.querySelector(".sy-hp"), om = old.querySelector(".sy-hm"), o1 = old.querySelector(".sy-hero h1");
    const ohr = R(oh.getBoundingClientRect()), omr = R(om.getBoundingClientRect()), o1r = R(o1.getBoundingClientRect());
    const src = fromEl && fromEl.getBoundingClientRect().width ? fromEl : null;
    const sr = src ? R(src.getBoundingClientRect()) : null;
    const snr = src && src.querySelector("b") ? R(src.querySelector("b").getBoundingClientRect()) : sr;
    const smr = src && src.querySelector("svg, .km3") ? R(src.querySelector("svg, .km3").getBoundingClientRect()) : sr;
    const sSize = src && src.querySelector("b") ? parseFloat(getComputedStyle(src.querySelector("b")).fontSize) : 13;
    // the old page steps out of the scroller where it stands
    const oy = old.getBoundingClientRect().top - root.getBoundingClientRect().top;
    old.classList.add("leaving"); old.style.top = oy + "px"; root.appendChild(old); sc.scrollTop = 0;
    if (rewind != null) S.trail = S.trail.slice(0, rewind + 1); else S.trail.push({ M, rel });
    if (rewind != null) S.trail[S.trail.length - 1].M = M;
    S.M = M;
    const page = S.page = await mount(M, { hidden: true });
    drawJump(); drawSide(M); drawStrip(M, true);
    const hp = page.querySelector(".sy-hp"), hm = page.querySelector(".sy-hm"), h1 = page.querySelector(".sy-hero h1");
    const hr = R(hp.getBoundingClientRect()), mr = R(hm.getBoundingClientRect()), tr = R(h1.getBoundingClientRect());
    // where the old one lands on the new page: its token there (in view), else its sibling chip
    const vis = (el) => { const r = el.getBoundingClientRect(); return r.width && r.top > 60 && r.bottom < innerHeight - 10; };
    let dest = [...page.querySelectorAll(`.sy-band [data-key="${CSS.escape(Mold.key)}"]`)].find(vis) || [...page.querySelectorAll(`[data-key="${CSS.escape(Mold.key)}"]`)].find(vis) || [...root.querySelectorAll(`.sy-sib[data-key="${CSS.escape(Mold.key)}"]`)][0] || null;
    // across packages: the symbol you left belongs to its package's row among the users of the new one
    if (!dest && Mold.P !== M.P) dest = [...page.querySelectorAll(`.sy-yr.other[data-pid="${CSS.escape(Mold.P.id)}"] .sy-yc, .sy-bn a.pk[data-pkg="${CSS.escape(Mold.P.id)}"]`)].find(vis) || null;
    const stacked = dest && dest.closest(".sy-stack");
    if (dest && !vis(dest) && stacked) dest = stacked;
    const dr = dest ? R(dest.getBoundingClientRect()) : null;
    const dmr = dest && dest.querySelector("svg, .km3") ? R(dest.querySelector("svg, .km3").getBoundingClientRect()) : dr;
    const dnr = dest && dest.querySelector("b") ? R(dest.querySelector("b").getBoundingClientRect()) : dr;
    const dSize = dest && dest.querySelector("b") ? parseFloat(getComputedStyle(dest.querySelector("b")).fontSize) : 13;
    const G = ghosts();
    const nf = G.add(`sy-gf ${M.fam}`), nmk = G.add("sy-gm", sigil(M.F, 64, "hero")), nn = G.add("sy-gn", esc(M.item.n));
    const of = G.add(`sy-gf ${Mold.fam} old`), omk = G.add("sy-gm", sigil(Mold.F, 64, "hero")), on = G.add("sy-gn", esc(Mold.item.n));
    hp.classList.add("ghosting"); hm.style.visibility = "hidden"; h1.style.visibility = "hidden";
    oh.classList.add("ghosting"); om.style.visibility = "hidden"; o1.style.visibility = "hidden";
    if (dest) dest.style.visibility = "hidden";
    const parts = unrollParts(page, hr.y);
    parts.forEach((p) => (p.el.style.opacity = "0"));
    page.classList.remove("entering");
    const a0 = sr || { x: tr.x, y: tr.y + 30, w: 80, h: 20 };
    await film("hop", 1000, (ms) => {
      const u = CARRY(ms), v = CARRY(ms - 40);
      slide(CARRY(ms - 60));
      old.style.opacity = 1 - clamp(ms / 150);
      put(nf, lr(a0, hr, u)); put(nmk, lr(smr || a0, mr, u)); nmk.style.setProperty("--detail", clamp((u - 0.2) / 0.6).toFixed(3));
      nameAt(nn, snr || a0, tr, sSize, 40, u);
      handoff(u, [nf, nmk, nn], hp, hm, h1);
      if (dr) {
        put(of, lr(ohr, dr, v)); put(omk, lr(omr, dmr, v)); omk.style.setProperty("--detail", (1 - clamp((v - 0.3) / 0.5)).toFixed(3));
        nameAt(on, o1r, dnr, 40, dSize, v);
        if (v > 0.96) { const e = clamp((ms - 560) / 140); [of, omk, on].forEach((g) => (g.style.opacity = 1 - e)); dest.style.visibility = ""; dest.style.opacity = e; }
      } else { [of, omk, on].forEach((g) => (g.style.opacity = 1 - clamp(ms / 160))); put(of, ohr); put(omk, omr); nameAt(on, o1r, o1r, 40, 40, 0); }
      unrollAt(parts, ms, 220);
      page.style.setProperty("--draw", (1 - clamp((ms - 260) / 520)).toFixed(3));
    });
    if (dest) { dest.style.visibility = ""; dest.style.opacity = ""; dest.classList.add("from"); }
    hp.classList.remove("ghosting"); hm.style.visibility = ""; h1.style.visibility = "";
    clearParts(parts); page.style.removeProperty("--draw");
    if (old._ro) old._ro.disconnect();
    old.remove(); G.done();
    S.busy = false;
  }

  // Back: the page folds into its card in the module grid, and focus lands on that card.
  async function back() {
    if (S.busy || !S.root) return;
    const M = S.M, root = S.root, page = S.page;
    const host = S.folioHost;
    let card = null;
    if (host && host.isConnected && M.P.id === (S.trail[0] && S.trail[0].M.P.id)) {
      const name = M.owner ? M.owner.it.n : M.item.n, mod = M.owner ? M.owner.mod : M.mod;
      const q = () => host.querySelector(`.mod3 .card3[data-it="${CSS.escape(name)}"][data-mod="${CSS.escape(mod)}"]`);
      card = q();
      if (!card) { const t = host.querySelector(`.rail3 .rt[data-m="${CSS.escape(mod)}"]`); if (t) { t.click(); await wait(700); card = q(); } }
    }
    if (!card) { location.href = `?v=package&p=${encodeURIComponent(M.P.id)}&mod=${encodeURIComponent(M.owner ? M.owner.mod : M.mod)}`; return; }
    S.busy = true;
    const scroller = card.closest(".f3");
    const cRect0 = card.getBoundingClientRect();
    if (cRect0.top < 60 || cRect0.bottom > innerHeight - 20) scroller.scrollTop += cRect0.top - innerHeight / 2 + cRect0.height / 2;
    const cards = [...card.parentElement.querySelectorAll(".card3")];
    const hp = page.querySelector(".sy-hp"), hm = page.querySelector(".sy-hm"), h1 = page.querySelector(".sy-hero h1");
    const hr = R(hp.getBoundingClientRect()), mr = R(hm.getBoundingClientRect()), tr = R(h1.getBoundingClientRect());
    const cr = R(card.getBoundingClientRect()), cm = R(card.querySelector(".km3").getBoundingClientRect()), cn = R(card.querySelector(".c3t b").getBoundingClientRect());
    const G = ghosts();
    const frame = G.add(`sy-gf ${M.fam}`), mark = G.add("sy-gm", sigil(M.F, 64, "hero")), nm = G.add("sy-gn", esc(M.item.n));
    const chips = new Map([...root.querySelectorAll(".sy-sib")].map((c) => [c.dataset.n, c]));
    const others = cards.filter((c) => c !== card).map((c, i) => {
      const chip = chips.get(c.dataset.it); if (!chip) return null;
      const f = G.add("sy-gf sib"), mk = G.add("sy-gm", chip.querySelector("svg").outerHTML);
      return { c, f, mk, a: R(chip.getBoundingClientRect()), am: R(chip.querySelector("svg").getBoundingClientRect()), b: R(c.getBoundingClientRect()), bm: R(c.querySelector(".km3").getBoundingClientRect()), delay: 40 + i * 14 };
    }).filter(Boolean);
    cards.forEach((c) => (c.style.visibility = "hidden"));
    hp.classList.add("ghosting"); hm.style.visibility = "hidden"; h1.style.visibility = "hidden";
    const parts = unrollParts(page, hr.y);
    root.classList.add("arriving");
    await film("back", 950, (ms) => {
      root.style.setProperty("--bg", (1 - clamp((ms - 120) / 260)).toFixed(3));
      parts.forEach((p) => { const e = clamp((ms - p.dy * 0.12) / 150); p.el.style.opacity = 1 - e; p.el.style.clipPath = `inset(0 0 ${(e * 100).toFixed(1)}% 0)`; });
      page.style.setProperty("--draw", clamp(ms / 200).toFixed(3));
      root.querySelector(".sy-top").style.opacity = 1 - clamp(ms / 160);
      const u = CARRY(ms - 30);
      put(frame, lr(hr, cr, u)); put(mark, lr(mr, cm, u)); mark.style.setProperty("--detail", (1 - clamp(u / 0.7)).toFixed(3));
      frame.style.background = `rgba(11,21,38,${clamp(u * 1.6).toFixed(3)})`;   // the plate's words read through it until they've faded
      nameAt(nm, tr, cn, 40, 14, u);
      chips.forEach((c) => (c.style.opacity = 1 - clamp(ms / 90)));
      for (const o of others) { const v = CARRY(ms - o.delay); put(o.f, lr(o.a, o.b, v)); put(o.mk, lr(o.am, o.bm, v)); o.f.style.background = `rgba(10,20,36,${clamp(v * 1.6).toFixed(3)})`; if (v > 0.97) { o.c.style.visibility = ""; o.c.style.opacity = clamp((ms - o.delay - 260) / 140); o.f.style.opacity = 1 - clamp((ms - o.delay - 260) / 140); o.mk.style.opacity = o.f.style.opacity; } }
      if (u > 0.97) { card.style.visibility = ""; card.style.opacity = clamp((ms - 430) / 140); [frame, mark, nm].forEach((g) => (g.style.opacity = 1 - clamp((ms - 430) / 140))); }
    });
    cards.forEach((c) => { c.style.visibility = ""; c.style.opacity = ""; });
    G.done();
    if (page._ro) page._ro.disconnect();
    root.remove(); S.root = null; S.page = null; S.strip = null; S.stripAt = null;
    if (S.saved) { side.innerHTML = S.saved.side; jump.innerHTML = S.saved.jump; side.onclick = S.saved.sideClick; jump.onclick = S.saved.jumpClick; S.saved = null; }
    card.focus({ preventScroll: true }); card.classList.add("lit"); setTimeout(() => card.classList.remove("lit"), 1200);
    S.busy = false;
  }

  // Esc steps back along the trail; at its start the page folds back into its card.
  window.addEventListener("keydown", (e) => {
    if (!S.root || e.key !== "Escape") return;
    e.stopPropagation(); e.preventDefault();
    if (S.at) { S.at = null; travel(null); return; }
    if (S.trail.length > 1) { const i = S.trail.length - 2; hopTo(S.trail[i].M, null, { rewind: i }); return; }
    back();
  }, true);

  window.__onSymbol = (P, it, cardEl, mod) => open(P, it.n, mod.path || mod, cardEl);

  // ?v=symbol&p=<pkg>&s=<item>&mod=<module>: the package page is laid beneath (so Back has its card),
  // then the symbol opens. &from=card plays the card → symbol film; &to=<name> hops along a relation;
  // &back=1 folds back; &hover=<name> holds a token's hover; &scrub=<member> holds the reach bar;
  // &fan=<crate> fans a deck; &at=<version> reads it at another release.
  async function route(Q) {
    const P = find(Q.get("p")) || find("toml");
    const name = Q.get("s") || "Value";
    if (Q.get("freeze") != null) { S.freeze = +Q.get("freeze"); S.freezeFor = Q.get("back") ? "back" : Q.get("to") ? "hop" : "open"; }
    const D = await detail(P.id);
    let mod = Q.get("mod");
    if (!mod && D) for (const m of D.modules) if (m.items.some((it) => it.n === name.split("::")[0])) { mod = m.path; break; }
    reader.innerHTML = "";
    const host = document.createElement("div"); host.style.cssText = "position:absolute;inset:0"; reader.appendChild(host);
    const fv = folio3(host, WD, { TRUST, LINES, onSymbol: (p, it, el, m) => window.__onSymbol(p, it, el, m) });
    await fv.render(P);
    jump.innerHTML = `<span class="c">Library</span><span class="rel">›</span><b>${esc(P.name)}</b>`;
    const owner = name.split("::")[0];
    try { await fv.unfurl(mod); } catch { /* a private module has no cards */ }
    S.folioHost = host;
    const card = host.querySelector(`.mod3 .card3[data-it="${CSS.escape(owner)}"]`);
    if (Q.get("from") === "card" && card) {
      await wait(+(Q.get("after") || 400));
      await open(P, name, mod, card);
    } else await open(P, name, mod, null);
    const to = Q.get("to");
    if (to) {
      await wait(+(Q.get("after2") || 700));
      const el = [...S.page.querySelectorAll(".sy-band [data-key], .sy-sec [data-key]")].find((x) => x.dataset.key.endsWith("|" + to) && !x.classList.contains("cur") && x.getBoundingClientRect().width) || [...S.root.querySelectorAll(".sy-sib")].find((x) => x.dataset.n === to);
      if (el) await hopKey(el.dataset.key, el);
    }
    if (Q.get("back")) { await wait(+(Q.get("after3") || 700)); await back(); return; }
    const hv = Q.get("hover");
    if (hv) { const t = [...S.page.querySelectorAll(".sy-tok, .sy-stack, .sy-mini, .sy-can")].find((x) => x.getBoundingClientRect().width > 0 && ((x.querySelector("b") || {}).textContent === hv || x.dataset.n === hv || (x.querySelector(".sy-st-l") || {}).textContent === hv)); if (t) t.classList.add("hov"); }
    const sm = Q.get("scrub");
    if (sm) { const rb = S.page.querySelector(".sy-reach .sy-rb"); const seg = rb && [...rb.children].find((i) => i.dataset.id === sm || (i.querySelector("b") || {}).textContent === sm); if (seg) { const r = rb.getBoundingClientRect(), s2 = seg.getBoundingClientRect(); scrubAt(rb, (s2.left + s2.width / 2 - r.left) / r.width); const s3 = seg.getBoundingClientRect(); scrubAt(rb, (s3.left + s3.width / 2 - r.left) / r.width); } }
    const fan = Q.get("fan");
    if (fan) { const row = [...S.page.querySelectorAll(".sy-yr")].find((r) => (r.dataset.crate || "").endsWith(fan)); if (row) row.querySelector(".sy-deck").classList.add(Q.get("open") ? "open" : "hov"); }
    const at = Q.get("at");
    if (at) { const h = S.M.hist.find((x) => x.v === at); if (h) { S.at = h.v; travel(h.st === "pin" ? null : h); const hs = S.page.querySelector(".sy-hist .sy-hs"), r = hs && hs.querySelector(`rect[data-v="${CSS.escape(at)}"]`); if (r) { S.histHot = null; histAt(hs, +r.getAttribute("x")); } } }
    if (Q.get("scrollto")) { const el = S.page.querySelector(Q.get("scrollto")); if (el) S.root.querySelector(".sy-scroll").scrollTop = el.offsetTop - 30; }
  }
  // the model of one symbol, for other pages that draw it their own way (symbol5.js)
  const modelOf = async (pname, sname, mpath) => { const P = find(pname); return P ? model(W, P, sname, mpath) : null; };
  return { route, open, modelOf };
}
