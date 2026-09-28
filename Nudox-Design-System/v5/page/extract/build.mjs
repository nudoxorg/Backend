// build.mjs: every v5 page model, one language-neutral shape, from real facts.
//
//   node v5/page/extract/build.mjs            → v5/page/data/pages.js (window.PAGES)
//
// Inputs (all read-only, all real):
//   data/go-facts.json   go/types over github.com/spf13/pflag 1.0.9 + github.com/spf13/cobra 1.10.2
//   data/ts-facts.json   the TypeScript compiler API over zod 4.1.8 and yaml 2.9.0
//   v4/page2/page2.json  page2's Rust facts (toml, serde_json, serde_core) + v4 world
//   ~/.cargo/registry    toml 0.8.23, toml_datetime 0.6.11, serde_json 1.0.151 sources
//
// The page model (one per page) is the contract this prototype proposes to W-Instant's
// page plans: hero, marks, spec (one of choice / record / callable / contract), sections.
// A type is always a list of tokens: {t:"w"} plain words, {t:"prim"} a plain value,
// {t:"ty"|"co"|"ca"} a named, linkable thing of that family, {t:"var"} a generic,
// {t:"lit"} a literal value, {t:"p"} punctuation. The renderer never parses code.
import fs from "node:fs";
import path from "node:path";

const HERE = path.dirname(new URL(import.meta.url).pathname);
const ROOT = path.resolve(HERE, "..");
const V4 = path.resolve(ROOT, "../../v4");
const REG = (() => { const d = path.join(process.env.HOME, ".cargo/registry/src"); return path.join(d, fs.readdirSync(d)[0]); })();
const P2 = JSON.parse(fs.readFileSync(path.join(V4, "page2/page2.json"), "utf8"));
const GO = JSON.parse(fs.readFileSync(path.join(ROOT, "data/go-facts.json"), "utf8"));
const TS = JSON.parse(fs.readFileSync(path.join(ROOT, "data/ts-facts.json"), "utf8"));
const W = await import(path.join(V4, "page2/world.mjs"));

