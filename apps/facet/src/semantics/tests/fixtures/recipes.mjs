// Getting one / Calling it: the pinned world and the golden, from the
// prototype's own recipes.js run headless (page.js supplies its type helpers).
//
//   node apps/facet/src/semantics/tests/fixtures/recipes.mjs trim   <world.json>
//   node apps/facet/src/semantics/tests/fixtures/recipes.mjs golden
//
// `trim` keeps what the four recipe targets can need: every producer whose
// output feeds a target (the backward closure over the AND-OR graph, from the
// full world's producer table), their owners and fields, and every type-like
// item (names resolve against all of them). Importance is the full world's
// (layout.mjs PageRank), stored as `imp` so a trimmed world ranks exactly as
// the full one. `golden` runs recipes.js over that pinned world.
import fs from "fs";
import path from "path";
import vm from "vm";

const HERE = path.dirname(new URL(import.meta.url).pathname);
const PROTO = path.join(HERE, "../../../../../../Nudox-Design-System/v4/graph");
export const TARGETS = [
  "serde_json::de::Deserializer",
  "serde_json::value::Value",
  "present::page::Page",
  "desktop::model::local_package::manifest::Manifest",
];

/// Callables whose Calling it the golden records.
export const CALLS = ["present::page::Page::new", "serde_json::de::Deserializer::from_slice"];

function importance(W) {
  const N = W.nodes, NN = N.length;
  const topOf = new Int32Array(NN); for (let i = 0; i < NN; i++) { let j = i; while (N[j].u >= 0) j = N[j].u; topOf[i] = j; }
  const itemEdges = new Map();
  for (const [a, b, bits] of W.edges) { const A = topOf[a], B = topOf[b]; if (A === B) continue; const k = A * 4194304 + B; const e = itemEdges.get(k); if (e) { e[0] |= bits; e[1]++; } else itemEdges.set(k, [bits, 1]); }
  const IE = [...itemEdges.entries()].map(([k, [bits, w]]) => [Math.floor(k / 4194304), k % 4194304, bits, w]);
  const items = []; for (let i = 0; i < NN; i++) if (N[i].u < 0) items.push(i);
  const outDeg = new Float64Array(NN); for (const [a, , , w] of IE) outDeg[a] += w;
  let pr = new Float64Array(NN); for (const i of items) pr[i] = 1 / items.length;
  for (let it = 0; it < 40; it++) { const nx = new Float64Array(NN); let dangling = 0; for (const i of items) if (!outDeg[i]) dangling += pr[i]; for (const [a, b, , w] of IE) nx[b] += 0.85 * pr[a] * w / outDeg[a]; for (const i of items) nx[i] += (0.15 + 0.85 * dangling) / items.length; pr = nx; }
  const prMax = Math.max(...items.map((i) => pr[i]));
  const imp = new Float32Array(NN); for (const i of items) imp[i] = Math.pow(pr[i] / prMax, 0.35); for (let i = 0; i < NN; i++) if (N[i].u >= 0) imp[i] = imp[topOf[i]] * 0.5;
  return Array.from(imp, (v) => Math.round(v * 1000) / 1000);
}

