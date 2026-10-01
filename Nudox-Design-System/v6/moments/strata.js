// The Library as strata: your crates on top, what you rest on beneath them, and everything those bring
// underneath, grouped by what brought it. Each package is its own territory in miniature (a block of
// shingles, mint where your code names it).
//   hover   strands down to what it rests on (solid) and up to who uses it (dotted); for anything
//           beneath, the one chain up to your code draws as a core through the layers.
//   loupe   over the dense lower band a lens magnifies what is under the pointer, names and all.
//   type    lights every matching item across the whole library.
//   add     relayout() moves every block to its new place on a spring, so a band visibly makes room.
import { C, canvas, block, cells, frame, text, measure } from "./paint.js";
import { Chase, every, ROOM, lerp, clamp } from "./motion.js";
import { above, why, fmt } from "./world.js";
import { verdict } from "./license.js";

const PAD = 40, TOP = 232;
export const BANDS = {
  yours: { pitch: 3, stone: 2.4, rows: 4, cap: 60, label: true, gapX: 12, gapY: 6, small: true },
  direct: { pitch: 5, stone: 4.2, rows: 5, cap: 150, label: true, gapX: 16, gapY: 12 },
  beneath: { pitch: 3, stone: 2.4, rows: 4, cap: 64, label: false, gapX: 5, gapY: 5 },
};
export const shown = (p) => (p.kind === "yours" ? p.name.replace(/^backend-/, "") : p.name);
const LABEL = 15, HEAD = 40, LENS = { w: 300, h: 132, k: 2.6 };
const FONT = { small: "400 10.5px 'Geist Mono'", label: "500 11px 'Geist Mono'", head: "600 13px Geist", n: "400 12px 'Geist Mono'", via: "500 10.5px 'Geist Mono'" };