const lines = (f) => fs.readFileSync(f, "utf8").split("\n");
const goLede = (doc, name) => { const f = first(doc); const re = new RegExp("^(A |An |The )?" + name.replace(/[$.]/g, "\\$&") + " (is |represents |defines |parses )?", "i"); const m = f.match(re); if (!m) return f; const rest = f.slice(m[0].length); const verb = m[2] ? m[2].trim() : ""; const out = (verb === "is" || verb === "") ? rest : verb.replace(/^\w/, (c) => c.toUpperCase()) + " " + rest; return out.replace(/^\w/, (c) => c.toUpperCase()); };
const first = (s) => { if (!s) return ""; const t = s.replace(/\s+/g, " ").trim(); const m = t.match(/^(.+?[.!?])(\s|$)/); return (m ? m[1] : t).replace(/\[`?([^\]`]+)`?\]\[[^\]]*\]/g, "`$1`").replace(/\[([^\]]+)\]\([^)]*\)/g, "$1"); };
const plural = (n, one, many) => `${n} ${n === 1 ? one : many || one + "s"}`;

// ---------------------------------------------------------------- plain words, per language
const T = {
  w: (s) => ({ t: "w", s }), prim: (s, exact) => ({ t: "prim", s, x: exact }), ty: (s, fam = "ty", exact) => ({ t: fam, s, x: exact }),
  v: (s) => ({ t: "var", s }), lit: (s) => ({ t: "lit", s }), p: (s) => ({ t: "p", s }),
};
function splitArgs(s) { const out = []; let d = 0, cur = ""; for (const c of s) { if ("<([{".includes(c)) d++; if (">)]}".includes(c)) d--; if (c === "," && d === 0) { out.push(cur.trim()); cur = ""; } else cur += c; } if (cur.trim()) out.push(cur.trim()); return out; }
const RUST_PRIM = { String: "text", str: "text", "&str": "text", i8: "integer", i16: "integer", i32: "integer", i64: "integer", u8: "integer", u16: "integer", u32: "integer", u64: "integer", usize: "integer", isize: "integer", f32: "float", f64: "float", bool: "bool", char: "character", "()": "nothing" };
const RUST_KNOWN = { Value: "ty", Datetime: "ty", Date: "ty", Time: "ty", Offset: "ty", Table: "ty", Error: "ty", Serializer: "co", Deserializer: "co", Serialize: "co", Deserialize: "co", DeserializeOwned: "co" };
function rustWords(ty) {
  let s = ty.trim().replace(/^&'?\w*\s*(mut\s+)?/, (m) => (m.includes("mut") ? "mut " : "")).replace(/^(crate|toml|serde|self)::(\w+::)*/, "");
  if (s.startsWith("mut ")) return [T.w("changeable"), ...rustWords(s.slice(4))];
  if (RUST_PRIM[s]) return [T.prim(RUST_PRIM[s], ty)];
  const g = s.match(/^([\w:]+)<(.+)>$/);
  if (g) {
    const head = g[1].split("::").pop(); const args = splitArgs(g[2]);
    if (head === "Option") return [T.w("maybe"), ...rustWords(args[0])];
    if (head === "Vec") return [T.w("list of"), ...rustWords(args[0])];
    if (head === "Box" || head === "Rc" || head === "Arc") return rustWords(args[0]);
    if (head === "Result") return [...rustWords(args[0]), T.w("or fails with"), ...rustWords(args[1] || "Error")];
    if (head === "BTreeMap" || head === "HashMap" || head === "Map") return [T.w("map"), ...rustWords(args[0]), T.p("→"), ...rustWords(args[1])];
    return [T.ty(head, RUST_KNOWN[head] || "ty", ty)];
  }
  const leaf = s.split("::").pop();
  if (/^[A-Z]$/.test(leaf)) return [T.v(leaf)];
  if (leaf === "Array") return [T.w("list of"), T.ty("Value")];
  if (leaf === "Table") return [T.ty("Table", "ty", "Map<String, Value>")];
  return [T.ty(leaf, RUST_KNOWN[leaf] || "ty", ty)];
}
const TS_PRIM = { string: "text", number: "number", bigint: "big integer", boolean: "bool", unknown: "anything", any: "anything", void: "nothing", undefined: "nothing", PropertyKey: "key", symbol: "symbol", null: "null" };
function tsWords(ty) {
  let s = ty.trim().replace(/^core\./, "").replace(/\bcore\./g, "").replace(/\butil\./g, "");
  if (!s) return [];
  const parts = splitUnion(s);
  if (parts.length > 1) {
    const lits = parts.filter((p) => /^["']/.test(p));
    const open = parts.some((p) => /\(string & \{\}\)/.test(p));
    const rest = parts.filter((p) => !/^["']/.test(p) && !/\(string & \{\}\)/.test(p));
    const out = [];
    if (lits.length) { out.push(T.w("one of")); lits.forEach((l, k) => { if (k) out.push(T.p("·")); out.push(T.lit(l.replace(/'/g, '"'))); }); }
    rest.forEach((r, k) => { if (out.length) out.push(T.w("or")); out.push(...tsWords(r)); });
    if (open) out.push(T.w("or any text"));
    return out;
  }
  if (/^["']/.test(s)) return [T.lit(s.replace(/'/g, '"'))];
  if (/\[\]$/.test(s)) return [T.w("list of"), ...tsWords(s.slice(0, -2).replace(/^\((.*)\)$/, "$1"))];
  if (TS_PRIM[s]) return [T.prim(TS_PRIM[s], s)];
  const g = s.match(/^([\w$.]+)<(.+)>$/);
  if (g) {
    const head = g[1].split(".").pop(); const args = splitArgs(g[2]);
    if (head === "Record") return [T.w("map"), ...tsWords(args[0]), T.p("→"), ...tsWords(args[1])];
    if (head === "Promise") return [T.w("later"), ...tsWords(args[0])];
    if (head === "Array") return [T.w("list of"), ...tsWords(args[0])];
    if (head === "output") return [T.w("its output")];
    return [T.ty(head, /Context|Issue|Error|Type$/.test(head) ? "ty" : "ty", s)];
  }
  if (/^[A-Z]\w*$/.test(s) && s.length <= 6 && /^(T|U|K|V|Input|Output)$/.test(s)) return [T.v(s)];
  return [T.ty(s.split(".").pop(), "ty", s)];
}
function splitUnion(s) { const out = []; let d = 0, cur = ""; for (const c of s) { if ("<([{".includes(c)) d++; if (">)]}".includes(c)) d--; if (c === "|" && d === 0) { out.push(cur.trim()); cur = ""; } else cur += c; } if (cur.trim()) out.push(cur.trim()); return out.filter(Boolean); }
const GO_PRIM = { string: "text", int: "integer", int64: "integer", uint: "integer", bool: "bool", float64: "float", error: "error", byte: "byte" };
function goWords(ty) {
  const s = ty.trim();
  if (GO_PRIM[s]) return [T.prim(GO_PRIM[s], s)];
  if (s.startsWith("[]")) return [T.w("list of"), ...goWords(s.slice(2))];
  if (s.startsWith("*")) return goWords(s.slice(1));
  const m = s.match(/^map\[(.+?)\](.+)$/); if (m) return [T.w("map"), ...goWords(m[1]), T.p("→"), ...goWords(m[2])];
  if (s.startsWith("func")) return [T.w("callback")];
  const leaf = s.split(".").pop();
  return [T.ty(leaf, leaf === "Value" ? "co" : "ty", s)];
}

// ---------------------------------------------------------------- the pieces every page shares
const base = (o) => ({ marks: [], sections: [], edges: { in: [], out: [] }, ...o });
const mark = (k, text, card) => ({ k, text, card: card || null });
// a doc that only restates the case's own name (and its package) says nothing: fold it
function redundant(doc, name, extra = []) {
  if (!doc) return true;
  const stop = new Set(["a", "an", "the", "represents", "is", "this", "value", ...extra.map((e) => e.toLowerCase())]);
  const words = doc.toLowerCase().replace(/[^a-z0-9 ]/g, " ").split(/\s+/).filter((w) => w && !stop.has(w));
  const nm = name.toLowerCase().replace(/[^a-z0-9]/g, "");
  return words.every((w) => nm.includes(w) || w.includes(nm) || w === "toml");
}
function bandsOf(rel) {
  return rel.bands.map((b) => ({ band: b.band, n: b.n, pkgs: b.pkgs.map((p) => ({ pkg: p.pkg, n: p.n, ids: (p.ids || []).slice(0, 6).map((id) => peekOf(id)).filter(Boolean) })) }));
}
function peekOf(id) { const p = P2.peeks[id]; if (!p) return null; return { name: p.n, kind: p.k, pkg: p.pk, yours: !!p.y, where: p.f }; }
function inuseOf(html) {
  const out = []; const re = /<figcaption><a class="gtl( y)?"[^>]*>([^<]+)<\/a><span class="uw">([^<]+)<\/span><\/figcaption>\s*<pre>([\s\S]*?)<\/pre>/g; let m;
  const un = (s) => s.replace(/<b>/g, "\u0001").replace(/<\/b>/g, "\u0002").replace(/<[^>]+>/g, "").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&").replace(/&quot;/g, '"');
  while ((m = re.exec(html))) { const [pkg, where] = m[3].split(" · "); const code = un(m[4]).split("\n").map((l) => l.replace(/\s+$/, "")); const ind = Math.min(...code.filter((l) => l.trim()).map((l) => l.match(/^\s*/)[0].length)); out.push({ caller: m[2], yours: !!m[1], pkg, where, code: code.map((l) => l.slice(ind)).filter((l, k, a) => l.trim() || k < a.length - 1) }); }
  return out;
}
function sinceOf(s) {
  if (!s) return null;
  return { first: s.first, oldestRead: s.oldest, pinned: s.pinned, versions: s.versions, read: s.read, changed: (s.changed || []).map((c) => ({ v: c.v, at: c.at, breaking: c.breaking, added: c.added, changed: c.changed, sample: (c.sample || []).map((x) => ({ path: x.path, before: x.before, after: x.after, same: x.same })) })) };
}

// ================================================================= RUST
const pv = P2.pages.value, pf = P2.pages.from_str, ps = P2.pages.serialize, pe = P2.pages.error;
const relOf = (page, verb) => page.relations.find((r) => r.verb === verb);

function rustValue() {
  const src = lines(path.join(REG, "toml-0.8.23/src/value.rs"));
  // the cases, with their own docs, straight from the enum
  const cases = []; let doc = [];
  for (let k = 24; k < 40; k++) {
    const l = src[k].trim();
    if (l.startsWith("///")) { doc.push(l.slice(3).trim()); continue; }
    const m = l.match(/^(\w+)\((\w+)\),?$/);
    if (m) { const d = doc.join(" "); cases.push({ name: m[1], payload: [{ type: rustWords(m[2]), exact: m[2] }], doc: d, quiet: redundant(d, m[1], ["toml", m[2]]), line: k + 1 }); doc = []; }
  }
  // which case each conversion lands on, read from the impl bodies (From impls and the impl_into_value! lines)
  const lands = [];
  src.forEach((l, k) => {
    const mm = l.match(/^impl_into_value!\((\w+): (.+)\);/); if (mm) lands.push({ from: mm[2], case: mm[1], line: k + 1 });
    const im = l.match(/^impl<.*?> From<(.+?)> for Value|^impl From<(.+?)> for Value/);
    if (im) { for (let j = k; j < k + 8; j++) { const v = src[j].match(/Value::(\w+)\(/); if (v) { lands.push({ from: (im[1] || im[2]).replace(/'a /, ""), case: v[1], line: k + 1 }); break; } } }
  });
  const byCase = new Map(); for (const x of lands) (byCase.get(x.case) || byCase.set(x.case, []).get(x.case)).push(x);
  const fromWords = (f) => { if (/^Vec</.test(f)) return [T.prim("list", f)]; if (/Map</.test(f) || f === "Table") return [T.prim("map", f)]; return rustWords(f); };
  const wrapRoutes = [...byCase].map(([c, xs]) => ({ from: fromWords(xs[0].from), alsoFrom: xs.slice(1).map((x) => x.from), steps: [{ name: "from", fam: "ca", note: "Value::from(x)" }], lands: { case: c }, code: `Value::from(${xs[0].from === "String" || xs[0].from === "&str" ? "s" : "x"})`, line: xs[0].line }));
  const comes = relOf(pv, "comes from");
  const getting = {
    kind: "getting", title: "Getting one", dir: "in", count: { n: comes.n + 0, yours: 0, tier: "compiler", label: `${comes.n} ways` },
    routes: [
      { from: [T.prim("text")], steps: [{ name: "parse", fam: "ca", fails: true, via: "FromStr" }], lands: { case: "Table", why: "a TOML document is a table" }, code: `let v: toml::Value = s.parse()?;`, gate: null, lead: true, evidence: "value.rs:391 → de.rs:41 → toml_edit de/mod.rs:178 (deserialize_any on the document root)" },
      { from: [T.prim("text")], steps: [{ name: "from", fam: "ca" }], lands: { case: "String", why: "wraps the text; does not parse it" }, code: `Value::from(s)`, trap: true },
      { from: [T.w("any"), T.ty("Serialize", "co")], steps: [{ name: "try_from", fam: "ca", fails: true }], lands: { case: null, why: "whichever case fits" }, code: `Value::try_from(x)?` },
    ],
    wraps: wrapRoutes.filter((r) => r.lands.case !== "String").map((r) => ({ from: r.from, also: r.alsoFrom, case: r.lands.case })),
    more: { n: comes.n - 3, label: "and 52 more that give one back", names: (comes.bands[0].pkgs[0].ids || []).slice(0, 6).map((id) => (P2.peeks[id] || {}).n).filter(Boolean) },
  };
  // what it does: its own methods by effect, then its contracts
  const inh0 = pv.src.inherent[0].members;
  const caseOf = (m) => { const mm = m.n.match(/^(as|is)_([a-z]+)(_mut)?$/); if (!mm) return null; const c = cases.find((k) => k.name.toLowerCase().startsWith(mm[2])); return c ? c.name : null; };
  for (const c of cases) c.reads = inh0.filter((m) => caseOf(m) === c.name).map((m) => ({ name: m.n, recv: m.recv, ret: m.ret }));
  const inh = inh0.filter((m) => !caseOf(m));
  const eff = { reads: "reads it", changes: "changes it", consumes: "uses it up", "": "makes one" };
  const groups = [];
  for (const key of ["reads", "changes", "consumes"]) {
    const ms = inh.filter((m) => m.recv === key);
    if (!ms.length) continue;
    // look-alikes fold: is_* become one member
    const is = ms.filter((m) => /^is_/.test(m.n)); const rest = ms.filter((m) => !/^is_/.test(m.n));
    const members = rest.map((m) => ({ name: m.n, result: rustWords(m.ret.replace(/^Option<&?(mut )?/, "Option<")), fails: /^Result/.test(m.ret), say: first(m.summary) }));
    if (is.length > 2) members.push({ name: "is_…", result: [T.prim("bool")], fold: is.map((m) => m.n.slice(3)), say: `one per case: ${is.map((m) => m.n.slice(3)).join(", ")}` });
    if (!members.length) continue;
    groups.push({ word: eff[key], own: true, members });
  }
  const traits = [];
  const seen = new Set();
  for (const w of pv.src.written) {
    if (seen.has(w.name)) continue; seen.add(w.name);
    const n = pv.src.written.filter((x) => x.name === w.name).length;
    const verb = { Index: "indexes", IndexMut: "indexes, changing", From: "converts from", Display: "prints", FromStr: "parses", Serialize: "serializes", Deserialize: "deserializes", Deserializer: "is a deserializer", IntoDeserializer: "becomes a deserializer" }[w.name] || w.name;
    traits.push({ name: w.name, verb, arrival: "written", n, panics: w.panics || null, gate: (pv.src.gates.find((g) => g.where.includes(w.name)) || {}).feature || null });
  }
  for (const d of pv.src.decl.derives) traits.push({ name: d, verb: { PartialEq: "compares", Clone: "clones", Debug: "debug-prints" }[d], arrival: "derived", usual: true });
  const autos = Object.entries(pv.src.autos.t).filter(([, v]) => v === "yes").map(([k]) => k);
  const does = { kind: "does", title: "What it does", dir: "out", count: { n: inh0.length + traits.length, label: `${inh0.length} of its own · ${traits.filter((t) => !t.usual && t.name !== "IndexMut").length} contracts` }, onCases: inh0.length - inh.length, groups, traits, usual: { n: 3 + autos.length + pv.src.blankets.length, names: [...pv.src.decl.derives, ...autos, ...pv.src.blankets.map((b) => b.t)], autos } };
  const fails = {
    kind: "fails", title: "What can go wrong", count: { label: "3 ways" },
    roots: [
      { op: "value[key]", via: "Index", how: "panics", what: "index not found", line: "value.rs:235", instead: { name: "get", words: [T.w("maybe"), T.ty("Value")] } },
      { op: "s.parse()", via: "FromStr", how: "fails with", err: { name: "de::Error", words: [T.ty("toml::de::Error")] }, what: "not a TOML document: says the message and the span", kinds: null, carries: ["message", "span"] },
      { op: "try_into::<T>()", via: "", how: "fails with", err: { name: "de::Error", words: [T.ty("toml::de::Error")] }, what: "the value does not have T's shape" },
    ],
  };
  const inuse = inuseOf(pv.harvest.inuse);
  const verbs = pv.relations.filter((r) => r.verb !== "comes from").map((r) => ({ verb: r.verb, n: r.n, yours: r.yours, bands: bandsOf(r) }));
  const total = pv.usedIn, yours = pv.usedYours;
  const uses = { kind: "uses", title: "Who uses it", dir: "out", count: { n: total, yours, tier: "compiler", label: `${total} places · ${yours} yours` }, sites: inuse, verbs };
  const changed = { kind: "changed", title: "What changed", since: sinceOf(pv.since) };
  const words = { kind: "words", title: "Its own words", blocks: [{ t: "p", s: "Representation of a TOML value." }], empty: "Its author wrote one sentence; it is the lede above." };
  return base({
    slug: "rs-value", lang: "rust", eco: "crates.io", pkg: { name: "toml", version: "0.8.23", license: "MIT OR Apache-2.0" },
    name: "Value", kind: "enum", fam: "ty", concept: "choice", lede: "Representation of a TOML value.",
    peeks: { Table: { name: "Table", kind: "type", fam: "ty", where: "type alias in toml::table · table.rs:13", say: "A map of text to `Value`: the payload of the `Table` case.", fact: "Keys sort by name. `preserve_order` keeps file order; it is off in your build." } },
    path: "toml::value", aliases: ["toml::Value"],
    marks: [mark("path", "toml::value", { lines: ["Declared in toml::value", "Also exported as toml::Value"] }), mark("since", "≤ 0.5.11", { lines: ["The oldest of the 4 releases read has it (0.5.11, 2023-01-20).", "120 releases exist."] }), mark("source", "value.rs:25")],
    spec: { kind: "choice", total: cases.length, cases, derives: pv.src.decl.derives, exact: pv.src.decl.code },
    sections: [getting, does, fails, uses, changed, words],
    edges: { in: [{ verb: "comes from", n: comes.n }], out: pv.relations.filter((r) => r.verb !== "comes from").map((r) => ({ verb: r.verb, n: r.n, yours: r.yours })) },
  });
}

function rustDatetime() {
  const f = path.join(REG, "toml_datetime-0.6.11/src/datetime.rs"); const src = lines(f);
  const at = src.findIndex((l) => /^pub struct Datetime \{/.test(l));
  // the type's doc: the /// block above the derive
  let k = at - 1; const docl = []; while (k >= 0 && (src[k].startsWith("///") || src[k].startsWith("#["))) { if (src[k].startsWith("///")) docl.unshift(src[k].slice(3).replace(/^ /, "")); k--; }
  const derives = (src[at - 1].match(/derive\((.+)\)/) || [])[1].split(", ");
  const fields = []; let d = [];
  for (let j = at + 1; j < at + 20; j++) {
    const l = src[j].trim(); if (l === "}") break;
    if (l.startsWith("///")) { d.push(l.slice(3).trim()); continue; }
    const m = l.match(/^pub (\w+): (.+),$/);
    if (m) { const opt = /^Option</.test(m[2]); const inner = opt ? m[2].slice(7, -1) : m[2]; const req = d.join(" ").match(/Required for: (.+?)\.?$/); fields.push({ name: m[1], type: rustWords(inner), exact: m[2], optional: opt, doc: d[0].replace(/\.$/, ""), requiredFor: req ? req[1].replace(/\*/g, "").split(/,\s*/) : [], line: j + 1 }); d = []; }
  }
  // the parts' own fields, one level down (Date, Time), for the rung's hover
  const partFields = (name) => { const a = src.findIndex((l) => new RegExp(`^pub struct ${name} \\{`).test(l)); const out = []; for (let j = a + 1; j < a + 20; j++) { const m = src[j].trim().match(/^pub (\w+): (\w+),$/); if (m) out.push({ name: m[1], exact: m[2] }); if (src[j].trim() === "}") break; } return out; };
  const offsetEnum = (() => { const a = src.findIndex((l) => /^pub enum Offset \{/.test(l)); const out = []; for (let j = a + 1; j < a + 20; j++) { const l = src[j]; if (l.trim() === "}" && /^\}/.test(l)) break; const m = l.match(/^    (\w+)/); if (m) out.push(m[1]); } return out; })();
  // who mentions it, matched by name in the sources beside it (dashed: not compiler-resolved)
  const scan = (dir, re) => { const hits = []; const walk = (p) => { for (const e of fs.readdirSync(p, { withFileTypes: true })) { const q = path.join(p, e.name); if (e.isDirectory()) walk(q); else if (e.name.endsWith(".rs")) lines(q).forEach((l, n) => { if (re.test(l) && !/^\s*\/\//.test(l)) hits.push({ file: path.relative(REG, q), line: n + 1, text: l.trim() }); }); } }; walk(dir); return hits; };
  const re = /\bDatetime\b/;
  const inToml = scan(path.join(REG, "toml-0.8.23/src"), re), inEdit = scan(path.join(REG, "toml_edit-0.22.27/src"), re);
  const REPO = path.resolve(V4, "../..");
  const mine = [];
  for (const d2 of ["apps", "crates"]) { try { const walk = (p) => { for (const e of fs.readdirSync(p, { withFileTypes: true })) { if (e.name === "target" || e.name.startsWith(".")) continue; const q = path.join(p, e.name); if (e.isDirectory()) walk(q); else if (e.name.endsWith(".rs")) lines(q).forEach((l, n) => { if (/toml::(value::)?Datetime|toml_datetime::Datetime/.test(l)) mine.push({ file: path.relative(REPO, q), line: n + 1, text: l.trim() }); }); } }; walk(path.join(REPO, d2)); } catch {} }
  const fromStr = src.findIndex((l) => /impl str::FromStr for Datetime|impl FromStr for Datetime/.test(l));
  const froms = src.map((l, n) => ({ l, n })).filter((x) => /^impl From<(\w+)> for Datetime/.test(x.l)).map((x) => ({ from: x.l.match(/From<(\w+)>/)[1], line: x.n + 1 }));
  const errAt = src.findIndex((l) => /^pub struct DatetimeParseError/.test(l));
  return base({
    slug: "rs-datetime", lang: "rust", eco: "crates.io", pkg: { name: "toml", version: "0.8.23", license: "MIT OR Apache-2.0" },
    name: "Datetime", kind: "struct", fam: "ty", concept: "record", lede: docl[0].replace(/\.?$/, "."),
    path: "toml_datetime", aliases: ["toml::value::Datetime"],
    marks: [mark("path", "toml_datetime", { lines: ["Declared in toml_datetime 0.6.11", "Re-exported as toml::value::Datetime (value.rs:15)"] }), mark("source", `datetime.rs:${at + 1}`)],
    spec: { kind: "record", fields, private: 0, derives, parts: { Date: partFields("Date"), Time: partFields("Time"), Offset: offsetEnum }, forms: ["Offset Date-Time", "Local Date-Time", "Local Date", "Local Time"], exact: src.slice(at, at + 13).join("\n") },
    sections: [
      { kind: "getting", title: "Getting one", dir: "in", count: { label: "4 ways" }, routes: [
        { from: [T.prim("text")], steps: [{ name: "parse", fam: "ca", fails: true, via: "FromStr" }], lands: { why: "any of the four forms" }, code: `let d: Datetime = "1979-05-27T07:32:00Z".parse()?;`, lead: true, line: fromStr + 1, err: "DatetimeParseError" },
        ...froms.map((x) => ({ from: [T.ty(x.from)], steps: [{ name: "from", fam: "ca" }], lands: { why: x.from === "Date" ? "date only: a Local Date" : "time only: a Local Time" }, code: `Datetime::from(${x.from.toLowerCase()})`, line: x.line })),
        { from: [T.ty("Value")], steps: [{ name: "as_datetime", fam: "ca", maybe: true }], lands: { why: "when the value is the Datetime case" }, code: `value.as_datetime()` },
      ] },
      { kind: "does", title: "What it does", dir: "out", groups: [], traits: [
        { name: "FromStr", verb: "parses", arrival: "written" }, { name: "Display", verb: "prints", arrival: "written" }, { name: "Serialize", verb: "serializes", arrival: "written", gate: "serde" }, { name: "Deserialize", verb: "deserializes", arrival: "written", gate: "serde" },
        ...derives.map((x) => ({ name: x, verb: { PartialEq: "compares", Eq: "compares", PartialOrd: "orders", Ord: "sorts", Copy: "copies freely", Clone: "clones", Debug: "debug-prints" }[x], arrival: "derived", usual: ["Clone", "Debug", "PartialEq", "Eq", "PartialOrd"].includes(x) })),
      ], usual: { n: 5, names: ["Clone", "Debug", "PartialEq", "Eq", "PartialOrd"] } },
      { kind: "fails", title: "What can go wrong", roots: [{ op: "s.parse()", via: "FromStr", how: "fails with", err: { name: "DatetimeParseError", words: [T.ty("DatetimeParseError")] }, what: "not one of the four forms", line: `datetime.rs:${errAt + 1}`, nonExhaustive: true }] },
      { kind: "uses", title: "Who uses it", dir: "out", tier: "name", count: { n: inToml.length + inEdit.length + mine.length, yours: mine.length, label: `${inToml.length + inEdit.length} mentions · ${mine.length} yours`, tier: "name" }, mentions: { toml: inToml.length, toml_edit: inEdit.length, yours: mine.slice(0, 6) }, sites: [], verbs: [] },
      { kind: "words", title: "Its own words", blocks: [{ t: "md", s: docl.slice(1).join("\n") }] },
    ],
    edges: { in: [{ verb: "comes from", n: 4 }], out: [{ verb: "used by", n: inToml.length + inEdit.length + mine.length, tier: "name" }] },
  });
}

function rustFromStr() {
  const decl = pf.src.decl; const docs = pf.src.docs;
  const kinds = pe.src.kinds.kinds;
  const serdeDe = lines(path.join(REG, "serde_core-1.0.229/src/de/mod.rs"));
  const dataCauses = [];
  serdeDe.forEach((l, k) => { const m = l.match(/fn (invalid_type|invalid_value|invalid_length|unknown_variant|unknown_field|missing_field|duplicate_field)\(/); if (m) { let msg = ""; for (let j = k; j < k + 18; j++) { const f = serdeDe[j].match(/format_args!\("([^"]+)"/); if (f) { msg = f[1]; break; } } dataCauses.push({ n: m[1], msg: msg || m[1].replace(/_/g, " ") }); } });
  const deTrait = W.find("serde_core", "de", "Deserialize", "trait");
  const impl = W.inEdges(deTrait, W.B.impl | W.B.derives); const tops = [...new Set(impl.map((j) => W.topOf[j]))]; const yoursT = tops.filter((j) => W.yours(j));
  const called = relOf(pf, "called by");
  return base({
    slug: "rs-from_str", lang: "rust", eco: "crates.io", pkg: { name: "serde_json", version: "1.0.151", license: "MIT OR Apache-2.0" },
    name: "from_str", kind: "fn", fam: "ca", concept: "callable", lede: first(docs.summary),
    path: "serde_json::de", aliases: ["serde_json::from_str"],
    marks: [mark("path", "serde_json::de", { lines: ["Declared in serde_json::de", "Also exported as serde_json::from_str"] }), mark("source", `de.rs:${decl.line}`)],
    spec: {
      kind: "callable", generics: [{ name: "T", words: "any type that can be deserialized, borrowing from s", bound: "T: de::Deserialize<'a>" }, { name: "'a", words: "how long s lives", lifetime: true }],
      inputs: [{ name: "s", type: [T.prim("text", "&'a str")], exact: "&'a str" }], output: { type: [T.v("T")], exact: "T" },
      fails: [{ how: "Result", type: [T.ty("Error")], exact: "serde_json::Error", kinds: 3 }],
      exact: decl.code,
    },
    sections: [
      { kind: "calling", title: "Calling it", dir: "in", count: { label: `${tops.length} types can be T` }, pick: { param: "T", n: tops.length, yours: yoursT.length, names: yoursT.slice(0, 5).map((j) => W.N[j].n), how: [{ way: "derive", code: "#[derive(Deserialize)] struct User { … }", arrival: "derived" }, { way: "any JSON", code: "serde_json::Value", note: "when the shape is unknown" }] } },
      { kind: "fails", title: "What can go wrong", count: { label: "3 kinds" }, err: { name: "Error", pkg: "serde_json", via: "classify()", line: `error.rs:${pe.src.kinds.line}` }, kinds: [
        { name: "Syntax", doc: "not valid JSON", causes: kinds.find((x) => x.n === "Syntax").causes.map((c) => c.msg) },
        { name: "Data", doc: "valid JSON, wrong shape for T", causes: [...dataCauses.map((c) => c.msg.replace(/\{\}/g, "…").replace(/`…`/g, "`…`")), "your Deserialize's own message"] },
        { name: "Eof", doc: "the text ended too soon", causes: kinds.find((x) => x.n === "Eof").causes.map((c) => c.msg) },
        { name: "Io", doc: "only when reading an io::Read", never: "from_str reads text: Error::io is built only in read.rs:270 and read.rs:290 (IoRead)", causes: [] },
      ], tells: { how: "err.classify()", also: ["err.line()", "err.column()"] } },
      { kind: "uses", title: "Who uses it", dir: "out", count: { n: called.n, yours: 0, label: `${called.n} callers`, tier: "compiler" }, sites: inuseOf(pf.harvest.inuse), verbs: [{ verb: "called by", n: called.n, yours: 0, bands: bandsOf(called) }] },
      { kind: "changed", title: "What changed", since: null, unknown: "serde_json's release history isn't read yet: 1 release on this machine (1.0.151)." },
      { kind: "words", title: "Its own words", blocks: docs.sections.flatMap((s) => [...(s.title ? [{ t: "h", s: s.title }] : []), { t: "md", s: s.md }]) },
    ],
    edges: { in: [{ verb: "called by", n: called.n }], out: [{ verb: "calls", n: 2 }] },
  });
}

function rustSerialize() {
  const c = ps.src.contract; const done = relOf(ps, "done by"), asked = relOf(ps, "asked for by"), on = relOf(ps, "called on by");
  return base({
    slug: "rs-serialize", lang: "rust", eco: "crates.io", pkg: { name: "serde_core", version: "1.0.229", license: "MIT OR Apache-2.0" },
    name: "Serialize", kind: "trait", fam: "co", concept: "contract", lede: first(ps.src.docs.summary).replace(/\*\*/g, ""),
    path: "serde_core::ser", aliases: ["serde::Serialize", "serde::ser::Serialize"],
    marks: [mark("path", "serde::ser", { lines: ["Declared in serde_core::ser", "Exported as serde::Serialize"] }), mark("source", `ser/mod.rs:${ps.src.decl.line}`)],
    spec: {
      kind: "contract", required: c.required.map((r) => ({ name: r.n, words: [T.w("into any"), T.ty("Serializer", "co")], say: first(r.summary), sig: r.sig })), provided: (c.provided || []).map((r) => ({ name: r.n, sig: r.sig })),
      sat: { n: done.n, yours: done.yours, bands: bandsOf(done), tier: "compiler" }, exact: ps.src.decl.code,
      doing: [
        { how: "derive", arrival: "derived", code: "#[derive(Serialize)]", note: "most do", n: (ps.treeUses || []).length },
        { how: "write", arrival: "written", code: "impl Serialize for T { fn serialize(…) }" },
      ],
    },
    sections: [
      { kind: "doing", title: "Doing it", dir: "in", count: { label: `${done.n} types · ${done.yours} yours` }, sites: (ps.treeUses || []).map((u) => ({ caller: (u.code.match(/(?:struct|enum) (\w+)/) || [])[1], pkg: u.pkg, where: `${u.file}:${u.line}`, yours: true, code: u.code.split("\n") })), bands: bandsOf(done) },
      { kind: "uses", title: "Who uses it", dir: "out", count: { n: asked.n, yours: asked.yours, label: `${asked.n} ask for it · ${asked.yours} yours`, tier: "compiler" }, sites: [], verbs: [{ verb: "asked for by", n: asked.n, yours: asked.yours, bands: bandsOf(asked) }, { verb: "called on by", n: on.n, yours: on.yours, bands: bandsOf(on) }] },
      { kind: "words", title: "Its own words", blocks: ps.src.docs.sections.filter((s) => s.kind === "body").map((s) => ({ t: "md", s: s.md })) },
    ],
    edges: { in: [{ verb: "done by", n: done.n, yours: done.yours }], out: [{ verb: "asked for by", n: asked.n, yours: asked.yours }, { verb: "called on by", n: on.n }] },
  });
}

// ================================================================= TYPESCRIPT
function tsChoice() {
  const c = TS.choice;
  const prop = (p) => ({ name: p.name, type: tsWords(p.type), exact: p.type, optional: p.optional, readonly: p.readonly, doc: p.doc });
  return base({
    slug: "ts-issue", lang: "ts", eco: "npm", pkg: { name: "zod", version: TS.packages.zod.version, license: TS.packages.zod.license },
    name: "$ZodIssue", kind: "type", fam: "ty", concept: "choice", lede: "", shapeLede: `One of ${c.cases.length} shapes, told apart by code.`,
    ledeSource: "no doc comment; the lede is the page's own summary of its shape",
    path: "zod/v4/core", aliases: ["z.core.$ZodIssue", "z.ZodIssue"],
    marks: [mark("path", "z.core", { lines: ["Declared in zod/v4/core errors.ts", "Exported as z.core.$ZodIssue"] }), mark("alias-deprecated", "z.ZodIssue", { lines: ["z.ZodIssue is deprecated:", c.alias.deprecated] }), mark("source", `errors.ts:${c.line}`)],
    spec: {
      kind: "choice", total: c.cases.length, disc: "code", shared: c.base.props.filter((p) => p.name !== "code").map(prop), sharedFrom: "$ZodIssueBase",
      cases: c.cases.map((k) => ({ name: k.name, lit: k.lit, payload: k.own.map(prop), doc: k.doc, quiet: true, line: k.line })), exact: c.decl, readonlyAll: true,
    },
    sections: [
      { kind: "getting", title: "Getting one", dir: "in", count: { label: "you receive them" }, receive: true, routes: [
        { from: [T.ty("ZodError")], steps: [{ name: ".issues", fam: "va" }], lands: { why: "list of every issue" }, code: "err.issues", lead: true },
        { from: [T.w("a failed"), T.ty("safeParse", "ca")], steps: [{ name: ".error.issues", fam: "va" }], lands: { why: "when success is false" }, code: "const r = schema.safeParse(x); if (!r.success) r.error.issues" },
      ], note: "Zod builds issues; you read them. Custom ones come from a refine or superRefine check." },
      { kind: "does", title: "What it does", dir: "out", groups: [], traits: [], tellApart: { by: "code", n: c.cases.length } },
      { kind: "uses", title: "Who uses it", dir: "out", tier: "name", count: { n: 0, yours: 0, label: "none outside zod on this machine", tier: "name" }, sites: [], verbs: [], none: "Nothing on this machine outside zod mentions it. @opencode-ai/plugin imports zod but never names an issue." },
      { kind: "changed", title: "What changed", since: null, unknown: "One release on this machine (4.1.8). The v3 shape ships beside it as zod/v3." },
    ],
    edges: { in: [{ verb: "comes from", n: 2 }], out: [{ verb: "used by", n: 0, tier: "name" }] },
  });
}
function tsRecord() {
  const r = TS.record;
  const prop = (p, from) => ({ name: p.name, type: tsWords(p.type), exact: p.type, optional: p.optional, readonly: p.readonly, doc: p.doc, from });
  const own = r.props.map((p) => prop(p));
  const inherited = r.base.props.filter((b) => !r.props.some((p) => p.name === b.name)).map((p) => prop(p, "$ZodIssueBase"));
  return base({
    slug: "ts-toosmall", lang: "ts", eco: "npm", pkg: { name: "zod", version: TS.packages.zod.version, license: TS.packages.zod.license },
    name: "$ZodIssueTooSmall", kind: "interface", fam: "ty", concept: "record", lede: "", shapeLede: `Holds ${own.length + inherited.length}, ${own.filter((f) => f.optional).length + inherited.filter((f) => f.optional).length} optional: the "too_small" case of $ZodIssue.`,
    ledeSource: "no doc comment; composed from its fields",
    path: "zod/v4/core", aliases: ["z.core.$ZodIssueTooSmall"],
    marks: [mark("path", "z.core"), mark("source", `errors.ts:${r.line}`)],
    spec: { kind: "record", generics: r.typeParams.map((t) => ({ name: t.name, words: t.dflt ? `defaults to ${TS_PRIM[t.dflt] || t.dflt}` : "" })), extends: [{ name: "$ZodIssueBase", fields: inherited }], fields: own, private: 0, readonlyAll: true, exact: r.decl, memberOf: { name: "$ZodIssue", case: '"too_small"' } },
    sections: [
      { kind: "getting", title: "Getting one", dir: "in", count: { label: "1 way" }, routes: [{ from: [T.ty("$ZodIssue")], steps: [{ name: 'code === "too_small"', fam: "va", narrow: true }], lands: { why: "TypeScript narrows the union" }, code: `if (issue.code === "too_small") issue.minimum`, lead: true }] },
      { kind: "uses", title: "Who uses it", dir: "out", count: { label: "held by $ZodIssue", n: 1 }, sites: [], verbs: [{ verb: "one case of", n: 1, bands: [{ band: "here", n: 1, pkgs: [{ pkg: "zod", n: 1, ids: [{ name: "$ZodIssue", kind: "type", pkg: "zod" }] }] }] }] },
    ],
    edges: { in: [{ verb: "comes from", n: 1 }], out: [{ verb: "held by", n: 1 }] },
  });
}
function tsCallable() {
  const c = TS.callable;
  const ctx = c.context.props.map((p) => ({ name: p.name, doc: p.doc.replace(/ Default `false`\./, ""), dflt: (p.doc.match(/Default `(\w+)`/) || [])[1] || null, type: tsWords(p.type) }));
  return base({
    slug: "ts-parse", lang: "ts", eco: "npm", pkg: { name: "zod", version: TS.packages.zod.version, license: TS.packages.zod.license },
    name: "parse", owner: "ZodType", kind: "method", fam: "ca", concept: "callable", lede: "", shapeLede: `Takes data and optional params; gives its output, or throws one of ${c.throws.length}.`,
    ledeSource: "no doc comment; composed from its body (core/parse.ts:16)",
    path: "zod · ZodType", aliases: ["schema.parse", "z.parse"],
    marks: [mark("path", "ZodType"), mark("source", `schemas.ts:${c.line}`)],
    spec: {
      kind: "callable", recv: { name: "schema", type: [T.ty("ZodType")], mode: "reads it" },
      inputs: [{ name: "data", type: tsWords("unknown"), exact: "unknown" }, { name: "params", type: [T.ty("ParseContext")], exact: "ParseContext<$ZodIssue>", optional: true, options: ctx }],
      output: { type: [T.w("its output")], exact: "core.output<this>", words: "the schema's output type: what z.infer<typeof schema> names" },
      fails: c.throws.map((t) => ({ how: "throws", type: [T.ty(t.cls === "ZodRealError" ? "ZodError" : t.cls)], exact: t.cls, when: t.cls.includes("Async") ? "a check or transform is async" : "the data breaks any rule", line: `${t.file.split("/").slice(-2).join("/")}:${t.line}`, message: t.message })),
      alt: { name: "safeParse", words: "never throws: gives { success, data } or { success, error }" },
      later: { name: "parseAsync", words: "for async checks: gives, later" },
      exact: c.decl,
    },
    sections: [
      { kind: "calling", title: "Calling it", dir: "in", count: { label: "on any schema" }, chain: c.chain },
      { kind: "fails", title: "What can go wrong", count: { label: "2 throws" }, throws: c.throws.map((t) => ({ cls: t.cls === "ZodRealError" ? "ZodError" : t.cls, line: `${t.file.replace(/^zod\/src\/v4\//, "")}:${t.line}`, message: t.message, issues: t.cls === "ZodRealError" })) },
      { kind: "uses", title: "Who uses it", dir: "out", tier: "name", count: { n: 2, yours: 0, label: "2 inside zod", tier: "name" }, sites: [
        { caller: "ZodType.decode", pkg: "zod", where: "classic/schemas.ts", yours: false, code: ["inst.decode = (data, params) => parse.decode(inst, data, params);"] },
      ], verbs: [], none: "Nothing on this machine outside zod calls it." },
    ],
    edges: { in: [{ verb: "called on by", n: 0 }], out: [{ verb: "throws", n: 2 }] },
  });
}
function tsContract() {
  const c = TS.contract;
  const req = c.members.filter((m) => m.abstract), prov = c.members.filter((m) => !m.abstract && m.kind === "method"), fields = c.members.filter((m) => m.kind === "field");
  return base({
    slug: "ts-collection", lang: "ts", eco: "npm", pkg: { name: "yaml", version: TS.packages.yaml.version, license: TS.packages.yaml.license },
    name: "Collection", kind: "abstract class", fam: "co", concept: "contract", lede: "", shapeLede: `Asks for ${req.map((m) => m.name).join(", ")}; gives ${prov.length} more.`,
    ledeSource: "no doc comment; composed from its members",
    path: "yaml", aliases: ["import { Collection } from 'yaml'"],
    marks: [mark("path", "yaml"), mark("source", `Collection.d.ts:${c.line}`)],
    spec: {
      kind: "contract", extendsFrom: c.extends,
      required: req.map((m) => ({ name: m.name, params: m.params, words: m.params.map((p) => p.name + (p.optional ? "?" : "")).join(", "), ret: tsWords(m.type), say: first(m.doc) })),
      provided: prov.map((m) => ({ name: m.name, params: m.params, ret: tsWords(m.type), say: first(m.doc) })),
      fields: fields.map((f) => ({ name: f.name, type: tsWords(f.type), optional: f.optional, doc: first(f.doc) })),
      sat: { n: c.satisfiers.length, yours: 0, tier: "compiler", list: c.satisfiers.map((s) => ({ name: s.name, through: s.through, fills: s.fills, where: `${s.file.split("/").pop()}:${s.line}` })) },
      doing: [{ how: "extend", arrival: "written", code: "class MyNode extends Collection { add; delete; get; has; set }" }],
      exact: c.decl,
    },
    sections: [
      { kind: "uses", title: "Who uses it", dir: "out", tier: "name", count: { n: c.uses, label: `${c.uses} mentions in yaml`, tier: "name" }, usedIn: c.usedIn, sites: [], verbs: [] },
      { kind: "changed", title: "What changed", since: null, unknown: "One release on this machine (2.9.0)." },
    ],
    edges: { in: [{ verb: "done by", n: c.satisfiers.length }], out: [{ verb: "used by", n: c.uses, tier: "name" }] },
  });
}

