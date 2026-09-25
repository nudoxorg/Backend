// layout.mjs — nested, deterministic layout for the world graph.
//
// world → packages → modules → items → members. Each level is a small force
// simulation (links + collision + gravity) seeded on a phyllotaxis spiral by
// importance, so the whole 50k-symbol world lays out in seconds and never
// becomes a hairball: containment does the clustering, forces only order
// neighbours. Members (variants, fields, methods) orbit their type on a
// golden-angle annulus: made-of inside, does outside.
//
// Output: world.js (window.WORLD = {...}) — extraction + geometry + rollups.
import fs from "node:fs";
import path from "node:path";

const HERE = path.dirname(new URL(import.meta.url).pathname);
const W = JSON.parse(fs.readFileSync(path.join(HERE, "world.json"), "utf8"));
const N = W.nodes, NN = N.length;
const GOLD = Math.PI * (3 - Math.sqrt(5));

// ------------------------------------------------------------- rollups
const top = new Int32Array(NN); // top-level item of every node
for (let i = 0; i < NN; i++) { let j = i; while (N[j].u >= 0) j = N[j].u; top[i] = j; }
const itemEdges = new Map(); // key a*2^22+b -> [bits, w]
for (const [a, b, bits] of W.edges) {
  const A = top[a], B = top[b]; if (A === B) continue;
  const k = A * 4194304 + B; const e = itemEdges.get(k);
  if (e) { e[0] |= bits; e[1]++; } else itemEdges.set(k, [bits, 1]);
}
const IE = [...itemEdges.entries()].map(([k, [bits, w]]) => [Math.floor(k / 4194304), k % 4194304, bits, w]);

// importance: PageRank over item edges (a depends on b ⇒ rank flows to b)
const items = []; for (let i = 0; i < NN; i++) if (N[i].u < 0) items.push(i);
const outDeg = new Float64Array(NN), inDeg = new Int32Array(NN), yoursIn = new Int32Array(NN);
for (const [a, b, , w] of IE) { outDeg[a] += w; inDeg[b]++; if (W.packages[N[a].p].yours && !W.packages[N[b].p].yours) yoursIn[b] += w; }
let pr = new Float64Array(NN); for (const i of items) pr[i] = 1 / items.length;
for (let it = 0; it < 40; it++) {
  const nx = new Float64Array(NN); let dangling = 0;
  for (const i of items) if (!outDeg[i]) dangling += pr[i];
  for (const [a, b, , w] of IE) nx[b] += 0.85 * pr[a] * w / outDeg[a];
  for (const i of items) nx[i] += (0.15 + 0.85 * dangling) / items.length;
  pr = nx;
}
const prMax = Math.max(...items.map((i) => pr[i]));
const imp = new Float32Array(NN);
for (const i of items) imp[i] = Math.pow(pr[i] / prMax, 0.35);
for (let i = 0; i < NN; i++) if (N[i].u >= 0) imp[i] = imp[top[i]] * 0.5;