let api_over = null;
export function strata(host, WD, { onOpen, onHover, onScroll, state: init = {} }) {
  const el = document.createElement("canvas");
  el.className = "strata";
  host.appendChild(el);
  const W = host.clientWidth, H = host.clientHeight;
  const ctx = canvas(el, W, H);
  const colW = W - 2 * PAD;
  const S = { hover: null, q: "", lens: "contents", scroll: 0, ...init };
  const dim = new Chase(0, 16), lift = new Chase(0, 22), lensA = new Chase(0, 20);
  let blocks = new Map(); // pkg → { p, b, x, y, w, h, band, top }
  let heads = [], clusters = [], contentH = 0;
  const hidden = new Set(), develop = new Map(), pulses = [];
  let moving = null; // { from: Map(pkg → {x,y}), t0 }
  const mctx = document.createElement("canvas").getContext("2d");
  const blockCache = new Map();
  const blockOf = (p, band) => { const k = p.id + band; return blockCache.get(k) || blockCache.set(k, block(p, BANDS[band])).get(k); };

  // ---------------------------------------------------------------- layout (pure: WD → positions)
  const prevN = new Map();
  function layoutAll() {
    for (const h of heads) prevN.set(h.title, h.n);
    blocks = new Map(); heads = []; clusters = [];
    function flow(pkgs, band, x0, y0, width) {
      const P = BANDS[band];
      let x = 0, y = 0, lineH = 0;
      const placed = [];
      for (const p of pkgs) {
        const b = blockOf(p, band);
        const lw = P.label ? measure(mctx, shown(p), P.small ? FONT.small : FONT.label) + 2 + (p.fresh ? 66 : 0) : 0;
        const w = Math.max(b.w, lw), h = (P.label ? LABEL : 0) + b.h;
        if (x > 0 && x + w > width) { x = 0; y += lineH + P.gapY; lineH = 0; }
        placed.push({ p, b, x: x0 + x, y: y0 + y, w, h, band, top: P.label ? LABEL : 0 });
        x += w + P.gapX; lineH = Math.max(lineH, h);
      }
      return { placed, w: y === 0 ? Math.max(0, x - P.gapX) : width, h: y + lineH };
    }
    let y = TOP;
    const band = (title, n, note, pkgs, key) => {
      heads.push({ title, n, note, y, band: key }); y += HEAD;
      const f = flow(pkgs, key, PAD, y, colW);
      for (const q of f.placed) blocks.set(q.p, q);
      y += f.h + 44;
    };
    const unnamed = WD.direct.filter((p) => !p.sites).length;
    band("Yours", WD.yours.length, "your workspace's crates", WD.yours, "yours");
    band("You rest on", WD.direct.length, unnamed ? `most-used first · ${unnamed} are never named in your code` : "most-used first", WD.direct, "direct");
    // Beneath: true strata, one layer per step further from your code. Each layer is ordered under
    // its parents (the mean x of what it holds up), so strands hang short, like roots.
    const deep = Math.max(...WD.beneath.map((p) => p.depth));
    heads.push({ title: "Beneath", n: WD.beneath.length, note: "one layer per step further from your code · the lens reads them", y, band: "beneath" });
    y += HEAD - 8;
    for (let d = 2; d <= deep; d++) {
      const layer = WD.beneath.filter((p) => p.depth === d);
      if (!layer.length) continue;
      const bary = (p) => { let s2 = 0, n = 0; for (const u of p.dependentsP) { const q = blocks.get(u); if (q) { s2 += q.x + q.b.w / 2; n++; } } return n ? s2 / n : 1e9; };
      layer.sort((a, b) => bary(a) - bary(b) || b.items - a.items);
      const label = `${d} steps down · ${layer.length}`;
      const f = flow(layer, "beneath", PAD, y + 16, colW);
      for (const q of f.placed) blocks.set(q.p, q);
      clusters.push({ g: { pkgs: layer }, label, x: PAD, y, w: colW, h: f.h + 16, depth: d });
      y += f.h + 16 + 18;
    }
    y += 60;
    contentH = y;
  }
  layoutAll();

  // ---------------------------------------------------------------- tints
  let hits = new Map(), hitCount = 0;
  function search(q) {
    hits = new Map(); hitCount = 0;
    if (!q) return;
    const needle = q.toLowerCase();
    for (const [p, q2] of blocks) for (const c of q2.b.cells) if (c.it.n.toLowerCase().includes(needle)) {
      (hits.get(p) || hits.set(p, new Set()).get(p)).add(c); hitCount++;
    }
  }
  // Overlays: the whole library coloured by one question.
  const overCache = new Map();
  function over(p) {
    const k = S.lens + p.id; if (overCache.has(k)) return overCache.get(k);
    let v = null;
    const T = (WD.trust || {})[p.id];
    if (S.lens === "licence") { const t = verdict(p.license).tone; v = t === "mint" ? null : t; }
    if (S.lens === "heads" && T) v = T.build_rs || T.proc_macro ? "build" : (T.caps && (T.caps.net_n || T.caps.process_n)) ? "reach" : null;
    overCache.set(k, v); return v;
  }
  api_over = over;
  const tint = (p, focus = false) => (c) => {
    if (S.q) {
      const h = hits.get(p);
      if (h && h.has(c)) return [C["peri-hi"], 1];
      return [C.fam[c.it.f] || C.ink2, h ? 0.16 : 0.07];
    }
    if (S.lens === "versions") {
      if (!p.newer.length) return [C.fam[c.it.f] || C.ink2, 0.1];
      return p.used.has(c.it.n) ? [C.amber, 1] : [C.fam[c.it.f] || C.ink2, 0.42];
    }
    if (S.lens === "licence") {
      const t = over(p);
      return t === "coral" ? [C.coral, 0.95] : t === "amber" ? [C.amber, 0.9] : [C.fam[c.it.f] || C.ink2, 0.1];
    }
    if (S.lens === "heads") {
      const t = over(p);
      return t === "build" ? [C.amber, 0.9] : t === "reach" ? [C["peri-hi"], 0.85] : [C.fam[c.it.f] || C.ink2, 0.08];
    }
    if (p.used.has(c.it.n)) return [C.mint, 1];
    const base = p.depth === 0 ? 0.5 : p.depth === 1 ? (p.sites ? 0.38 : 0.2) : Math.max(0.16, 0.34 - (p.depth - 2) * 0.035);
    return [C.fam[c.it.f] || C.ink2, focus ? Math.min(1, base + 0.28) : base];
  };
  function paintBlock(g, q, focus = false, at = null) {
    if (hidden.has(q.p)) return;
    const { p, b, band } = q;
    const x = at ? at.x : q.x, by = at ? at.y : q.y;
    const dv = develop.get(p);
    if (dv != null) {
      // Unread: a dashed frame the size it will be, filling as the index reads it.
      g.save(); g.setLineDash([2, 2]); g.strokeStyle = dv >= 1 ? C.line3 : C.peri; g.globalAlpha = dv >= 1 ? 0 : 0.8; g.lineWidth = 1;
      frame(g, x - 2.5, by + q.top - 2.5, b.w + 5, b.h + 5, 2); g.stroke(); g.restore();
      const n = Math.floor(b.cells.length * clamp(dv));
      cells(g, b.cells.slice(0, n), x, by + q.top, b.stone, tint(p, focus));
    } else cells(g, b.cells, x, by + q.top, b.stone, tint(p, focus));
    if (band !== "beneath") {
      const named = S.q && p.name.toLowerCase().includes(S.q.toLowerCase());
      const col = named ? C["peri-hi"] : focus ? C.ink0 : S.lens === "versions" && p.newer.length ? C.amber : p.depth === 1 && !p.sites ? C.ink4 : C.ink2;
      text(g, shown(p), x, by + 10, { font: q.band === "yours" ? FONT.small : FONT.label, color: dv != null && dv < 1 ? C.peri : p.fresh ? C["peri-hi"] : col });
      if (p.fresh) text(g, "just added", x + q.w, by + 10, { font: "400 10px Geist", color: C.peri, align: "right" });
      if (b.more && q.band === "direct") { const lw = measure(g, shown(p), FONT.label); if (q.w - lw > 40 && !(S.lens === "versions" && p.newer.length)) text(g, "+" + fmt(b.more), x + q.w, by + 10, { font: "400 10px 'Geist Mono'", color: C.ink4, align: "right" }); }
      if (S.lens === "versions" && p.newer.length) {
        const lw = measure(g, shown(p), FONT.label);
        if (q.w - lw > 44) text(g, "→ " + p.latest, x + q.w, by + 10, { font: "400 10.5px 'Geist Mono'", color: C.amber, align: "right" });
      }
    } else if (S.lens === "versions" && p.newer.length) {
      g.globalAlpha = 0.7; g.strokeStyle = C.amber; g.lineWidth = 1; frame(g, x - 1.5, by + q.top - 1.5, b.w + 3, b.h + 3, 2); g.stroke(); g.globalAlpha = 1;
    }
  }
  function paintChrome(g) {
    for (const h of heads) {
      text(g, h.title, PAD, h.y + 16, { font: FONT.head, color: C.ink1 });
      const tw = measure(g, h.title, FONT.head);
      const from = prevN.has(h.title) ? prevN.get(h.title) : h.n;
      const roll = moving && from !== h.n ? clamp((performance.now() - moving.t0 - (h.tickAt || 0)) / 360) : 1;
      const shownN = Math.round(from + (h.n - from) * roll);
      text(g, fmt(shownN), PAD + tw + 10, h.y + 16, { font: FONT.n, color: from !== h.n && roll < 1 ? C["peri-hi"] : C.ink3 });
      const nw = measure(g, fmt(h.n), FONT.n);
      text(g, h.note, PAD + tw + nw + 22, h.y + 16, { font: "400 12px Geist", color: C.ink3 });
    }
    for (const c of clusters) {
      const has = S.q && c.g.pkgs.some((p) => hits.has(p));
      const fresh = c.g.fresh;
      text(g, c.label, c.x, c.y + 9, { font: FONT.via, color: has || fresh ? C["peri-hi"] : C.ink3 });
      g.fillStyle = fresh ? C.peri : C.line2; g.fillRect(c.x, c.y + 12.5, Math.max(12, c.w - 4), 1);
    }
  }

  // ---------------------------------------------------------------- the rest layer
  const rest = document.createElement("canvas");
  let rctx = null;
  function paintRest() {
    rctx = canvas(rest, W, contentH);
    rctx.clearRect(0, 0, W, contentH);
    paintChrome(rctx);
    for (const [, q] of blocks) paintBlock(rctx, q);
  }

  // ---------------------------------------------------------------- focus, strands, the core, the lens
  let focus = { p: null, deps: new Set(), users: new Set(), chain: new Set(), core: [], excl: new Set() };
  let pointer = null;
  function setFocus(p) {
    if (focus.p === p) return;
    const g = p && p.depth === 1 ? WD.groups.find((x) => x.via === p) : null;
    focus = p ? { p, deps: new Set(p.depsP), users: new Set(p.dependentsP), chain: p.depth >= 2 ? new Set(above(p)) : new Set(), core: p.depth >= 1 ? why(p) : [], excl: new Set(g ? g.pkgs : []) }
      : { p: null, deps: new Set(), users: new Set(), chain: new Set(), core: [], excl: new Set() };
    dim.to = p ? 1 : 0; lift.v = 0; lift.to = p ? 1 : 0;
    onHover && onHover(p, p ? rectOf(p) : null);
    kick();
  }
  const posOf = (q) => (moving && moving.cur.get(q.p)) || q;
  const center = (q, side) => { const o = posOf(q); return { x: o.x + Math.min(q.w, q.b.w) / 2, y: side === "top" ? o.y + q.top - 2 : o.y + q.h + 2 }; };
  function strands(g) {
    const q = blocks.get(focus.p); if (!q) return;
    g.lineWidth = 1.2;
    const draw = (to, down) => {
      const r = blocks.get(to); if (!r || hidden.has(to)) return;
      const a = center(q, down ? "bottom" : "top"), b = center(r, down ? "top" : "bottom");
      const k = Math.abs(a.y - b.y) < 30 ? 26 : Math.max(24, Math.abs(b.y - a.y) * 0.45);
      g.beginPath(); g.moveTo(a.x, a.y);
      g.bezierCurveTo(a.x, a.y + (down ? k : -k), b.x, b.y + (down ? -k : k), b.x, b.y);
      g.stroke();
    };
    g.strokeStyle = C.peri; g.globalAlpha = (focus.deps.size > 40 ? 0.25 : 0.6) * dim.v;
    for (const d of focus.deps) draw(d, true);
    g.globalAlpha = (focus.users.size > 40 ? 0.25 : 0.55) * dim.v; g.setLineDash([2, 4]);
    for (const d of focus.users) draw(d, false);
    g.setLineDash([]); g.globalAlpha = 1;
  }
  function core(g) {
    // The one chain from your code down to it, drawn as a single bright line through the layers.
    const chain = focus.core.map((p) => blocks.get(p)).filter(Boolean);
    if (chain.length < 2) return;
    g.strokeStyle = C["peri-hi"]; g.lineWidth = 1.6; g.globalAlpha = dim.v;
    g.beginPath();
    for (let i = 1; i < chain.length; i++) {
      const a = center(chain[i - 1], "bottom"), b = center(chain[i], "top");
      const k = Math.max(18, Math.abs(b.y - a.y) * 0.45);
      g.moveTo(a.x, a.y); g.bezierCurveTo(a.x, a.y + k, b.x, b.y - k, b.x, b.y);
    }
    g.stroke();
    for (const q of chain) { const t = center(q, "top"); g.fillStyle = C["peri-hi"]; g.beginPath(); g.arc(t.x, t.y, 2, 0, 7); g.fill(); }
    g.globalAlpha = 1;
  }
  function lens(g) {
    if (!pointer || lensA.v < 0.02) return;
    const { x: px, y: py } = pointer; // content coordinates
    const w = LENS.w, h = LENS.h, k = LENS.k;
    const lx = clamp(px - w / 2, 4, W - w - 4), ly = py + 22;
    g.save(); g.globalAlpha = lensA.v;
    g.fillStyle = C.g2; frame(g, lx, ly, w, h, 8); g.fill();
    g.save(); frame(g, lx, ly, w, h, 8); g.clip();
    g.translate(lx + w / 2, ly + h / 2); g.scale(k, k); g.translate(-px, -py);
    const R = { x0: px - w / 2 / k, x1: px + w / 2 / k, y0: py - h / 2 / k, y1: py + h / 2 / k };
    const inside = [];
    for (const [, q] of blocks) {
      if (q.band !== "beneath") continue;
      const o = posOf(q);
      if (o.x + q.b.w < R.x0 || o.x > R.x1 || o.y + q.h < R.y0 || o.y > R.y1) continue;
      paintBlock(g, q, q.p === focus.p, o); inside.push(q);
    }
    for (const c of clusters) if (c.x < R.x1 && c.x + c.w > R.x0 && c.y + 12 > R.y0 && c.y < R.y1) { g.fillStyle = C.line3; g.fillRect(c.x, c.y + 12.5, c.w - 4, 1 / k); }
    g.restore();
    // Names at reading size, in the lens's space.
    g.font = "500 10.5px 'Geist Mono'"; g.textBaseline = "alphabetic";
    const placed = [];
    for (const q of inside) {
      const o = posOf(q);
      const sx = lx + w / 2 + (o.x - px) * k, sy = ly + h / 2 + (o.y + q.top - py) * k - 3;
      const tw = g.measureText(q.p.name).width;
      if (sx < lx + 4 || sx + tw > lx + w - 4 || sy < ly + 12 || sy > ly + h - 2) continue;
      if (placed.some((r) => sx < r.x1 + 6 && sx + tw > r.x0 && Math.abs(sy - r.y) < 12)) continue;
      placed.push({ x0: sx, x1: sx + tw, y: sy });
      g.fillStyle = q.p === focus.p ? C.ink0 : C.ink2; g.fillText(q.p.name, sx, sy);
    }
    for (const c of clusters) {
      const sx = lx + w / 2 + (c.x - px) * k, sy = ly + h / 2 + (c.y + 9 - py) * k;
      if (sx > lx + 4 && sx < lx + w - 60 && sy > ly + 12 && sy < ly + h - 2) { g.fillStyle = C.ink3; g.fillText(c.label, sx, sy); }
    }
    g.strokeStyle = C.line3; g.lineWidth = 1; frame(g, lx + 0.5, ly + 0.5, w - 1, h - 1, 8); g.stroke();
    // A hairline from the lens to the point it reads.
    g.strokeStyle = C.line3; g.beginPath(); g.moveTo(px, ly); g.lineTo(px, py + 6); g.stroke();
    g.restore();
  }
  function pulse(g, now) {
    for (let i = pulses.length - 1; i >= 0; i--) {
      const u = (now - pulses[i].t0) / 700;
      if (u >= 1) { pulses.splice(i, 1); continue; }
      const q = blocks.get(pulses[i].p); if (!q) continue;
      const o = posOf(q), r = 3 + u * 10;
      g.strokeStyle = pulses[i].col; g.globalAlpha = 1 - u; g.lineWidth = 1.5;
      frame(g, o.x - r, o.y + q.top - r, q.b.w + 2 * r, q.b.h + 2 * r, 3); g.stroke(); g.globalAlpha = 1;
    }
  }

  // ---------------------------------------------------------------- the frame
  function draw(now = performance.now()) {
    onScroll && onScroll(S.scroll);
    ctx.clearRect(0, 0, W, H);
    ctx.save(); ctx.translate(0, -S.scroll);
    if (moving || develop.size) {
      // Live: every block at its current place (a band making room, a block developing).
      ctx.globalAlpha = 1 - 0.8 * dim.v;
      paintChrome(ctx);
      for (const [, q] of blocks) paintBlock(ctx, q, false, posOf(q));
      ctx.globalAlpha = 1;
    } else {
      ctx.globalAlpha = 1 - 0.8 * dim.v;
      ctx.drawImage(rest, 0, 0, W, contentH);
      ctx.globalAlpha = 1;
    }
    if (focus.p && dim.v > 0.02) {
      const on = (set, col, a) => { for (const p of set) { const q = blocks.get(p); if (!q) continue; ctx.globalAlpha = dim.v * a; paintBlock(ctx, q, false, posOf(q)); if (col) { const o = posOf(q); ctx.strokeStyle = col; ctx.lineWidth = 1; frame(ctx, o.x - 2.5, o.y + q.top - 2.5, q.b.w + 5, q.b.h + 5, 2); ctx.stroke(); } ctx.globalAlpha = 1; } };
      on(focus.chain, null, 0.4);
      // What only it keeps alive: remove it and these go with it (Path of Building's red set, in coral cut frames).
      for (const e of focus.excl) { const q = blocks.get(e); if (!q) continue; const o = posOf(q); ctx.globalAlpha = dim.v; paintBlock(ctx, q, false, o); ctx.setLineDash([2, 2]); ctx.strokeStyle = C.coral; ctx.lineWidth = 1; frame(ctx, o.x - 2, o.y + q.top - 2, q.b.w + 4, q.b.h + 4, 2); ctx.stroke(); ctx.setLineDash([]); ctx.globalAlpha = 1; }
      on(focus.users, C.line3, 1);
      on(focus.deps, C.line3, 1);
      strands(ctx); core(ctx);
      const q = blocks.get(focus.p);
      if (q) {
        const o = posOf(q);
        ctx.save(); ctx.translate(0, -2 * lift.v);
        ctx.fillStyle = C.g1; ctx.globalAlpha = 0.9 * lift.v; frame(ctx, o.x - 5, o.y + q.top - 5, q.b.w + 10, q.b.h + 10, 3); ctx.fill(); ctx.globalAlpha = 1;
        paintBlock(ctx, q, true, o);
        ctx.strokeStyle = C["peri-hi"]; ctx.globalAlpha = lift.v; ctx.lineWidth = 1.2; frame(ctx, o.x - 5, o.y + q.top - 5, q.b.w + 10, q.b.h + 10, 3); ctx.stroke();
        ctx.restore(); ctx.globalAlpha = 1;
      }
    }
    pulse(ctx, now);
    lens(ctx);
    ctx.restore();
  }
  let last = 0, alive = false;
  function kick() {
    if (alive) return; alive = true; last = performance.now();
    every((now) => {
      const dt = Math.min(0.05, (now - last) / 1000); last = now;
      let m = dim.step(dt) | lift.step(dt) | lensA.step(dt);
      if (moving) {
        const ms = now - moving.t0;
        for (const [p, q] of blocks) {
          const f = moving.from.get(p) || q;
          const u = ROOM(ms - (moving.delay.get(p) || 0));
          moving.cur.set(p, { x: lerp(f.x, q.x, u), y: lerp(f.y, q.y, u) });
        }
        if (ms > Math.max(900, (heads[0] && heads[0].tickAt || 0) + 420)) { moving = null; for (const h of heads) prevN.set(h.title, h.n); paintRest(); } else m = true;
      }
      if (pulses.length || develop.size) m = true;
      draw(now);
      if (!m) alive = false;
      return m;
    });
  }

  // ---------------------------------------------------------------- input
  function at(mx, my) {
    const cy = my + S.scroll;
    for (const [p, q] of blocks) { if (hidden.has(p)) continue; const o = posOf(q); if (mx >= o.x - 3 && mx <= o.x + Math.max(q.b.w, q.band === "beneath" ? 0 : q.w) + 3 && cy >= o.y - 2 && cy <= o.y + q.h + 3) return p; }
    return null;
  }
  function inBeneath(cy) { const h = heads.find((x) => x.band === "beneath"); return h && cy > h.y + HEAD - 8; }
  function rectOf(p) {
    const q = blocks.get(p), r = el.getBoundingClientRect(), o = posOf(q);
    return { x: r.left + o.x, y: r.top + o.y + q.top - S.scroll, w: q.b.w, h: q.b.h, top: r.top + o.y - S.scroll };
  }
  el.addEventListener("mousemove", (e) => {
    const r = el.getBoundingClientRect(); const mx = e.clientX - r.left, my = e.clientY - r.top;
    const cy = my + S.scroll;
    pointer = { x: mx, y: cy };
    lensA.to = inBeneath(cy) ? 1 : 0;
    setFocus(at(mx, my)); kick();
    el.style.cursor = focus.p ? "pointer" : "";
  });
  el.addEventListener("mouseleave", () => { pointer = null; lensA.to = 0; setFocus(null); kick(); });
  el.addEventListener("click", () => { if (focus.p && onOpen) onOpen(focus.p, api); });
  el.addEventListener("wheel", (e) => { e.preventDefault(); S.scroll = Math.max(0, Math.min(contentH - H, S.scroll + e.deltaY)); if (pointer) pointer.y += e.deltaY; draw(); }, { passive: false });

  const api = {
    el, S, get blocks() { return blocks; }, rectOf, get contentH() { return contentH; },
    hover(p) { setFocus(p); },
    set(patch) { Object.assign(S, patch); if ("q" in patch) search(S.q); paintRest(); draw(); },
    hits: () => ({ items: hitCount, pkgs: hits.size }),
    count(lens) { const was = S.lens; S.lens = lens; const n = new Map(); for (const [p] of blocks) { const v = api_over(p); if (v) n.set(v, (n.get(v) || 0) + 1); } S.lens = was; return n; },
    reveal(p, where = 1 / 3) { const q = blocks.get(p); if (!q) return; if (q.y < S.scroll + 20 || q.y > S.scroll + H - 60) { S.scroll = Math.max(0, Math.min(contentH - H, q.y - H * where)); draw(); } },
    scrollTo(y) { S.scroll = Math.max(0, Math.min(contentH - H, y)); draw(); },
    cellsOf(p) { const q = blocks.get(p), r = el.getBoundingClientRect(), o = posOf(q); return q.b.cells.map((c) => ({ c, x: r.left + o.x + c.x, y: r.top + o.y + q.top + c.y - S.scroll, s: q.b.stone, tint: tint(p)(c) })); },
    // Relayout after WD changed: every block glides from where it was to where it now goes.
    relayout({ hide = [], stagger = null, tick = 0 } = {}) {
      const from = new Map([...blocks].map(([p, q]) => [p, { x: (moving && moving.cur.get(p) || q).x, y: (moving && moving.cur.get(p) || q).y }]));
      layoutAll(); search(S.q);
      for (const h of heads) h.tickAt = tick;
      for (const p of hide) hidden.add(p);
      const delay = new Map();
      if (stagger) for (const [p, q] of blocks) delay.set(p, stagger(q, from.get(p)));
      moving = { from, cur: new Map(), t0: performance.now(), delay };
      paintRest(); kick();
    },
    show(p) { hidden.delete(p); kick(); },
    develop(p, v) { if (v == null) develop.delete(p); else develop.set(p, v); kick(); },
    pulse(p, col = C.mint) { pulses.push({ p, t0: performance.now(), col }); kick(); },
    draw, paintRest, kick,
    fade(a) { el.style.opacity = a; },
    blockOf(p) { return blocks.get(p); },
  };
  search(S.q); paintRest(); draw();
  if (init.at) { pointer = init.at; lensA.v = lensA.to = inBeneath(init.at.y) ? 1 : 0; setFocus(at(init.at.x, init.at.y - S.scroll)); dim.v = dim.to; lift.v = lift.to; draw(); }
  return api;
}