// The prototype's page.js + recipes.js in a sandbox with no DOM.
function prototype(W, imp) {
  const N = W.nodes, NN = N.length, PK = W.packages;
  const topOf = new Int32Array(NN); for (let i = 0; i < NN; i++) { let j = i; while (N[j].u >= 0) j = N[j].u; topOf[i] = j; }
  const kids = Array.from({ length: NN }, () => null); for (let i = 0; i < NN; i++) { const u = N[i].u; if (u >= 0) (kids[u] ||= []).push(i); }
  const G = { N, PK, MD: W.modules, IMP: Float32Array.from(imp), topOf, kids, yours: (i) => PK[N[i].p].yours, nameOf: (j) => N[j].n };
  const stub = () => stubObj; const stubObj = new Proxy(function () {}, { get: (_, k) => (k === Symbol.iterator ? [][Symbol.iterator] : k === "length" ? 0 : stub), apply: () => null });
  const window = { addEventListener() {}, KINDS: {} };
  const document = { getElementById: () => null, addEventListener() {}, querySelector: () => null, querySelectorAll: () => [], createElement: () => stubObj, body: stubObj };
  const ctx = vm.createContext({ window, document, location: { search: "" }, URLSearchParams, history: { replaceState() {} }, setTimeout: () => 0, clearTimeout() {}, requestAnimationFrame: () => 0, console });
  for (const f of ["recipes.js", "page.js"]) vm.runInContext(fs.readFileSync(path.join(PROTO, f), "utf8"), ctx, { filename: f });
  window.GRAPH_PAGE.init(G);
  return { G, R: window.GRAPH_RECIPES };
}

const qualOf = (W, i) => { const N = W.nodes, n = N[i]; const mp = W.modules[n.m].path; return `${W.packages[n.p].name.replace(/^backend-/, "")}${mp ? "::" + mp : ""}${n.u >= 0 ? "::" + N[n.u].n : ""}`; };
const find = (W, q) => { const k = q.lastIndexOf("::"); const place = q.slice(0, k), name = q.slice(k + 2); return W.nodes.findIndex((n, i) => n.n === name && qualOf(W, i) === place); };

function trim(file) {
  const W = JSON.parse(fs.readFileSync(file, "utf8"));
  const imp = importance(W);
  const { R } = prototype(W, imp);
  const table = R.table; const N = W.nodes;
  const targets = TARGETS.map((q) => find(W, q));
  if (targets.some((t) => t < 0)) throw new Error("a target is missing: " + TARGETS.filter((_, k) => targets[k] < 0));
  // Every producer of a target (the candidates), then, from each target's
  // package perspective, the cheapest producer of every key a kept
  // producer needs, recursively: exactly what routes() and tree() read.
  const producers = new Set();
  for (const t of targets) {
    const run = R.run(N[t].p); const key = "#" + t; const seen = new Set();
    const walk = (k) => { if (seen.has(k)) return; seen.add(k); const b = run.best.get(k); if (b === undefined || b < 0) return; producers.add(b); for (const x of table[b].ins) walk(x); };
    table.forEach((e, q) => { if (e.out === key || e.i === t) { producers.add(q); for (const x of e.ins) walk(x); } });
  }
  const keep = new Set();
  const top = (i) => { let j = i; while (N[j].u >= 0) j = N[j].u; return j; };
  for (const q of producers) { const i = table[q].i; keep.add(i); keep.add(top(i)); }
  // A struct literal reads its fields.
  for (const i of [...keep]) if (N[i].k === "struct") N.forEach((n, j) => { if (n.u === i && n.k === "field") keep.add(j); });
  // Names resolve against every type-like item.
  N.forEach((n, i) => { if (n.u < 0 && ["struct", "enum", "trait", "type", "union"].includes(n.k)) keep.add(i); });
  for (const t of targets) keep.add(t);
  const ids = [...keep].sort((a, b) => a - b); const newId = new Map(ids.map((o, k) => [o, k]));
  const pkUsed = [...new Set(ids.map((i) => N[i].p))].sort((a, b) => a - b); const newPk = new Map(pkUsed.map((o, k) => [o, k]));
  const mdUsed = [...new Set(ids.map((i) => N[i].m))].sort((a, b) => a - b); const newMd = new Map(mdUsed.map((o, k) => [o, k]));
  // A kept node that is not itself a producer (or a producer's field) is
  // there for names and owners only: marked orphan so no table builds a
  // producer from it, and slimmed to what resolution reads.
  const makers = new Set([...producers].map((q) => table[q].i));
  const slim = (i) => {
    const n = N[i];
    if (makers.has(i) || (n.k === "field" && makers.has(n.u))) { const o = { ...n }; delete o.d; delete o.s; delete o.l; delete o.e; return o; }
    return { k: n.k, n: n.n, p: n.p, m: n.m, u: n.u, v: n.v, orphan: 1 };
  };
  const nodes = ids.map((i) => {
    const n = slim(i); n.p = newPk.get(N[i].p); n.m = newMd.get(N[i].m); n.u = N[i].u >= 0 ? newId.get(N[i].u) : -1;
    // Only a producer or its owner needs its derives/impls/fields.
    if (n.impls) n.impls = n.impls.map((im) => ({ ...im, trait: im.trait >= 0 && newId.has(im.trait) ? newId.get(im.trait) : -1 }));
    return n;
  });
  const out = {
    packages: pkUsed.map((o) => ({ ...W.packages[o], deps: [] })),
    modules: mdUsed.map((o) => ({ ...W.modules[o], pkg: newPk.get(W.modules[o].pkg) })),
    nodes, rel: W.rel, edges: [], imp: ids.map((i) => imp[i]),
  };
  const dest = path.join(HERE, "recipes.json"); fs.writeFileSync(dest, JSON.stringify(out));
  console.log(`${nodes.length} nodes (${producers.size} producers) -> recipes.json ${fs.statSync(dest).size} bytes`);
  // The same four routes over the full world and the pinned one must agree.
  const full = targets.map((t) => summary(W, prototype(W, imp), t));
  const pinned = out.nodes.length && TARGETS.map((q) => summary(out, prototype(out, out.imp), find(out, q)));
  for (let k = 0; k < targets.length; k++) console.log(JSON.stringify(full[k]) === JSON.stringify(pinned[k]) ? "same" : "DIFFERS", TARGETS[k], "\n full  ", JSON.stringify(full[k]), "\n pinned", JSON.stringify(pinned[k]));
}

