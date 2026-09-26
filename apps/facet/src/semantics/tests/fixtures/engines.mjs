// Start here (tour.js) and How it fails (fails.js): the pinned world and the
// golden, from the prototype's own code run headless (page.js and recipes.js
// supply the type helpers both read). Run from the repository root:
//
//   node apps/facet/src/semantics/tests/fixtures/engines.mjs trim     # world.js -> engines.json
//   node apps/facet/src/semantics/tests/fixtures/engines.mjs golden   # engines.json -> ../engines.golden
//   node apps/facet/src/semantics/tests/fixtures/engines.mjs full OUT # the whole world.js, for parity runs
//
// `trim` keeps the tour packages whole, every error in ERRORS with its
// variants, every callable that returns it and everything that mentions a
// variant; then, slimmed to what the engines read (kind, name, package,
// module, owner, visibility, line), every source of an edge into those and
// every type-like item whose name the engines resolved. Importance is the
// full world's (world.js `imp`), stored as `imp`. It then runs the same
// subjects over the full world and the pinned one and fails unless every
// line agrees. `golden` writes the prototype's answers over the pinned world.
import fs from "fs";
import path from "path";
import vm from "vm";
import crypto from "crypto";

const HERE = path.dirname(new URL(import.meta.url).pathname);
const PROTO = path.join(HERE, "../../../../../../Nudox-Design-System/v4/graph");
export const PACKAGES = ["toml", "serde_json", "serde_core", "smallvec", "backend-client", "backend-runtime"];
export const ERRORS = [
  "runtime::RuntimeError",
  "client::ClientError",
  // Recursive calls: the answer depends on the order pages are opened.
  "semantic::ir::semantic_render::canonical::CanonicalTypeRenderError",
];

const sha = (f) => crypto.createHash("sha1").update(fs.readFileSync(path.join(PROTO, f))).digest("hex").slice(0, 12);
const callable = (n) => n.k === "function" || n.k === "method";