// ================================================================= GO
function goChoice() {
  const o = GO.objects.choice;
  return base({
    slug: "go-errorhandling", lang: "go", eco: "go", pkg: { name: "pflag", version: "1.0.9", license: "BSD-3-Clause", path: GO.path },
    name: "ErrorHandling", kind: "type", fam: "ty", concept: "choice", lede: goLede(o.doc, "ErrorHandling"),
    path: "github.com/spf13/pflag", aliases: ["pflag.ErrorHandling"],
    marks: [mark("path", "pflag"), mark("source", `${o.file}:${o.line}`)],
    spec: { kind: "choice", total: o.consts.length, values: true, underlying: "int", cases: o.consts.map((c) => ({ name: c.name, lit: c.value, doc: c.doc.replace(new RegExp("^" + c.name + " "), ""), quiet: false, line: c.line })), open: { why: "Go can't close it: any int converts", exact: "type ErrorHandling int" }, exact: o.decl },
    sections: [
      { kind: "getting", title: "Getting one", dir: "in", count: { label: "pick one of 3" }, pick: true, routes: [], note: "Pick one of the three constants above and hand it to NewFlagSet." },
      { kind: "does", title: "What it does", dir: "out", decides: { what: "what Parse does when it fails", link: "go-parse" }, takers: o.takers.map((m) => m.name) },
      { kind: "uses", title: "Who uses it", dir: "out", count: { n: o.useCount.pflag + o.useCount.cobra, label: `${o.useCount.pflag + o.useCount.cobra} references`, tier: "compiler" }, sites: o.uses.filter((u) => u.pkg === "cobra").slice(0, 4).map((u) => ({ caller: u.caller, pkg: u.pkg, where: `${u.file}:${u.line}`, code: [u.text] })), verbs: [{ verb: "taken by", n: o.takers.length, bands: [{ band: "here", n: o.takers.length, pkgs: [{ pkg: "pflag", n: o.takers.length, ids: o.takers.map((m) => ({ name: m.name, kind: "fn", pkg: "pflag" })) }] }] }] },
    ],
    edges: { in: [{ verb: "comes from", n: 3 }], out: [{ verb: "taken by", n: o.takers.length }, { verb: "used by", n: o.useCount.cobra }] },
  });
}
function goRecord() {
  const o = GO.objects.record;
  const cobraSites = o.uses.filter((u) => u.pkg === "cobra");
  return base({
    slug: "go-flag", lang: "go", eco: "go", pkg: { name: "pflag", version: "1.0.9", license: "BSD-3-Clause", path: GO.path },
    name: "Flag", kind: "struct", fam: "ty", concept: "record", lede: goLede(o.doc, "Flag"),
    path: "github.com/spf13/pflag", aliases: ["pflag.Flag"],
    marks: [mark("path", "pflag"), mark("source", `${o.file}:${o.line}`)],
    spec: { kind: "record", fields: o.fields.map((f) => ({ name: f.name, type: goWords(f.type), exact: f.type, optional: false, doc: f.doc, deprecatedNote: /deprecated/i.test(f.name) })), private: o.fields.filter((f) => !f.exported).length, exact: o.decl },
    sections: [
      { kind: "getting", title: "Getting one", dir: "in", count: { label: `${o.makers.length} ways` }, routes: [
        { from: [T.prim("text", "string")], steps: [{ name: "Lookup", fam: "ca", maybe: true, on: "FlagSet" }], lands: { why: "nil when no such flag" }, code: `fs.Lookup("verbose")`, lead: true },
        { from: [T.ty("Value", "co"), T.w("+ name, shorthand, usage")], steps: [{ name: "VarPF", fam: "ca", on: "FlagSet" }], lands: { why: "defines it and gives it back" }, code: `fs.VarPF(v, "level", "l", "log level")` },
        { from: [T.w("every flag")], steps: [{ name: "VisitAll", fam: "ca", on: "FlagSet" }], lands: { why: "one at a time, sorted" }, code: `fs.VisitAll(func(f *pflag.Flag) { … })` },
      ], more: { n: o.makers.length - 2, names: o.makers.map((m) => m.name) } },
      { kind: "uses", title: "Who uses it", dir: "out", count: { n: o.useCount.pflag + o.useCount.cobra, label: `${o.useCount.cobra} in cobra · ${o.useCount.pflag} in pflag`, tier: "compiler" }, sites: cobraSites.slice(0, 5).map((u) => ({ caller: u.caller, pkg: "cobra", where: `${u.file}:${u.line}`, code: [u.text] })), verbs: [{ verb: "taken by", n: o.takers.length, bands: [{ band: "here", n: o.takers.length, pkgs: [{ pkg: "pflag", n: o.takers.length, ids: o.takers.slice(0, 6).map((m) => ({ name: m.name, kind: "fn", pkg: "pflag" })) }] }] }] },
    ],
    edges: { in: [{ verb: "comes from", n: o.makers.length }], out: [{ verb: "taken by", n: o.takers.length }, { verb: "used by", n: o.useCount.cobra }] },
  });
}
function goCallable() {
  const o = GO.objects.callable;
  const kinds = o.kinds.map((k) => ({ name: k.type || k.sentinel || "", sentinel: !!k.sentinel, through: k.through || null, wraps: k.wraps, messages: (k.messages || []).map((m) => m.replace(/%q/g, "“…”").replace(/%[svd]/g, "…")), makers: k.makers, line: `${k.file}:${k.line}` }));
  const through = kinds.find((k) => k.through); const wraps = kinds.find((k) => k.wraps);
  return base({
    slug: "go-parse", lang: "go", eco: "go", pkg: { name: "pflag", version: "1.0.9", license: "BSD-3-Clause", path: GO.path },
    name: "Parse", owner: "FlagSet", kind: "method", fam: "ca", concept: "callable", lede: goLede(o.doc, "Parse"),
    path: "github.com/spf13/pflag", aliases: ["(*pflag.FlagSet).Parse"],
    marks: [mark("path", "pflag · FlagSet"), mark("source", `${o.file}:${o.line}`)],
    spec: {
      kind: "callable", recv: { name: "f", type: [T.ty("FlagSet")], mode: "changes it", exact: o.recv.type },
      inputs: [{ name: "arguments", type: goWords(o.params[0].type), exact: o.params[0].type, hint: "os.Args[1:]" }],
      output: null, nothing: "gives nothing back but the error",
      fails: [{ how: "error", type: [T.prim("error")], exact: "error", kinds: kinds.filter((k) => !k.through).length }],
      policy: o.policy.map((p) => ({ case: p.case, action: p.action, note: p.note })),
      exact: o.decl,
    },
    sections: [
      { kind: "calling", title: "Calling it", dir: "in", count: { label: "needs a FlagSet" }, routes: [
        { from: [T.prim("text", "name"), T.w("+"), T.ty("ErrorHandling")], steps: [{ name: "NewFlagSet", fam: "ca" }], lands: { why: "the receiver" }, code: `fs := pflag.NewFlagSet("app", pflag.ContinueOnError)`, lead: true },
        { from: [T.w("the program's own")], steps: [{ name: "CommandLine", fam: "va" }], lands: { why: "set to ExitOnError" }, code: "pflag.CommandLine" },
      ] },
      { kind: "fails", title: "What can go wrong", count: { label: `${kinds.filter((k) => !k.through).length} kinds · then the policy` }, kinds: kinds.filter((k) => !k.through), through: through ? { via: through.through, into: wraps ? wraps.name : null } : null, policy: o.policy, tells: { how: "errors.As(err, &target)", sentinel: "errors.Is(err, pflag.ErrHelp)" } },
      { kind: "uses", title: "Who uses it", dir: "out", count: { n: o.useCount.cobra + o.useCount.pflag, label: "1 in cobra · 1 in pflag", tier: "compiler" }, sites: o.uses.map((u) => ({ caller: u.caller, pkg: u.pkg, where: `${u.file}:${u.line}`, code: [u.text] })), verbs: [] },
    ],
    edges: { in: [{ verb: "called by", n: o.useCount.cobra + o.useCount.pflag }], out: [{ verb: "fails with", n: kinds.length }] },
  });
}
function goContract() {
  const o = GO.objects.contract;
  const sat = o.satisfiers;
  const family = (n) => (n.match(/^(bool|int|uint|float|string|ip|duration|bytes|count|func|time|text|boolfunc|flagValue)/) || [n])[0];
  const fams = new Map(); for (const s of sat) { const f = family(s.name).replace(/^(int|uint)$/, "$1").replace("boolfunc", "func"); (fams.get(f) || fams.set(f, []).get(f)).push(s); }
  return base({
    slug: "go-value", lang: "go", eco: "go", pkg: { name: "pflag", version: "1.0.9", license: "BSD-3-Clause", path: GO.path },
    name: "Value", kind: "interface", fam: "co", concept: "contract", lede: goLede(o.doc, "Value"),
    path: "github.com/spf13/pflag", aliases: ["pflag.Value"],
    marks: [mark("path", "pflag"), mark("source", `${o.file}:${o.line}`)],
    spec: {
      kind: "contract", implicit: true,
      required: o.methods.map((m) => ({ name: m.name, sig: m.sig, words: m.name === "Set" ? "text, may fail" : "", ret: goWords(m.sig.replace(/^func\(.*?\)\s*/, "") || "") })), provided: [],
      sat: { n: sat.length, yours: 0, exported: sat.filter((s) => s.exported).length, tier: "compiler", computed: true, pkgs: [{ pkg: "pflag", n: sat.filter((s) => s.pkg === "pflag").length }, { pkg: "cobra", n: sat.filter((s) => s.pkg === "cobra").length }], families: [...fams].map(([f, xs]) => ({ family: f, n: xs.length, names: xs.map((x) => x.name) })).sort((a, b) => b.n - a.n), slice: GO.counts.SliceValue },
      doing: [{ how: "implicit", arrival: "written", code: "func (v *level) String() string · Set(string) error · Type() string", note: "no declaration: having the three methods is enough" }],
      exact: o.decl,
    },
    sections: [
      { kind: "uses", title: "Who uses it", dir: "out", count: { n: o.useCount.pflag + o.useCount.cobra, label: `taken by ${o.takers.length} · ${o.useCount.pflag} references`, tier: "compiler" }, sites: [], verbs: [{ verb: "taken by", n: o.takers.length, bands: [{ band: "here", n: o.takers.length, pkgs: [{ pkg: "pflag", n: o.takers.length, ids: o.takers.map((m) => ({ name: m.name, kind: "fn", pkg: "pflag" })) }] }] }] },
    ],
    edges: { in: [{ verb: "done by", n: sat.length }], out: [{ verb: "taken by", n: o.takers.length }] },
  });
}

