// source.mjs — per-item facts read from the crate source (see facts.mjs, rustsrc.mjs).
import fs from "node:fs";
import path from "node:path";
import * as W from "./world.mjs";
import { cfgOf, deprecationOf, splitTop, norm } from "./rustsrc.mjs";
import { CAPW, splitDocs, codeBlocks, whereWords, gateOf, sliceMethods, colonAt } from "./facts.mjs";

const colons = (s) => s.replace(/(?<!:):(?!:)\s*/g, ": ");
const last = (p) => p.replace(/<.*$/s, "").split("::").pop().trim();
const argsOf = (p) => { const a = p.indexOf("<"); return a < 0 ? "" : p.slice(a + 1, p.lastIndexOf(">")); };
const REG = path.join(W.V4, "graph/registry");

// Auto traits are never written; the compiler derives them from the fields. The prototype
// reads them off the fields by hand (checked against the source); the engine must ask the
// trait solver (ra_ap_hir) and fill Arrival::Auto. Each entry: trait → "yes" | "no" | condition.
const AUTO = {
  value: { why: "String, i64, f64, bool, Datetime, Vec<Value> and Map<String, Value> all have them", t: { Send: "yes", Sync: "yes", Unpin: "yes", UnwindSafe: "yes", RefUnwindSafe: "yes", Freeze: "yes" } },
  error: { why: "it can hold an io::Error, whose boxed inner error is not unwind-safe", t: { Send: "yes", Sync: "yes", Unpin: "yes", Freeze: "yes", UnwindSafe: "no", RefUnwindSafe: "no" } },
  smallvec: { why: "its buffer holds raw pointers when it spills", t: { Send: "written: when its items cross threads", Sync: "when A shares across threads", Unpin: "when A is Unpin", UnwindSafe: "when A is UnwindSafe and its items are RefUnwindSafe", RefUnwindSafe: "when A and its items are RefUnwindSafe", Freeze: "yes" } },
};
const AUTO_WORD = { Send: "crosses threads", Sync: "shares across threads", Unpin: "moves when pinned", UnwindSafe: "survives a panic", RefUnwindSafe: "survives a panic by reference", Freeze: "has no inner mutability" };

