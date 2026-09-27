// world.mjs — the graph prototype's world (graph/world.js, read-only) as a node module:
// the same derived structure app.js builds (kids, topOf, CSR in/out edges, yours, importance),
// and relationsOf() entry for entry, so page2's relations read what the prism reads.
import fs from "node:fs";
import path from "node:path";
const HERE = path.dirname(new URL(import.meta.url).pathname);
export const V4 = path.resolve(HERE, "..");
const src = fs.readFileSync(path.join(V4, "graph/world.js"), "utf8");
const window = {}; (0, eval)("(function(window){" + src + "\n})")(window);
export const WD = window.WORLD;
export const N = WD.nodes, NN = N.length, PK = WD.packages, MD = WD.modules, IMP = WD.imp, REL = WD.rel;
const bit = (name) => 1 << REL.indexOf(name);
export const B = { has: bit("has"), takes: bit("takes"), gives: bit("gives"), is: bit("is"), derives: bit("derives"), calls: bit("calls"), uses: bit("uses"), type: bit("type"), impl: bit("impl") };
export const kids = Array.from({ length: NN }, () => null);
for (let i = 0; i < NN; i++) { const u = N[i].u; if (u >= 0) (kids[u] ||= []).push(i); }
export const topOf = new Int32Array(NN); for (let i = 0; i < NN; i++) { let j = i; while (N[j].u >= 0) j = N[j].u; topOf[i] = j; }
function csr(pairs, from, to) {
  const off = new Int32Array(NN + 1); for (const e of pairs) off[e[from] + 1]++;
  for (let i = 0; i < NN; i++) off[i + 1] += off[i];
  const at = off.slice(0, NN); const dst = new Int32Array(pairs.length), bits = new Int32Array(pairs.length);
  for (const e of pairs) { const k = at[e[from]]++; dst[k] = e[to]; bits[k] = e[2]; }
  return { off, dst, bits };
}
export const OUT = csr(WD.edges, 0, 1), IN = csr(WD.edges, 1, 0);
export const yours = (i) => PK[N[i].p].yours;
export const inEdges = (t, mask) => { const out = []; for (let e = IN.off[t]; e < IN.off[t + 1]; e++) if (IN.bits[e] & mask) out.push(IN.dst[e]); return out; };
export const outEdges = (t, mask) => { const out = []; for (let e = OUT.off[t]; e < OUT.off[t + 1]; e++) if (OUT.bits[e] & mask) out.push(OUT.dst[e]); return out; };
export const inWithBits = (t) => { const out = []; for (let e = IN.off[t]; e < IN.off[t + 1]; e++) out.push([IN.dst[e], IN.bits[e]]); return out; };
export const nameOf = (j) => { const n = N[j]; if (n.u < 0) return n.n; const sep = n.k === "field" ? "." : "::"; return N[n.u].n + sep + n.n; };
export const pkgName = (p) => PK[p].name.replace(/^backend-/, "");
export const qual = (i) => { const n = N[i]; const mp = MD[n.m].path; return `${pkgName(n.p)}${mp ? "::" + mp : ""}${n.u >= 0 ? "::" + N[n.u].n : ""}`; };
export const KFAM = { module: "ns", package: "ns", struct: "ty", enum: "ty", union: "ty", type: "ty", trait: "co", function: "ca", method: "ca", macro: "ca", constant: "va", field: "va", variant: "va" };
const IMPLIED = { Clone: "Copy", PartialEq: "Eq", PartialOrd: "Ord", Eq: "Ord" };
export function minimalDerives(list) { const has = new Set(list); return list.filter((d) => !(IMPLIED[d] && has.has(IMPLIED[d]))); }
export function find(pkg, path, name, kind) {
  for (let i = 0; i < NN; i++) { const n = N[i]; if (n.u < 0 && n.n === name && PK[n.p].name === pkg && MD[n.m].path === path && (!kind || n.k === kind)) return i; }
  return -1;
}
// app.js relationsOf, entry for entry (the prism's groups)
export function relationsOf(i) {
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
