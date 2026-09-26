// Pins the semantic tests' world: a trimmed copy of the prototype's world.json
// at a fixed git revision, plus sparse copies of the source files In use reads
// (only the callers' lines survive; every other line is blank, so line numbers
// hold). Run from the repository root:
//   node apps/facet/src/semantics/tests/fixtures/trim.mjs <git-rev>
import fs from "fs";
import path from "path";
import { execFileSync } from "child_process";

const REV = process.argv[2] || "HEAD";
const ROOT = process.cwd();
const OUT = path.join(ROOT, "apps/facet/src/semantics/tests/fixtures");
const REGISTRY = path.join(process.env.HOME, ".cargo/registry/src/index.crates.io-1949cf8c6b5b557f");
const git = (p) => execFileSync("git", ["show", `${REV}:${p}`], { maxBuffer: 1 << 30 }).toString();

const W = JSON.parse(git("Nudox-Design-System/v4/graph/world.json"));
const N = W.nodes, PK = W.packages, MD = W.modules;
const REL = W.rel; const bit = (n) => 1 << REL.indexOf(n);
const CALLS = bit("calls"), DERIVES = bit("derives");
const qual = (i) => { const n = N[i]; const mp = MD[n.m].path; return `${PK[n.p].name.replace(/^backend-/, "")}${mp ? "::" + mp : ""}${n.u >= 0 ? "::" + N[n.u].n : ""}`; };
const find = (q) => { const k = q.lastIndexOf("::"); const place = q.slice(0, k), name = q.slice(k + 2); return N.findIndex((n, i) => n.n === name && qual(i) === place); };
const TARGETS = ["present::glyph::RelationLabel", "present::page::RelationGroup", "serde_json::de::from_str", "serde_core::de::Visitor"].map(find);
if (TARGETS.some((t) => t < 0)) throw new Error("a target is missing at " + REV);
const CORE = new Set(["backend-present", "serde_core", "serde_json"]);

const kids = new Map(); N.forEach((n, i) => { if (n.u >= 0) (kids.get(n.u) || kids.set(n.u, []).get(n.u)).push(i); });
const keep = new Set();
N.forEach((n, i) => { if (CORE.has(PK[n.p].name)) keep.add(i); });
const focus = new Set(); for (const t of TARGETS) { focus.add(t); for (const k of kids.get(t) || []) focus.add(k); }
for (const [a, b, bits] of W.edges) { if (focus.has(a)) keep.add(b); if (focus.has(b)) keep.add(a); }
// Close under parents, written impls' traits and derive targets.
let grew = true;
while (grew) {
  grew = false;
  for (const i of [...keep]) {
    const n = N[i]; const add = (j) => { if (j >= 0 && !keep.has(j)) { keep.add(j); grew = true; } };
    add(n.u); for (const im of n.impls || []) add(im.trait);
  }
}
for (const [a, b, bits] of W.edges) if (keep.has(a) && (bits & DERIVES)) keep.add(b);

const ids = [...keep].sort((a, b) => a - b);
const newId = new Map(ids.map((old, k) => [old, k]));
const pkUsed = [...new Set(ids.map((i) => N[i].p))].sort((a, b) => a - b); const newPk = new Map(pkUsed.map((o, k) => [o, k]));
const mdUsed = [...new Set(ids.map((i) => N[i].m))].sort((a, b) => a - b); const newMd = new Map(mdUsed.map((o, k) => [o, k]));
const nodes = ids.map((i) => {
  const n = { ...N[i], p: newPk.get(N[i].p), m: newMd.get(N[i].m), u: N[i].u >= 0 ? newId.get(N[i].u) : -1 };
  if (n.impls) n.impls = n.impls.map((im) => ({ ...im, trait: im.trait >= 0 && newId.has(im.trait) ? newId.get(im.trait) : -1 }));
  return n;
});
const edges = W.edges.filter(([a, b]) => keep.has(a) && keep.has(b)).map(([a, b, bits]) => [newId.get(a), newId.get(b), bits]);
const packages = pkUsed.map((o) => ({ ...PK[o], deps: (PK[o].deps || []).filter((d) => newPk.has(d)).map((d) => newPk.get(d)) }));
const modules = mdUsed.map((o) => ({ ...MD[o], pkg: newPk.get(MD[o].pkg) }));
const world = { packages, modules, nodes, rel: REL, edges };
fs.writeFileSync(path.join(OUT, "world.json"), JSON.stringify(world));

// Sparse sources: every callable that refers to a target (or a target's
// member) keeps its span.
const top = (i) => { let j = i; while (N[j].u >= 0) j = N[j].u; return j; };
const spans = new Map();
// The targets' own bodies (the usage tests mine them directly).
for (const t of TARGETS) {
  const n = N[t]; if (!n.f || !["function", "method"].includes(n.k)) continue;
  const key = (PK[n.p].external ? "registry/" : "repo/") + n.f;
  (spans.get(key) || spans.set(key, []).get(key)).push([n.l, n.e || n.l]);
}
for (const [a, b] of W.edges) {
  if (!keep.has(a) || !focus.has(b)) continue;
  const n = N[a]; if (!["function", "method"].includes(n.k)) continue;
  const t = N[top(a)]; if (!t.f) continue;
  const from = n.l || t.l, to = n.e || t.e || t.l;
  const key = (PK[t.p].external ? "registry/" : "repo/") + t.f;
  (spans.get(key) || spans.set(key, []).get(key)).push([from, to]);
}
let bytes = 0, files = 0;
for (const [key, list] of spans) {
  let text;
  try { text = key.startsWith("registry/") ? fs.readFileSync(path.join(REGISTRY, key.slice(9)), "utf8") : git(key.slice(5)); } catch { continue; }
  const lines = text.split("\n"); const on = new Uint8Array(lines.length + 1);
  for (const [a, b] of list) for (let k = Math.max(1, a); k <= Math.min(lines.length, b); k++) on[k] = 1;
  const sparse = lines.map((l, k) => (on[k + 1] ? l : "")).join("\n");
  const dest = path.join(OUT, "src", key); fs.mkdirSync(path.dirname(dest), { recursive: true }); fs.writeFileSync(dest, sparse);
  bytes += sparse.length; files++;
}
console.log(`${REV}: ${nodes.length} nodes, ${edges.length} edges, ${packages.length} packages -> world.json ${fs.statSync(path.join(OUT, "world.json")).size} bytes; ${files} sparse sources, ${bytes} bytes`);