function modCfgs(src) { // #[cfg] on `mod x;` applies to everything in x
  const m = new Map();
  for (const it of src.items) if (it.k === "mod") { const key = (it.mod ? it.mod + "::" : "") + it.n; const c = cfgOf(it.attrs); if (c.length) m.set(key, c); }
  return (mod) => { const out = []; const parts = (mod || "").split("::").filter(Boolean); for (let k = 1; k <= parts.length; k++) { const c = m.get(parts.slice(0, k).join("::")); if (c) out.push(...c); } return out; };
}
function resolveLink(target, i, peek) {
  let t = target.replace(/^`|`$/g, "").replace(/^(struct|enum|trait|fn|mod|type|macro|method|variant|const)@/, "").replace(/\(\)$/, "").replace(/!$/, "").trim();
  if (/^https?:/.test(t)) return { url: t };
  const name = t.split("::").pop().split("#")[0]; if (!/^\w+$/.test(name)) return null;
  const me = W.N[i]; const cands = [];
  for (let j = 0; j < W.NN; j++) { const n = W.N[j]; if (n.n !== name || n.orphan) continue; cands.push(j); }
  if (!cands.length) return null;
  const quals = t.split("::"); const q2 = quals.length > 1 ? quals[quals.length - 2] : null;
  cands.sort((a, b) => ((W.N[b].p === me.p) - (W.N[a].p === me.p)) || ((q2 && W.qual(b).endsWith(q2)) - (q2 && W.qual(a).endsWith(q2))) || ((W.N[b].u === i) - (W.N[a].u === i)) || W.IMP[b] - W.IMP[a]);
  peek(cands[0]); return { id: cands[0] };
}
function docsOf(md, i, peek) {
  const d = splitDocs(md);
  const links = {};
  const all = [d.summary, ...d.sections.map((s) => s.md)].join("\n");
  for (const m of all.matchAll(/\[([^\]]+)\](?:\(([^)]+)\)|\[([^\]]*)\])?/g)) {
    const label = m[1]; const tgt = m[2] || (m[3] ? d.defs[m[3]] || m[3] : d.defs[label] || label);
    if (links[label] !== undefined || /^\s*$/.test(tgt)) continue;
    const r = /^https?:/.test(tgt) ? { url: tgt } : resolveLink(tgt, i, peek); if (r) links[label] = r;
  }
  return { summary: d.summary, sections: d.sections, links, examples: codeBlocks(md) };
}
const recvOf = (params) => { const p = (params[0] || "").replace(/\s/g, ""); return /^&(\'\w+)?mutself$/.test(p) ? "changes" : /^&(\'\w+)?self$/.test(p) ? "reads" : /^(mut)?self(:|$)/.test(p) ? "consumes" : ""; };
const firstSentence = (s) => { const m = /^(.*?[.!?])(\s|$)/s.exec((s || "").replace(/\n/g, " ")); return (m ? m[1] : s || "").trim(); };

export function sourceFacts(it, i, src, feats, crateOf, H) {
  const { peek, memberSince } = H;
  const n = W.N[i]; const crateName = it.pkg; const cfgFor = modCfgs(src);
  const KIND = { enum: "enum", struct: "struct", trait: "trait", function: "fn" }[n.k];
  const decl = src.items.find((x) => x.k === KIND && x.n === it.name && (x.mod || "") === it.mod);
  if (!decl) return { missing: true };
  const gate = (x) => gateOf([...cfgFor(x.mod), ...cfgOf(x.attrs || [])], crateName, feats);
  const isSelf = (self) => { const base = self.replace(/<.*$/s, "").trim(); return last(self) === it.name && !/^&/.test(self) && (!base.includes("::") || /^(crate|self|super)::/.test(base)); };
  const since = (owner, name) => memberSince(it, owner, name);
  const ownerPath = it.rpath || null;
  const array = /\bA:\s*Array\b/.test(decl.gen || "");
  const baseBounds = new Set(whereWords(decl.gen, decl.wh, { array }).map((b) => b.exact));

  // ------------------------------------------------ the declaration
  const attrsShown = decl.attrs.filter((a) => ["non_exhaustive", "must_use", "repr"].includes(a.name)).map((a) => `#[${a.text}]`);
  const derives = (decl.attrs.find((a) => a.name === "derive") || { text: "" }).text.replace(/^derive\(|\)$/g, "").split(",").map((s) => s.trim()).filter(Boolean);
  let code;
  if (decl.k === "fn") code = `${decl.vis ? decl.vis + " " : ""}${decl.quals.length ? decl.quals.join(" ") + " " : ""}fn ${decl.n}${decl.gen ? `<${colons(decl.gen)}>` : ""}(${colons(decl.params.join(", "))})${decl.ret ? " -> " + decl.ret : ""}${decl.wh ? `\nwhere\n    ${splitTop(decl.wh.replace(/,\s*$/, "")).map(colons).join(",\n    ")},` : ""}`;
  else if (decl.k === "struct") { const pub = decl.parts.filter((f) => f.vis === "pub"); code = `pub struct ${decl.n}${decl.gen ? `<${decl.gen}>` : ""}${decl.wh ? ` where ${decl.wh}` : ""} {${pub.length ? "\n" + pub.map((f) => `    pub ${f.n}: ${f.ty},`).join("\n") + (pub.length < decl.parts.length ? "\n    /* private fields */" : "") + "\n" : " /* private fields */ "}}`; }
  else if (decl.k === "enum") code = `pub enum ${decl.n}${decl.gen ? `<${decl.gen}>` : ""} {\n${decl.parts.map((v) => `    ${v.n}${v.shape === "tuple" ? `(${v.ty})` : v.shape === "record" ? ` { ${v.ty} }` : ""},`).join("\n")}\n}`;
  else if (decl.k === "trait") { const req = decl.members.filter((m) => m.k === "fn" && m.required); const prov = decl.members.filter((m) => m.k === "fn" && !m.required); code = `pub trait ${decl.n}${decl.gen ? `<${decl.gen}>` : ""}${decl.sup ? ": " + decl.sup : ""} {\n${req.length ? "    // Required method" + (req.length > 1 ? "s" : "") + "\n" + req.map((m) => `    ${colons(m.sig).replace(/\s+where\s+/, "\n       where ")};`).join("\n") : ""}${prov.length ? `\n\n    // Provided methods\n` + prov.map((m) => `    ${m.sig} { ... }`).join("\n") : ""}\n}`; }
  code = [...attrsShown, code].join("\n");
  const declBounds = whereWords(decl.gen, decl.wh, { array });
  const declOut = { code, file: decl.file, start: decl.start, line: decl.line, end: decl.end, bounds: declBounds, derives, gate: gate(decl), deprecated: deprecationOf(decl.attrs) };

  // ------------------------------------------------ docs
  const docs = docsOf(decl.docs, i, peek);

  // ------------------------------------------------ members, grouped by the condition of their impl block
  const memberOf = (m, im) => {
    const d = docsOf(m.docs, i, peek);
    const sec = (k) => (d.sections.find((s) => s.kind === k) || {}).md || null;
    return { n: m.n, sig: m.sig, gen: m.gen, wh: m.wh, ret: m.ret, params: m.params, recv: recvOf(m.params || []), vis: m.vis, unsafe: m.quals.includes("unsafe"), konst: m.quals.includes("const"),
      summary: d.summary, docs: d.sections, links: d.links, examples: d.examples.length,
      panics: sec("panics"), safety: sec("safety"), errors: sec("errors"),
      gate: gate({ mod: im.mod, attrs: [...im.attrs, ...m.attrs] }), deprecated: deprecationOf(m.attrs),
      since: ownerPath && since(ownerPath, m.n), line: m.start, end: m.end, file: m.file || im.file };
  };
  const inherent = src.items.filter((x) => x.k === "impl" && !x.trait && isSelf(x.self)).map((im) => {
    const cond = whereWords(im.gen, im.wh, { array }).filter((b) => !baseBounds.has(b.exact));
    const special = im.self !== `${it.name}${decl.gen ? "<" + splitTop(decl.gen).map((g) => g.replace(/:.*$/, "").trim()).join(", ") + ">" : ""}` ? im.self : null;
    return { head: im.head, self: im.self, special, cond, gate: gate(im), line: im.start, end: im.end, file: im.file,
      members: im.members.filter((m) => m.k === "fn" && m.vis === "pub").map((m) => memberOf(m, im)) };
  }).filter((b) => b.members.length);

  // ------------------------------------------------ trait impls: written here, derived, through another trait, from its parts
  const traitDecls = new Map(); for (const x of src.items) if (x.k === "trait") traitDecls.set(x.n, x);
  const written = src.items.filter((x) => x.k === "impl" && x.trait && isSelf(x.self)).map((im) => {
    const nm = last(im.trait); const td = traitDecls.get(nm);
    const cond = whereWords(im.gen, im.wh, { array }).filter((b) => !baseBounds.has(b.exact));
    const assoc = im.members.filter((m) => m.k === "type" || m.k === "const").map((m) => m.sig.replace(/;$/, ""));
    return { trait: im.trait, name: nm, label: /^(de|ser)::Error$/.test(im.trait.replace(/^serde::/, "")) ? im.trait.replace(/^serde::/, "") : nm, args: argsOf(im.trait), head: im.head, cond, gate: gate(im), neg: im.neg, unsafe: im.quals.includes("unsafe"),
      deprecated: td ? deprecationOf(td.attrs) : null, viaMacro: im.viaMacro || null, line: im.start, end: im.end, file: im.file,
      members: im.members.filter((m) => m.k === "fn").map((m) => ({ n: m.n, sig: m.sig, summary: firstSentence(m.docs), docs: m.docs ? docsOf(m.docs, i, peek).sections : [] })), assoc,
      panics: /^(Index|IndexMut)$/.test(nm) ? panicText(im, it) : null };
  });
  // conversions away: impl From<Name> for Other
  const becomes = src.items.filter((x) => x.k === "impl" && x.trait && /^(From|TryFrom)$/.test(last(x.trait)) && last(argsOf(x.trait)) === it.name && !isSelf(x.self)).map((im) => ({ into: im.self, head: im.head, gate: gate(im), line: im.start, end: im.end, file: im.file, docs: im.members.map((m) => docsOf(m.docs, i, peek)).find((d) => d.summary) || null }));

  // ------------------------------------------------ blanket impls: they arrive through another trait
  const has = (t) => derives.includes(t) || written.some((w) => w.name === t);
  const blankets = [
    { t: "From<T>", why: "every type converts from itself" }, { t: "Into<U>", why: "wherever U converts from it" },
    { t: "TryFrom<U>", why: "wherever it converts from U" }, { t: "TryInto<U>", why: "wherever U tries from it" },
    { t: "Borrow<T>", why: "every type" }, { t: "BorrowMut<T>", why: "every type" }, { t: "Any", why: "every 'static type" },
  ];
  if (decl.k !== "trait" && decl.k !== "fn") {
    if (has("Clone")) blankets.push({ t: "ToOwned", why: "through Clone" }, { t: "CloneToUninit", why: "through Clone" });
    if (has("Display")) blankets.push({ t: "ToString", why: "through Display" });
    const de = written.find((w) => w.name === "Deserialize"); if (de && /'de\b/.test(de.head)) blankets.push({ t: "DeserializeOwned", why: "through Deserialize for every 'de" });
  }

  // ------------------------------------------------ acts like: the Deref target and what it brings
  let actsLike = null;
  const deref = written.find((w) => w.name === "Deref");
  if (deref) {
    const target = (deref.assoc.find((a) => /^type Target/.test(a)) || "").replace(/^type Target\s*=\s*/, "");
    const own = new Set(inherent.flatMap((b) => b.members.map((m) => m.n)));
    const ms = /^\[.*\]$/.test(target) ? sliceMethods() : [];
    const groups = [
      { cond: "", on: "[T]", words: "" }, { cond: "[u8]", on: "[u8]", words: "when its items are bytes" }, { cond: "[[T; N]]", on: "[[T; N]]", words: "when its items are arrays" },
    ].map((g) => ({ ...g, methods: ms.filter((m) => m.on === g.on).map((m) => ({ ...m, shadowed: own.has(m.n) })) })).filter((g) => g.methods.length);
    actsLike = { target, through: "Deref", mutable: written.some((w) => w.name === "DerefMut"), groups, count: ms.length, shadowed: ms.filter((m) => own.has(m.n)).map((m) => m.n),
      since: ms.length ? [...new Set(ms.map((m) => m.since).filter(Boolean))].sort((a, b) => a.localeCompare(b, undefined, { numeric: true })) : [] };
  }

  // ------------------------------------------------ a trait: what you write, what you get, whether it can be `dyn`
  let contract = null;
  if (decl.k === "trait") {
    const fns = decl.members.filter((m) => m.k === "fn");
    const generic = fns.filter((m) => splitTop(m.gen || "").some((g) => !/^'/.test(g.trim())) && !/Self\s*:\s*Sized/.test(m.wh || ""));
    const notes = decl.attrs.filter((a) => a.name === "cfg_attr" && /on_unimplemented/.test(a.text)).flatMap((a) => [...a.text.matchAll(/note\s*=\s*"([^"]*)"/g)].map((m) => m[1]));
    contract = { required: fns.filter((m) => m.required).map((m) => ({ n: m.n, sig: m.sig, summary: firstSentence(m.docs), docs: docsOf(m.docs, i, peek), bounds: whereWords(m.gen, m.wh) })),
      provided: fns.filter((m) => !m.required).map((m) => ({ n: m.n, sig: m.sig, summary: firstSentence(m.docs) })),
      assoc: decl.members.filter((m) => m.k === "type" || m.k === "const").map((m) => m.sig),
      dyn: generic.length ? { ok: false, why: `${generic.map((m) => m.n).join(", ")} ${generic.length === 1 ? "is" : "are"} generic over ${splitTop(generic[0].gen).filter((g) => !/^'/.test(g))[0].replace(/:.*/, "")}` } : { ok: true },
      notes, supertraits: decl.sup || "" };
    // done by, in its own crate: the std types it covers, written or made by a macro
    const here = src.items.filter((x) => x.k === "impl" && x.trait && last(x.trait) === it.name);
    const inv = src.invocations.filter((v) => !v.expanded && /_impl|impls|nonzero|atomic/.test(v.name));
    contract.foreign = families(here.map((x) => ({ self: x.self, gate: gate(x), line: x.start, file: x.file, viaMacro: x.viaMacro })), inv, (x) => gate(x));
  }

  // ------------------------------------------------ an error type: its kinds, told apart how, with their causes
  let kinds = null;
  if (it.slug === "error") kinds = errorKinds(src);

  // ------------------------------------------------ the gates on this page, and whether your project opens them
  const gates = new Map();
  const note = (g, where) => { if (!g || !g.need) return; for (const x of g.need) { if (x.not) continue; const e = gates.get(x.f) || gates.set(x.f, { feature: x.f, crate: crateName, open: (feats.on[crateName] || []).includes(x.f), why: (feats.why[crateName] || {})[x.f] || null, where: [] }).get(x.f); e.where.push(where); } };
  for (const b of inherent) { note(b.gate, b.head); for (const m of b.members) note(m.gate, m.n); }
  for (const w of written) note(w.gate, w.name);
  for (const b of becomes) note(b.gate, `into ${b.into}`);
  const all = feats.crates || null;
  const P = feats.project[crateName] || feats.project[{ serde_core: "serde" }[crateName]] || null;
  for (const g of gates.values()) g.how = P ? howLine(crateName in feats.project ? crateName : "serde", P, crateName === "serde_core" ? g.feature : g.feature) : null;

  return { decl: declOut, docs, inherent, written, becomes, blankets, autos: AUTO[it.slug] ? { ...AUTO[it.slug], words: AUTO_WORD } : null, actsLike, contract, kinds, gates: [...gates.values()] };
}
function howLine(crate, P, f) { const feats = [...new Set([...P.feats, f])].map((x) => `"${x}"`).join(", "); return `${crate} = { version = "${P.ver}"${P.noDefault ? ", default-features = false" : ""}, features = [${feats}] }`; }
function panicText(im, it) { // what an Index impl says when it gives up
  const file = path.join(REG, im.file.replace(/^[^/]+\//, it.crate + "/"));
  const body = fs.readFileSync(file, "utf8").split("\n").slice(im.start - 1, im.end).join("\n");
  const m = /\.expect\("([^"]*)"\)/.exec(body) || /panic!\("([^"]*)"/.exec(body); return m ? m[1] : null;
}
// the std types a trait covers, grouped the way a reader thinks about them
const FAM = [
  ["numbers", /^(bool|char|[iu](8|16|32|64|128|size)|f(32|64)|NonZero\w*|Wrapping<.*|Saturating<.*|Reverse(<.*)?)$/],
  ["text", /^(str|String|CStr|CString|fmt::Arguments.*|OsStr|OsString|Path|PathBuf)$/],
  ["collections", /^(\[T\]|\[T; .*|Vec(<.*)?|VecDeque(<.*)?|BinaryHeap(<.*)?|BTreeSet(<.*)?|HashSet(<.*)?|LinkedList(<.*)?|BTreeMap(<.*)?|HashMap(<.*)?)$/],
  ["pointers and cells", /^(&.*|Box(<.*)?|Rc(<.*)?|Arc(<.*)?|Cow(<.*)?|RcWeak<.*|ArcWeak<.*|Cell<.*|RefCell(<.*)?|Mutex(<.*)?|RwLock(<.*)?|Pin<.*)$/],
  ["options and results", /^(Option<.*|Result<.*|PhantomData<.*|\(\)|!|Bound<.*|Range.*)$/],
  ["time and network", /^(Duration|SystemTime|net::.*)$/],
  ["atomics", /^Atomic/],
];
function families(impls, invocations, gate) {
  const out = new Map(); const seen = new Set();
  const famOf = (name) => { const base = name.replace(/<.*$/s, ""); return (FAM.find(([, re]) => re.test(name) || re.test(base + "<>") || re.test(base)) || ["other"])[0]; };
  const add = (name, g, fam) => { const key = name.replace(/<.*$/s, ""); if (seen.has(key)) return; seen.add(key); fam ||= famOf(name); const f = out.get(fam) || out.set(fam, { fam, names: [], gated: 0 }).get(fam); f.names.push(name); if (g) f.gated++; };
  for (const x of impls) add(x.self.trim(), x.gate && x.gate.need);
  let unexpanded = 0;
  for (const v of invocations) {
    if (/tuple/.test(v.name)) { add("(T0, …, T15)", false, "tuples"); continue; }
    if (/array/.test(v.name)) { add("[T; 1 … 32]", false, "collections"); continue; }
    if (/atomic|nonzero/.test(v.name)) { for (const m of v.args.matchAll(/\b(Atomic\w+|NonZero\w+)\b/g)) add(m[1], false); continue; }
    const a = v.args.replace(/#\[[^\]]*\]/g, " ");
    const heads = [...a.matchAll(/(?:^|[,;{}]|=>)\s*(?:\d+\s*=>\s*\()?\s*([A-Z][A-Za-z0-9]*(?:<[^>]*>)?|[iu](?:8|16|32|64|128|size)|f32|f64|bool|char)\b/g)].map((m) => m[1]);
    const names = [...new Set(heads)].filter((h) => !/^(T|K|V|E|S|H|Idx|Serialize|Serializer)$/.test(h.replace(/<.*/, "")));
    if (!names.length) { unexpanded++; continue; }
    for (const h of names) add(h, false);
  }
  const order = ["numbers", "text", "collections", "tuples", "pointers and cells", "options and results", "time and network", "atomics", "other"];
  return { groups: [...out.values()].sort((a, b) => order.indexOf(a.fam) - order.indexOf(b.fam)), unexpanded, count: [...out.values()].reduce((s, f) => s + f.names.length, 0) };
}
function errorKinds(src) {
  const cat = src.items.find((x) => x.k === "enum" && x.n === "Category");
  const code = src.items.find((x) => x.k === "enum" && x.n === "ErrorCode");
  const file = path.join(REG, "serde_json-1.0.151/src/error.rs"); const text = fs.readFileSync(file, "utf8");
  const cls = /pub fn classify\(&self\) -> Category \{([\s\S]*?)\n    \}/.exec(text);
  const map = new Map();
  if (cls) for (const arm of cls[1].split("=>").slice(0, -1).map((_, k, a) => k)) { /* arms parsed below */ }
  if (cls) { const re = /((?:\|?\s*ErrorCode::\w+(?:\(_\))?\s*)+)=>\s*Category::(\w+)/g; let m; while ((m = re.exec(cls[1]))) for (const c of m[1].matchAll(/ErrorCode::(\w+)/g)) map.set(c[1], m[2]); }
  const msg = new Map(); const disp = /impl Display for ErrorCode \{([\s\S]*?)\n\}/.exec(text);
  if (disp) for (const m of disp[1].matchAll(/ErrorCode::(\w+)(?:\([^)]*\))?\s*=>\s*\{?\s*f\.write_str\("((?:[^"\\]|\\.)*)"\)/g)) msg.set(m[1], m[2].replace(/\\(.)/g, "$1"));
  return { via: "classify", line: text.slice(0, text.indexOf("pub fn classify")).split("\n").length,
    kinds: (cat ? cat.parts : []).map((v) => ({ n: v.n, doc: v.docs.split("\n\n")[0].replace(/\n/g, " "), more: v.docs.split("\n\n").slice(1).join(" ").replace(/\n/g, " "),
      causes: (code ? code.parts : []).filter((c) => map.get(c.n) === v.n).map((c) => ({ n: c.n, msg: msg.get(c.n) || (c.n === "Message" ? "your type's own message (custom)" : c.n === "Io" ? "the io::Error's own message" : "") })) })) };
}