// The prototype, with the internals the golden reads exported (the source is
// patched in memory only; every line it runs is the prototype's).
export function prototype(W, imp) {
  const N = W.nodes, NN = N.length, PK = W.packages;
  const topOf = new Int32Array(NN); for (let i = 0; i < NN; i++) { let j = i; while (N[j].u >= 0) j = N[j].u; topOf[i] = j; }
  const kids = Array.from({ length: NN }, () => null); for (let i = 0; i < NN; i++) { const u = N[i].u; if (u >= 0) (kids[u] ||= []).push(i); }
  function csr(pairs, from, to) {
    const off = new Int32Array(NN + 1); for (const e of pairs) off[e[from] + 1]++;
    for (let i = 0; i < NN; i++) off[i + 1] += off[i];
    const at = off.slice(0, NN); const dst = new Int32Array(pairs.length), bits = new Int32Array(pairs.length);
    for (const e of pairs) { const k = at[e[from]]++; dst[k] = e[to]; bits[k] = e[2]; }
    return { off, dst, bits };
  }
  // layout.mjs itemEdges, verbatim.
  const ie = new Map();
  for (const [a, b, bits] of W.edges) { const A = topOf[a], B = topOf[b]; if (A === B) continue; const k = A * 4194304 + B; const e = ie.get(k); if (e) { e[0] |= bits; e[1]++; } else ie.set(k, [bits, 1]); }
  const itemEdges = [...ie.entries()].map(([k, [bits, w]]) => [Math.floor(k / 4194304), k % 4194304, bits, w]);
  const OUT = csr(W.edges, 0, 1), IN = csr(W.edges, 1, 0), IOUT = csr(itemEdges, 0, 1), IIN = csr(itemEdges, 1, 0);
  const REL = W.rel; const bit = (name) => 1 << REL.indexOf(name);
  const B = { has: bit("has"), takes: bit("takes"), gives: bit("gives"), is: bit("is"), derives: bit("derives"), calls: bit("calls"), uses: bit("uses"), type: bit("type"), impl: bit("impl") };
  // app.js inEdges / outEdges / qual, verbatim.
  const inEdges = (t, mask) => { const out = []; for (let e = IN.off[t]; e < IN.off[t + 1]; e++) if (IN.bits[e] & mask) out.push(IN.dst[e]); return out; };
  const outEdges = (t, mask) => { const out = []; for (let e = OUT.off[t]; e < OUT.off[t + 1]; e++) if (OUT.bits[e] & mask) out.push(OUT.dst[e]); return out; };
  const qual = (i) => { const n = N[i]; const mp = W.modules[n.m].path; return `${PK[n.p].name.replace(/^backend-/, "")}${mp ? "::" + mp : ""}${n.u >= 0 ? "::" + N[n.u].n : ""}`; };
  const G = { N, PK, MD: W.modules, IMP: Float32Array.from(imp), topOf, kids, yours: (i) => PK[N[i].p].yours, isItem: (i) => N[i].u < 0, IN, OUT, IIN, IOUT, B, inEdges, outEdges, nameOf: (j) => N[j].n, qual };
  const stub = () => stubObj; const stubObj = new Proxy(function () {}, { get: (_, k) => (k === Symbol.iterator ? [][Symbol.iterator] : k === "length" ? 0 : stub), apply: () => null });
  const window = { addEventListener() {}, KINDS: {} };
  const document = { getElementById: () => null, addEventListener() {}, querySelector: () => null, querySelectorAll: () => [], createElement: () => stubObj, body: stubObj };
  const ctx = vm.createContext({ window, document, location: { search: "" }, URLSearchParams, history: { replaceState() {} }, setTimeout: () => 0, clearTimeout() {}, requestAnimationFrame: () => 0, console, performance });
  const patch = (src, from, to) => { if (!src.includes(from)) throw new Error("the prototype moved: " + from); return src.replace(from, to); };
  const read = (f) => fs.readFileSync(path.join(PROTO, f), "utf8");
  vm.runInContext(read("recipes.js"), ctx, { filename: "recipes.js" });
  vm.runInContext(read("fails.js"), ctx, { filename: "fails.js" });
  vm.runInContext(patch(read("page.js"), "window.GRAPH_PAGE = { init,", "window.GRAPH_PAGE = { failKinds, howItFails, init,"), ctx, { filename: "page.js" });
  vm.runInContext(patch(read("tour.js"), "window.GRAPH_TOUR = { init,", "window.GRAPH_TOUR = { partsOf, useOf, init,"), ctx, { filename: "tour.js" });
  window.GRAPH_PAGE.init(G); window.GRAPH_TOUR.init(G);
  return { G, T: window.GRAPH_TOUR, F: window.GRAPH_FAILS, P: window.GRAPH_PAGE.types, PAGE: window.GRAPH_PAGE };
}

// A symbol, independent of its index: `qual::name:line`.
export const key = (G, i) => `${G.qual(i)}::${G.N[i].n}:${G.N[i].l || 0}`;
const fullName = (G, j) => (G.N[j].u >= 0 ? G.N[G.N[j].u].n + "::" : "") + G.N[j].n;
// Rendered HTML as the words a reader sees: blocks are spaced, inline marks
// (links, italics) are not.
const words = (html) => html.replace(/<\/?(?:div|section|h2|span)\b[^>]*>/g, " ").replace(/<[^>]+>/g, "").replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/\s+/g, " ").trim();