// ------------------------------------------------------------- simulation
function simulate(nodes, links, { gravity = 0.04, iters = 320, charge = 0 } = {}) {
  // nodes: {r, w (weight for seeding)}; links: [i, j, strength]
  const n = nodes.length; if (!n) return;
  const order = nodes.map((_, i) => i).sort((a, b) => nodes[b].w - nodes[a].w);
  const meanR = nodes.reduce((s, x) => s + x.r, 0) / n;
  order.forEach((i, k) => { const r = meanR * 1.9 * Math.sqrt(k + 0.5); nodes[i].x = r * Math.cos(k * GOLD); nodes[i].y = r * Math.sin(k * GOLD); nodes[i].vx = 0; nodes[i].vy = 0; });
  if (n === 1) { nodes[0].x = nodes[0].y = 0; return; }
  const deg = new Float64Array(n); for (const [a, b] of links) { deg[a]++; deg[b]++; }
  let alpha = 1; const decay = 1 - Math.pow(0.001, 1 / iters);
  for (let it = 0; it < iters; it++) {
    alpha += (0 - alpha) * decay;
    for (const [a, b, s] of links) {
      const A = nodes[a], B = nodes[b];
      let dx = B.x + B.vx - A.x - A.vx, dy = B.y + B.vy - A.y - A.vy; let l = Math.hypot(dx, dy) || 1e-6;
      const target = A.r + B.r + 0.6 * meanR;
      const k = (l - target) / l * alpha * s / Math.min(deg[a], deg[b]) * 0.5;
      dx *= k; dy *= k; B.vx -= dx; B.vy -= dy; A.vx += dx; A.vy += dy;
    }
    if (charge) for (let i = 0; i < n; i++) for (let j = i + 1; j < n; j++) {
      const A = nodes[i], B = nodes[j]; const dx = B.x - A.x, dy = B.y - A.y; const l2 = dx * dx + dy * dy + 1e-6;
      const f = charge * alpha * meanR * meanR / l2; A.vx -= dx * f; A.vy -= dy * f; B.vx += dx * f; B.vy += dy * f;
    }
    for (const A of nodes) { A.vx -= A.x * gravity * alpha; A.vy -= A.y * gravity * alpha; }
    for (const A of nodes) { A.x += A.vx; A.y += A.vy; A.vx *= 0.6; A.vy *= 0.6; }
    // collision, a few relaxation passes (exact, O(n²) — n ≤ ~230 per level)
    for (let pass = 0; pass < 2; pass++) for (let i = 0; i < n; i++) for (let j = i + 1; j < n; j++) {
      const A = nodes[i], B = nodes[j]; const dx = B.x - A.x, dy = B.y - A.y; const min = A.r + B.r; const l2 = dx * dx + dy * dy;
      if (l2 < min * min) { const l = Math.sqrt(l2) || 1e-6; const push = (min - l) / l * 0.5; const wa = B.r * B.r / (A.r * A.r + B.r * B.r);
        A.x -= dx * push * wa * 2; A.y -= dy * push * wa * 2; B.x += dx * push * (1 - wa) * 2; B.y += dy * push * (1 - wa) * 2; }
    }
  }
  // recentre on the weighted centroid of the enclosing circle
  let cx = 0, cy = 0, tw = 0; for (const A of nodes) { cx += A.x * A.r * A.r; cy += A.y * A.r * A.r; tw += A.r * A.r; }
  cx /= tw; cy /= tw; for (const A of nodes) { A.x -= cx; A.y -= cy; }
}
const enclose = (nodes) => nodes.reduce((m, A) => Math.max(m, Math.hypot(A.x, A.y) + A.r), 0);

// ------------------------------------------------------------- level 3: members around items
const X = new Float32Array(NN), Y = new Float32Array(NN), R = new Float32Array(NN);
const CORE = { struct: 1.25, enum: 1.25, trait: 1.25, type: 0.8, function: 0.8, constant: 0.6, macro: 0.7 };
const kidsOf = new Map(); for (let i = 0; i < NN; i++) if (N[i].u >= 0) { if (!kidsOf.has(N[i].u)) kidsOf.set(N[i].u, []); kidsOf.get(N[i].u).push(i); }
// Members sit on shells around their type, like an atom: the first shells hold
// what it is made of (variants, fields), the outer shells what it does (methods).
// Each shell is evenly spaced, so at close range the member ring reads as a cut polygon.
const SPACING = 0.78, SHELL_GAP = 0.72;
const rel = new Map(); // item -> [{id, dx, dy, shell}]
const shells = new Map(); // item -> [{r, n}]
for (const it of items) {
  const kids = (kidsOf.get(it) || []).slice();
  const inner = (k) => (N[k].k === "variant" || N[k].k === "field") ? 0 : 1;
  kids.sort((a, b) => inner(a) - inner(b) || (N[a].l || 0) - (N[b].l || 0));
  const r0 = CORE[N[it].k] ?? 0.8;
  const flat = [], sh = [];
  let r = r0 + 0.55, j = 0;
  for (const group of [kids.filter((k) => !inner(k)), kids.filter((k) => inner(k))]) {
    let q = 0;
    while (q < group.length) {
      const cap = Math.max(4, Math.floor(2 * Math.PI * r / SPACING));
      const take = group.slice(q, q + cap);
      const phase = -Math.PI / 2 + (sh.length % 2 ? Math.PI / take.length : 0);
      take.forEach((k, t) => { const th = phase + t * 2 * Math.PI / take.length; flat.push({ id: k, dx: r * Math.cos(th), dy: r * Math.sin(th) }); });
      sh.push({ r, n: take.length, from: j }); j += take.length; q += take.length; r += SHELL_GAP;
    }
  }
  rel.set(it, flat); shells.set(it, sh);
  R[it] = sh.length ? sh[sh.length - 1].r + 0.4 : r0;
}

