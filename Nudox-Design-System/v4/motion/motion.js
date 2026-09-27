/* FACET v4 — motion.js: the engine, the pieces the stage is built from, and the board.
 *
 * Every value on the stage is a pure function of the demo's time t (ms): curves are the same
 * cubic-beziers as facet::motion::curve, springs are the same closed form as facet::motion::spring,
 * retargets keep value (tweens) or value and velocity (springs) exactly as facet::motion::store does.
 * ?demo=<id>&t=<ms> renders one exact still; add &reduced=1 for the reduced-motion version and
 * &int=1 for the interrupted version where a demo has one. Demos are registered by demos.js.
 */
(() => {
  "use strict";

  // ------------------------------------------------------------------ curves (facet::motion::curve)
  function bezier(x1, y1, x2, y2) {
    const cx = 3 * x1, bx = 3 * (x2 - x1) - cx, ax = 1 - cx - bx;
    const cy = 3 * y1, by = 3 * (y2 - y1) - cy, ay = 1 - cy - by;
    const sx = (t) => ((ax * t + bx) * t + cx) * t, sy = (t) => ((ay * t + by) * t + cy) * t;
    const dx = (t) => (3 * ax * t + 2 * bx) * t + cx;
    const solve = (x) => {
      let t = x;
      for (let i = 0; i < 8; i++) { const e = sx(t) - x; if (Math.abs(e) < 1e-7) return t; const d = dx(t); if (Math.abs(d) < 1e-6) break; t -= e / d; }
      let lo = 0, hi = 1; t = x;
      for (let i = 0; i < 60 && hi - lo > 1e-7; i++) { const v = sx(t); if (Math.abs(v - x) < 1e-7) return t; if (x > v) lo = t; else hi = t; t = (lo + hi) / 2; }
      return t;
    };
    const f = (x) => (x <= 0 ? 0 : x >= 1 ? 1 : sy(solve(x)));
    f.css = `cubic-bezier(${x1},${y1},${x2},${y2})`;
    return f;
  }
  const C = {
    glide: bezier(0.22, 1, 0.36, 1), // decelerate: arriving, settling (tokens::motion::GLIDE)
    snap: bezier(0.3, 0, 0, 1), // crisp (SNAP)
    spring: bezier(0.2, 0.9, 0.25, 1.18), // light overshoot (SPRING)
    bounce: bezier(0.34, 1.56, 0.64, 1), // play only (BOUNCE)
    drop: bezier(0.5, 0, 0.9, 0.6), // accelerate: leaving (DROP)
    linear: (x) => (x <= 0 ? 0 : x >= 1 ? 1 : x),
  };
  C.linear.css = "linear";

  // ------------------------------------------------------------------ springs (facet::motion::spring)
  function sstep(sp, x0, v0, dt) {
    if (dt <= 0) return [x0, v0];
    const w = (2 * Math.PI) / Math.max(1e-3, sp.response), z = Math.max(0, sp.damping);
    if (Math.abs(z - 1) < 1e-4) { const b = v0 + w * x0, e = Math.exp(-w * dt); return [(x0 + b * dt) * e, (b - w * (x0 + b * dt)) * e]; }
    if (z < 1) {
      const wd = w * Math.sqrt(1 - z * z), e = Math.exp(-z * w * dt), b = (v0 + z * w * x0) / wd, s = Math.sin(wd * dt), c = Math.cos(wd * dt);
      return [e * (x0 * c + b * s), e * ((b * wd - z * w * x0) * c - (x0 * wd + z * w * b) * s)];
    }
    const r = Math.sqrt(z * z - 1), r1 = -w * (z - r), r2 = -w * (z + r), c2 = (v0 - r1 * x0) / (r2 - r1), c1 = x0 - c2;
    const e1 = Math.exp(r1 * dt), e2 = Math.exp(r2 * dt);
    return [c1 * e1 + c2 * e2, c1 * r1 * e1 + c2 * r2 * e2];
  }
  const S = {
    snappy: { response: 0.28, damping: 0.86 }, // spring::SNAPPY
    gentle: { response: 0.45, damping: 1.0 }, // spring::GENTLE
    bouncy: { response: 0.42, damping: 0.6 }, // spring::BOUNCY
    carry: { response: 0.34, damping: 1.0 }, // new: CARRY — one driver for a whole transition
    reel: { response: 0.24, damping: 0.92 }, // new: REEL — rolls, reels, odometer wheels
    follow: { response: 0.2, damping: 1.0 }, // new: TRACK — follows a pointer or a caret
  };
  /** A spring-driven value: starts at `from`, retargets at [tMs, to, spec?] (value and velocity kept). */
  function spring(from, events, spec = S.snappy) {
    const ev = [...events].sort((a, b) => a[0] - b[0]);
    return (tMs) => {
      let val = from, vel = 0, t0 = 0, target = from, sp = spec;
      for (const [te, to, s2] of ev) {
        if (te > tMs) break;
        const [x, v] = sstep(sp, val - target, vel, (te - t0) / 1000);
        val = target + x; vel = v; t0 = te; target = to; if (s2) sp = s2;
      }
      const [x] = sstep(sp, val - target, vel, (tMs - t0) / 1000);
      return target + x;
    };
  }
  /** A tween-driven value: [tMs, to, durMs, curve] retargets start from the current value. */
  function tween(from, events) {
    const ev = [...events].sort((a, b) => a[0] - b[0]);
    const at = (seg, t) => seg.from + (seg.to - seg.from) * seg.curve(clamp((t - seg.t0) / seg.dur));
    return (t) => {
      let seg = null;
      for (const [te, to, dur, curve] of ev) {
        if (te > t) break;
        seg = { t0: te, from: seg ? at(seg, te) : from, to, dur: Math.max(1, dur), curve: curve || C.glide };
      }
      return seg ? at(seg, t) : from;
    };
  }
  const clamp = (x, a = 0, b = 1) => (x < a ? a : x > b ? b : x);
  const P = (t, t0, d) => clamp((t - t0) / Math.max(1e-6, d));
  const E = (t, t0, d, c = C.glide) => c(P(t, t0, d));
  const lerp = (a, b, p) => a + (b - a) * p;
  const band = (p, a, b) => clamp((p - a) / (b - a)); // a sub-range of a driver
  /** CSS-@keyframes semantics: the curve eases each interval, as facet::motion::keys does. */
  function keys(frames, ease = C.glide) {
    return (p) => {
      if (p <= frames[0][0]) return frames[0][1];
      for (let i = 1; i < frames.length; i++) {
        const [p1, v1] = frames[i], [p0, v0] = frames[i - 1];
        if (p <= p1) { const q = ease((p - p0) / Math.max(1e-6, p1 - p0)); return typeof v0 === "number" ? lerp(v0, v1, q) : Object.fromEntries(Object.keys(v0).map((k) => [k, lerp(v0[k], v1[k], q)])); }
      }
      return frames[frames.length - 1][1];
    };
  }
  // colours
  const hex = (h) => { h = h.replace("#", ""); if (h.length === 3) h = [...h].map((c) => c + c).join(""); return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16)); };
  const mixc = (a, b, p) => { const x = hex(a), y = hex(b); return `rgb(${x.map((v, i) => Math.round(lerp(v, y[i], clamp(p)))).join(",")})`; };

  // ------------------------------------------------------------------ styling a frame
  const tf = (el, x = 0, y = 0, sx = 1, sy = sx) => { if (el) el.style.transform = `translate(${x.toFixed(2)}px,${y.toFixed(2)}px) scale(${sx.toFixed(4)},${sy.toFixed(4)})`; };
  /** Clip in px from each edge (GPUI: a content mask; Reveal in fractions). Negative = let shadows out. */
  const clip = (el, top = 0, right = 0, bottom = 0, left = 0) => { if (el) el.style.clipPath = `inset(${top.toFixed(2)}px ${right.toFixed(2)}px ${bottom.toFixed(2)}px ${left.toFixed(2)}px)`; };
  const noclip = (el) => { if (el) el.style.clipPath = "none"; };
  const css = (el, o) => { if (el) for (const k in o) el.style[k] = o[k]; };
  const box = (el, root) => { const a = el.getBoundingClientRect(), b = root.getBoundingClientRect(); return { x: a.left - b.left, y: a.top - b.top, w: a.width, h: a.height }; };

  // ------------------------------------------------------------------ DOM pieces
  const MD = () => window.MD;
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  function h(html) { const t = document.createElement("template"); t.innerHTML = html.trim(); return t.content.firstElementChild; }
  const FAM = { module: "ns", package: "ns", import: "ns", unknown: "ns", struct: "ty", class: "ty", enum: "ty", union: "ty", type: "ty", trait: "co", interface: "co", function: "ca", method: "ca", constructor: "ca", macro: "ca", constant: "va", field: "va", property: "va", variable: "va", variant: "va" };
  const icon = (group, name) => MD().icons[`${group}:${name}`] || "";
  const kind = (k, size = "sm") => `<span class="k ${FAM[k] || "ns"} ${size}"><svg viewBox="0 0 24 24">${icon("kind", k)}</svg></span>`;
  const ico = (name, cls = "s14", style = "") => `<svg class="ico ${cls}" viewBox="0 0 24 24" aria-hidden="true"${style ? ` style="${style}"` : ""}>${icon("ui", name)}</svg>`;
  const chev = (cls = "s12", style = "") => `<svg class="ico ${cls}" viewBox="0 0 24 24" style="${style}"><path d="m9 6 6 6-6 6"></path></svg>`;
  const LIT = [0.4, 0.3, 0.34, 0.14, 0.08, 0.11, 0.22, 0.2, 0.3, 0.56, 0.5, 0.66];
  const FACETS = ["24,2 35,13 24,10", "24,10 35,13 38,24", "35,13 46,24 38,24", "46,24 35,35 38,24", "38,24 35,35 24,38", "35,35 24,46 24,38",
    "24,46 13,35 24,38", "24,38 13,35 10,24", "13,35 2,24 10,24", "2,24 13,13 10,24", "10,24 13,13 24,10", "13,13 24,2 24,10"];
  let gemId = 0;
  /** The 12-facet gem (paint::Gem). Returned handle animates what paint::Gem can paint:
   *  per-facet light (turn, progress, glint), hue, the glyph inside the table, a crack drawn in. */
  function gem(k, px = 28, o = {}) {
    const id = `gm${++gemId}`;
    const glyphs = (o.glyphs || [k]).map((g, i) => `<g class="gg" data-i="${i}"><g transform="translate(17.4 17.4) scale(.55)" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="square" stroke-linejoin="miter">${icon("kind", g).replace(/class="f"/g, 'class="f" fill="currentColor" stroke="none" opacity=".5"')}</g></g>`).join("");
    const svg = h(`<svg class="gem ${FAM[k] || "ns"} ${o.cls || ""}" width="${px}" height="${px}" viewBox="0 0 48 48" style="overflow:visible">
      <defs><clipPath id="${id}"><rect x="15" y="15" width="18" height="18"></rect></clipPath></defs>
      <g class="stone">${FACETS.map((pts, i) => `<polygon class="fc" points="${pts}" style="opacity:${LIT[i]}"></polygon>`).join("")}
      <path class="rim" d="M24 2 46 24 24 46 2 24z" fill="none" stroke="currentColor" stroke-width="1"></path>
      <path class="tb" d="M24 10 38 24 24 38 10 24z" stroke="currentColor" stroke-width=".7" stroke-opacity=".55"></path></g>
      <g clip-path="url(#${id})">${px >= 22 ? glyphs : ""}</g>
      <path class="crack" d="M33.4 11.4 29.6 13.2 31 15.4 27.2 17" style="opacity:0"></path></svg>`);
    const stoneG = svg.querySelector(".stone");
    const facets = [...svg.querySelectorAll(".fc")], crackEl = svg.querySelector(".crack"), gg = [...svg.querySelectorAll(".gg")];
    const len = 12.2;
    crackEl.style.strokeDasharray = `${len}`;
    return {
      el: svg,
      /** turn: the stone turns clockwise by `turn` facets (30° each) under a fixed top-left light: its geometry
       *  rotates and every facet takes the light of the place it passes, so a turn by a symmetry step
       *  (3 facets = 90° for the diamond) ends looking exactly as it started. progress: facets lit 0..12;
       *  glint: a one-pass sweep 0..1; flash: the facet index flashing to full; coralFrom: facets past it fail. */
      light({ turn = 0, progress = 12, glint = -1, flash = -1, unlit = 0.05, coralFrom = 99 } = {}) {
        stoneG.setAttribute("transform", turn ? `rotate(${(turn * 30).toFixed(2)} 24 24)` : "");
        facets.forEach((f, i) => {
          const j = ((i + turn) % 12 + 12) % 12, j0 = Math.floor(j), fr = j - j0;
          let t = lerp(LIT[j0], LIT[(j0 + 1) % 12], fr);
          const lit = clamp(progress - i);
          t = lerp(unlit, t, lit);
          if (glint >= 0) { const d = glint * 14 - i; if (d > 0 && d < 2) t += 0.42 * Math.sin((d / 2) * Math.PI); }
          if (flash >= 0 && Math.floor(flash) === i) t = lerp(1, t, flash - i);
          f.style.opacity = clamp(t, 0, 1).toFixed(3);
          f.style.fill = i >= coralFrom ? "var(--coral)" : "";
        });
      },
      color(c) { svg.style.color = c; },
      /** the glyph reel inside the table: pos 0 = first glyph; fractional = mid-roll (up) */
      glyph(pos) { gg.forEach((g, i) => g.setAttribute("transform", `translate(0 ${((i - pos) * 18).toFixed(2)})`)); },
      crack(p) { crackEl.style.opacity = p > 0 ? 1 : 0; crackEl.style.strokeDashoffset = `${(len * (1 - p)).toFixed(2)}`; },
      rim(dash) { svg.querySelector(".rim").setAttribute("stroke-dasharray", dash ? "3 3.5" : ""); },
    };
  }
  /** A reel: labels stacked in list order inside a window; pos 0 shows the first (sibling, token rolls). */
  function reel(items, cls = "", lineH = null) {
    const el = h(`<span class="reel ${cls}"><span class="rl">${items.map((s) => `<span>${s}</span>`).join("")}</span></span>`);
    return {
      el,
      measure() { const kids = [...el.querySelector(".rl").children]; this.hs = kids.map((k) => k.getBoundingClientRect().height); this.ws = kids.map((k) => k.getBoundingClientRect().width); this.lh = lineH || this.hs[0]; el.style.height = `${this.lh}px`; },
      set(pos) {
        if (!this.ws) this.measure();
        const i = clamp(Math.floor(pos), 0, this.ws.length - 1), f = pos - Math.floor(pos);
        const w = lerp(this.ws[i], this.ws[Math.min(i + 1, this.ws.length - 1)], clamp(f));
        el.style.width = `${w.toFixed(2)}px`;
        el.querySelector(".rl").style.transform = `translateY(${(-pos * this.lh).toFixed(2)}px)`;
      },
    };
  }
  /** A horizontal reel: labels side by side in order; pos 0 shows the first (across: names slide sideways). */
  function hreel(items) {
    const el = h(`<span class="hreel" style="display:inline-block;overflow:hidden;vertical-align:bottom;white-space:nowrap"><span class="hrl" style="display:inline-flex;white-space:nowrap">${items.map((s) => `<span style="display:inline-block">${s}</span>`).join("")}</span></span>`);
    return {
      el,
      set(pos) {
        if (!this.ws) { this.ws = [...el.firstElementChild.children].map((k) => k.getBoundingClientRect().width); this.xs = this.ws.map((_, i) => this.ws.slice(0, i).reduce((a, b) => a + b, 0)); }
        const i = clamp(Math.floor(pos), 0, this.ws.length - 1), j = Math.min(i + 1, this.ws.length - 1), f = clamp(pos - i);
        el.style.width = `${lerp(this.ws[i], this.ws[j], f).toFixed(2)}px`;
        el.firstElementChild.style.transform = `translateX(${(-lerp(this.xs[i], this.xs[j], f)).toFixed(2)}px)`;
      },
    };
  }
  /** An odometer. Versions: one wheel per semver segment, a changed wheel rolls one step per release crossed
   *  (forward in time rolls up, back rolls down). Counts: one wheel per digit; a changed digit rolls one step
   *  in numeric direction (up when the count grows), ones first (carry order). A slot that appears or
   *  vanishes opens or closes its width. */
  function odo(cls = "") {
    const el = h(`<span class="odo ${cls}"></span>`);
    const o = {
      el,
      states(list, mode = "count") {
        this.list = list; this.mode = mode; this.m = null;
        if (mode === "version") {
          const toks = list.map((s) => s.split(/(\.)/));
          const n = Math.max(...toks.map((x) => x.length));
          this.cols = Array.from({ length: n }, (_, c) => toks.map((x) => x[c] ?? ""));
        } else {
          const n = Math.max(...list.map((s) => s.length));
          this.cols = Array.from({ length: n }, (_, c) => list.map((s) => s.padStart(n, " ")[c]));
        }
        el.innerHTML = this.cols.map(() => `<span class="whl"><b>0</b><i></i><i></i></span>`).join("");
        this.wheels = [...el.children];
        return this;
      },
      measure() {
        const probe = h(`<span style="visibility:hidden;position:absolute;white-space:pre"></span>`); el.appendChild(probe);
        const cache = new Map();
        this.W = (s) => { s = s.trim(); if (!s) return 0; if (!cache.has(s)) { probe.textContent = s; cache.set(s, probe.getBoundingClientRect().width); } return cache.get(s); };
        this.cols.forEach((col) => col.forEach((s) => this.W(s)));
        probe.remove();
        this.ch = this.wheels[0].getBoundingClientRect().height;
        this.m = true;
      },
      set(pos) {
        if (!this.m) this.measure();
        const L = this.list.length;
        const i = clamp(Math.floor(pos), 0, L - 1), j = Math.min(i + 1, L - 1), f = i === j ? 0 : clamp(pos - i);
        const num = (s) => parseFloat(String(s).replace(/[^0-9.]/g, "")) || 0;
        const up = this.mode === "count" ? num(this.list[j]) >= num(this.list[i]) : true;
        const nc = this.cols.length;
        this.cols.forEach((col, c) => {
          const a = col[i], b = col[j], w = this.wheels[c], [, ia, ib] = w.children;
          // carry order for counts: the ones wheel moves first
          const lag = this.mode === "count" ? (nc - 1 - c) * 0.14 : 0;
          const fc = this.mode === "count" ? clamp((f - lag) / Math.max(0.2, 1 - (nc - 1) * 0.14)) : f;
          w.style.width = `${lerp(this.W(a), this.W(b), fc).toFixed(2)}px`;
          const put = (el2, txt, y) => { el2.textContent = txt; el2.style.transform = y ? `translateY(${y.toFixed(2)}px)` : ""; };
          if (a === b || fc <= 0 || fc >= 1) { put(ia, (fc >= 1 ? b : a).trim(), 0); put(ib, "", 0); return; }
          const dir = up ? 1 : -1;
          if (false) {
          } else {
            put(ia, a.trim(), -dir * fc * this.ch);
            put(ib, b.trim(), dir * (1 - fc) * this.ch);
          }
        });
      },
    };
    return o;
  }

  /** The stage window: ground, jump bar, shelf, reader, status. */
  function stage(W, H, o = {}) {
    const root = h(`<div class="nx v4 mstage" style="width:${W}px;height:${H}px;background:var(--g0);position:relative;overflow:hidden"></div>`);
    const win = h(`<div class="win calm" style="width:${W}px;height:${H}px">${MD().ground}</div>`);
    root.appendChild(win);
    const tb = o.titlebar === false ? null : h(`<header class="mtb"><span class="lights live"><i></i><i></i><i></i></span>
      <span class="nav"><button class="ibtn">${chev("s14", "transform:rotate(180deg)")}</button><button class="ibtn">${chev("s14")}</button></span>
      <div class="jb"></div>
      <button class="ibtn">${ico("inbox")}</button></header>`);
    if (tb) win.appendChild(tb);
    const body = h(`<div class="cbody"></div>`); win.appendChild(body);
    const shelf = o.shelf === false ? null : h(`<aside class="cshelf"></aside>`);
    if (shelf) body.appendChild(shelf);
    const reader = h(`<main class="reader creader"></main>`); body.appendChild(reader);
    const status = h(`<footer class="cstatus"><span class="mono addr"></span></footer>`); win.appendChild(status);
    return { root, win, tb, jb: tb && tb.querySelector(".jb"), body, shelf, reader, status, addr: status.querySelector(".addr"), ground: win.querySelector(".ground") };
  }
  const seg = (k, name, cur = false) => `<span class="jseg${cur ? " cur" : ""}">${k ? kind(k) : ""}<span class="sn">${esc(name)}</span></span>`;
  const segs = (list) => list.map(([k, n, cur], i) => (i ? `<span class="gt">›</span>` : "") + seg(k, n, cur)).join("") + `<span class="find">${ico("search", "s14")}</span>`;
  const shelfRows = (rows) => rows.map((r) => `<div class="crow${r.cur ? " cur" : ""}" data-n="${esc(r.n)}" style="padding-left:${12 + 16 * (r.depth || 0)}px">${kind(r.k)}<span class="n">${esc(r.n)}</span></div>`).join("");

  // ------------------------------------------------------------------ registry + board
  const DEMOS = [];
  const demo = (d) => DEMOS.push(d);
  const Q = new URLSearchParams(location.search);

  async function fonts() {
    const faces = ['700 44px "Bricolage Grotesque"', '400 13px "Geist"', '500 13px "Geist Mono"', 'italic 400 17px "Newsreader"', '400 17px "Newsreader"'];
    try { await Promise.all(faces.map((f) => document.fonts.load(f))); await document.fonts.ready; } catch (e) { /* offline fonts */ }
  }

  let current = null;
  function mount(d, host, opts) {
    host.innerHTML = "";
    const ctx = d.build(opts);
    host.appendChild(ctx.root);
    host.style.width = `${d.w}px`; host.style.height = `${d.h}px`;
    if (ctx.after) ctx.after();
    return ctx;
  }

  async function boot() {
    if (Q.has("list")) { const pre = document.createElement("pre"); pre.id = "list"; pre.textContent = JSON.stringify(DEMOS.map((d) => ({ id: d.id, group: d.group, title: d.title, relation: d.relation, w: d.w, h: d.h, dur: d.dur, film: d.film, crop: d.crop || null, interrupt: d.interrupt || null }))); document.body.appendChild(pre); return; }
    await fonts();
    const id = Q.get("demo") || DEMOS[0].id;
    const d = DEMOS.find((x) => x.id === id) || DEMOS[0];
    const still = Q.has("t");
    const opts = { reduced: Q.get("reduced") === "1", int: Q.get("int") === "1", v: Q.get("v") || "" };
    document.body.classList.toggle("still", still);
    const app = h(`<div class="board nx v4"><nav><h1>Motion</h1><p class="sub">motion says where things went</p></nav>
      <main><div class="head"><h2></h2><span class="rel"></span></div><p class="say"></p>
      <div class="ctl"><button class="play">pause</button><button class="rs">restart</button><input type="range" min="0" max="1000" value="0"><span class="tm"></span>
      <button class="spd" data-s="1">1×</button><button class="spd" data-s="0.25">¼×</button><button class="spd" data-s="0.1">⅒×</button>
      <button class="rd">reduced motion</button><button class="intr">interrupt</button></div>
      <div class="mframe"></div><div class="mspec"></div></main></div>`);
    document.body.appendChild(app);
    const nav = app.querySelector("nav");
    let grp = "";
    for (const x of DEMOS) {
      if (x.group !== grp) { grp = x.group; nav.appendChild(h(`<div class="grp">${esc(grp)}</div>`)); }
      nav.appendChild(h(`<a href="?demo=${x.id}" class="${x.id === d.id ? "on" : ""}">${esc(x.id)}</a>`));
    }
    const frame = app.querySelector(".mframe");
    app.querySelector("h2").textContent = d.title;
    app.querySelector(".rel").textContent = d.relation;
    app.querySelector(".say").textContent = d.sentence;
    app.querySelector(".mspec").innerHTML = (d.spec || []).map(([k, v]) => `<div><h4>${esc(k)}</h4>${v}</div>`).join("");
    const ctx = mount(d, frame, opts);
    current = { d, ctx, opts };
    if (still) {
      d.frame(ctx, +Q.get("t"), opts);
      document.title = `ready ${d.id} ${Q.get("t")}`;
      if (Q.has("debug")) { const r = (sel) => { const e = document.querySelector(sel); if (!e) return sel + ":none"; const b = e.getBoundingClientRect(); return `${sel}:${Math.round(b.left)},${Math.round(b.top)},${Math.round(b.width)}x${Math.round(b.height)}`; }; document.body.setAttribute("data-debug", Q.get("debug").split(",").map(r).join(" | ")); }
      return;
    }
    // live board
    const fit = () => { const avail = frame.parentElement.clientWidth; const s = Math.min(1, avail / d.w); frame.style.transform = `scale(${s})`; frame.style.marginBottom = `${-(1 - s) * d.h}px`; };
    fit(); addEventListener("resize", fit);
    let t = 0, speed = 1, playing = true, last = performance.now();
    const range = app.querySelector("input"), tm = app.querySelector(".tm");
    const dur = d.dur + 700;
    range.max = d.dur;
    const draw = () => { d.frame(current.ctx, Math.min(t, d.dur), current.opts); range.value = Math.min(t, d.dur); tm.textContent = `${Math.round(Math.min(t, d.dur))} ms`; };
    const tick = (now) => { const dt = now - last; last = now; if (playing) { t += dt * speed; if (t > dur) t = 0; draw(); } requestAnimationFrame(tick); };
    requestAnimationFrame(tick);
    app.querySelector(".play").onclick = (e) => { playing = !playing; e.target.textContent = playing ? "pause" : "play"; };
    app.querySelector(".rs").onclick = () => { t = 0; draw(); };
    range.oninput = () => { playing = false; app.querySelector(".play").textContent = "play"; t = +range.value; draw(); };
    for (const b of app.querySelectorAll(".spd")) { b.classList.toggle("on", +b.dataset.s === 1); b.onclick = () => { speed = +b.dataset.s; app.querySelectorAll(".spd").forEach((x) => x.classList.toggle("on", x === b)); }; }
    const rd = app.querySelector(".rd"), it = app.querySelector(".intr");
    rd.classList.toggle("on", opts.reduced); it.classList.toggle("on", opts.int); it.style.display = d.interrupt ? "" : "none";
    if (d.interrupt) it.textContent = d.interrupt;
    const remount = () => { current.ctx = mount(d, frame, current.opts); t = 0; fit(); draw(); };
    rd.onclick = () => { current.opts = { ...current.opts, reduced: !current.opts.reduced }; rd.classList.toggle("on", current.opts.reduced); remount(); };
    it.onclick = () => { current.opts = { ...current.opts, int: !current.opts.int }; it.classList.toggle("on", current.opts.int); remount(); };
    addEventListener("keydown", (e) => { if (e.key === " ") { e.preventDefault(); app.querySelector(".play").click(); } if (e.key === "ArrowRight" || e.key === "ArrowLeft") { playing = false; t = clamp(t + (e.key === "ArrowRight" ? 10 : -10), 0, d.dur); draw(); } });
  }

  window.MO = { C, S, bezier, spring, tween, sstep, clamp, P, E, lerp, band, keys, mixc, tf, clip, noclip, css, box, h, esc, kind, ico, chev, gem, reel, hreel, odo, stage, seg, segs, shelfRows, FAM, LIT, demo, DEMOS, boot };
  addEventListener("DOMContentLoaded", () => setTimeout(boot, 0));
})();