// Every line the golden holds, for the given subjects.
function lines(W, P, subjects) {
  const { G, T, F, PAGE } = P; const N = G.N; const out = [];
  const K = (i) => (i >= 0 ? key(G, i) : "-");
  for (const name of subjects.packages) {
    const pk = W.packages.findIndex((p) => p.name === name); if (pk < 0) throw new Error("no package " + name);
    const t = T.of(pk);
    out.push(`tour|${name}|${t.items}|${t.stops.map((s) => `${K(s.i)}=${s.role}=${s.why}`).join(";")}`);
    for (const s of t.stops) out.push(`stop|${name}|${fullName(G, s.i)}|${s.role}|${s.why}|${T.plain(N[s.i].d)}`);
    // Every item of the package: its use, and a type's parts (the heart's inside-it candidates).
    for (let i = 0; i < N.length; i++) {
      if (N[i].p !== pk || N[i].u >= 0) continue;
      const u = T.useOf(i, pk); out.push(`use|${K(i)}|${u.n}|${u.yours}`);
      if (["struct", "enum", "union", "type"].includes(N[i].k)) out.push(`parts|${K(i)}|${[...T.partsOf(i).keys()].map(K).join(",")}`);
    }
  }
  const errors = subjects.errors.map((q) => { const e = N.findIndex((n, i) => `${G.qual(i)}::${n.n}` === q); if (e < 0) throw new Error("no " + q); return e; });
  // error_of for every callable the fixture holds whole.
  for (const j of subjects.callables(G)) out.push(`error|${K(j)}|${K(F.errorOf(j))}`);
  const failing = (E) => N.map((_, j) => j).filter((j) => callable(N[j]) && F.errorOf(j) === E);
  for (const E of errors) {
    // fails_of in node order from a fresh memo, then the pipe's line for each.
    for (const j of failing(E)) { const r = F.failsOf(j); out.push(`fails|${K(j)}|${K(r.E)}|${r.all}|${[...r.kinds].map(([v, via]) => `${N[v].n}@${via >= 0 ? K(via) : "-"}`).join(",")}`); }
    for (const j of failing(E)) out.push(`line|${K(j)}|${words(PAGE.failKinds(j))}`);
    for (const [v, ms] of F.makersOf(E)) out.push(`makers|${K(E)}|${N[v].n}|${ms.map((j) => (G.yours(j) ? "*" : "") + K(j)).join(",")}`);
    const r = F.reachOf(E); out.push(`reach|${K(E)}|${r.can}|${[...r.told].map((v) => N[v].n).join(",")}`);
    const html = PAGE.howItFails(E); const col = /--gfk:(\d+)ch/.exec(html);
    out.push(`section|${K(E)}|${words(html)}|${col ? col[1] : "-"}`);
  }
  return out;
}

// The same fails_of answers from a fresh memo, asked in reverse node order.
function reversed(W, P, subjects) {
  const { G, F } = P; const N = G.N; const out = [];
  const K = (i) => (i >= 0 ? key(G, i) : "-");
  for (const q of subjects.errors) {
    const E = N.findIndex((n, i) => `${G.qual(i)}::${n.n}` === q);
    const js = N.map((_, j) => j).filter((j) => callable(N[j]) && F.errorOf(j) === E).reverse();
    for (const j of js) { const r = F.failsOf(j); out.push(`fails-desc|${K(j)}|${[...r.kinds].map(([v, via]) => `${N[v].n}@${via >= 0 ? K(via) : "-"}`).join(",")}`); }
  }
  return out;
}

function loadWorldJs() {
  const ctx = { window: {} }; vm.createContext(ctx);
  vm.runInContext(fs.readFileSync(path.join(PROTO, "world.js"), "utf8"), ctx);
  return ctx.window.WORLD;
}