// ================================================================= the package page (toml 0.8.23)
function tomlPackage() {
  const dir = path.join(REG, "toml-0.8.23/src");
  const KIND = { fn: "fn", struct: "struct", enum: "enum", trait: "trait", type: "type", const: "const", static: "const", mod: "module" };
  // the public items a module declares at its top level, read from its source (pub use lists are counted per name)
  const itemsOf = (f) => {
    const out = [];
    const read = (file) => lines(file).forEach((l, k, all) => {
      if (/#\[doc\(hidden\)\]/.test(all[k - 1] || "")) return;
      let m = l.match(/^pub (fn|struct|enum|trait|type|const|static) (\w+)/); if (m) { out.push({ name: m[2], kind: KIND[m[1]] }); return; }
      m = l.match(/^pub use [\w:]+::\{([^}]+)\}/); if (m) { m[1].split(",").map((x) => x.trim()).filter(Boolean).forEach((x) => out.push({ name: x.split(" as ").pop(), kind: /^[a-z]/.test(x) ? "fn" : "struct", reexport: true })); return; }
      m = l.match(/^pub use [\w:]+::(\w+);/); if (m) { out.push({ name: m[1], kind: /^[a-z]/.test(m[1]) ? "fn" : "struct", reexport: true }); return; }
      if (/^#\[macro_export\]/.test(l)) { const n = (all[k + 1] || "").match(/macro_rules! (\w+)/); if (n) out.push({ name: n[1] + "!", kind: "macro" }); }
    });
    if (fs.statSync(f).isDirectory()) { const walk = (d) => fs.readdirSync(d, { withFileTypes: true }).forEach((e) => { const q = path.join(d, e.name); if (e.isDirectory()) walk(q); else if (e.name.endsWith(".rs")) read(q); }); walk(f); } else read(f);
    return out;
  };
  const pkgIdx = W.PK.findIndex((p) => p.name === "toml");
  // your uses: items of this package that your code refers to, by name (compiler-resolved in the world)
  const usedBy = new Map();
  for (let i = 0; i < W.NN; i++) {
    const n = W.N[i]; if (n.p !== pkgIdx || n.u >= 0) continue;
    for (const [j] of W.inWithBits(i)) { const t = W.topOf[j]; if (W.N[t].p === pkgIdx) continue; const r = usedBy.get(n.n) || usedBy.set(n.n, { all: new Set(), yours: new Set() }).get(n.n); r.all.add(t); if (W.yours(t)) r.yours.add(t); }
    for (const k of W.kids[i] || []) for (const [j] of W.inWithBits(k)) { const t = W.topOf[j]; if (W.N[t].p === pkgIdx) continue; const r = usedBy.get(n.n) || usedBy.set(n.n, { all: new Set(), yours: new Set() }).get(n.n); r.all.add(t); if (W.yours(t)) r.yours.add(t); }
  }
  const FAM = { fn: "ca", struct: "ty", enum: "ty", type: "ty", trait: "co", const: "va", macro: "ca", module: "ns" };
  const mods = [["value", "value.rs"], ["de", "de.rs"], ["ser", "ser"], ["map", "map.rs"], ["macros", "macros.rs"]].map(([m, f]) => {
    const items = itemsOf(path.join(dir, f)).map((it) => { const u = usedBy.get(it.name.replace(/!$/, "")); return { ...it, fam: FAM[it.kind], uses: u ? u.all.size : 0, yours: u ? u.yours.size : 0 }; });
    return { mod: m, items, uses: items.reduce((a, x) => a + x.uses, 0), yours: items.reduce((a, x) => a + x.yours, 0) };
  });
  const root = itemsOf(path.join(dir, "lib.rs")).filter((x) => x.name !== "ReadmeDoctests").map((it) => { const u = usedBy.get(it.name); return { ...it, fam: FAM[it.kind], uses: u ? u.all.size : 0, yours: u ? u.yours.size : 0 }; });
  const rel = JSON.parse(fs.readFileSync(path.join(V4, "graph/releases.json"), "utf8"));
  const tr = (rel.crates || rel).toml;
  const vs = tr ? (tr.versions || tr.releases || []) : [];
  return base({
    slug: "pkg-toml", lang: "rust", eco: "crates.io", kindPage: "package", pkg: { name: "toml", version: "0.8.23", license: "MIT OR Apache-2.0" },
    name: "toml", kind: "package", fam: "ns", concept: "package", lede: "A native Rust encoder and decoder of TOML-formatted files and streams.",
    deps: [{ name: "serde", why: "Serialize / Deserialize on Value" }, { name: "serde_spanned", why: "Spanned<T>" }, { name: "toml_datetime", why: "Datetime" }, { name: "toml_edit", why: "the parser underneath", optional: true, on: true }, { name: "indexmap", why: "keeps key order", optional: true, on: false, feature: "preserve_order" }],
    releases: { n: vs.length, sample: vs.slice(0, 3) },
    tour: [
      { name: "from_str", kind: "fn", fam: "ca", role: "the door", say: "text in, your type out" },
      { name: "Value", kind: "enum", fam: "ty", role: "what you hold", say: "one of 7 TOML kinds" },
      { name: "Table", kind: "type", fam: "ty", role: "inside it", say: "a map of text to Value" },
      { name: "to_string", kind: "fn", fam: "ca", role: "the way back", say: "your type to TOML text" },
      { name: "de::Error", kind: "struct", fam: "ty", role: "when it fails", say: "a message and a span" },
    ],
    root, mods,
    sections: [],
  });
}

const pages = [rustValue(), rustDatetime(), rustFromStr(), rustSerialize(), tsChoice(), tsRecord(), tsCallable(), tsContract(), goChoice(), goRecord(), goCallable(), goContract(), tomlPackage()];
const out = Object.fromEntries(pages.map((p) => [p.slug, p]));
fs.writeFileSync(path.join(ROOT, "data/pages.js"), "window.PAGES=" + JSON.stringify(out) + ";\n");
fs.writeFileSync(path.join(ROOT, "data/pages.json"), JSON.stringify(out, null, 1));
for (const p of pages) console.log(p.slug.padEnd(18), p.concept.padEnd(9), p.name, "·", p.sections.map((s) => s.kind).join(" "));

// the release order for the comb (left = older), from the registry index the v4 extractor read
{
  const rel = JSON.parse(fs.readFileSync(path.join(V4, "graph/releases.json"), "utf8"));
  const t = (rel.crates || rel).toml; const vs = (t.versions || t.releases || []).slice().sort((a, b) => (a.at < b.at ? -1 : 1)).map((v) => v.v.replace(/\+.*$/, ""));
  fs.writeFileSync(path.join(ROOT, "data/releases.js"), "window.TOML_RELEASES=" + JSON.stringify(vs) + ";\n");
}
