// Regenerates relations.golden: app.js relationsOf and page.js inUse's
// caller ranking (verbatim logic) over the pinned fixture, with importance
// computed exactly as layout.mjs does. Run after re-pinning the fixture:
//   node apps/facet/src/semantics/tests/golden.mjs apps/facet/src/semantics/tests/fixtures/world.json apps/facet/src/semantics/tests/relations.golden
import fs from "fs";
const WD = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const N = WD.nodes, NN = N.length, PK = WD.packages;
// layout.mjs importance, verbatim: PageRank over rolled-up item edges.
const topOf = new Int32Array(NN); for (let i = 0; i < NN; i++) { let j = i; while (N[j].u >= 0) j = N[j].u; topOf[i] = j; }
const itemEdges = new Map();
for (const [a, b, bits] of WD.edges) { const A = topOf[a], B = topOf[b]; if (A === B) continue; const k = A * 4194304 + B; const e = itemEdges.get(k); if (e) { e[0] |= bits; e[1]++; } else itemEdges.set(k, [bits, 1]); }
const IE = [...itemEdges.entries()].map(([k, [bits, w]]) => [Math.floor(k / 4194304), k % 4194304, bits, w]);
const items = []; for (let i = 0; i < NN; i++) if (N[i].u < 0) items.push(i);
const outDeg = new Float64Array(NN); for (const [a, , , w] of IE) outDeg[a] += w;
let pr = new Float64Array(NN); for (const i of items) pr[i] = 1 / items.length;
for (let it = 0; it < 40; it++) { const nx = new Float64Array(NN); let dangling = 0; for (const i of items) if (!outDeg[i]) dangling += pr[i]; for (const [a, b, , w] of IE) nx[b] += 0.85 * pr[a] * w / outDeg[a]; for (const i of items) nx[i] += (0.15 + 0.85 * dangling) / items.length; pr = nx; }
const prMax = Math.max(...items.map((i) => pr[i]));
const imp0 = new Float32Array(NN); for (const i of items) imp0[i] = Math.pow(pr[i] / prMax, 0.35); for (let i = 0; i < NN; i++) if (N[i].u >= 0) imp0[i] = imp0[topOf[i]] * 0.5;
const IMP = Float32Array.from(Array.from(imp0, (v) => Math.round(v * 1000) / 1000));
const REL = WD.rel; const bit = (name) => 1 << REL.indexOf(name);
const B = { has: bit("has"), takes: bit("takes"), gives: bit("gives"), is: bit("is"), derives: bit("derives"), calls: bit("calls"), uses: bit("uses"), type: bit("type"), impl: bit("impl") };
const kids = Array.from({ length: NN }, () => null);
for (let i = 0; i < NN; i++) { const u = N[i].u; if (u >= 0) (kids[u] ||= []).push(i); }
function csr(pairs, from, to) {
  const off = new Int32Array(NN + 1); for (const e of pairs) off[e[from] + 1]++;
  for (let i = 0; i < NN; i++) off[i + 1] += off[i];
  const at = off.slice(0, NN); const dst = new Int32Array(pairs.length), bits = new Int32Array(pairs.length);
  for (const e of pairs) { const k = at[e[from]]++; dst[k] = e[to]; bits[k] = e[2]; }
  return { off, dst, bits };
}
const OUT = csr(WD.edges, 0, 1), IN = csr(WD.edges, 1, 0);
const yours = (i) => PK[N[i].p].yours;
const IMPLIED = { Clone: "Copy", PartialEq: "Eq", PartialOrd: "Ord", Eq: "Ord" };
function minimalDerives(list) { const has = new Set(list); return list.filter((d) => !(IMPLIED[d] && has.has(IMPLIED[d]))); }
const inEdges = (t, mask) => { const out = []; for (let e = IN.off[t]; e < IN.off[t + 1]; e++) if (IN.bits[e] & mask) out.push(IN.dst[e]); return out; };
const outEdges = (t, mask) => { const out = []; for (let e = OUT.off[t]; e < OUT.off[t + 1]; e++) if (OUT.bits[e] & mask) out.push(OUT.dst[e]); return out; };
function relationsOf(i) {
  const n = N[i]; const groups = []; const seen = new Set([i, ...(kids[i] || [])]);
  const add = (word, side, ids, extra = []) => {
    const list = []; for (const j of ids) { if (seen.has(j)) continue; seen.add(j); list.push({ j }); }
    list.sort((a, b) => (yours(b.j) - yours(a.j)) || IMP[b.j] - IMP[a.j]);
    const all = [...extra, ...list]; if (all.length) groups.push({ word, side, entries: all });
  };
  if (["struct", "enum", "trait", "type", "union"].includes(n.k)) {
    const written = (n.impls || []).map((x) => x.trait).filter((t) => t >= 0);
    const extra = written.map((t) => ({ j: t, note: "written" }));
    for (const t of written) seen.add(t);
    const derives = minimalDerives(n.derives || []);
    if (derives.length) extra.push({ text: derives.join(" · "), note: "derived", j: outEdges(i, B.derives)[0] ?? -1, cap: derives });
    for (const t of (n.implsExt || [])) extra.push({ text: t.split("::").pop(), note: "written", j: -1 });
    if (written.some((t) => N[t].n === "Display")) extra.push({ text: "ToString", note: "via Display", j: -1 });
    add("is", 0, outEdges(i, B.is), extra);
    add("made of", -1, outEdges(i, B.has));
    add("made by", -1, inEdges(i, B.gives));
    if (n.k === "trait") add("implemented by", -1, inEdges(i, B.impl | B.derives));
    add("taken by", 1, inEdges(i, B.takes));
    add("held by", 1, inEdges(i, B.type));
    const callers = []; for (const m of kids[i] || []) callers.push(...inEdges(m, B.calls));
    add("calls it", 1, callers);
    add("used by", 1, inEdges(i, B.uses | B.calls).filter((j) => !(N[j].u >= 0 && seen.has(N[j].u))));
  } else {
    const self = [i, ...(kids[i] || [])];
    add("takes", -1, self.flatMap((t) => outEdges(t, B.takes)));
    add("called from", -1, inEdges(i, B.calls));
    add("gives", 1, self.flatMap((t) => outEdges(t, B.gives)));
    add("calls", 1, self.flatMap((t) => outEdges(t, B.calls)));
    add("used by", 1, inEdges(i, B.uses | B.takes | B.gives | B.type));
  }
  return groups;
}
// the four pages, plus a deterministic spread: every 61st node
const find = (q) => { const [place, name] = [q.slice(0, q.lastIndexOf("::")), q.slice(q.lastIndexOf("::") + 2)];
  const qual = (i) => { const n = N[i]; const mp = WD.modules[n.m].path; return `${PK[n.p].name.replace(/^backend-/, "")}${mp ? "::" + mp : ""}${n.u >= 0 ? "::" + N[n.u].n : ""}`; };
  return N.findIndex((n, i) => n.n === name && qual(i) === place); };