function trim() {
  const W = loadWorldJs(); const imp = W.imp;
  const full = prototype(W, imp); const { G, T, F, P } = full; const N = W.nodes;
  const pks = PACKAGES.map((name) => W.packages.findIndex((p) => p.name === name));
  const whole = new Set();
  N.forEach((n, i) => { if (pks.includes(n.p)) whole.add(i); });
  const errors = ERRORS.map((q) => N.findIndex((n, i) => `${G.qual(i)}::${n.n}` === q));
  for (const E of errors) {
    whole.add(E); for (const k of G.kids[E] || []) whole.add(k);
    for (let j = 0; j < N.length; j++) if (callable(N[j]) && F.errorOf(j) === E) whole.add(j);
    for (const v of F.kinds(E)) for (const j of G.inEdges(v, -1)) whole.add(j);
  }
  // Every `type Result` alias (fails.js reads the first per package).
  N.forEach((n, i) => { if (n.k === "type" && n.n === "Result" && n.u < 0) whole.add(i); });
  // The names the engines resolve for what is whole: the errors of its
  // callables, the parts of its types (a fresh instance, so nothing is memoised).
  const probe = prototype(W, imp); const looked = new Set(); const resolve = probe.P.resolveName;
  probe.P.resolveName = (name, from) => { looked.add(name); return resolve(name, from); };
  for (const j of whole) if (callable(N[j])) probe.F.errorOf(j);
  for (const pk of pks) { probe.T.of(pk); N.forEach((n, i) => { if (n.p === pk && n.u < 0 && ["struct", "enum", "union", "type"].includes(n.k)) probe.T.partsOf(i); }); }
  probe.P.resolveName = resolve;
  const slim = new Set();
  // Sources of edges into what is whole (use counts, doers, makers), and every
  // candidate for a resolved name.
  for (const [a, b] of W.edges) if (whole.has(b) && !whole.has(a)) slim.add(a);
  N.forEach((n, i) => { if (!whole.has(i) && n.u < 0 && ["struct", "enum", "trait", "type", "union"].includes(n.k) && looked.has(n.n)) slim.add(i); });
  const keep = new Set([...whole, ...slim]);
  for (const i of [...keep]) if (N[i].u >= 0 && !keep.has(N[i].u)) { keep.add(N[i].u); slim.add(N[i].u); }
  const wholeKeys = new Set([...whole].map((i) => key(G, i)));
  const subjects = { packages: PACKAGES, errors: ERRORS, callables: (g) => g.N.map((_, j) => j).filter((j) => callable(g.N[j]) && wholeKeys.has(key(g, j))) };
  const before = [...lines(W, full, subjects), ...reversed(W, prototype(W, imp), subjects)];
  const ids = [...keep].sort((a, b) => a - b); const newId = new Map(ids.map((o, k) => [o, k]));
  const pkUsed = [...new Set(ids.map((i) => N[i].p))].sort((a, b) => a - b); const newPk = new Map(pkUsed.map((o, k) => [o, k]));
  const mdUsed = [...new Set(ids.map((i) => N[i].m))].sort((a, b) => a - b); const newMd = new Map(mdUsed.map((o, k) => [o, k]));
  const nodes = ids.map((i) => {
    const o = N[i];
    // What the engines read: a whole node's identity, visibility, types and
    // generics (the file of an item: tests are skipped; the lede of a tour
    // package's symbol: a stop shows it); a slim one's identity.
    const tour = pks.includes(o.p);
    const n = Object.fromEntries(Object.entries(whole.has(i)
      ? { k: o.k, n: o.n, p: o.p, m: o.m, u: o.u, v: o.v, l: o.l, f: o.u < 0 ? o.f : undefined, d: tour ? o.d : undefined, ty: o.ty, ret: o.ret, recv: o.recv, gen: o.gen, wh: o.wh, via: o.via, orphan: o.orphan }
      : { k: o.k, n: o.n, p: o.p, m: o.m, u: o.u, v: o.v, l: o.l, slim: 1 }).filter(([, x]) => x !== undefined));
    n.p = newPk.get(o.p); n.m = newMd.get(o.m); n.u = o.u >= 0 ? newId.get(o.u) : -1;
    return n;
  });
  // An edge matters when one end is whole: into it (use, doers, makers) or out of it (calls).
  const edges = W.edges.filter(([a, b]) => keep.has(a) && keep.has(b) && (whole.has(a) || whole.has(b))).map(([a, b, bits]) => [newId.get(a), newId.get(b), bits]);
  const pinned = {
    packages: pkUsed.map((o) => ({ ...W.packages[o], deps: (W.packages[o].deps || []).filter((d) => newPk.has(d)).map((d) => newPk.get(d)) })),
    modules: mdUsed.map((o) => ({ ...W.modules[o], pkg: newPk.get(W.modules[o].pkg) })),
    nodes, rel: W.rel, edges, imp: ids.map((i) => imp[i]),
    pinned: { world: crypto.createHash("sha1").update(fs.readFileSync(path.join(PROTO, "world.json"))).digest("hex").slice(0, 12), "tour.js": sha("tour.js"), "fails.js": sha("fails.js"), "page.js": sha("page.js"), "recipes.js": sha("recipes.js") },
  };
  const after = [...lines(pinned, prototype(pinned, pinned.imp), subjects), ...reversed(pinned, prototype(pinned, pinned.imp), subjects)];
  let same = before.length === after.length;
  for (let k = 0, shown = 0; k < Math.max(before.length, after.length); k++) if (before[k] !== after[k]) { same = false; if (shown++ < 12) console.log("DIFFERS\n full  ", before[k], "\n pinned", after[k]); }
  if (!same) throw new Error("the pinned world answers differently");
  const dest = path.join(HERE, "engines.json"); fs.writeFileSync(dest, JSON.stringify(pinned));
  console.log(`${nodes.length} nodes (${whole.size} whole), ${edges.length} edges, ${pkUsed.length} packages -> engines.json ${fs.statSync(dest).size} bytes; ${before.length} lines agree with the full world`);
}