// ------------------------------------------------------------- level 2: items in modules
const modItems = W.modules.map(() => []);
for (const it of items) modItems[N[it].m].push(it);
const MR = new Float32Array(W.modules.length), MX = new Float32Array(W.modules.length), MY = new Float32Array(W.modules.length);
const local = new Map(); // item -> local x,y in module
for (let m = 0; m < W.modules.length; m++) {
  const ids = modItems[m]; const idx = new Map(ids.map((id, k) => [id, k]));
  const nodes = ids.map((id) => ({ r: R[id] + 0.45, w: imp[id] + (N[id].k === "struct" || N[id].k === "enum" || N[id].k === "trait" ? 1 : 0) }));
  const links = [];
  for (const [a, b, bits, w] of IE) if (idx.has(a) && idx.has(b)) links.push([idx.get(a), idx.get(b), Math.min(1, 0.4 + 0.2 * Math.log2(1 + w))]);
  simulate(nodes, links, { gravity: 0.06, iters: 260, charge: ids.length < 120 ? 0.08 : 0 });
  ids.forEach((id, k) => local.set(id, [nodes[k].x, nodes[k].y]));
  MR[m] = Math.max(2, enclose(nodes)) + 0.9;
}

// ------------------------------------------------------------- level 1: modules in packages
const P = W.packages.length;
const pkgMods = W.packages.map(() => []);
W.modules.forEach((m, i) => pkgMods[m.pkg].push(i));
const modEdges = new Map();
for (const [a, b, bits, w] of IE) { const ma = N[a].m, mb = N[b].m; if (ma === mb) continue; const k = ma * 65536 + mb; modEdges.set(k, (modEdges.get(k) || 0) + w); }
const PR = new Float32Array(P), PX = new Float32Array(P), PY = new Float32Array(P);
const mlocal = new Map();
for (let p = 0; p < P; p++) {
  const ids = pkgMods[p]; const idx = new Map(ids.map((id, k) => [id, k]));
  const nodes = ids.map((id) => ({ r: MR[id] + 1.2, w: MR[id] }));
  const links = [];
  for (const [k, w] of modEdges) { const a = Math.floor(k / 65536), b = k % 65536; if (idx.has(a) && idx.has(b)) links.push([idx.get(a), idx.get(b), Math.min(1, 0.3 + 0.15 * Math.log2(1 + w))]); }
  // module tree: a::b sits near a
  const byPath = new Map(ids.map((id) => [W.modules[id].path, id]));
  for (const id of ids) { const pth = W.modules[id].path; const up = pth.includes("::") ? pth.slice(0, pth.lastIndexOf("::")) : pth ? "" : null; if (up !== null && byPath.has(up)) links.push([idx.get(id), idx.get(byPath.get(up)), 1]); }
  simulate(nodes, links, { gravity: 0.05, iters: 300, charge: 0.05 });
  ids.forEach((id, k) => mlocal.set(id, [nodes[k].x, nodes[k].y]));
  PR[p] = Math.max(4, enclose(nodes)) + 3;
}

// ------------------------------------------------------------- level 0: packages in the world
const pkgEdges = new Map();
for (const [k, w] of modEdges) { const a = W.modules[Math.floor(k / 65536)].pkg, b = W.modules[k % 65536].pkg; if (a === b) continue; const kk = a * 1024 + b; pkgEdges.set(kk, (pkgEdges.get(kk) || 0) + w); }
{
  const nodes = W.packages.map((pk, i) => ({ r: PR[i] + 10, w: PR[i] }));
  const links = [];
  for (const [k, w] of pkgEdges) links.push([Math.floor(k / 1024), k % 1024, Math.min(1, 0.2 + 0.12 * Math.log2(1 + w))]);
  simulate(nodes, links, { gravity: 0.05, iters: 500, charge: 0.1 });
  nodes.forEach((A, i) => { PX[i] = A.x; PY[i] = A.y; });
}

// ------------------------------------------------------------- compose absolute positions
for (let m = 0; m < W.modules.length; m++) { const p = W.modules[m].pkg; const [lx, ly] = mlocal.get(m); MX[m] = PX[p] + lx; MY[m] = PY[p] + ly; }
for (const it of items) {
  const [lx, ly] = local.get(it); const m = N[it].m;
  X[it] = MX[m] + lx; Y[it] = MY[m] + ly;
  for (const { id, dx, dy } of rel.get(it)) { X[id] = X[it] + dx; Y[id] = Y[it] + dy; R[id] = 0.26; }
}