const ids = ["present::glyph::RelationLabel", "present::page::RelationGroup", "serde_json::de::from_str", "serde_core::de::Visitor"].map(find);
for (let i = 0; i < NN; i += 61) ids.push(i);
// page.js inUse: the ranked callables whose bodies may show a use.
function callers(i) {
  const n = N[i];
  const refs = new Set(inEdges(i, B.calls | B.takes | B.gives | B.uses | B.type | B.has));
  for (const m of (kids[i] || [])) for (const j of inEdges(m, B.calls)) refs.add(j);
  const own = new Set([i, ...(kids[i] || [])]);
  const list = [...refs].filter((j) => !own.has(j) && N[topOf[j]].f && !N[j].orphan)
    .sort((a, b) => (yours(b) - yours(a)) || ((N[b].p !== n.p) - (N[a].p !== n.p)) || IMP[b] - IMP[a]);
  return list.filter((j) => !["field", "variant", "struct", "enum", "union", "type", "trait"].includes(N[j].k)).slice(0, 12);
}
const lines = [];
for (const i of ids) {
  for (const g of relationsOf(i)) {
    const e = g.entries.map((x) => (x.text ? `${x.text}~${x.note}~${x.j}` : x.note ? `${x.j}~${x.note}` : `${x.j}`)).join(",");
    lines.push(`${i}|${N[i].n}|${g.word}|${g.side}|${e}`);
  }
  lines.push(`${i}|${N[i].n}|end`);
}
for (const i of ids.slice(0, 4)) lines.push(`${i}|${N[i].n}|callers|${callers(i).join(",")}`);
fs.writeFileSync(process.argv[3], lines.join("\n") + "\n");
console.log(ids.length, "symbols", lines.length, "lines");
