// The world graph — prototype renderer over window.WORLD (graph/world.js).
//
// Design rules this code encodes (gui-plan §6.2 applied to a graph):
//   * containment clusters, forces only order neighbours (layout.mjs), so the
//     whole workspace is one legible map instead of a hairball;
//   * semantic zoom: packages → modules → symbols → members, each level fades
//     in by its on-screen size, never by a fixed zoom step;
//   * monochrome at rest; mint = yours (your packages and every symbol your code
//     reaches), periwinkle = focus; direction is motion, not colour;
//   * the camera flies on the van Wijk–Nuij optimal path (zoom out, pan, zoom in);
//   * paint cost follows what is visible: modules are culled by box, shapes are
//     batched per style, labels are budgeted and placed by importance.
(() => {
  "use strict";
  const WD = window.WORLD;
  const N = WD.nodes, NN = N.length, PK = WD.packages, MD = WD.modules;
  const X = Float32Array.from(WD.x), Y = Float32Array.from(WD.y), R = Float32Array.from(WD.r), IMP = Float32Array.from(WD.imp);
  const YIN = Int32Array.from(WD.yoursIn), INDEG = Int32Array.from(WD.inDeg);
  const MX = WD.mod.x, MY = WD.mod.y, MR = WD.mod.r, PX = WD.pkg.x, PY = WD.pkg.y, PR = WD.pkg.r;
  const REL = WD.rel; const bit = (name) => 1 << REL.indexOf(name);
  const B = { has: bit("has"), takes: bit("takes"), gives: bit("gives"), is: bit("is"), derives: bit("derives"), calls: bit("calls"), uses: bit("uses"), type: bit("type"), impl: bit("impl") };
  const Q = new URLSearchParams(location.search);
  const STILL = Q.has("still");

  // ------------------------------------------------------------------ derived structure
  const isItem = (i) => N[i].u < 0;
  const kids = Array.from({ length: NN }, () => null);
  for (let i = 0; i < NN; i++) { const u = N[i].u; if (u >= 0) (kids[u] ||= []).push(i); }
  const modItems = MD.map(() => []); const pkgMods = PK.map(() => []);
  for (let i = 0; i < NN; i++) if (N[i].u < 0) modItems[N[i].m].push(i);
  MD.forEach((m, i) => pkgMods[m.pkg].push(i));
  for (const list of modItems) list.sort((a, b) => IMP[b] - IMP[a]);
  const yoursPkg = PK.map((p) => p.yours);
  // member shells: members grouped by orbit radius, in angular order (layout.mjs places them evenly)
  const shellsOf = new Map();
  for (let i = 0; i < NN; i++) if (N[i].u < 0 && kids[i]) {
    const by = new Map();
    for (const j of kids[i]) { const r = Math.round(Math.hypot(X[j] - X[i], Y[j] - Y[i]) * 20); (by.get(r) || by.set(r, []).get(r)).push(j); }
    shellsOf.set(i, [...by.values()].map((list) => list.sort((a, b) => Math.atan2(Y[a] - Y[i], X[a] - X[i]) - Math.atan2(Y[b] - Y[i], X[b] - X[i]))));
  }
  const topOf = new Int32Array(NN); for (let i = 0; i < NN; i++) { let j = i; while (N[j].u >= 0) j = N[j].u; topOf[i] = j; }

  // adjacency, member level (CSR) — out and in
  function csr(pairs, from, to) {
    const off = new Int32Array(NN + 1); for (const e of pairs) off[e[from] + 1]++;
    for (let i = 0; i < NN; i++) off[i + 1] += off[i];
    const at = off.slice(0, NN); const dst = new Int32Array(pairs.length), bits = new Int32Array(pairs.length);
    for (const e of pairs) { const k = at[e[from]]++; dst[k] = e[to]; bits[k] = e[2]; }
    return { off, dst, bits };
  }
  const OUT = csr(WD.edges, 0, 1), IN = csr(WD.edges, 1, 0);
  const IOUT = csr(WD.itemEdges, 0, 1), IIN = csr(WD.itemEdges, 1, 0);
  const IW = new Map(); for (const [a, b, , w] of WD.itemEdges) IW.set(a * 65536 + b, w);

  // bounds
  const bbox = (poly) => { let x0 = 1e9, y0 = 1e9, x1 = -1e9, y1 = -1e9; for (const [x, y] of poly) { if (x < x0) x0 = x; if (y < y0) y0 = y; if (x > x1) x1 = x; if (y > y1) y1 = y; } return [x0, y0, x1, y1]; };
  const MB = WD.mod.hull.map(bbox), PB = WD.pkg.hull.map(bbox);
  const WB = PB.reduce((a, b) => [Math.min(a[0], b[0]), Math.min(a[1], b[1]), Math.max(a[2], b[2]), Math.max(a[3], b[3])]);
  const pkgSize = PK.map((_, p) => pkgMods[p].reduce((s, m) => s + modItems[m].length, 0));
  const reachPkg = PK.map((_, p) => pkgMods[p].reduce((s, m) => s + modItems[m].filter((i) => YIN[i] > 0).length, 0));

  // spatial grid over items and members (points), for picking
  const GC = 3; const GX0 = WB[0], GY0 = WB[1]; const GW = Math.ceil((WB[2] - WB[0]) / GC) + 1, GH = Math.ceil((WB[3] - WB[1]) / GC) + 1;
  const gcount = new Int32Array(GW * GH + 1);
  const cellOf = (x, y) => Math.min(GW - 1, Math.max(0, ((x - GX0) / GC) | 0)) + Math.min(GH - 1, Math.max(0, ((y - GY0) / GC) | 0)) * GW;
  for (let i = 0; i < NN; i++) gcount[cellOf(X[i], Y[i]) + 1]++;
  for (let c = 0; c < GW * GH; c++) gcount[c + 1] += gcount[c];
  const gfill = gcount.slice(0, GW * GH); const gids = new Int32Array(NN);
  for (let i = 0; i < NN; i++) gids[gfill[cellOf(X[i], Y[i])]++] = i;

  // ------------------------------------------------------------------ canvas + camera
  const view = document.getElementById("gview"), cv = document.getElementById("gc"), cx = cv.getContext("2d");
  let VW = 800, VH = 600, DPR = 1;
  const cam = { x: (WB[0] + WB[2]) / 2, y: (WB[1] + WB[3]) / 2, w: (WB[2] - WB[0]) * 1.08 };
  const tgt = { ...cam }; let flight = null; let inertia = null;
  const K = () => VW / cam.w;
  const sx = (x) => (x - cam.x) * (VW / cam.w) + VW / 2, sy = (y) => (y - cam.y) * (VW / cam.w) + VH / 2;
  const wx = (px) => (px - VW / 2) * cam.w / VW + cam.x, wy = (py) => (py - VH / 2) * cam.w / VW + cam.y;
  const fitW = (x0, y0, x1, y1, pad = 1.15) => Math.max((x1 - x0), (y1 - y0) * VW / VH) * pad;
  const worldW = () => fitW(...WB, 1.06);

  function resize() {
    const r = view.getBoundingClientRect(); DPR = window.devicePixelRatio || 1;
    VW = Math.max(1, r.width); VH = Math.max(1, r.height);
    cv.width = Math.round(VW * DPR); cv.height = Math.round(VH * DPR); dirty = true;
  }

  // van Wijk & Nuij (2003): the optimal zoom-and-pan path, ρ = √2.
  function planFlight(p0, p1, rho = Math.SQRT2) {
    const [ux0, uy0, w0] = p0, [ux1, uy1, w1] = p1;
    const dx = ux1 - ux0, dy = uy1 - uy0, d2 = dx * dx + dy * dy, r2 = rho * rho, r4 = r2 * r2;
    if (d2 < 1e-12) { const S = Math.log(w1 / w0) / rho; return { S: Math.abs(S), at: (t) => [ux0 + t * dx, uy0 + t * dy, w0 * Math.exp(rho * t * S)] }; }
    const d1 = Math.sqrt(d2);
    const b0 = (w1 * w1 - w0 * w0 + r4 * d2) / (2 * w0 * r2 * d1), b1 = (w1 * w1 - w0 * w0 - r4 * d2) / (2 * w1 * r2 * d1);
    const r0 = Math.log(Math.sqrt(b0 * b0 + 1) - b0), rr1 = Math.log(Math.sqrt(b1 * b1 + 1) - b1);
    const S = (rr1 - r0) / rho; const ch = Math.cosh(r0), sh = Math.sinh(r0);
    return { S, at: (t) => { const s = t * S; const u = w0 / (r2 * d1) * (ch * Math.tanh(rho * s + r0) - sh); return [ux0 + u * dx, uy0 + u * dy, w0 * ch / Math.cosh(rho * s + r0)]; } };
  }
  function flyTo(x, y, w, then) {
    // w is measured against the current view width; the path is planned in world units
    const plan = planFlight([cam.x, cam.y, cam.w], [x, y, w]);
    const ms = Q.has("slow") ? 4000 : Math.max(320, Math.min(1500, 210 * plan.S + 260));
    flight = { plan, t0: performance.now(), ms, then }; inertia = null;
    tgt.x = x; tgt.y = y; tgt.w = w; wake();
  }
  const ease = (t) => (1 - Math.cos(Math.PI * t)) / 2;

  // ------------------------------------------------------------------ state
  let hover = -1, hoverTerr = null, focus = -1, dirty = true, raf = 0, drag = null, lastT = performance.now();
  let hoverA = 0; // hover highlight fades in
  const peekEl = document.getElementById("gpeek"), focusEl = document.getElementById("gfocus"), whereEl = document.getElementById("gwhere"), hudEl = document.getElementById("ghud");
  const findEl = document.getElementById("gfind"), qEl = document.getElementById("gq"), resEl = document.getElementById("gres"), addrEl = document.getElementById("gaddr");
  let flow = 0; // edge flow phase (direction as motion)

  function wake() { if (!raf && !STILL) raf = requestAnimationFrame(frame); dirty = true; if (STILL) queueMicrotask(() => draw(performance.now())); }

  // ------------------------------------------------------------------ neighbourhood
  function neighbourhood(i) {
    const useItem = isItem(i); const O = useItem ? IOUT : OUT, I = useItem ? IIN : IN;
    const outs = [], ins = [];
    for (let k = O.off[i]; k < O.off[i + 1]; k++) outs.push([O.dst[k], O.bits[k]]);
    for (let k = I.off[i]; k < I.off[i + 1]; k++) ins.push([I.dst[k], I.bits[k]]);
    // members: also show the parent link
    return { outs, ins };
  }
  let nbCache = { i: -2, v: null };
  const nb = (i) => (nbCache.i === i ? nbCache.v : (nbCache = { i, v: neighbourhood(i) }).v);

  // ------------------------------------------------------------------ colours
  const INK = [210, 217, 229], MINT = [108, 235, 173], PERI = [147, 162, 250], LINE = [158, 176, 255];
  const rgba = (c, a) => `rgba(${c[0]},${c[1]},${c[2]},${a.toFixed(3)})`;
  const smooth = (v, a, b) => { const t = Math.min(1, Math.max(0, (v - a) / (b - a))); return t * t * (3 - 2 * t); };
  const yours = (i) => yoursPkg[N[i].p];
  const reached = (i) => YIN[topOf[i]] > 0;

  // ------------------------------------------------------------------ labels
  const occ = { cell: 8, w: 0, h: 0, a: null };
  function occReset() { occ.w = Math.ceil(VW / occ.cell) + 1; occ.h = Math.ceil(VH / occ.cell) + 1; occ.a = new Uint8Array(occ.w * occ.h); }
  function occTry(x0, y0, x1, y1) {
    const c = occ.cell; const a0 = Math.max(0, (x0 / c) | 0), b0 = Math.max(0, (y0 / c) | 0), a1 = Math.min(occ.w - 1, (x1 / c) | 0), b1 = Math.min(occ.h - 1, (y1 / c) | 0);
    if (x1 < 0 || y1 < 0 || x0 > VW || y0 > VH) return false;
    for (let b = b0; b <= b1; b++) for (let a = a0; a <= a1; a++) if (occ.a[a + b * occ.w]) return false;
    for (let b = b0; b <= b1; b++) for (let a = a0; a <= a1; a++) occ.a[a + b * occ.w] = 1;
    return true;
  }
  const widthCache = new Map();
  function tw(text, font) { const key = font + "\u0000" + text; let w = widthCache.get(key); if (w === undefined) { cx.font = font; w = cx.measureText(text).width; widthCache.set(key, w); } return w; }
  const F = { pkg: (s) => `620 ${s}px "Bricolage Grotesque"`, mod: '500 11.5px "Geist Mono"', item: '500 11.5px "Geist Mono"', itemB: '600 12.5px "Geist Mono"', mem: '400 10.5px "Geist Mono"', sub: '400 11px "Geist"' };

  // ------------------------------------------------------------------ draw
  const stats = { items: 0, members: 0, edges: 0, labels: 0, ms: 0 };
  function draw(now) {
    const t0 = performance.now();
    const k = K();
    cx.setTransform(DPR, 0, 0, DPR, 0, 0);
    cx.fillStyle = "#040a16"; cx.fillRect(0, 0, VW, VH);
    occReset(); stats.items = stats.members = stats.edges = stats.labels = 0;
    const vx0 = wx(0), vy0 = wy(0), vx1 = wx(VW), vy1 = wy(VH);
    const inView = (b, m = 0) => b[2] >= vx0 - m && b[0] <= vx1 + m && b[3] >= vy0 - m && b[1] <= vy1 + m;
    const hi = focus >= 0 ? focus : hover;
    const hiNb = hover >= 0 && !prism ? nb(hover) : null;
    const lit = new Set(); if (hiNb) { lit.add(hover); for (const [j] of hiNb.outs) lit.add(j); for (const [j] of hiNb.ins) lit.add(j); if (N[hover].u >= 0) lit.add(N[hover].u); }
    const G = prism ? prism.g : 0;
    const dimAll = (G > 0 ? 1 - 0.7 * G : hover >= 0 ? 1 - 0.45 * hoverA : 1) * (sought || ripple || chainOn() || tour ? 0.42 : 1);

    // ---- territories: packages
    const visP = [];
    for (let p = 0; p < PK.length; p++) if (inView(PB[p])) visP.push(p);
    for (const p of visP) {
      const px = PR[p] * k; const hull = WD.pkg.hull[p];
      cx.beginPath(); hull.forEach(([x, y], j) => (j ? cx.lineTo(sx(x), sy(y)) : cx.moveTo(sx(x), sy(y)))); cx.closePath();
      const y = yoursPkg[p]; const terrHi = hoverTerr && hoverTerr.p === p;
      cx.fillStyle = y ? rgba(MINT, 0.035) : rgba(LINE, terrHi ? 0.035 : 0.014); cx.fill();
      cx.lineWidth = 1; cx.strokeStyle = y ? rgba(MINT, 0.22 * (1 - 0.85 * smooth(px, 500, 1800))) : rgba(LINE, (terrHi ? 0.26 : 0.12) * (1 - 0.85 * smooth(px, 500, 1800))); cx.stroke();
    }
    // ---- territories: modules
    const visM = [];
    for (const p of visP) {
      const show = smooth(PR[p] * k, 160, 360);
      for (const m of pkgMods[p]) if (inView(MB[m])) {
        visM.push(m);
        if (show > 0.01 && MR[m] * k > 6) {
          const hull = WD.mod.hull[m]; cx.beginPath(); hull.forEach(([x, y], j) => (j ? cx.lineTo(sx(x), sy(y)) : cx.moveTo(sx(x), sy(y)))); cx.closePath();
          const th = hoverTerr && hoverTerr.m === m;
          cx.strokeStyle = rgba(yoursPkg[p] ? MINT : LINE, (th ? 0.22 : 0.075) * show); cx.lineWidth = 1; cx.stroke();
          if (th) { cx.fillStyle = rgba(LINE, 0.03); cx.fill(); }
        }
      }
    }

    // ---- ambient edges, by level
    cx.lineCap = "round";
    const far = visP.length ? Math.max(...visP.map((p) => PR[p] * k)) : 0;
    const pkgEdgeA = 1 - smooth(far, 260, 700);
    if (pkgEdgeA > 0.01) {
      for (const [a, b, w] of WD.pkgEdges) {
        if (PK[a].name === "std" || PK[b].name === "std") continue;
        const on = hoverTerr && (hoverTerr.p === a || hoverTerr.p === b);
        if (!on && w < 40) continue;
        const ax = sx(PX[a]), ay = sy(PY[a]), bx = sx(PX[b]), by = sy(PY[b]);
        const mx = (ax + bx) / 2 + (by - ay) * 0.12, my = (ay + by) / 2 - (bx - ax) * 0.12;
        cx.beginPath(); cx.moveTo(ax, ay); cx.quadraticCurveTo(mx, my, bx, by);
        cx.lineWidth = 0.6 + Math.log2(1 + w) * 0.18; cx.strokeStyle = rgba(yoursPkg[a] ? MINT : on ? PERI : LINE, (on ? 0.32 : 0.035) * pkgEdgeA * dimAll); cx.stroke(); stats.edges++;
      }
    }
    const modEdgeA = smooth(far, 300, 700) * (1 - smooth(Math.max(1, ...visM.map((m) => MR[m] * k)), 110, 240));
    if (modEdgeA > 0.01) {
      const vis = new Set(visM);
      cx.lineWidth = 0.8;
      for (const [a, b, w] of WD.modEdges) {
        if (w < 2 || !(vis.has(a) || vis.has(b))) continue;
        cx.beginPath(); cx.moveTo(sx(MX[a]), sy(MY[a])); cx.lineTo(sx(MX[b]), sy(MY[b]));
        cx.strokeStyle = rgba(LINE, Math.min(0.12, 0.02 + 0.012 * Math.log2(w)) * modEdgeA * dimAll); cx.stroke(); stats.edges++;
      }
    }
    // symbol edges inside visible modules, once symbols are big enough to read
    const itemEdgeA = smooth(k, 3.2, 7);
    if (itemEdgeA > 0.01) {
      cx.lineWidth = 0.8; cx.strokeStyle = rgba(LINE, 0.1 * itemEdgeA * dimAll); cx.beginPath();
      for (const m of visM) for (const a of modItems[m]) {
        for (let e = IOUT.off[a]; e < IOUT.off[a + 1]; e++) {
          const b = IOUT.dst[e]; if (N[b].m !== m) continue;
          cx.moveTo(sx(X[a]), sy(Y[a])); cx.lineTo(sx(X[b]), sy(Y[b])); stats.edges++;
        }
      }
      cx.stroke();
    }

    // ---- nodes: batched per (shape, tone)
    const shapes = { dia: [], hollow: [], sq: [], dot: [] };
    const tone = (i) => (reached(i) || yours(i) ? 1 : 0);
    const batches = new Map(); // key -> Path2D
    const bpath = (key) => { let p = batches.get(key); if (!p) { p = new Path2D(); batches.set(key, p); } return p; };
    const memberA = (it) => smooth(R[it] * k, 12, 26);
    const labels = []; // candidates: [prio, i, x, y, size]
    for (const m of visM) {
      for (const i of modItems[m]) {
        const x = sx(X[i]), y = sy(Y[i]);
        const ri = R[i] * k;
        if (x < -ri - 40 || y < -ri - 40 || x > VW + ri + 40 || y > VH + ri + 40) continue;
        stats.items++;
        const kd = N[i].k; const core = (kd === "struct" || kd === "enum" || kd === "trait" ? 1.25 : kd === "function" || kd === "type" ? 0.8 : 0.6) * k;
        const bright = Math.min(3, (IMP[i] * 4) | 0);
        const t = tone(i);
        if (core < 1.6) { // a star
          const s = (core < 0.7 ? 1 : 1.5) + (t && !yours(i) ? 0.6 : 0);
          bpath(`dot:${t}:${bright}`).rect(Math.round(x - s / 2), Math.round(y - s / 2), s, s);
        } else {
          const s = Math.min(core, 9) * 0.8;
          const shape = kd === "trait" ? "hollow" : kd === "struct" || kd === "enum" || kd === "type" ? "dia" : "sq";
          const p = bpath(`${shape}:${t}:${bright}`);
          if (shape === "sq") p.rect(x - s * 0.55, y - s * 0.55, s * 1.1, s * 1.1);
          else { p.moveTo(x, y - s); p.lineTo(x + s, y); p.lineTo(x, y + s); p.lineTo(x - s, y); p.closePath(); }
        }
        if (core > 4.5 || lit.has(i) || i === hi) labels.push([IMP[i] * smooth(core, 4.5, 11) + (lit.has(i) ? 10 : 0) + (i === hi ? 20 : 0), i, x, y, Math.min(core, 9) * 0.8]);
        // members orbit their parent
        const ma = memberA(i);
        if (ma > 0.02 && kids[i]) {
          for (const ring of shellsOf.get(i)) {
            if (ring.length < 3) continue;
            const sp = bpath(`shell:${t}:${Math.round(ma * 3)}`);
            ring.forEach((j, q) => (q ? sp.lineTo(sx(X[j]), sy(Y[j])) : sp.moveTo(sx(X[j]), sy(Y[j])))); sp.closePath();
          }
          for (const j of kids[i]) {
            const mx = sx(X[j]), my = sy(Y[j]); if (mx < -10 || my < -10 || mx > VW + 10 || my > VH + 10) continue;
            stats.members++;
            const s = Math.max(1, Math.min(4.5, 0.26 * k * 0.8));
            const mk = N[j].k === "method" ? "msq" : "mdot";
            bpath(`${mk}:${t}:${Math.round(ma * 3)}`).rect(mx - s / 2, my - s / 2, s, s);
            if (k > 38) labels.push([IMP[j] + 0.2 + (lit.has(j) ? 10 : 0) + (j === hi ? 20 : 0), j, mx, my, s / 2]);
          }
        }
      }
    }
    for (const [key, p] of batches) {
      const [shape, t, br] = key.split(":"); const c = +t ? MINT : INK;
      let a;
      if (shape === "shell") { cx.strokeStyle = rgba(c === MINT ? MINT : LINE, 0.09 * (+br / 3) * dimAll); cx.lineWidth = 1; cx.stroke(p); continue; }
      if (shape === "msq" || shape === "mdot") a = (shape === "msq" ? 0.55 : 0.4) * (+br / 3) * dimAll;
      else a = (0.3 + 0.2 * +br) * dimAll * (shape === "dot" ? 0.9 : 1);
      if (shape === "hollow") { cx.strokeStyle = rgba(c, a); cx.lineWidth = 1.2; cx.stroke(p); }
      else { cx.fillStyle = rgba(c, a); cx.fill(p); }
    }

    // ---- the constellation: everything the find box matches, wherever it lives
    if (sought) {
      const pad = 12; const halo = new Path2D(), core = new Path2D(); let n = 0;
      for (const j of sought) {
        const x = sx(X[j]), y = sy(Y[j]); if (x < -pad || y < -pad || x > VW + pad || y > VH + pad) continue;
        const s = Math.max(2.2, Math.min(5, 1.6 + k * 0.35)); n++;
        core.moveTo(x, y - s); core.lineTo(x + s, y); core.lineTo(x, y + s); core.lineTo(x - s, y); core.closePath();
        halo.moveTo(x + s + 4, y); halo.arc(x, y, s + 4, 0, Math.PI * 2);
        labels.push([6 + IMP[j] * 2, j, x, y, s]);
      }
      cx.fillStyle = rgba(PERI, 0.13); cx.fill(halo); cx.fillStyle = rgba(PERI, 0.95); cx.fill(core);
      stats.sought = n;
    }

    // ---- a chain (find "have -> need", in steps): your value's road through the world. The road runs
    // through types as places: calls on the same type fold into one stop, named "Language · from › name".
    const CH = chainOn();
    if (CH) {
      const stops = chainStops(CH);
      const pts = stops.map((st) => [sx(X[st.at]), sy(Y[st.at])]);
      const arcs = []; // [x0, y0, cx, cy, x1, y1]: gentle arcs, all bending the same way
      for (let a = 1; a < pts.length; a++) { const [x0, y0] = pts[a - 1], [x1, y1] = pts[a]; const dx = x1 - x0, dy = y1 - y0; arcs.push([x0, y0, (x0 + x1) / 2 - dy * 0.18, (y0 + y1) / 2 + dx * 0.18, x1, y1]); }
      const t = STILL ? 1 : ease(Math.min(1, (now - CH.t0) / (380 + 260 * Math.max(1, arcs.length))));
      const road = new Path2D(); arcs.forEach(([x0, y0, qx, qy, x1, y1], a) => { if (!a) road.moveTo(x0, y0); road.quadraticCurveTo(qx, qy, x1, y1); });
      cx.lineWidth = 1.5; cx.strokeStyle = rgba(PERI, 0.7); cx.stroke(road);
      const s = Math.max(3.2, Math.min(6, 2.4 + k * 0.4));
      const dia = (x, y, r) => { const p = new Path2D(); p.moveTo(x, y - r); p.lineTo(x + r, y); p.lineTo(x, y + r); p.lineTo(x - r, y); p.closePath(); return p; };
      stops.forEach((st, a) => {
        const [x, y] = pts[a]; const reached = !arcs.length || a / arcs.length <= t + 1e-6; const last = a === stops.length - 1;
        if (st.yours) { cx.lineWidth = 1.4; cx.strokeStyle = rgba(MINT, 0.95); cx.stroke(dia(x, y, s + 2.5)); } // what you have
        else { cx.fillStyle = rgba(last ? PERI : INK, reached ? 1 : 0.35); cx.fill(dia(x, y, last ? s + 1.5 : s)); }
        // its words: the place, then the calls made there
        const name = N[st.at].n, calls = st.calls.join(" › ");
        cx.font = F.item; const w0 = tw(name, F.item), w1 = calls ? tw(" · " + calls, F.item) : 0;
        // right of the stop, else left, else below: two stops close together must not share a line
        const W = w0 + w1;
        const spots = [[x + s + 8, y + 4], [x - s - 8 - W, y + 4], [x - W / 2, y + s + 16], [x - W / 2, y - s - 8], [x + s + 8, y + 20], [x + s + 8, y - 12]];
        const inside = ([a, b]) => a - 3 >= 8 && a + W + 3 <= VW - 8 && b - 12 >= 8 && b + 4 <= VH - 8;
        let [lx, ly] = spots.find((p) => inside(p) && occTry(p[0] - 3, p[1] - 12, p[0] + W + 3, p[1] + 4)) || spots.find(inside) || spots[0];
        cx.fillStyle = "rgba(4,10,22,.72)"; cx.fillRect(lx - 3, ly - 12, w0 + w1 + 6, 16);
        cx.fillStyle = rgba(st.yours ? MINT : INK, reached ? 0.95 : 0.4); cx.fillText(name, lx, ly);
        if (calls) { cx.fillStyle = rgba(PERI, reached ? 0.95 : 0.4); cx.fillText(" · " + calls, lx + w0, ly); }
      });
      if (t < 1 && arcs.length) { // the value, travelling
        const u = t * arcs.length, a = Math.min(arcs.length - 1, Math.floor(u)), f = u - a; const [x0, y0, qx, qy, x1, y1] = arcs[a];
        const bx = (1 - f) * (1 - f) * x0 + 2 * (1 - f) * f * qx + f * f * x1, by = (1 - f) * (1 - f) * y0 + 2 * (1 - f) * f * qy + f * f * y1;
        cx.fillStyle = rgba(MINT, 0.18); cx.beginPath(); cx.arc(bx, by, 7, 0, Math.PI * 2); cx.fill();
        cx.fillStyle = rgba(MINT, 1); cx.fill(dia(bx, by, 3));
      }
    }

    // ---- a tour: the road through the package's stops, numbered; the current stop in periwinkle
    if (tour) {
      const pts = tour.stops.map((st) => [sx(X[st.i]), sy(Y[st.i])]);
      // the road: the legs into and out of the current stop are drawn, the rest only hinted
      const near = new Path2D(), far = new Path2D();
      pts.forEach(([x1, y1], a) => { if (!a) return; const [x0, y0] = pts[a - 1]; const dx = x1 - x0, dy = y1 - y0; const p = a === tour.at || a - 1 === tour.at ? near : far; p.moveTo(x0, y0); p.quadraticCurveTo((x0 + x1) / 2 - dy * 0.18, (y0 + y1) / 2 + dx * 0.18, x1, y1); });
      cx.setLineDash([2, 5]); cx.lineWidth = 1.2; cx.strokeStyle = rgba(PERI, 0.12); cx.stroke(far); cx.strokeStyle = rgba(PERI, 0.55); cx.stroke(near); cx.setLineDash([]);
      const s = Math.max(3.2, Math.min(6.5, 2.4 + k * 0.4));
      tour.stops.forEach((st, a) => {
        const [x, y] = pts[a]; const on = a === tour.at, past = a < tour.at;
        const p = new Path2D(); const r = on ? s + 2 : s; p.moveTo(x, y - r); p.lineTo(x + r, y); p.lineTo(x, y + r); p.lineTo(x - r, y); p.closePath();
        if (on) { cx.fillStyle = rgba(PERI, 0.16); cx.beginPath(); cx.arc(x, y, r + 7, 0, Math.PI * 2); cx.fill(); cx.fillStyle = rgba(PERI, 1); cx.fill(p); }
        else { cx.lineWidth = 1.3; cx.strokeStyle = rgba(past ? INK : PERI, past ? 0.55 : 0.8); cx.stroke(p); }
        const text = `${a + 1}  ${fullName(st.i)}`; const font = on ? F.itemB : F.item; const W = tw(text, font);
        const spots = [[x + r + 9, y + 4], [x - r - 9 - W, y + 4], [x - W / 2, y + r + 17], [x - W / 2, y - r - 9]];
        const inside = ([a0, b0]) => a0 - 3 >= 8 && a0 + W + 3 <= VW - 8 && b0 - 12 >= 8 && b0 + 4 <= VH - 8;
        const [lx, ly] = spots.find((q) => inside(q) && occTry(q[0] - 3, q[1] - 12, q[0] + W + 3, q[1] + 4)) || spots.find(inside) || spots[0];
        cx.font = font; cx.fillStyle = "rgba(4,10,22,.72)"; cx.fillRect(lx - 3, ly - 12, W + 6, 16);
        cx.fillStyle = on ? rgba(PERI, 1) : rgba(INK, past ? 0.55 : 0.85); cx.fillText(text, lx, ly);
      });
    }

    // ---- a scrubbed release (the page's comb): removed symbols go hollow, real changes get a periwinkle ring
    const RM = window.GRAPH_RELEASES_UI && window.GRAPH_RELEASES_UI.marks();
    if (RM && (RM.removed.size || RM.changed.size)) {
      const ring = new Path2D(), ghost = new Path2D();
      const put = (p, j, s) => { const x = sx(X[j]), y = sy(Y[j]); if (x < -12 || y < -12 || x > VW + 12 || y > VH + 12) return false; p.moveTo(x, y - s); p.lineTo(x + s, y); p.lineTo(x, y + s); p.lineTo(x - s, y); p.closePath(); return true; };
      const s = Math.max(2.4, Math.min(6, 1.8 + k * 0.4));
      for (const j of RM.changed) if (put(ring, j, s + 2.5) && k > 6) labels.push([5 + IMP[j], j, sx(X[j]), sy(Y[j]), s]);
      for (const j of RM.removed) put(ghost, j, s + 1);
      cx.lineWidth = 1.2; cx.strokeStyle = rgba(PERI, 0.85); cx.stroke(ring);
      cx.setLineDash([2, 2]); cx.strokeStyle = rgba(INK, 0.55); cx.stroke(ghost); cx.setLineDash([]);
    }

    // ---- reach: dependents light up wave by wave (R); first-wave threads run back to the source
    if (ripple) {
      const el = now - ripple.t0; const sx0 = sx(X[ripple.src]), sy0 = sy(Y[ripple.src]);
      ripple.waves.forEach((wave, d) => {
        const f = Math.max(0, Math.min(1, (el - d * WAVE_MS) / 320)); if (f <= 0) return;
        const s = d === 0 ? 4.2 : d === 1 ? 3.2 : 2.2; const a = (d === 0 ? 0.95 : d === 1 ? 0.66 : d === 2 ? 0.34 : 0.2) * f;
        const threads = d === 0 ? new Set(wave.slice().sort((p, q) => IMP[q] - IMP[p]).slice(0, 48)) : null;
        const pm = new Path2D(), pp = new Path2D(), th = new Path2D();
        for (const t of wave) {
          const x = sx(X[t]), y = sy(Y[t]); if (x < -20 || y < -20 || x > VW + 20 || y > VH + 20) continue;
          const p = yours(t) ? pm : pp; p.moveTo(x, y - s); p.lineTo(x + s, y); p.lineTo(x, y + s); p.lineTo(x - s, y); p.closePath();
          if (threads && threads.has(t)) { th.moveTo(x, y); th.lineTo(sx0, sy0); }
          if (d <= 1) labels.push([(d === 0 ? 8 : 4) + IMP[t] * 2, t, x, y, s]);
        }
        if (d === 0) { cx.lineWidth = 0.8; cx.strokeStyle = rgba(PERI, 0.16 * f); cx.stroke(th); }
        cx.fillStyle = rgba(PERI, a); cx.fill(pp); cx.fillStyle = rgba(MINT, Math.min(1, a + 0.1)); cx.fill(pm);
      });
      // the source, ringed
      cx.lineWidth = 1; cx.strokeStyle = rgba(PERI, 0.6); cx.beginPath(); cx.moveTo(sx0, sy0 - 9); cx.lineTo(sx0 + 9, sy0); cx.lineTo(sx0, sy0 + 9); cx.lineTo(sx0 - 9, sy0); cx.closePath(); cx.stroke();
    }

    // ---- the lit neighbourhood (hover): bundled edges with flow, bright nodes
    const hi2 = hover;
    if (hiNb) {
      const hi = hi2; const A = hoverA;
      const path = (a, b) => {
        const pts = [[X[a], Y[a]]];
        const ma = N[a].m, mb = N[b].m, pa = N[a].p, pb = N[b].p;
        if (ma !== mb) { pts.push([MX[ma], MY[ma]]); if (pa !== pb) { pts.push([PX[pa], PY[pa]], [PX[pb], PY[pb]]); } pts.push([MX[mb], MY[mb]]); }
        pts.push([X[b], Y[b]]);
        const n = pts.length - 1, beta = 0.72; const [x0, y0] = pts[0], [xn, yn] = pts[n];
        const sp = pts.map(([x, y], j) => [sx(beta * x + (1 - beta) * (x0 + (xn - x0) * j / n)), sy(beta * y + (1 - beta) * (y0 + (yn - y0) * j / n))]);
        cx.beginPath(); cx.moveTo(sp[0][0], sp[0][1]);
        for (let j = 1; j < n; j++) { const mx2 = (sp[j][0] + sp[j + 1][0]) / 2, my2 = (sp[j][1] + sp[j + 1][1]) / 2; cx.quadraticCurveTo(sp[j][0], sp[j][1], j === n - 1 ? sp[n][0] : mx2, j === n - 1 ? sp[n][1] : my2); }
        if (n === 1) cx.lineTo(sp[1][0], sp[1][1]);
      };
      cx.setLineDash([2, 7]);
      for (const [j] of hiNb.outs) { path(hi, j); cx.lineDashOffset = -flow; cx.lineWidth = 1.2; cx.strokeStyle = rgba(INK, 0.5 * A); cx.stroke(); stats.edges++; }
      for (const [j] of hiNb.ins) { path(j, hi); cx.lineDashOffset = -flow; cx.lineWidth = 1.2; cx.strokeStyle = rgba(yours(j) ? MINT : PERI, 0.6 * A); cx.stroke(); stats.edges++; }
      cx.setLineDash([]);
      for (const j of lit) {
        const x = sx(X[j]), y = sy(Y[j]); const s = j === hi ? 7 : 3.5;
        cx.beginPath(); cx.moveTo(x, y - s); cx.lineTo(x + s, y); cx.lineTo(x, y + s); cx.lineTo(x - s, y); cx.closePath();
        cx.fillStyle = j === hi ? rgba(PERI, 1) : rgba(yours(j) || reached(j) ? MINT : INK, 0.9 * A + 0.1); cx.fill();
        if (j === hi) { cx.lineWidth = 1; cx.strokeStyle = rgba(PERI, 0.5); cx.beginPath(); cx.moveTo(x, y - s - 5); cx.lineTo(x + s + 5, y); cx.lineTo(x, y + s + 5); cx.lineTo(x - s - 5, y); cx.closePath(); cx.stroke(); }
        if (!labels.some((l) => l[1] === j)) labels.push([(j === hi ? 30 : 12) + IMP[j], j, x, y, s]);
      }
    }

    drawTrail();

    // ---- the prism: the focused symbol's relations gathered beside it
    const P2 = prism ? layoutPrism(prism) : null;
    if (P2) for (const sl of P2.slots) if (sl.lab) occTry(...sl.lab);
    if (P2) occTry(P2.fx - 60, P2.fy - 14, P2.fx + 60, P2.fy + 14);

    // ---- labels: packages, then modules, then symbols by importance
    cx.textBaseline = "middle";
    for (const p of visP) {
      const px = PR[p] * k; const a = smooth(px, 26, 60) * (1 - smooth(px, 1400, 2600));
      if (a < 0.02) continue;
      const size = Math.round(Math.min(22, 12 + px / 40)); const font = F.pkg(size);
      const name = PK[p].name.replace(/^backend-/, "");
      const w = tw(name, font); const b = PB[p];
      let x = sx(PX[p]) - w / 2, y = px > 260 ? sy(b[1]) + 18 : sy(PY[p]);
      if (!occTry(x - 4, y - size / 2 - 3, x + w + 4, y + size / 2 + 3)) continue;
      cx.font = font; cx.fillStyle = rgba(yoursPkg[p] ? MINT : INK, (yoursPkg[p] ? 0.9 : 0.75) * a); cx.fillText(name, x, y); stats.labels++;
      if (px > 60 && px < 500) { const sub = pkgSize[p].toLocaleString() + " symbols" + (!yoursPkg[p] && reachPkg[p] ? ` · you use ${reachPkg[p]}` : ""); cx.font = F.sub; cx.fillStyle = rgba(INK, 0.32 * a); cx.fillText(sub, sx(PX[p]) - tw(sub, F.sub) / 2, y + size * 0.5 + 9); occTry(x, y, x + w, y + size); }
    }
    const modLabelA = (m) => smooth(MR[m] * k, 40, 80) * (1 - smooth(MR[m] * k, 900, 1500));
    for (const m of visM.slice().sort((a, b) => MR[b] - MR[a])) {
      const a = modLabelA(m) * smooth(PR[MD[m].pkg] * k, 200, 400); if (a < 0.03) continue;
      const name = MD[m].path || "(root)"; const w = tw(name, F.mod); const b = MB[m];
      const x = sx((b[0] + b[2]) / 2) - w / 2, y = sy(b[1]) + 11;
      if (!occTry(x - 3, y - 8, x + w + 3, y + 8)) continue;
      cx.font = F.mod; cx.fillStyle = rgba(INK, 0.42 * a * (focus >= 0 ? 0.6 : 1)); cx.fillText(name, x, y); stats.labels++;
    }
    labels.sort((a, b) => b[0] - a[0]);
    let budget = Math.round(40 + 120 * smooth(K(), 4, 30));
    const said = CH ? new Set(chainStops(CH).map((st) => st.at)) : tour ? new Set(tour.stops.map((st) => st.i)) : null; // a road names its own stops
    for (const [prio, i, x, y, s] of labels) {
      if (said && said.has(i)) continue;
      if (budget <= 0) break;
      const member = N[i].u >= 0; const strong = prio >= 10; const font = i === hi ? F.itemB : member ? F.mem : F.item;
      const text = N[i].n; const w = tw(text, font);
      const lx = x + s + 5, ly = y;
      if (!occTry(lx - 2, ly - 7, lx + w + 2, ly + 7)) continue;
      budget--; stats.labels++;
      cx.font = font;
      const rip = ripple && ripple.byTop.has(i) && ripple.byTop.get(i) <= 2;
      const c = i === hi || (sought && sought.has(i)) || (rip && !yours(i)) ? PERI : yours(i) || (reached(i) && strong) ? MINT : INK;
      const a = i === hi || (sought && sought.has(i)) || rip ? 1 : strong ? 0.95 : (member ? 0.45 : 0.4 + 0.45 * IMP[i]) * dimAll;
      cx.fillStyle = rgba(c, a); cx.fillText(text, lx, ly);
    }

    if (P2) drawPrism(P2);

    // ---- where: the package and module under the camera
    const at = territoryAt(cam.x, cam.y);
    const level = k < 0.9 ? "world" : k < 3 ? "packages" : k < 12 ? "modules" : k < 40 ? "symbols" : "members";
    const rmLine = RM && RM.crates.length ? `<span class="sep">·</span><span class="rel">${RM.crates.map(([c, v]) => `${esc(c)} at ${esc(window.GRAPH_RELEASES_UI.short(v))}`).join(", ")}: ${RM.changed.size} changed, ${RM.removed.size} gone, ${RM.added} new (not on the map)</span>` : "";
    const tk = !tour && at && k >= 0.9 && k < 12 && tourOf(at.p) ? `<span class="tk"><kbd>T</kbd>start here</span>` : ""; // the package under the camera has a tour
    whereEl.innerHTML = (rmLine ? (at ? `<b>${esc(PK[at.p].name)}</b>` : "") + rmLine : at ? `<b>${esc(PK[at.p].name)}</b>${at.m >= 0 && k > 1.5 ? `<span class="sep">›</span>${esc(MD[at.m].path || "(root)")}` : ""}<span class="alt">${level}</span>` : `<span class="alt">${level}</span>`) + tk;
    stats.ms = performance.now() - t0;
    if (Q.has("hud")) hudEl.textContent = `${stats.ms.toFixed(1)} ms · ${stats.items} symbols · ${stats.members} members · ${stats.edges} edges · ${stats.labels} labels\nk ${k.toFixed(2)} px/unit · ${NN.toLocaleString()} nodes · ${WD.edges.length.toLocaleString()} relations`;
    placePeek();
    dirty = false;
  }

  function territoryAt(x, y) {
    const inside = (poly) => { let c = false; for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) { const [xi, yi] = poly[i], [xj, yj] = poly[j]; if ((yi > y) !== (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi) c = !c; } return c; };
    for (let p = 0; p < PK.length; p++) {
      const b = PB[p]; if (x < b[0] || x > b[2] || y < b[1] || y > b[3] || !inside(WD.pkg.hull[p])) continue;
      for (const m of pkgMods[p]) { const mb = MB[m]; if (x >= mb[0] && x <= mb[2] && y >= mb[1] && y <= mb[3] && inside(WD.mod.hull[m])) return { p, m }; }
      return { p, m: -1 };
    }
    return null;
  }

  // ------------------------------------------------------------------ picking
  function pick(px, py) {
    const k = K(); const x = wx(px), y = wy(py);
    const rad = Math.max(10 / k, 0.5); let best = -1, bd = Infinity;
    const c0 = Math.max(0, ((x - rad - GX0) / GC) | 0), c1 = Math.min(GW - 1, ((x + rad - GX0) / GC) | 0);
    const r0 = Math.max(0, ((y - rad - GY0) / GC) | 0), r1 = Math.min(GH - 1, ((y + rad - GY0) / GC) | 0);
    for (let r = r0; r <= r1; r++) for (let c = c0; c <= c1; c++) {
      const cell = c + r * GW;
      for (let q = gcount[cell]; q < gcount[cell + 1]; q++) {
        const i = gids[q];
        if (N[i].u >= 0 && smooth(R[N[i].u] * k, 12, 26) < 0.5) continue; // members only once visible
        const d = Math.hypot(X[i] - x, Y[i] - y) - (N[i].u < 0 ? 0.6 : 0.1);
        if (d < rad && d < bd) { bd = d; best = i; }
      }
    }
    return best;
  }

  // ------------------------------------------------------------------ peek + focus cards
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const KSVG = window.KINDS || {};
  const kindSpan = (kd) => { const f = KFAM[kd] || "ns"; const svg = KSVG[kd] || KSVG.unknown || ""; return `<span class="k ${f} sm"><svg viewBox="0 0 24 24">${svg}</svg></span>`; };
  const KFAM = { module: "ns", package: "ns", struct: "ty", enum: "ty", union: "ty", type: "ty", trait: "co", function: "ca", method: "ca", macro: "ca", constant: "va", field: "va", variant: "va" };
  const qual = (i) => { const n = N[i]; const mp = MD[n.m].path; return `${PK[n.p].name.replace(/^backend-/, "")}${mp ? "::" + mp : ""}${n.u >= 0 ? "::" + N[n.u].n : ""}`; };
  const yoursUses = (i) => { let c = 0; const I = isItem(i) ? IIN : IN; for (let e = I.off[i]; e < I.off[i + 1]; e++) if (yours(I.dst[e])) c++; return c; };
  function sigOf(i) {
    const n = N[i];
    if (n.k === "field" || n.k === "variant") return n.ty ? `${n.n}${n.k === "variant" ? (n.shape === "record" ? " { " + n.ty + " }" : "(" + n.ty + ")") : ": " + n.ty}` : n.n;
    if ((n.k === "enum" || n.k === "struct") && kids[i]) {
      const parts = kids[i].filter((j) => N[j].k === "variant" || N[j].k === "field");
      if (parts.length) {
        const txt = n.k === "enum" ? parts.map((j) => N[j].n + (N[j].ty ? (N[j].shape === "record" ? " {…}" : "(" + N[j].ty + ")") : "")).join(" | ")
          : "{ " + parts.map((j) => N[j].n + ": " + (N[j].ty || "")).join(", ") + " }";
        return txt;
      }
    }
    return n.s || n.n;
  }
  function peekHTML(i) {
    const n = N[i]; const nbv = nb(i); const yu = yoursUses(i);
    const facts = [];
    if (kids[i]) facts.push(`<span><b>${kids[i].length}</b> members</span>`);
    facts.push(`<span>used by <b>${nbv.ins.length}</b></span>`, `<span>uses <b>${nbv.outs.length}</b></span>`);
    if (yu) facts.push(`<span><b class="y">${yu}</b> in your code</span>`);
    return `<div class="h">${kindSpan(n.k)}<span class="nm">${esc(n.n)}</span></div><div class="wh">${esc(qual(i))}</div>`
      + `<div class="sg">${esc(sigOf(i))}</div>` + (n.d ? `<div class="sy">${esc(n.d)}</div>` : "") + `<div class="fc">${facts.join("")}</div>`;
  }
  let peekFor = -1, peekTimer = 0, peekSlot = -1;
  function setHover(i, slot = -1) {
    peekSlot = slot;
    if (i === hover) return;
    hover = i; hoverA = 0; clearTimeout(peekTimer);
    cv.classList.toggle("point", i >= 0);
    if (i < 0 || i === focus) { peekEl.classList.remove("on"); peekFor = -1; wake(); return; }
    peekTimer = setTimeout(() => { peekFor = i; peekEl.innerHTML = peekHTML(i); peekEl.classList.add("on"); placePeek(); }, STILL ? 0 : 220);
    if (STILL) { peekFor = i; peekEl.innerHTML = peekHTML(i); peekEl.classList.add("on"); }
    wake();
  }
  function placePeek() {
    if (peekFor < 0) return;
    let x = sx(X[peekFor]), y = sy(Y[peekFor]); const w = peekEl.offsetWidth, h = peekEl.offsetHeight;
    let side = 1;
    if (peekSlot >= 0 && prism) { const sl = layoutPrism(prism).slots[peekSlot]; if (sl) { side = sl.side; x = side > 0 ? sl.lab[2] : sl.lab[0]; y = sl.py; } }
    let left = side > 0 ? x + 18 : x - 18 - w, top = y - 22;
    if (left + w > VW - 12) left = x - 18 - w;
    if (left < 12) left = x + 18;
    top = Math.max(12, Math.min(VH - h - 12, top));
    peekEl.style.transform = `translate(${Math.round(left)}px,${Math.round(top)}px)`;
    if (x < -20 || y < -20 || x > VW + 20 || y > VH + 20) peekEl.classList.remove("on");
  }
  function axes(i) {
    const nbv = nb(i); const n = N[i];
    const group = (list, mask) => list.filter(([, b]) => b & mask).map(([j]) => j);
    const uniq = (a) => [...new Set(a)];
    const rows = [];
    const typeLike = ["struct", "enum", "trait", "type"].includes(n.k);
    if (typeLike) {
      rows.push(["is", uniq(group(nbv.outs, B.impl | B.derives | B.is))]);
      rows.push(["made of", uniq(group(nbv.outs, B.has))]);
      rows.push(["made by", uniq(group(nbv.ins, B.gives))]);
      rows.push(["taken by", uniq(group(nbv.ins, B.takes))]);
      rows.push(["used by", uniq(group(nbv.ins, B.uses | B.calls | B.has | B.type)).filter((j) => !rows.some((r) => r[1].includes(j)))]);
    } else {
      rows.push(["takes", uniq(group(nbv.outs, B.takes))]);
      rows.push(["gives", uniq(group(nbv.outs, B.gives))]);
      rows.push(["calls", uniq(group(nbv.outs, B.calls | B.uses))]);
      rows.push(["called by", uniq(group(nbv.ins, B.calls | B.uses))]);
    }
    return rows.filter((r) => r[1].length);
  }
  function showFocus(i) {
    if (i < 0) { focusEl.classList.remove("on"); addrEl.textContent = "nudox://graph"; return; }
    const n = N[i];
    const parts = (kids[i] || []).reduce((a, j) => { a[N[j].k] = (a[N[j].k] || 0) + 1; return a; }, {});
    const facts = [];
    const pl = (c, w) => `${c} ${w}${c === 1 ? "" : "s"}`;
    if (parts.variant) facts.push(pl(parts.variant, "variant")); if (parts.field) facts.push(pl(parts.field, "field")); if (parts.method) facts.push(pl(parts.method, "method"));
    const refs = new Set(inEdges(i, -1).concat(...(kids[i] || []).map((m) => inEdges(m, -1))).map((j) => topOf[j]));
    refs.delete(i); facts.push(`used in ${refs.size} place${refs.size === 1 ? "" : "s"}`);
    const yu = [...refs].filter((j) => yours(j)).length; if (yu) facts.push(`<b>${yu}</b> in your code`);
    focusEl.innerHTML = `<div class="h">${kindSpan(n.k)}<div style="min-width:0"><div class="nm">${esc(n.n)}</div><div class="wh">${esc(qual(i))}</div></div></div>`
      + (n.d ? `<div class="sy">${esc(n.d)}</div>` : "") + capsLine(i) + `<div class="fx">${facts.join('<i>·</i>')}</div>`
      + (ripple && ripple.src === i ? rippleLine(ripple) : "")
      + `<div class="ft"><span><kbd>↵</kbd>open page</span><span><kbd>R</kbd>${ripple && ripple.src === i ? "hide reach" : "reach"}</span><span><kbd>esc</kbd>back out</span></div>`;
    focusEl.classList.add("on");
    const here = document.querySelector(".titlebar .here"); if (here) { here.querySelector(".nm").textContent = n.n; here.querySelector(".path").textContent = qual(i).replace(/::/g, " › "); }
    addrEl.textContent = `nudox://graph/${qual(i).replace(/::/g, "/")}/${n.n}`;
  }
  function frameOf(i) {
    // the symbol with its neighbourhood, trimmed to the nearest 85% so one far edge cannot zoom the world out
    const nbv = nb(i); const pts = [[X[i], Y[i]]];
    for (const [j] of [...nbv.outs, ...nbv.ins]) pts.push([X[j], Y[j]]);
    const d = pts.map(([x, y]) => Math.hypot(x - X[i], y - Y[i])).sort((a, b) => a - b);
    const lim = d[Math.floor((d.length - 1) * 0.85)] || 0;
    const keep = pts.filter(([x, y]) => Math.hypot(x - X[i], y - Y[i]) <= lim + 1e-6);
    let x0 = Math.min(...keep.map((p) => p[0])), x1 = Math.max(...keep.map((p) => p[0])), y0 = Math.min(...keep.map((p) => p[1])), y1 = Math.max(...keep.map((p) => p[1]));
    const r = Math.max(R[topOf[i]] * 3.2, 5);
    x0 = Math.min(x0, X[i] - r); x1 = Math.max(x1, X[i] + r); y0 = Math.min(y0, Y[i] - r); y1 = Math.max(y1, Y[i] + r);
    const w = Math.min(fitW(x0, y0, x1, y1, 1.35), worldW() * 0.7);
    // keep the symbol central: centre on it, widen to include the box
    const half = Math.max(X[i] - x0, x1 - X[i], (Y[i] - y0) * VW / VH, (y1 - Y[i]) * VW / VH) * 2 * 1.2;
    return [X[i], Y[i], Math.min(Math.max(w, half), worldW() * 0.7)];
  }
  // ------------------------------------------------------------------ reach (R): what would feel it if this changed, wave by wave
  // Edges point from the dependent to what it depends on, so dependents are in-edges. A method passes the change to its
  // callers; a field or variant passes it to its owner too (the shape changed). Waves are counted per top-level symbol.
  let ripple = null;
  const WAVE_MS = 240;
  function reach(i) {
    const seen = new Uint8Array(NN); const byTop = new Map([[topOf[i], 0]]); const waves = [];
    let front = [i, ...(kids[i] || [])]; for (const a of front) seen[a] = 1;
    for (let d = 1; d <= 8 && front.length; d++) {
      const next = [], wave = [];
      for (const a of front) for (let e = IN.off[a]; e < IN.off[a + 1]; e++) {
        const b = IN.dst[e]; if (seen[b] || N[b].orphan) continue; seen[b] = 1; next.push(b);
        const k = N[b].k; if ((k === "field" || k === "variant") && N[b].u >= 0 && !seen[N[b].u]) { seen[N[b].u] = 1; next.push(N[b].u); }
        const t = topOf[b]; if (!byTop.has(t)) { byTop.set(t, d); wave.push(t); }
      }
      if (wave.length) waves.push(wave); front = next;
    }
    const all = waves.flat();
    return { src: i, waves, byTop, all, yoursN: all.filter((t) => yours(t)).length, pkgs: new Set(all.map((t) => N[t].p)).size, t0: performance.now() };
  }
  function rippleLine(r) {
    if (!r.all.length) return `<div class="rip">Nothing in this world depends on it.</div>`;
    const w1 = r.waves[0] ? r.waves[0].length : 0, w2 = w1 + (r.waves[1] ? r.waves[1].length : 0);
    const parts = [`<b>${w1}</b> direct`];
    if (r.waves.length > 1) parts.push(`<b>${w2}</b> within two steps`);
    if (r.waves.length > 2) parts.push(`<b>${r.all.length}</b> in all`);
    parts.push(`across ${r.pkgs} package${r.pkgs === 1 ? "" : "s"}`);
    if (r.yoursN) parts.push(`<b class="y">${r.yoursN}</b> in your code`);
    const per = new Map(); for (const t of r.all) per.set(N[t].p, (per.get(N[t].p) || 0) + 1);
    const most = [...per].sort((a, b) => b[1] - a[1]).slice(0, 3).map(([p, c]) => `<span class="${PK[p].yours ? "y" : ""}">${esc(PK[p].name.replace(/^backend-/, ""))}</span> ${c}`);
    return `<div class="rip"><span class="rh">If it changes</span>${parts.join('<i>·</i>')}${per.size > 1 ? `<div class="rmost">most in ${most.join('<i>·</i>')}</div>` : ""}</div>`;
  }
  function toggleReach() {
    if (focus < 0) return;
    if (ripple && ripple.src === focus) { ripple = null; showFocus(focus); wake(); return; }
    ripple = reach(focus); showFocus(focus);
    if (prism) prism.target = 0; // the reach replaces the prism while it is shown
    // frame the source with the nearest 90 % of what it reaches
    const pts = ripple.all.map((t) => [X[t], Y[t]]); pts.push([X[focus], Y[focus]]);
    if (pts.length > 1) {
      const d = pts.map(([x, y]) => Math.hypot(x - X[focus], y - Y[focus])).sort((a, b) => a - b); const lim = d[Math.floor((d.length - 1) * 0.9)];
      const keep = pts.filter(([x, y]) => Math.hypot(x - X[focus], y - Y[focus]) <= lim + 1e-6);
      const x0 = Math.min(...keep.map((p) => p[0])), x1 = Math.max(...keep.map((p) => p[0])), y0 = Math.min(...keep.map((p) => p[1])), y1 = Math.max(...keep.map((p) => p[1]));
      flyTo((x0 + x1) / 2, (y0 + y1) / 2, Math.min(worldW() * 1.1, fitW(x0, y0, x1, y1, 1.3)), () => { ripple && (ripple.t0 = performance.now()); wake(); });
    }
    wake();
  }
  function setFocus(i, fly = true) {
    if (i >= 0 && tour) endTour(); // one plate at a time
    if (ripple && ripple.src !== i) ripple = null;
    focus = i; nbCache.i = -2; showFocus(i); peekEl.classList.remove("on"); peekFor = -1; prismSel = -1; visit(i);
    if (i >= 0) {
      if (prism && prism.g > 0.05) prism.target = 0; // release the old one, gather the new one after the flight
      const gather = () => { prism = buildPrism(i); prism.g = 0; prism.target = 1; wake(); };
      if (fly) { const [x, y, w] = focusCam(i); flyTo(x, y, w, gather); } else gather();
    } else if (prism) prism.target = 0;
    wake();
  }
  // the symbol at reading scale, placed in the middle of the space the focus card leaves
  function focusCam(i) {
    const w = Math.max(VW / 26, R[topOf[i]] * 7);
    const card = VW > 900 ? 380 : 0;
    return [X[i] + (card / 2) * w / VW, Y[i], w];
  }

  function prismMove(dx, dy) {
    const P = layoutPrism(prism); const real = P.slots.map((sl, q) => [sl, q]).filter(([sl]) => !sl.more && sl.j >= 0);
    if (!real.length) return;
    let q;
    if (prismSel < 0) q = real.find(([sl]) => sl.side === (dx < 0 ? -1 : 1))?.[1] ?? real[0][1];
    else {
      const cur = P.slots[prismSel];
      if (dy) { const same = real.filter(([sl]) => sl.side === cur.side); const at = same.findIndex(([, qq]) => qq === prismSel); q = same[Math.max(0, Math.min(same.length - 1, at + dy))][1]; }
      else { const other = real.filter(([sl]) => sl.side === (dx < 0 ? -1 : 1)); if (!other.length) return; q = other.reduce((b, c) => (Math.abs(c[0].y - cur.y) < Math.abs(b[0].y - cur.y) ? c : b))[1]; }
    }
    prismSel = q; const sl = P.slots[q]; hover = -1; setHover(sl.j, q); wake();
  }
  // walking: arrow keys move focus to the neighbour lying most in that screen direction
  function walk(dx, dy) {
    if (focus < 0) return;
    const nbv = nb(focus); let best = -1, bs = -Infinity;
    for (const [j] of [...nbv.outs, ...nbv.ins]) {
      const vx = X[j] - X[focus], vy = Y[j] - Y[focus]; const l = Math.hypot(vx, vy) || 1e-6;
      const cos = (vx * dx + vy * dy) / l; if (cos < 0.35) continue;
      const score = cos * 2 - Math.log(1 + l) * 0.35 + IMP[j] * 0.3;
      if (score > bs) { bs = score; best = j; }
    }
    if (best >= 0) setFocus(best);
  }


  // ------------------------------------------------------------------ relations, the semantic core
  // One rule, every language: the left names what refers to it, the right what it
  // refers to. Group words say how. Shared by the prism, the focus card and the page.
  const IMPLIED = { Clone: "Copy", PartialEq: "Eq", PartialOrd: "Ord", Eq: "Ord" };
  function minimalDerives(list) { const has = new Set(list); return list.filter((d) => !(IMPLIED[d] && has.has(IMPLIED[d]))); }
  const inEdges = (t, mask) => { const out = []; for (let e = IN.off[t]; e < IN.off[t + 1]; e++) if (IN.bits[e] & mask) out.push(IN.dst[e]); return out; };
  const outEdges = (t, mask) => { const out = []; for (let e = OUT.off[t]; e < OUT.off[t + 1]; e++) if (OUT.bits[e] & mask) out.push(OUT.dst[e]); return out; };
  const nameOf = (j) => { const n = N[j]; if (n.u < 0) return n.n; const sep = n.k === "field" ? "." : "::"; return N[n.u].n + sep + n.n; };
  // capabilities in plain words; derived / written / via say how each one arrives
  const CAPW = { Copy: "copies freely", Clone: "clones", Debug: "debug-prints", Display: "prints", PartialEq: "compares", Eq: "compares", PartialOrd: "orders", Ord: "sorts", Hash: "hashes", Default: "has a default", Serialize: "serializes", Deserialize: "deserializes", Error: "is an error", Iterator: "iterates", From: "converts", ToString: "to text", FromStr: "parses", Send: "crosses threads", Sync: "shares across threads", Deref: "derefs", Drop: "cleans up", AsRef: "borrows as" };
  function caps(i) {
    const n = N[i]; const out = [];
    for (const d of minimalDerives(n.derives || [])) out.push({ word: CAPW[d] || d, trait: d, how: "derived" });
    for (const im of n.impls || []) if (im.trait >= 0) { const t = N[im.trait].n; out.push({ word: CAPW[t] || t, trait: t, how: "written", j: im.trait }); }
    for (const t of n.implsExt || []) { const nm = t.split("::").pop().replace(/<.*/, ""); out.push({ word: CAPW[nm] || nm, trait: nm, how: "written" }); }
    if (out.some((c) => c.trait === "Display")) out.push({ word: "to text", trait: "ToString", how: "via Display" });
    const seen = new Set(); return out.filter((c) => (seen.has(c.word) ? false : seen.add(c.word)));
  }
  function capsLine(i) {
    const c = caps(i); if (!c.length) return "";
    return `<div class="gcaps">` + c.map((x) => `<span class="gcap ${x.how.split(" ")[0]}" title="${esc(x.trait)} — ${esc(x.how)}"><i></i>${esc(x.word)}</span>`).join("") + `</div>`;
  }
  function relationsOf(i) {
    const n = N[i]; const groups = []; const seen = new Set([i, ...(kids[i] || [])]);
    const add = (word, side, ids, extra = []) => {
      const list = []; for (const j of ids) { if (seen.has(j)) continue; seen.add(j); list.push({ j }); }
      list.sort((a, b) => (yours(b.j) - yours(a.j)) || IMP[b.j] - IMP[a.j]);
      const all = [...extra, ...list]; if (all.length) groups.push({ word, side, entries: all });
    };
    if (["struct", "enum", "trait", "type", "union"].includes(n.k)) {
      // right: what it is, what it is made of
      const written = (n.impls || []).map((x) => x.trait).filter((t) => t >= 0);
      const extra = written.map((t) => ({ j: t, note: "written" }));
      for (const t of written) seen.add(t);
      const derives = minimalDerives(n.derives || []);
      if (derives.length) extra.push({ text: derives.join(" · "), note: "derived", j: outEdges(i, B.derives)[0] ?? -1, cap: derives });
      for (const t of (n.implsExt || [])) extra.push({ text: t.split("::").pop(), note: "written", j: -1 });
      if (written.some((t) => N[t].n === "Display")) extra.push({ text: "ToString", note: "via Display", j: -1 });
      add("is", 0, outEdges(i, B.is), extra);
      // left: what it comes from
      add("made of", -1, outEdges(i, B.has));
      add("made by", -1, inEdges(i, B.gives));
      if (n.k === "trait") add("implemented by", -1, inEdges(i, B.impl | B.derives));
      // right: what it goes into
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

  // ------------------------------------------------------------------ the prism
  // Focus gathers a symbol's relations beside it: what feeds it on the left,
  // what it is and reaches on the right, grouped and named, each proxy flying in
  // from where it lives in the map (a faint tether keeps the way home). Release
  // sends them back. The page shows the same prism at rest, so page ⇄ graph is
  // one gesture: the prism releasing into the map.
  let prism = null, prismSel = -1;
  // the trail: symbols you visited, joined on the map (the titlebar's thread, drawn in place)
  const trail = []; let graphSeen = !Q.has("page");
  function visit(i) { if (i < 0) return; const t = topOf[i]; if (trail[trail.length - 1] !== t) { trail.push(t); if (trail.length > 24) trail.shift(); } }
  function drawTrail() {
    if (trail.length < 2) return;
    cx.save(); cx.lineWidth = 1.2; cx.strokeStyle = rgba(MINT, 0.32); cx.setLineDash([]);
    cx.beginPath(); trail.forEach((j, q) => (q ? cx.lineTo(sx(X[j]), sy(Y[j])) : cx.moveTo(sx(X[j]), sy(Y[j])))); cx.stroke();
    for (let q = 0; q < trail.length; q++) { const j = trail[q]; const x = sx(X[j]), y = sy(Y[j]); const s = q === trail.length - 1 ? 0 : 3;
      if (!s) continue; cx.beginPath(); cx.moveTo(x, y - s); cx.lineTo(x + s, y); cx.lineTo(x, y + s); cx.lineTo(x - s, y); cx.closePath(); cx.fillStyle = rgba(MINT, 0.55); cx.fill(); }
    cx.restore();
  }
  // page → graph: from wherever the map was left; the first time, from the package's altitude
  function enterGraph(i) {
    if (!graphSeen) { const b = PB[N[i].p]; Object.assign(cam, { x: (b[0] + b[2]) / 2, y: (b[1] + b[3]) / 2, w: fitW(...b, 1.25) }); Object.assign(tgt, cam); graphSeen = true; }
    setFocus(i, true);
  }
  const LEFT = new Set(["made by", "taken by", "used by", "called by"]);
  function buildPrism(i) {
    const groups = relationsOf(i);
    const col = (side) => groups.filter((g) => g.side === side && g.side !== 0).map((g) => ({ word: g.word, list: g.entries.slice(0, 6), more: Math.max(0, g.entries.length - 6) }));
    return { i, g: 0, target: 1, left: col(-1), right: col(1) };
  }
  const PF = { head: 'italic 400 13px "Newsreader"', name: '500 12px "Geist Mono"', nameB: '600 12px "Geist Mono"', where: '400 10.5px "Geist Mono"', more: 'italic 400 12.5px "Newsreader"' };
  function layoutPrism(pr) {
    const i = pr.i; const fx = sx(X[i]), fy = sy(Y[i]);
    const avail = VW - (VW > 900 ? 380 : 0);
    const narrow = avail < 720;
    const gap = narrow ? 0 : Math.max(150, Math.min(250, avail * 0.2));
    const ROW = 22, HEAD = 22, GROUPGAP = 10;
    const slots = []; const heads = [];
    const dup = new Map(); for (const g of [...pr.left, ...pr.right]) for (const en of g.list) { const t = en.text || nameOf(en.j); dup.set(t, (dup.get(t) || 0) + 1); }
    const place = (groups, side, x, y0) => {
      let y = y0;
      for (const g of groups) {
        heads.push({ text: g.word, x, y: y + 9, side });
        y += HEAD;
        for (const en of g.list) { slots.push({ j: en.j, text: en.text || nameOf(en.j), note: en.note || (en.j >= 0 && N[en.j].p !== N[pr.i].p ? PK[N[en.j].p].name.replace(/^backend-/, "") : en.j >= 0 && dup.get(en.text || nameOf(en.j)) > 1 ? MD[N[topOf[en.j]].m].path : ""), side, x, y: y + ROW / 2 - 2, word: g.word }); y += ROW; }
        if (g.more) { slots.push({ more: g.more, side, x, y: y + ROW / 2 - 2 }); y += ROW; }
        y += GROUPGAP;
      }
      return y - y0;
    };
    const height = (groups) => groups.reduce((h, g) => h + HEAD + ROW * (g.list.length + (g.more ? 1 : 0)) + GROUPGAP, 0) - GROUPGAP;
    if (narrow) {
      const all = [...pr.left, ...pr.right]; const h = height(all);
      place(all, 1, fx + 64, Math.max(70, fy - h / 2));
    } else {
      place(pr.left, -1, fx - gap, fy - height(pr.left) / 2);
      place(pr.right, 1, fx + gap, fy - height(pr.right) / 2);
    }
    const e = ease(Math.min(1, pr.g));
    const M = 24;
    for (const sl of slots) {
      if (sl.more) continue;
      let hx = sl.j >= 0 ? sx(X[sl.j]) : sl.x, hy = sl.j >= 0 ? sy(Y[sl.j]) : sl.y;
      sl.homeOn = sl.j >= 0 && hx > 0 && hy > 0 && hx < VW && hy < VH;
      hx = Math.min(VW - M, Math.max(M, hx)); hy = Math.min(VH - M, Math.max(M, hy));
      sl.hx = hx; sl.hy = hy; sl.px = hx + (sl.x - hx) * e; sl.py = hy + (sl.y - hy) * e;
      const w = tw(sl.text, PF.name) + (sl.note ? tw(sl.note, PF.where) + 8 : 0);
      sl.lab = sl.side > 0 ? [sl.px + 8, sl.py - 8, sl.px + 14 + w, sl.py + 8] : [sl.px - 14 - w, sl.py - 8, sl.px - 8, sl.py + 8];
    }
    return { fx, fy, slots, heads, e, narrow };
  }
  function drawPrism(P) {
    const { fx, fy, slots, heads, e } = P; const la = smooth(e, 0.55, 1);
    const bg = "rgba(4,10,22,0.92)";
    // tethers home, then strands to the focus
    cx.setLineDash([2, 5]); cx.lineWidth = 1;
    for (const sl of slots) if (!sl.more && sl.homeOn && Math.hypot(sl.hx - sl.px, sl.hy - sl.py) > 30) {
      cx.beginPath(); cx.moveTo(sl.px, sl.py); cx.lineTo(sl.hx, sl.hy); cx.strokeStyle = rgba(LINE, 0.12 * e); cx.stroke();
      cx.beginPath(); cx.arc(sl.hx, sl.hy, 3, 0, 7); cx.strokeStyle = rgba(INK, 0.25 * e); cx.stroke();
    }
    cx.setLineDash([]);
    for (const sl of slots) {
      if (sl.more) continue;
      const out = sl.side > 0; const x0 = fx + (P.narrow ? 10 : out ? 12 : -12), x1 = sl.px + (out ? -7 : 7);
      const c = (x1 - x0) * 0.5;
      cx.beginPath(); cx.moveTo(x0, fy); cx.bezierCurveTo(x0 + c, fy, x1 - c, sl.py, x1, sl.py);
      const sel = prismSel >= 0 && P.slots[prismSel] === sl, hov = hover === sl.j;
      cx.lineWidth = sel || hov ? 1.5 : 1;
      cx.strokeStyle = sel || hov ? rgba(PERI, 0.9) : sl.j >= 0 && yours(sl.j) ? rgba(MINT, 0.55 * e) : rgba(INK, 0.26 * e); cx.stroke();
      if (!STILL) { cx.setLineDash([1.5, 9]); cx.lineDashOffset = out ? -flow * 1.4 : flow * 1.4; cx.strokeStyle = rgba(sel || hov ? PERI : INK, 0.5 * e); cx.stroke(); cx.setLineDash([]); }
    }
    // the focus gem
    { const s = 9; cx.beginPath(); cx.moveTo(fx, fy - s); cx.lineTo(fx + s, fy); cx.lineTo(fx, fy + s); cx.lineTo(fx - s, fy); cx.closePath(); cx.fillStyle = rgba(PERI, 1); cx.fill();
      cx.beginPath(); cx.moveTo(fx, fy - s - 5); cx.lineTo(fx + s + 5, fy); cx.lineTo(fx, fy + s + 5); cx.lineTo(fx - s - 5, fy); cx.closePath(); cx.lineWidth = 1; cx.strokeStyle = rgba(PERI, 0.45); cx.stroke(); }
    // proxies and names
    cx.textBaseline = "middle"; cx.lineJoin = "round";
    for (const sl of slots) {
      if (sl.more) {
        if (la < 0.02) continue;
        const t = `and ${sl.more} more`; cx.font = PF.more; cx.fillStyle = rgba(INK, 0.38 * la);
        cx.fillText(t, sl.side > 0 ? sl.x + 8 : sl.x - 8 - tw(t, PF.more), sl.y); continue;
      }
      const j = sl.j; const kd = j >= 0 ? N[j].k : "trait"; const s = 4.2;
      const col = j >= 0 && (yours(j) || reached(j) && yours(prism.i)) ? MINT : INK;
      const sel = prismSel >= 0 && P.slots[prismSel] === sl, hov = j >= 0 && hover === j && peekSlot >= 0 && P.slots[peekSlot] === sl;
      cx.beginPath();
      if (kd === "function" || kd === "method" || kd === "constant" || kd === "macro") cx.rect(sl.px - s * 0.7, sl.py - s * 0.7, s * 1.4, s * 1.4);
      else { cx.moveTo(sl.px, sl.py - s); cx.lineTo(sl.px + s, sl.py); cx.lineTo(sl.px, sl.py + s); cx.lineTo(sl.px - s, sl.py); cx.closePath(); }
      if (kd === "trait") { cx.lineWidth = 1.2; cx.strokeStyle = rgba(sel || hov ? PERI : col, 0.95); cx.stroke(); } else { cx.fillStyle = rgba(sel || hov ? PERI : col, 0.95); cx.fill(); }
      if (la < 0.02) continue;
      const name = sl.text; cx.font = sel || hov ? PF.nameB : PF.name;
      const w = tw(name, cx.font);
      const tx = sl.side > 0 ? sl.px + 10 : sl.px - 10 - w;
      cx.strokeStyle = bg; cx.lineWidth = 4; cx.globalAlpha = la; cx.strokeText(name, tx, sl.py); cx.globalAlpha = 1;
      cx.fillStyle = rgba(sel || hov ? PERI : col === MINT ? MINT : INK, (sel || hov ? 1 : 0.92) * la); cx.fillText(name, tx, sl.py);
      if (sl.note) {
        const wh = sl.note; cx.font = PF.where; const ww = tw(wh, PF.where);
        const wx2 = sl.side > 0 ? tx + w + 8 : tx - 8 - ww;
        cx.fillStyle = rgba(INK, 0.32 * la); cx.fillText(wh, wx2, sl.py + 0.5);
      }
    }
    for (const h of heads) {
      if (la < 0.02) continue;
      cx.font = PF.head; const w = tw(h.text, PF.head);
      cx.fillStyle = rgba(INK, 0.5 * la); cx.fillText(h.text, h.side > 0 ? h.x - 4 : h.x + 4 - w, h.y);
    }
  }
  function prismPick(px, py) {
    if (!prism || prism.g < 0.8) return -1;
    const P = layoutPrism(prism);
    for (let q = 0; q < P.slots.length; q++) { const sl = P.slots[q]; if (sl.more || sl.j < 0) continue; const [a, b, c, d] = sl.lab; if ((px >= a - 14 && px <= c + 4 && py >= b - 3 && py <= d + 3) || Math.hypot(px - sl.px, py - sl.py) < 9) return q; }
    return -1;
  }

  // ------------------------------------------------------------------ find
  const names = N.map((n) => n.n.toLowerCase());
  let results = [], sel = 0, sought = null;
  // chains: "have -> need" when no single call (or too few) answers; previewed while selected, held on ↵
  let chains = [], chainPreview = null, chainHeld = null;
  const chainOn = () => chainHeld || chainPreview;
  // a chain's stops: places (types; a free function is its own place), the calls made at each
  function chainStops(c) {
    const out = [];
    c.path.forEach((j, a) => {
      const at = N[j].u >= 0 ? N[j].u : j; const call = a === 0 && c.from && c.from === "#" + j ? null : N[j].k === "variant" || N[j].k === "function" || N[j].k === "method" ? N[j].n : null;
      const prev = out[out.length - 1];
      if (prev && prev.at === at) { if (call) prev.calls.push(call); return; }
      out.push({ at, calls: call ? [call] : [], yours: a === 0 && c.from === "#" + j });
    });
    return out;
  }
  const chainEl = document.createElement("div"); chainEl.className = "gchain gget"; view.appendChild(chainEl);
  function previewChain() {
    const c = sel >= results.length ? chains[sel - results.length] : null;
    if (c !== chainPreview) { chainPreview = c; if (c) c.t0 = performance.now(); wake(); }
  }
  function holdChain(c) {
    if (tour) endTour();
    chainHeld = c; chainPreview = null; c.t0 = performance.now();
    const R = window.GRAPH_RECIPES; const n = c.steps;
    const from = c.from && c.from.startsWith("#") ? N[+c.from.slice(1)].n : "your value", to = R.words(R.table[c.node.q].out, false).replace(/<[^>]+>/g, "");
    chainEl.innerHTML = `<div class="ch"><span class="chn">${esc(from)} <i>to</i> ${esc(to)}, <i>in ${["no", "one", "two", "three", "four"][n] || n} step${n === 1 ? "" : "s"}</i></span></div>${R.chainRail(c)}<div class="rfoot">your value is the spine; the rest rides along · ⌥ for code · esc to let go</div>`;
    chainEl.classList.add("on");
    const at = chainStops(c).map((st) => st.at); const xs = at.map((j) => X[j]), ys = at.map((j) => Y[j]);
    const x0 = Math.min(...xs), x1 = Math.max(...xs), y0 = Math.min(...ys), y1 = Math.max(...ys);
    flyTo((x0 + x1) / 2, (y0 + y1) / 2 + (y1 - y0) * 0.12, Math.max(fitW(x0, y0, x1, y1, VW < 640 ? 2.8 : 1.9), worldW() * 0.02));
  }
  // start here: a package's reading path (tour.js), flown stop to stop (T; → ← ↵ esc)
  let tour = null; // { pk, stops, at, t0 }
  const tourEl = document.createElement("div"); tourEl.className = "gtour"; view.appendChild(tourEl);
  function tourOf(pk) { const T = window.GRAPH_TOUR; if (!T || pk === undefined || pk < 0) return null; T.init(api); const t = T.of(pk); return t.stops.length >= 3 ? t : null; }
  function startTour(pk, at = 0, instant = false) {
    const t = tourOf(pk); if (!t) return false;
    dropChain(); ripple = null; if (focus >= 0) setFocus(-1, false);
    tour = { pk, stops: t.stops, at: -1, t0: performance.now() }; goStop(at, instant); return true;
  }
  function goStop(a, instant = false) {
    if (!tour) return; a = Math.max(0, Math.min(tour.stops.length - 1, a)); if (a === tour.at) return;
    tour.at = a; tour.t0 = performance.now();
    const i = tour.stops[a].i; const [x, y, w] = focusCam(i); const W = w * 2.6; // the stop, among its neighbours
    const dy = W * (VH / VW) * 0.12; // the plate covers the foot: sit the stop a little above centre
    if (instant) { Object.assign(cam, { x, y: y + dy, w: W }); Object.assign(tgt, cam); } else flyTo(x, y + dy, W);
    renderTour(); wake();
  }
  function endTour() { tour = null; tourEl.classList.remove("on"); wake(); }
  const fullName = (j) => (N[j].u >= 0 ? N[N[j].u].n + "::" : "") + N[j].n;
  function renderTour() {
    const st = tour.stops[tour.at], n = N[st.i];
    tourEl.innerHTML = `<div class="th"><b>${esc(PK[tour.pk].name)}</b><i>in ${["", "one", "two", "three", "four", "five", "six"][tour.stops.length] || tour.stops.length} stops</i><span class="tn">${tour.at + 1} of ${tour.stops.length}</span></div>`
      + `<div class="tstrip">${tour.stops.map((s, a) => `<a class="ts${a === tour.at ? " on" : a < tour.at ? " past" : ""}" data-a="${a}"><i></i><span>${esc(fullName(s.i))}</span></a>`).join('<span class="tl"></span>')}</div>`
      + `<div class="tnow"><div class="tr"><em>${esc(st.role)}</em><b>${esc(fullName(st.i))}</b><span>${esc(st.why)}</span></div>${n.d ? `<p>${esc(window.GRAPH_TOUR.plain(n.d))}</p>` : ""}</div>`
      + `<div class="ft"><span><kbd>→</kbd>next</span><span><kbd>←</kbd>back</span><span><kbd>↵</kbd>open its page</span><span><kbd>esc</kbd>end</span></div>`;
    tourEl.classList.add("on");
  }
  tourEl.addEventListener("click", (e) => { const a = e.target.closest("[data-a]"); if (a) goStop(+a.dataset.a); });
  function dropChain() { chainHeld = null; chainPreview = null; chainEl.classList.remove("on"); document.body.classList.remove("xray"); wake(); }
  function seek(q) { // every match, lit in the graph while the find box holds the query
    const t = q.trim(); if (t.length < 2) return null;
    const all = window.GRAPH_RECIPES && window.GRAPH_RECIPES.isShape(t) ? window.GRAPH_RECIPES.shape(t, 400) : find(t, 400);
    return all.length ? new Set(all) : null;
  }
  function find(q, max = 7) {
    if (window.GRAPH_RECIPES && window.GRAPH_RECIPES.isShape(q)) { if (window.GRAPH_PAGE) window.GRAPH_PAGE.init(api); return window.GRAPH_RECIPES.shape(q); }
    q = q.trim().toLowerCase(); if (!q) return [];
    const parts = q.split("::"); const last = parts[parts.length - 1];
    const out = [];
    for (let i = 0; i < NN; i++) {
      const nm = names[i]; const at = nm.indexOf(last); if (at < 0) continue;
      if (parts.length > 1 && !qual(i).toLowerCase().includes(parts.slice(0, -1).join("::"))) continue;
      if (N[i].orphan) continue;
      const score = (nm === last ? 3 : at === 0 ? 1.6 : 0.6) + IMP[i] * 1.5 + (N[i].u < 0 ? 0.4 : 0) + (yours(i) ? 0.2 : 0) - nm.length * 0.01;
      out.push([score, i]);
    }
    return out.sort((a, b) => b[0] - a[0]).slice(0, max).map((x) => x[1]);
  }
  // an empty find box teaches what it can do: a name, a shape, a shape in steps
  const HINTS = [["Value", "a name"], ["path -> maybe text", "a shape: what it takes, what it gives"], ["Invocation -> list of text", "what you have → what you need, in steps if it takes more than one call"]];
  function renderHints() {
    resEl.innerHTML = HINTS.map(([q, w]) => `<div class="r hint" data-q="${esc(q)}"><span class="n">${esc(q)}</span><span class="w">${esc(w)}</span></div>`).join("");
    resEl.classList.add("on");
  }
  function renderResults() {
    if (!qEl.value.trim() && document.activeElement === qEl) { renderHints(); return; }
    const q = qEl.value.trim().toLowerCase().split("::").pop();
    const shaped = window.GRAPH_RECIPES && window.GRAPH_RECIPES.isShape(qEl.value);
    const lit = sought && sought.size > results.length ? `<div class="r lit">${sought.size >= 400 ? "400+" : sought.size} lit in the graph, across ${whereOf(sought)} package${whereOf(sought) === 1 ? "" : "s"}</div>` : "";
    if (shaped) {
      const direct = results.map((i, j) => `<div class="r shp${j === sel ? " sel" : ""}" data-i="${i}">${kindSpan(N[i].k)}<span class="n">${N[i].u >= 0 ? `<i>${esc(N[i].u >= 0 ? N[N[i].u].n : "")}::</i>` : ""}${esc(N[i].n)}</span><span class="w gsig">${window.GRAPH_RECIPES.label(i)}</span></div>`).join("");
      const steps = chains.map((c, j) => `<div class="r chn${results.length + j === sel ? " sel" : ""}" data-c="${j}"><span class="st" title="${c.steps} steps">${"<i></i>".repeat(c.steps)}</span><span class="w gsig">${window.GRAPH_RECIPES.brief(c)}</span></div>`).join("");
      resEl.innerHTML = (direct || (steps ? "" : `<div class="r none">nothing in this world has that shape</div>`))
        + (steps ? `<div class="r hd">${direct ? "or, in steps" : "no one call does it; in steps"}</div>` + steps : "") + lit;
      resEl.classList.toggle("on", true); previewChain(); return;
    }
    resEl.innerHTML = results.map((i, j) => {
      const nm = N[i].n; const at = nm.toLowerCase().indexOf(q);
      const hl = at >= 0 ? esc(nm.slice(0, at)) + `<u>${esc(nm.slice(at, at + q.length))}</u>` + esc(nm.slice(at + q.length)) : esc(nm);
      return `<div class="r${j === sel ? " sel" : ""}" data-i="${i}">${kindSpan(N[i].k)}<span class="n">${hl}</span><span class="w">${esc(qual(i))}</span></div>`;
    }).join("") + lit;
    resEl.classList.toggle("on", results.length > 0);
  }
  const whereOf = (set) => { const ps = new Set(); for (const j of set) ps.add(N[j].p); return ps.size; };
  qEl.addEventListener("input", () => {
    results = find(qEl.value); sel = 0; sought = seek(qEl.value);
    chains = window.GRAPH_RECIPES && window.GRAPH_RECIPES.isShape(qEl.value) && results.length < 3 ? window.GRAPH_RECIPES.chains(qEl.value) : [];
    renderResults(); wake();
  });
  qEl.addEventListener("focus", () => { findEl.classList.add("on"); if (tour) endTour(); if (!qEl.value.trim()) renderHints(); });
  qEl.addEventListener("blur", () => { findEl.classList.remove("on"); setTimeout(() => resEl.classList.remove("on"), 150); if (sought) { sought = null; wake(); } if (chainPreview) { chainPreview = null; wake(); } });
  const pickRow = (j) => { qEl.blur(); resEl.classList.remove("on"); if (j >= results.length) holdChain(chains[j - results.length]); else { dropChain(); setFocus(results[j]); } };
  qEl.addEventListener("keydown", (e) => {
    const n = results.length + chains.length;
    if (e.key === "ArrowDown") { sel = Math.min(n - 1, sel + 1); renderResults(); e.preventDefault(); }
    else if (e.key === "ArrowUp") { sel = Math.max(0, sel - 1); renderResults(); e.preventDefault(); }
    else if (e.key === "Enter" && n) { pickRow(sel); e.preventDefault(); }
    else if (e.key === "Escape") { qEl.value = ""; results = []; chains = []; renderResults(); qEl.blur(); }
    e.stopPropagation();
  });
  resEl.addEventListener("mousedown", (e) => {
    const r = e.target.closest(".r"); if (!r) return;
    if (r.dataset.q !== undefined) { e.preventDefault(); qEl.value = r.dataset.q; qEl.dispatchEvent(new Event("input")); return; } // a hint fills the box
    if (r.dataset.c !== undefined) pickRow(results.length + +r.dataset.c); else if (r.dataset.i !== undefined) pickRow(results.indexOf(+r.dataset.i));
  });
  resEl.addEventListener("mousemove", (e) => { const r = e.target.closest(".r.chn"); if (r && sel !== results.length + +r.dataset.c) { sel = results.length + +r.dataset.c; renderResults(); } });
  chainEl.addEventListener("click", (e) => { const a = e.target.closest("[data-i]"); if (a) { const i = +a.dataset.i; dropChain(); setFocus(i); } });

  // ------------------------------------------------------------------ input
  cv.addEventListener("pointerdown", (e) => {
    cv.setPointerCapture(e.pointerId); inertia = null; flight = null;
    drag = { x: e.clientX, y: e.clientY, cx: cam.x, cy: cam.y, moved: 0, hist: [[performance.now(), e.clientX, e.clientY]] };
  });
  cv.addEventListener("pointermove", (e) => {
    const r = cv.getBoundingClientRect(); const px = e.clientX - r.left, py = e.clientY - r.top;
    if (drag) {
      const dx = e.clientX - drag.x, dy = e.clientY - drag.y; drag.moved = Math.max(drag.moved, Math.hypot(dx, dy));
      if (drag.moved > 3) { cv.classList.add("grab"); cam.x = drag.cx - dx * cam.w / VW; cam.y = drag.cy - dy * cam.w / VW; tgt.x = cam.x; tgt.y = cam.y; setHover(-1); wake(); }
      drag.hist.push([performance.now(), e.clientX, e.clientY]); if (drag.hist.length > 6) drag.hist.shift();
      return;
    }
    const q = prismPick(px, py);
    if (q >= 0) { setHover(layoutPrism(prism).slots[q].j, q); return; }
    const i = pick(px, py); setHover(i);
    if (i < 0) { const t = K() < 6 ? territoryAt(wx(px), wy(py)) : null; if ((t && t.p) !== (hoverTerr && hoverTerr.p) || (t && t.m) !== (hoverTerr && hoverTerr.m)) { hoverTerr = t; wake(); } }
    else if (hoverTerr) { hoverTerr = null; wake(); }
  });
  cv.addEventListener("pointerup", (e) => {
    cv.classList.remove("grab");
    if (drag && drag.moved <= 3) {
      const r = cv.getBoundingClientRect(); const q = prismPick(e.clientX - r.left, e.clientY - r.top);
      const i = q >= 0 ? layoutPrism(prism).slots[q].j : pick(e.clientX - r.left, e.clientY - r.top);
      if (i >= 0) setFocus(i);
      else if (hoverTerr && K() < 6) { const b = hoverTerr.m >= 0 && K() > 1.2 ? MB[hoverTerr.m] : PB[hoverTerr.p]; flyTo((b[0] + b[2]) / 2, (b[1] + b[3]) / 2, fitW(...b, 1.25)); }
      else if (focus >= 0) setFocus(-1, false);
    } else if (drag) {
      const h = drag.hist; const a = h[0], b = h[h.length - 1]; const dt = Math.max(1, b[0] - a[0]);
      if (performance.now() - b[0] < 60) inertia = { vx: -(b[1] - a[1]) / dt * cam.w / VW, vy: -(b[2] - a[2]) / dt * cam.w / VW };
      wake();
    }
    drag = null;
  });
  cv.addEventListener("pointerleave", () => { if (!drag) { setHover(-1); hoverTerr = null; wake(); } });
  cv.addEventListener("dblclick", (e) => { const r = cv.getBoundingClientRect(); const i = pick(e.clientX - r.left, e.clientY - r.top); if (i >= 0) openPage(i); });
  cv.addEventListener("wheel", (e) => {
    e.preventDefault(); flight = null; inertia = null;
    const r = cv.getBoundingClientRect(); const px = e.clientX - r.left, py = e.clientY - r.top;
    if (e.shiftKey) { tgt.x += e.deltaY * tgt.w / VW; tgt.y += e.deltaX * tgt.w / VW; wake(); return; }
    const f = Math.exp(e.deltaY * (e.ctrlKey ? 0.012 : 0.0022));
    const nw = Math.min(worldW() * 1.6, Math.max(2.5, tgt.w * f));
    // keep the world point under the cursor fixed, measured against the target camera
    const ox = (px - VW / 2) * tgt.w / VW, oy = (py - VH / 2) * tgt.w / VW;
    tgt.x += ox - ox * nw / tgt.w; tgt.y += oy - oy * nw / tgt.w; tgt.w = nw; wake();
  }, { passive: false });
  window.addEventListener("keydown", (e) => {
    if (e.target === qEl) return;
    if (e.key === "/" || e.code === "Slash" || (e.key === "k" && e.metaKey)) { qEl.focus(); qEl.select(); e.preventDefault(); return; }
    if ((e.key === "r" || e.key === "R") && !e.metaKey && !pageOpen && focus >= 0) { toggleReach(); e.preventDefault(); return; }
    if (e.key === "Escape" && ripple) { ripple = null; showFocus(focus); if (focus >= 0) setFocus(focus, false); wake(); return; }
    if (e.key === "Escape" && chainHeld && !pageOpen) { dropChain(); return; }
    if (tour && !pageOpen) {
      if (e.key === "ArrowRight" || e.key === " ") { goStop(tour.at + 1); e.preventDefault(); return; }
      if (e.key === "ArrowLeft") { goStop(tour.at - 1); e.preventDefault(); return; }
      if (e.key === "Escape") { endTour(); return; }
      if (e.key === "Enter") { const i = tour.stops[tour.at].i; endTour(); focus = i; showFocus(i); openPage(i); return; }
    }
    if ((e.key === "t" || e.key === "T") && !e.metaKey && !pageOpen) { const at = territoryAt(cam.x, cam.y); if (startTour(focus >= 0 ? N[focus].p : at ? at.p : -1)) { e.preventDefault(); return; } }
    if (e.key === "Escape") { if (pageOpen) closePage(); else if (prismSel >= 0) { prismSel = -1; setHover(-1); wake(); } else if (focus >= 0) { const f = focus; setFocus(-1, false); const [x, y, w] = frameOf(f); flyTo(x, y, Math.min(worldW(), w * 6)); } else flyTo(...worldCam()); return; }
    if (e.key === "Enter" && focus >= 0) {
      if (prismSel >= 0 && prism) { const sl = layoutPrism(prism).slots[prismSel]; if (sl && !sl.more) { setFocus(sl.j); return; } }
      openPage(focus); return;
    }
    const dirs = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] };
    if (dirs[e.key]) { if (focus >= 0 && prism) prismMove(...dirs[e.key]); else { tgt.x += dirs[e.key][0] * tgt.w * 0.15; tgt.y += dirs[e.key][1] * tgt.w * 0.15; wake(); } e.preventDefault(); return; }
    if (e.key === "=" || e.key === "+") { tgt.w /= 1.5; wake(); } if (e.key === "-") { tgt.w = Math.min(worldW() * 1.6, tgt.w * 1.5); wake(); }
    if (e.key === "0") flyTo(...worldCam());
  });
  const worldCam = () => [(WB[0] + WB[2]) / 2, (WB[1] + WB[3]) / 2, worldW()];

  // ------------------------------------------------------------------ page (stub until the page renderer lands)
  let pageOpen = false;
  function openPage(i) { if (window.GRAPH_PAGE) { pageOpen = true; window.GRAPH_PAGE.open(i, api); } }
  function closePage() { if (window.GRAPH_PAGE) { pageOpen = false; window.GRAPH_PAGE.close(api); } }

  // ------------------------------------------------------------------ loop
  function frame(now) {
    raf = 0; const dt = Math.min(64, now - lastT); lastT = now;
    let moving = false;
    if (flight) {
      const t = Math.min(1, (now - flight.t0) / flight.ms); const [x, y, w] = flight.plan.at(ease(t));
      cam.x = x; cam.y = y; cam.w = w; tgt.x = x; tgt.y = y; tgt.w = w;
      if (t >= 1) { const then = flight.then; flight = null; if (then) then(); } moving = true;
    } else if (inertia) {
      cam.x += inertia.vx * dt; cam.y += inertia.vy * dt; tgt.x = cam.x; tgt.y = cam.y;
      const d = Math.exp(-dt / 260); inertia.vx *= d; inertia.vy *= d;
      if (Math.hypot(inertia.vx, inertia.vy) * VW / cam.w < 0.02) inertia = null; moving = true;
    } else {
      const a = 1 - Math.exp(-dt / 70);
      const lw = Math.log(cam.w), lt = Math.log(tgt.w);
      if (Math.abs(lt - lw) > 1e-4 || Math.abs(tgt.x - cam.x) * VW / cam.w > 0.05 || Math.abs(tgt.y - cam.y) * VW / cam.w > 0.05) {
        cam.w = Math.exp(lw + (lt - lw) * a); cam.x += (tgt.x - cam.x) * a; cam.y += (tgt.y - cam.y) * a; moving = true;
      }
    }
    if (prism) { const a = 1 - Math.exp(-dt / 120); prism.g += (prism.target - prism.g) * a; if (Math.abs(prism.target - prism.g) < 0.002) prism.g = prism.target; else moving = true; if (prism.target === 0 && prism.g === 0) prism = null; }
    if (ripple && now - ripple.t0 < ripple.waves.length * WAVE_MS + 340) moving = true;
    { const c = chainOn(); if (c && now - c.t0 < 380 + 260 * c.steps + 120) moving = true; }
    const hi = focus >= 0 ? focus : hover;
    if (hover >= 0 && hoverA < 1) { hoverA = Math.min(1, hoverA + dt / 140); moving = true; }
    if (hi >= 0) { flow = (flow + dt * 0.018) % 9; moving = true; }
    if (moving || dirty) draw(now);
    if (moving) raf = requestAnimationFrame(frame);
  }

  // ------------------------------------------------------------------ boot
  const api = { peekHTML, KFAM, visit, enterGraph, relationsOf, caps, capsLine, nameOf, inEdges, outEdges, openPage, closePage, focusCam, buildPrism, get prism() { return prism; }, set prism(v) { prism = v; }, planFlight, worldCam, get pageOpen() { return pageOpen; }, set pageOpen(v) { pageOpen = v; }, draw: () => draw(performance.now()), N, X, Y, R, PK, MD, kids, nb, qual, sigOf, axes, yours, reached, kindSpan, esc, IMP, B, isItem, topOf, yoursUses, setFocus, flyTo, frameOf, sx, sy, K, cam, view, get focus() { return focus; }, wake, OUT, IN, IOUT, IIN };
  window.addEventListener("keydown", (e) => { if (e.key === "Alt" && chainHeld) document.body.classList.add("xray"); });
  window.addEventListener("keyup", (e) => { if (e.key === "Alt" && !pageOpen) document.body.classList.remove("xray"); });
  window.GRAPH = api; if (window.GRAPH_PAGE) window.GRAPH_PAGE.init(api);
  const byQuery = (q) => { if (!q) return -1; if (/^\d+$/.test(q)) return +q; const r = find(q); return r.length ? r[0] : -1; };
  new ResizeObserver(() => { resize(); if (STILL) draw(performance.now()); else wake(); }).observe(view);
  resize();
  document.fonts.ready.then(() => Promise.all(['620 16px "Bricolage Grotesque"', '500 12px "Geist Mono"', '600 12px "Geist Mono"', '400 11px "Geist"'].map((f) => document.fonts.load(f)))).then(() => {
    widthCache.clear();
    Object.assign(cam, (([x, y, w]) => ({ x, y, w }))(worldCam())); Object.assign(tgt, cam);
    if (Q.has("frame")) { const fr = Q.get("frame"); const kind = fr.slice(0, fr.indexOf(":")), name = fr.slice(fr.indexOf(":") + 1); let b = null;
      if (kind === "pkg") { const p = PK.findIndex((q) => q.name === name); if (p >= 0) b = PB[p]; }
      else { const m = MD.findIndex((q) => PK[q.pkg].name + "::" + q.path === name); if (m >= 0) b = MB[m]; }
      if (b) { Object.assign(cam, { x: (b[0] + b[2]) / 2, y: (b[1] + b[3]) / 2, w: fitW(...b, 1.2) }); Object.assign(tgt, cam); } }
    if (Q.has("cam")) { const [x, y, w] = Q.get("cam").split(",").map(Number); Object.assign(cam, { x, y, w }); Object.assign(tgt, cam); }
    const f = byQuery(Q.get("focus")), h = byQuery(Q.get("hover"));
    if (Q.has("fly")) { // a frame of a flight between two symbols (or "world")
      const [a, b] = Q.get("fly").split(","); const t = +(Q.get("t") || 0.5);
      const pa = a === "world" ? worldCam() : focusCam(byQuery(a)), pb = b === "world" ? worldCam() : focusCam(byQuery(b));
      const plan = planFlight(pa, pb); const [x, y, w] = plan.at(ease(t)); Object.assign(cam, { x, y, w }); Object.assign(tgt, cam);
      if (b !== "world" && t >= 1) { focus = byQuery(b); showFocus(focus); prism = buildPrism(focus); prism.g = 1; }
      hudEl.textContent = Q.has("hud") ? "" : ""; draw(performance.now()); return;
    }
    if (f >= 0) { if (STILL || Q.has("cam")) { focus = f; showFocus(f); prism = buildPrism(f); prism.g = +(Q.get("g") || 1); if (!Q.has("cam")) { const [x, y, w] = focusCam(f); Object.assign(cam, { x, y, w }); Object.assign(tgt, cam); } if (Q.has("sel")) { prismSel = +Q.get("sel"); } } else setTimeout(() => setFocus(f), 500); }
    if (h >= 0) setHover(h);
    if (Q.has("reach") && focus >= 0) { ripple = reach(focus); ripple.t0 = -1e9; prism = null; showFocus(focus); if (!Q.has("cam")) { const pts = ripple.all.map((t) => [X[t], Y[t]]).concat([[X[focus], Y[focus]]]); const x0 = Math.min(...pts.map((p) => p[0])), x1 = Math.max(...pts.map((p) => p[0])), y0 = Math.min(...pts.map((p) => p[1])), y1 = Math.max(...pts.map((p) => p[1])); Object.assign(cam, { x: (x0 + x1) / 2, y: (y0 + y1) / 2, w: Math.min(worldW() * 1.1, fitW(x0, y0, x1, y1, 1.3)) }); Object.assign(tgt, cam); } }
    if (Q.has("hints")) { findEl.classList.add("on"); renderHints(); }
    if (Q.has("find")) {
      qEl.value = Q.get("find"); findEl.classList.add("on"); results = find(qEl.value); sought = seek(qEl.value);
      chains = window.GRAPH_RECIPES.isShape(qEl.value) && results.length < 3 ? window.GRAPH_RECIPES.chains(qEl.value) : [];
      if (Q.has("sel")) sel = +Q.get("sel");
      if (Q.has("chain") && chains[+Q.get("chain")]) { const c = chains[+Q.get("chain")]; sought = null; findEl.classList.remove("on"); holdChain(c); if (flight) { const [x, y, w] = flight.plan.at(1); Object.assign(cam, { x, y, w }); Object.assign(tgt, cam); flight = null; } if (Q.has("xray")) document.body.classList.add("xray"); }
      else renderResults();
    }
    if (Q.has("tour")) { const p = PK.findIndex((q) => q.name === Q.get("tour")); if (p >= 0) startTour(p, +(Q.get("stop") || 1) - 1, true); }
    if (Q.has("page") && window.GRAPH_PAGE) { const pi = byQuery(Q.get("page")); if (pi >= 0) { focus = pi; showFocus(pi); const [x, y, w] = focusCam(pi); Object.assign(cam, { x, y, w }); Object.assign(tgt, cam); window.GRAPH_PAGE.open(pi, api, { instant: true, view: Q.get("view") || "page" }); } }
    draw(performance.now()); wake();
  });
})();