function golden() {
  const W = JSON.parse(fs.readFileSync(path.join(HERE, "engines.json"), "utf8"));
  const subjects = { packages: PACKAGES, errors: ERRORS, callables: (g) => g.N.map((_, j) => j).filter((j) => callable(g.N[j]) && !g.N[j].slim) };
  const out = [`# prototype ${Object.entries(W.pinned).map(([k, v]) => `${k} ${v}`).join(" · ")}`, ...lines(W, prototype(W, W.imp), subjects), ...reversed(W, prototype(W, W.imp), subjects)];
  const dest = path.join(HERE, "../engines.golden"); fs.writeFileSync(dest, out.join("\n") + "\n");
  console.log(`${out.length} lines -> engines.golden`);
}

// The whole world, for a parity run against the Rust engines (no golden: the
// world moves). Every package's tour; every callable's fails_of in node order
// from a fresh memo; every error's makers, reach and section.
function whole(file) {
  const W = loadWorldJs(); const P = prototype(W, W.imp); const { G, T, F, PAGE } = P; const N = G.N;
  const K = (i) => (i >= 0 ? key(G, i) : "-"); const out = [];
  let t0 = performance.now();
  const tours = W.packages.map((_, pk) => T.of(pk)); const tTour = performance.now() - t0;
  tours.forEach((t, pk) => out.push(`tour|${W.packages[pk].name}|${t.items}|${t.stops.map((s) => `${K(s.i)}=${s.role}=${s.why}`).join(";")}`));
  t0 = performance.now(); const fails = []; for (let j = 0; j < N.length; j++) if (callable(N[j])) fails.push([j, F.failsOf(j)]); const tFails = performance.now() - t0;
  for (const [j, r] of fails) out.push(`fails|${K(j)}|${r ? `${K(r.E)}|${r.all}|${[...r.kinds].map(([v, via]) => `${N[v].n}@${via >= 0 ? K(via) : "-"}`).join(",")}` : "-"}`);
  for (let E = 0; E < N.length; E++) {
    if (!F.kinds(E).length) continue; const r = F.reachOf(E); if (!r.can) continue;
    for (const [v, ms] of F.makersOf(E)) out.push(`makers|${K(E)}|${N[v].n}|${ms.map((j) => (G.yours(j) ? "*" : "") + K(j)).join(",")}`);
    out.push(`reach|${K(E)}|${r.can}|${[...r.told].map((v) => N[v].n).join(",")}`);
    const html = PAGE.howItFails(E); const col = /--gfk:(\d+)ch/.exec(html); out.push(`section|${K(E)}|${words(html)}|${col ? col[1] : "-"}`);
  }
  for (const [j, r] of fails) if (r) { const w = words(PAGE.failKinds(j)); if (w) out.push(`line|${K(j)}|${w}`); }
  fs.writeFileSync(file, out.join("\n") + "\n");
  console.log(`${out.length} lines -> ${file}; prototype: tours of ${W.packages.length} packages ${tTour.toFixed(0)} ms, fails of ${fails.length} callables ${tFails.toFixed(0)} ms`);
}

const [mode, file] = process.argv.slice(2);
if (mode === "trim") trim(); else if (mode === "golden") golden(); else if (mode === "full") whole(file);
else console.log("usage: engines.mjs trim | golden | full <out>");