// A section's HTML as the words a reader sees: tags become spaces, runs of
// space collapse, the ⌥ code block is dropped (the code is compared apart).
const words = (html) => html.replace(/<code[^>]*>[\s\S]*?<\/code>/g, " ").replace(/<[^>]+>/g, " ").replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/\s+/g, " ").trim();

function summary(W, P, i) {
  const { R } = P; const N = W.nodes;
  const kind = N[i].k;
  const section = words(R.gettingOne(i) || R.callingIt(i) || "");
  if (kind === "function" || kind === "method") { const r = R.callRoute(i); return r ? { call: R.code(r.node), section } : null; }
  const { routes, makers, picks } = R.routes(i);
  return { makers, picks, section, routes: routes.map((r) => ({ cost: Math.round(r.cost * 1000) / 1000, code: R.code(r.node), alts: r.alts.map((k) => k.replace(/#(\d+)/g, (_, j) => "#" + N[+j].n)) })) };
}

function golden() {
  const W = JSON.parse(fs.readFileSync(path.join(HERE, "recipes.json"), "utf8"));
  const P = prototype(W, W.imp); const { R } = P; const N = W.nodes;
  const lines = [];
  for (const q of [...TARGETS, ...CALLS]) { const i = find(W, q); lines.push(`target|${q}|${JSON.stringify(summary(W, P, i))}`); }
  // The producer table itself, entry by entry.
  R.table.forEach((e, q) => { lines.push(`table|${e.i}|${N[e.i].n}|${e.how}|${e.ins.join(";")}|${e.out}|${e.fails ? 1 : 0}${e.maybe ? 1 : 0}`); });
  const dest = path.join(HERE, "../recipes.golden"); fs.writeFileSync(dest, lines.join("\n") + "\n");
  console.log(`${lines.length} lines -> recipes.golden`);
}

const [mode, file] = process.argv.slice(2);
if (mode === "trim") trim(file); else if (mode === "golden") golden(); else console.log("usage: recipes.mjs trim <world.json> | golden");