// ------------------------------------------------------------- hulls (faceted)
function hull(pts) {
  pts.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  const cross = (o, a, b) => (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
  const lo = [], up = [];
  for (const p of pts) { while (lo.length >= 2 && cross(lo[lo.length - 2], lo[lo.length - 1], p) <= 0) lo.pop(); lo.push(p); }
  for (let i = pts.length - 1; i >= 0; i--) { const p = pts[i]; while (up.length >= 2 && cross(up[up.length - 2], up[up.length - 1], p) <= 0) up.pop(); up.push(p); }
  up.pop(); lo.pop(); return lo.concat(up);
}
// simplify: drop vertices whose turn is tiny, so hulls read as cut facets, not circles
function facet(poly, minTurn = 0.2) {
  let out = poly.slice(); let changed = true;
  while (changed && out.length > 5) {
    changed = false;
    for (let i = 0; i < out.length && out.length > 5; i++) {
      const a = out[(i - 1 + out.length) % out.length], b = out[i], c = out[(i + 1) % out.length];
      const t = Math.abs(Math.atan2(c[1] - b[1], c[0] - b[0]) - Math.atan2(b[1] - a[1], b[0] - a[0]));
      const turn = Math.min(t, 2 * Math.PI - t);
      if (turn < minTurn) { out.splice(i, 1); changed = true; i--; }
    }
  }
  return out;
}
const ring = (x, y, r, k, rot = 0) => Array.from({ length: k }, (_, i) => [x + r * Math.cos(rot + i * 2 * Math.PI / k), y + r * Math.sin(rot + i * 2 * Math.PI / k)]);
const round2 = (v) => Math.round(v * 100) / 100;
const modHull = W.modules.map((_, m) => {
  const pts = modItems[m].flatMap((it) => ring(X[it], Y[it], R[it] + 0.7, 10, 0.3));
  if (!pts.length) pts.push(...ring(MX[m], MY[m], 1.5, 6));
  return facet(hull(pts), 0.28).map(([x, y]) => [round2(x), round2(y)]);
});
const pkgHull = W.packages.map((_, p) => {
  const pts = pkgMods[p].flatMap((m) => modHull[m].flatMap(([x, y]) => { const dx = x - MX[m], dy = y - MY[m], l = Math.hypot(dx, dy) || 1; return [[x + dx / l * 2.2, y + dy / l * 2.2]]; }));
  if (pts.length < 3) pts.push(...ring(PX[p], PY[p], PR[p], 8));
  return facet(hull(pts), 0.22).map(([x, y]) => [round2(x), round2(y)]);
});

// ------------------------------------------------------------- write
const out = {
  ...W,
  x: Array.from(X, round2), y: Array.from(Y, round2), r: Array.from(R, round2), imp: Array.from(imp, (v) => Math.round(v * 1000) / 1000),
  inDeg: Array.from(inDeg), yoursIn: Array.from(yoursIn),
  mod: { x: Array.from(MX, round2), y: Array.from(MY, round2), r: Array.from(MR, round2), hull: modHull },
  pkg: { x: Array.from(PX, round2), y: Array.from(PY, round2), r: Array.from(PR, round2), hull: pkgHull },
  itemEdges: IE,
  shells: items.filter((i) => shells.get(i).length).map((i) => [i, shells.get(i).map((x) => [round2(x.r), x.n])]),
  modEdges: [...modEdges.entries()].map(([k, w]) => [Math.floor(k / 65536), k % 65536, w]),
  pkgEdges: [...pkgEdges.entries()].map(([k, w]) => [Math.floor(k / 1024), k % 1024, w]),
};
fs.writeFileSync(path.join(HERE, "world.js"), "window.WORLD=" + JSON.stringify(out) + ";\n");
const ext = Math.max(...W.packages.map((_, i) => Math.hypot(PX[i], PY[i]) + PR[i]));
console.log(JSON.stringify({ items: items.length, itemEdges: IE.length, modEdges: modEdges.size, pkgEdges: pkgEdges.size, worldRadius: Math.round(ext), bytes: fs.statSync(path.join(HERE, "world.js")).size }));
