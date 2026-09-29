// Add: a registry search dealt as a hand. A card's corners are its cost to you (what it would bring
// that you don't have; what it shares with what you do). Hover lifts it and lights what it shares.
// Play it (click, or drag it up) and it flies to its place among what you rest on: the band makes room,
// what only it brings settles into a new cluster beneath, what it now shares leaves its old owner's
// cluster, and the block lands unread (dashed) and develops as the index reads it.
import { C, canvas, block, cells, gem } from "./paint.js";
import { PLAY, CARRY, play, lerp, clamp, ease, arc, heading, wait } from "./motion.js";
import { fmt, plural } from "./world.js";
import { comb, ago } from "./charts.js";
import { chip, verdict } from "./license.js";
import { heads } from "./preview.js";

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const CW = 196, CH = 262;

export async function add({ WD, reader, st, Q, closureOf }) {
  let CAND = null;
  try { CAND = await (await fetch("data/candidates.json", { cache: "reload" })).json(); } catch { return; }
  const into = WD.yours.find((p) => p.name === "backend-desktop" || p.name === "desktop") || WD.yours[0];
  const queries = Object.keys(CAND.queries).filter((k) => CAND.queries[k].length);
  let query = Q.get("q2") || (queries.includes("http") ? "http" : queries[0]);
  const pickOf = (q) => (CAND.picks && CAND.picks[q] ? (CAND.picks[q].id || CAND.picks[q].name || CAND.picks[q]) : null);
  const hand = document.createElement("div"); hand.className = "hand"; reader.appendChild(hand);
  const toast = document.createElement("div"); toast.className = "toast"; reader.appendChild(toast);
  st.scrollTo(0);

  function record(c, depth, via, kind = "registry") {
    const p = { id: c.id || `${c.name}@${c.version}`, name: c.name, version: c.version, kind, lede: c.lede, license: c.license, latest: c.latest || null, newer: c.newer || [], yanked: [],
      deps: [], dependents: [], depth, via, items: c.items || 0, modules: c.modules || [], uses: null, depsP: [], dependentsP: [], used: new Map(), sites: 0, dup: false };
    let k = 0; for (const m of p.modules) for (const it of m.items) it.k = k++;
    return p;
  }
  const lockNames = new Set(WD.all.map((p) => p.name));

  // ---------------------------------------------------------------- the cards
  function thumb(c) {
    const cv = document.createElement("canvas");
    const b = block({ modules: c.modules || [], items: c.items || 0 }, { pitch: 5, stone: 4, rows: 7, cap: 180 });
    const w = Math.min(CW - 24, Math.max(10, b.w)), h = 58;
    const g = canvas(cv, CW - 24, h);
    const s = Math.min(1, (CW - 24) / Math.max(1, b.w));
    g.save(); g.scale(s, s);
    cells(g, b.cells, 0, 0, b.stone, (x) => [C.fam[x.it.f] || C.ink2, 0.55]);
    g.restore();
    return { cv, w, b };
  }
  function cardEl(c) {
    const el = document.createElement("div"); el.className = "card unread";
    const T = (WD.trust || {})[c.id] || null;
    const nw = c.brings ? c.brings.new.length : 0, sh = c.brings ? c.brings.shared.length : 0;
    const v = verdict(c.license);
    el.innerHTML = `<div class="top"><span class="gm">${gem(22, "var(--k-ns)", true)}</span><span class="nm">${esc(c.name)}</span><span class="vv">${esc(c.version)}</span></div>
      <div class="ld">${esc(c.lede || "")}</div>
      <div class="cmb">${T && T.releases ? comb(T.releases, { w: CW - 24, h: 16, latest: c.version }) : ""}<span>${T ? `${plural(T.n || T.releases.length, "release")} · last ${ago(T.last)}` : ""}</span></div>
      <div class="lic2">${chip(c.license)}<span class="${v.tone}">${v.word}</span></div>
      <div class="hds">${heads(T)}</div>
      <div class="cost"><div><div class="k">brings new</div><div class="val ${nw ? "new" : "zero"}">${fmt(nw)}${c.brings_total > nw ? `<span style="font:400 11px var(--ui);color:var(--ink3)"> +${fmt(c.brings_total - nw)}</span>` : ""}</div></div>
      <div><div class="k">shares with you</div><div class="val sh">${fmt(sh)}</div></div></div>`;
    return el;
  }
  let cards = [];
  const rr = () => reader.getBoundingClientRect();
  function deal(list) {
    hand.innerHTML = `<div class="ask">Add to <b>${esc(into.name)}</b> from crates.io <span style="color:var(--ink4)">·</span> ${queries.map((q) => `<span class="kb" data-q="${q}" style="cursor:pointer;pointer-events:auto;${q === query ? "color:var(--ink0);box-shadow:inset 0 0 0 1px var(--peri)" : ""}">${q}</span>`).join(" ")}</div>`;
    hand.querySelectorAll("[data-q]").forEach((k) => (k.onclick = () => { query = k.dataset.q; deal(CAND.queries[query]); }));
    cards = list.slice(0, 6).map((c, i) => ({ c, el: cardEl(c), i, hover: 0, x: 0, y: 0, r: 0 }));
    const n = cards.length, mid = (n - 1) / 2, W = rr().width;
    cards.forEach((k) => {
      hand.appendChild(k.el);
      const d = k.i - mid;
      k.home = { x: W / 2 - CW / 2 + d * 158, y: 24 + d * d * 5, r: d * 2.6 };
      k.el.style.left = "0px"; k.el.style.top = "0px";
      k.el.onmouseenter = () => focusCard(k);
      k.el.onmouseleave = () => focusCard(null);
      k.el.onpointerdown = (e) => grab(k, e);
    });
    // Dealt in from below, one after another.
    return play(700, (ms) => {
      cards.forEach((k) => {
        const u = PLAY(ms - k.i * 70);
        setPose(k, k.home.x, k.home.y + (1 - u) * 260, k.home.r * u - (1 - u) * 8, 1);
      });
    });
  }
  function setPose(k, x, y, r, s) {
    k.x = x; k.y = y; k.r = r; k.s = s;
    k.el.style.transform = `translate(${x}px, ${y}px) rotate(${r}deg) scale(${s})`;
  }
  let hot = null, pulseTimer = 0;
  const nest = document.createElementNS("http://www.w3.org/2000/svg", "svg"); nest.classList.add("nest"); reader.appendChild(nest);
  function ratsnest(k) {
    nest.innerHTML = ""; if (!k || !k.c.brings) return;
    const R = reader.getBoundingClientRect(), cr = k.el.getBoundingClientRect();
    nest.setAttribute("width", R.width); nest.setAttribute("height", R.height);
    const a = { x: cr.left - R.left + cr.width / 2, y: cr.top - R.top + 4 };
    let s2 = "";
    const shared = k.c.brings.shared.map((x) => WD.byId.get(x.id)).filter(Boolean);
    shared.forEach((p, i) => {
      const b = st.blockOf(p); if (!b) return;
      const r = st.rectOf(p); const bx = r.x - R.left + r.w / 2, by = r.y - R.top + r.h + 2;
      if (by < 0 || by > R.height) return;
      const my = Math.min(a.y, by) - 30;
      s2 += `<path class="sh" style="animation-delay:${i * 18}ms" d="M${a.x} ${a.y}C${a.x} ${my} ${bx} ${by + 40} ${bx} ${by}"/><circle class="sh" cx="${bx}" cy="${by}" r="2.5"/>`;
    });
    k.c.brings.new.forEach((n, i) => {
      const x = a.x - 40 + i * 30, y = a.y - 40 - (i % 2) * 14;
      s2 += `<path class="nw" d="M${a.x} ${a.y}Q${x} ${a.y - 10} ${x} ${y}"/><text x="${x}" y="${y - 6}">+ ${esc(n.name)}</text>`;
    });
    nest.innerHTML = s2;
  }
  function focusCard(k) {
    if (flying) return;
    hot = k;
    clearInterval(pulseTimer);
    cards.forEach((o) => {
      if (o.el.classList.contains("played")) return;
      const push = k && o !== k ? Math.sign(o.i - k.i) * 34 : 0;
      o.el.style.transition = "transform 220ms cubic-bezier(.2,1.3,.4,1)";
      o.el.style.zIndex = o === k ? 20 : 10 + o.i;
      if (o === k) setPose(o, o.home.x, o.home.y - 96, 0, 1.04);
      else setPose(o, o.home.x + push, o.home.y, o.home.r, 1);
    });
    // What it would connect to: lines to what you already have; what it would bring new dangles, unresolved.
    setTimeout(() => ratsnest(hot === k ? k : null), 230);
    if (!k) ratsnest(null);
    if (k && k.c.brings) {
      const shared = k.c.brings.shared.map((s) => WD.byId.get(s.id)).filter(Boolean);
      const beat = () => shared.forEach((p) => st.pulse(p, C.mint));
      beat(); pulseTimer = setInterval(beat, 900);
    }
  }

  // ---------------------------------------------------------------- drag to play
  let flying = false;
  function grab(k, e) {
    if (flying) return;
    e.preventDefault();
    const start = { x: e.clientX, y: e.clientY }, base = { x: k.x, y: k.y };
    k.el.style.transition = "none"; k.el.classList.add("flying");
    let lastX = e.clientX, moved = false;
    const move = (ev) => {
      const dx = ev.clientX - start.x, dy = ev.clientY - start.y;
      if (Math.abs(dx) + Math.abs(dy) > 4) moved = true;
      const lean = clamp((ev.clientX - lastX) * 0.8, -10, 10); lastX = ev.clientX;
      setPose(k, base.x + dx, base.y + dy, lean, 1.04);
    };
    const up = (ev) => {
      window.removeEventListener("pointermove", move); window.removeEventListener("pointerup", up);
      k.el.classList.remove("flying");
      if (!moved || ev.clientY - start.y < -70) playCard(k); else { k.el.style.transition = "transform 320ms cubic-bezier(.2,1.3,.4,1)"; setPose(k, k.home.x, k.home.y - 96, 0, 1.04); }
    };
    window.addEventListener("pointermove", move); window.addEventListener("pointerup", up);
  }

  // ---------------------------------------------------------------- play: the flight, the room, the settling
  async function playCard(k) {
    if (flying) return; flying = true;
    clearInterval(pulseTimer); ratsnest(null);
    const c = k.c;
    const P = record(c, 1, []);
    P.dependents = [into.id]; P.dependentsP = [into];
    const shared = (c.brings ? c.brings.shared : []).map((s) => WD.byId.get(s.id)).filter(Boolean);
    const fresh = (c.brings ? c.brings.new : []).filter((n) => n.id && CAND.extra && CAND.extra[n.id] && !lockNames.has(n.name)).map((n) => record(CAND.extra[n.id], 2, [P.id]));
    P.depsP = [...shared, ...fresh]; P.deps = P.depsP.map((p) => p.id);
    for (const f of fresh) { f.dependentsP = [P]; f.dependents = [P.id]; }
    // Into the world: the band, the new cluster, and what it now shares leaves its old owner's cluster.
    WD.all.push(P, ...fresh);
    for (const p of [P, ...fresh]) { WD.byId.set(p.id, p); (WD.byName.get(p.name) || WD.byName.set(p.name, []).get(p.name)).push(p); }
    into.depsP.push(P);
    for (const s of shared) s.dependentsP.push(P);
    // Just added: it enters at the front of what you rest on (until your code names it), so the band makes room.
    P.fresh = true; WD.direct.unshift(P);
    const moved = [];
    const sharedGroup = WD.groups.find((g) => g.key === "shared");
    for (const s of shared) {
      if (s.depth >= 2 && s.via.length === 1) {
        const g = WD.groups.find((x) => x.key === s.via[0]);
        if (g && sharedGroup) { g.pkgs = g.pkgs.filter((x) => x !== s); sharedGroup.pkgs.push(s); sharedGroup.pkgs.sort((a, b) => b.items - a.items); s.via = [...s.via, P.id]; moved.push(s); }
      }
    }
    if (fresh.length) { WD.beneath.push(...fresh); const i = WD.groups.findIndex((g) => g.key === "shared"); WD.groups.splice(i < 0 ? WD.groups.length : i, 0, { key: P.id, via: P, pkgs: fresh, fresh: true }); }

    // 0. Consequences flash on the card while it is still big: new, then shared.
    const vals = [...k.el.querySelectorAll(".cost .val")];
    for (const [i, v] of vals.entries()) { setTimeout(() => { v.classList.add("flash"); setTimeout(() => v.classList.remove("flash"), 260); }, i * 200); }
    await wait(380);
    // 1. Anticipation: the card gathers itself (a short dip), the others step back.
    cards.forEach((o) => { if (o !== k) { o.el.style.transition = "transform 260ms ease, opacity 260ms"; setPose(o, o.home.x, o.home.y + 60, o.home.r, 0.96); o.el.style.opacity = 0.5; } });
    k.el.style.transition = "none";
    const from = { x: k.x, y: k.y, r: k.r };
    await play(110, (ms) => { const u = ease.out(ms / 110); setPose(k, from.x, from.y + 10 * u, from.r * (1 - u), 1.04 - 0.06 * u); });

    // 2. The band makes room (every block glides on a spring), then the card flies to its gap.
    let order = 0; const idx = new Map(WD.direct.map((p) => [p, order++]));
    st.relayout({ tick: 700, hide: [P, ...fresh], stagger: (q) => (idx.has(q.p) ? idx.get(q.p) * 9 : q.band === "beneath" ? 260 + (q.x - 40) * 0.15 : 0) });
    const q = st.blockOf(P);
    st.reveal(P, 0.25);
    await wait(30);
    const R = reader.getBoundingClientRect(), el = st.el.getBoundingClientRect();
    const target = { x: el.left - R.left + q.x, y: el.top - R.top + q.y + q.top - st.S.scroll, w: q.b.w, h: q.b.h };
    const hr = hand.getBoundingClientRect();
    const a = { x: hr.left - R.left + k.x + CW / 2, y: hr.top - R.top + k.y + CH / 2 };
    const b = { x: target.x + target.w / 2, y: target.y + target.h / 2 };
    const lift = Math.min(160, Math.hypot(b.x - a.x, b.y - a.y) * 0.3);
    const inner = [...k.el.querySelectorAll(".ld,.cost,.top,.cmb,.lic2,.hds")];
    const T = 620;
    k.el.style.zIndex = 60;
    await play(T, (ms) => {
      const u = PLAY(ms), v = clamp(ms / T);
      const p = arc(a, b, clamp(u, 0, 1.08), lift);
      const lean = (heading(a, b, clamp(v, 0, 1), lift) * 180) / Math.PI;
      const s = lerp(1.0, target.w / (CW - 24), ease.inOut(v));
      const sx = p.x - (hr.left - R.left) - CW / 2, sy = p.y - (hr.top - R.top) - CH / 2;
      // Only the frame and the shingles travel: the words let go in the first third.
      inner.forEach((n) => (n.style.opacity = 1 - clamp(v / 0.3)));
      const tilt = clamp((lean + 90) * 0.12, -14, 14) * Math.sin(Math.PI * v);
      k.el.style.transform = `translate(${sx}px, ${sy}px) rotate(${tilt}deg) scale(${Math.max(0.05, s)})`;
      k.el.style.transformOrigin = "50% 50%";
    });
    // 3. Landing: the block takes the card's place, unread; a ring spreads from it.
    k.el.remove();
    st.show(P); st.develop(P, 0); st.pulse(P, C["peri-hi"]);
    // 4. What only it brings settles beneath it; what it shares lights; moved ones glide to "shared".
    const blockRect = st.rectOf(P);
    const ov = document.createElement("canvas"); ov.style.cssText = "position:fixed;left:0;top:0;pointer-events:none;z-index:40";
    document.body.appendChild(ov);
    const g = canvas(ov, innerWidth, innerHeight);
    const falls = fresh.map((f, i) => ({ f, i, to: st.rectOf(f), b: st.blockOf(f).b }));
    shared.forEach((s, i) => setTimeout(() => st.pulse(s, C.mint), 120 + i * 60));
    await play(900 + falls.length * 90, (ms) => {
      g.clearRect(0, 0, innerWidth, innerHeight);
      for (const fl of falls) {
        const u = PLAY(ms - fl.i * 90);
        if (u <= 0) continue;
        if (u >= 0.999 && !fl.done) { fl.done = true; st.show(fl.f); st.develop(fl.f, 0); st.pulse(fl.f, C.peri); }
        if (fl.done) continue;
        const a2 = { x: blockRect.x + blockRect.w / 2, y: blockRect.y + blockRect.h }, b2 = { x: fl.to.x, y: fl.to.y };
        const p = arc(a2, b2, clamp(u, 0, 1), -20);
        cells(g, fl.b.cells.slice(0, 24), p.x, p.y, fl.b.stone, (x) => [C.peri, 0.9]);
      }
    });
    ov.remove();
    // 5. It develops as the index reads it: shingles fill in, the dashed frame goes solid.
    const t0 = performance.now();
    st.hover(P);
    await play(2400, (ms) => {
      st.develop(P, ease.inOut(clamp(ms / 2200)));
      fresh.forEach((f, i) => st.develop(f, ease.inOut(clamp((ms - 300 - i * 200) / 1200))));
    });
    st.develop(P, null); fresh.forEach((f) => st.develop(f, null));
    st.paintRest(); st.kick();
    toast.innerHTML = `Added <b>${esc(P.name)}</b> to ${esc(into.name)} <span class="am">brought ${plural(fresh.length, "new package")}</span> <span class="y">shares ${fmt(shared.length)}</span>${moved.length ? `<span>${moved.map((m) => esc(m.name)).join(", ")} ${moved.length === 1 ? "is" : "are"} no longer only one package's</span>` : ""}<span class="u">Undo</span>`;
    toast.classList.add("on");
    cards.forEach((o) => { if (o !== k) { o.el.style.opacity = 1; setPose(o, o.home.x, o.home.y, o.home.r, 1); } });
    flying = false;
  }

  await deal(CAND.queries[query]);
  const auto = Q.get("play");
  if (auto) { const k = cards.find((x) => x.c.name === auto) || cards.find((x) => x.c.id === pickOf(query) || x.c.name === pickOf(query)) || cards[0]; focusCard(k); await wait(700); playCard(k); }
  else if (Q.get("lift")) { const k = cards.find((x) => x.c.name === Q.get("lift")) || cards.find((x) => x.c.id === pickOf(query) || x.c.name === pickOf(query)) || cards[0]; focusCard(k); }
}
