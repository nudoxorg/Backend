// A package page whose top is the relay: what it rests on on the left (where it comes from), the package
// in the middle, what uses it on the right (where it goes). Hopping to a neighbour slides the whole relay
// one column: the chosen chip grows into the hero, the package you left shrinks into the column its
// relation to the new one puts it in, shared neighbours stay put, and the rest leave by their side.
// The page then shows the new package through the relation you came by.
import { C, canvas, cells, frame, text, gem } from "./paint.js";
import { layout as terrLayout } from "../cohesion/territory.js";
import { CARRY, PLAY, play, lerp, clamp, ease } from "./motion.js";
import { fmt, plural } from "./world.js";

const COLS = 8;
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const tintOf = (p) => (p.kind === "yours" ? "var(--mint)" : "var(--k-ns)");

// Which of `p`'s items does `other` reach? Known only for your crates (the source scan); for a
// registry pair it comes from the index (the board carries it for the demo chains in data/via.json).
export function reach(p, other, VIA) {
  const key = other.id + ">" + p.id;
  if (VIA && VIA[key]) return new Set(VIA[key]);
  if (other.kind === "yours" && p.uses && p.uses.by && p.uses.by[other.name]) return new Set(p.used.keys());
  return null;
}

function mini(p, n = 36) {
  // A chip's thumbnail: the package's first shingles, two rows, mint where your code reaches.
  let s = `<svg class="mini" width="${Math.ceil(Math.min(n, p.items) / 2) * 4}" height="7" viewBox="0 0 ${Math.ceil(Math.min(n, p.items) / 2) * 4} 7">`;
  let k = 0;
  for (const m of p.modules) for (const it of m.items) {
    if (k >= n) break;
    const col = p.used.has(it.n) ? "var(--mint)" : { type: "var(--k-ty)", callable: "var(--k-ca)", contract: "var(--k-co)" }[it.f] || "var(--ink2)";
    s += `<rect x="${Math.floor(k / 2) * 4}" y="${(k % 2) * 4}" width="3" height="3" fill="${col}" fill-opacity="${p.used.has(it.n) ? 1 : 0.5}"/>`;
    k++;
  }
  return s + `</svg>`;
}

function chip(p, side, P, from) {
  const yours = p.kind === "yours";
  const sites = side === "left" ? (P.kind === "yours" && p.uses && p.uses.by ? p.uses.by[P.name] : 0) : (yours && P.uses && P.uses.by ? P.uses.by[p.name] : 0);
  const fig = sites ? `<span class="fig y">${fmt(sites)}</span>` : `<span class="fig">${fmt(p.items)}</span>`;
  const newer = p.latest ? `<span class="nv">→ ${esc(p.latest)}</span>` : "";
  return `<div class="chip ${side} ${from === p ? "from" : ""} ${yours ? "yours" : ""}" data-id="${esc(p.id)}">
    <span class="cg" data-g="${esc(p.id)}">${gem(16, tintOf(p))}</span>
    <span class="cn" data-n="${esc(p.id)}" data-pkg="${esc(p.id)}">${esc(p.name)}</span>${newer}
    ${mini(p)}${fig}</div>`;
}

export function pageView(host, WD, { VIA, LINES = {}, onHop, onBack, onChain }) {
  host.innerHTML = `<div class="pg"><div class="relay"><svg class="rails"></svg><div class="col lcol"></div><div class="mid"></div><div class="col rcol"></div></div>
    <div class="via"></div><div class="how"></div><div class="terr"><canvas></canvas></div></div>`;
  const pg = host.querySelector(".pg"), relay = host.querySelector(".relay"), rails = host.querySelector(".rails");
  const L = host.querySelector(".lcol"), M = host.querySelector(".mid"), R = host.querySelector(".rcol");
  const viaEl = host.querySelector(".via"), howEl = host.querySelector(".how"), tcan = host.querySelector(".terr canvas");
  let P = null, from = null, lit = null, T = null, deal = 1, dealT0 = 0;

  function columns(p, fromP) {
    const left = [...p.depsP].sort((a, b) => (b === fromP) - (a === fromP) || b.items - a.items);
    const right = [...p.dependentsP].sort((a, b) => (b === fromP) - (a === fromP) || (b.kind === "yours") - (a.kind === "yours") || b.items - a.items);
    return { left: left.slice(0, COLS), right: right.slice(0, COLS), moreL: left.length - COLS, moreR: right.length - COLS };
  }
  const more = (n, side) => (n > 0 ? `<div class="chip more ${side}">+ ${fmt(n)} more</div>` : "");
  function heroHtml(p) {
    const facts = [
      `<span class="v">${esc(p.version)}</span>`,
      p.latest ? `<span class="am">→ ${esc(p.latest)}</span>` : "",
      `${plural(p.items, "public item")}`,
      `${plural(p.modules.length, "module")}`,
      p.sites ? `<span class="y">you name ${plural(p.used.size, "item")}, ${plural(p.sites, "time")}</span>` : p.kind === "yours" ? `<span class="y">yours</span>` : "",
      p.license ? `<span class="lc">${esc(p.license)}</span>` : "",
    ].filter(Boolean).join(`<i>·</i>`);
    return `<div class="hero2"><span class="hg" data-g="${esc(p.id)}">${gem(56, tintOf(p))}</span><div class="ht">
      <h1 data-n="${esc(p.id)}">${esc(p.name)}</h1>
      <div class="lede">${esc(p.lede || "")}</div><div class="facts">${facts}</div></div></div>`;
  }
  function fill(p, fromP) {
    const c = columns(p, fromP);
    L.innerHTML = `<div class="ch">Rests on <b>${fmt(p.depsP.length)}</b></div>` + c.left.map((d) => chip(d, "left", p, fromP)).join("") + more(c.moreL, "left") + (p.depsP.length ? "" : `<div class="none">Rests on nothing</div>`);
    R.innerHTML = `<div class="ch">Used by <b>${fmt(p.dependentsP.length)}</b></div>` + c.right.map((d) => chip(d, "right", p, fromP)).join("") + more(c.moreR, "right") + (p.dependentsP.length ? "" : `<div class="none">Nothing here uses it</div>`);
    M.innerHTML = heroHtml(p);
    for (const el of relay.querySelectorAll(".chip[data-id]")) {
      el.onclick = () => onHop(WD.byId.get(el.dataset.id));
      el.onmouseenter = () => hoverChip(WD.byId.get(el.dataset.id), el);
      el.onmouseleave = () => hoverChip(null);
    }
  }
  // How the one uses the other, in its own words: each reached item with the lines that name it.
  function how(p, fromP) {
    howEl.innerHTML = "";
    if (!fromP) return;
    const down = p.dependentsP.includes(fromP);
    const user = down ? fromP : p, used = down ? p : fromP;
    const ex = LINES[user.id + ">" + used.id];
    if (!ex) return;
    const items = new Map(); for (const m of used.modules) for (const it of m.items) if (!items.has(it.n)) items.set(it.n, { it, m });
    const names = Object.keys(ex).sort((a, b) => ex[b].length - ex[a].length || a.localeCompare(b));
    const total = names.reduce((s2, n) => s2 + ex[n].length, 0);
    const ident = used.name.replace(/-/g, "_");
    const hl = (line, n) => esc(line).replace(new RegExp("(" + ident + "::(?:\\w+::)*)(" + n + ")\\b"), '$1<u>$2</u>');
    const row = (n) => {
      const e = items.get(n), f = e ? e.it.f : "value";
      return `<div class="hw" data-n="${esc(n)}"><div class="hn"><span class="km2 ${f}"></span><b>${esc(n)}</b><i>${esc(e ? e.m.path : "")}</i></div>
        <div class="hl">${ex[n].map(([file, line, text2]) => `<div><span class="at">${esc(file)}:${line}</span><code>${hl(text2, n)}</code></div>`).join("")}</div></div>`;
    };
    howEl.innerHTML = `<div class="hh">How <b>${esc(user.short || user.name)}</b> uses <b>${esc(used.name)}</b><span>${plural(names.length, "item")} · the lines that name them</span></div>${names.slice(0, 6).map(row).join("")}${names.length > 6 ? `<div class="hm">+ ${names.length - 6} more</div>` : ""}`;
    for (const el of howEl.querySelectorAll(".hw")) {
      el.onmouseenter = () => { if (used === P) { lit = new Set([el.dataset.n]); paintTerr(performance.now() + 1e6); } };
      el.onmouseleave = () => { if (used === P) { lit = from ? reach(P, from, VIA) : null; paintTerr(performance.now() + 1e6); } };
    }
  }
  function viaLine(p, fromP) {
    lit = fromP ? reach(p, fromP, VIA) : null;
    how(p, fromP);
    if (!fromP) { viaEl.innerHTML = ""; return; }
    const down = p.dependentsP.includes(fromP); // we came down from a dependent
    const what = lit ? `${plural(lit.size, "item")} of ${esc(p.name)}` : `${esc(p.name)} (the index has not read which items)`;
    const who = fromP.kind === "yours" ? `your code` : `<b>${esc(fromP.name)}</b>`;
    const times = fromP.kind === "yours" && p.uses && p.uses.by && p.uses.by[fromP.name] ? ` · ${esc(fromP.short || fromP.name)} names it ${plural(p.uses.by[fromP.name], "time")}` : "";
    viaEl.innerHTML = down
      ? `<span class="pill">${gem(12, tintOf(fromP))}${who} reaches ${what}${times}</span><span class="x">esc shows all of it</span>`
      : `<span class="pill">${gem(12, tintOf(fromP))}<b>${esc(p.name)}</b> is one of ${esc(fromP.name)}'s ${fmt(fromP.dependentsP.length)} users</span><span class="x">esc</span>`;
  }

  // ---------------------------------------------------------------- the territory, full size
  let tw = 0, tctx = null;
  function layoutTerr(p) {
    tw = host.clientWidth - 80;
    const mods = p.modules.map((m) => ({ path: m.path, items: m.items }));
    T = terrLayout(mods, tw, { rows: 3 });
    tctx = canvas(tcan, tw, T.h + 4);
  }
  function paintTerr(now = performance.now()) {
    if (!T) return;
    tctx.clearRect(0, 0, tw, T.h + 4);
    const dim = lit && lit.size;
    let order = 0;
    T.regions.forEach((r, ri) => {
      const ra = clamp((now - dealT0 - ri * 26) / 160);
      if (ra <= 0) return;
      tctx.globalAlpha = ra;
      tctx.fillStyle = "rgba(158,176,255,.022)"; tctx.fillRect(r.rect.x, r.rect.y, r.rect.w, r.rect.h);
      tctx.strokeStyle = C.line2; tctx.lineWidth = 1; tctx.strokeRect(r.rect.x + 0.5, r.rect.y + 0.5, r.rect.w - 1, r.rect.h - 1);
      text(tctx, r.module.path, r.rect.x + 8, r.rect.y + 19, { font: "500 11.5px 'Geist Mono'", color: C.ink3, alpha: ra });
      const list = [];
      for (const sh of r.shingles) {
        const k = order++;
        const u = CARRY(now - dealT0 - 60 - ri * 26 - k * 2.2);
        if (u <= 0.01) continue;
        const s = 10 * u;
        list.push({ px: sh.x + (10 - s) / 2, py: sh.y + (10 - s) / 2 - (1 - u) * 6, ps: s, it: sh.item });
      }
      cells(tctx, list, 0, 0, 10, (c) => {
        if (lit && lit.has(c.it.n)) return [C["peri-hi"], 1];
        if (P.used.has(c.it.n)) return [C.mint, dim ? 0.5 : 1];
        return [C.fam[c.it.f] || C.ink2, dim ? 0.1 : 0.34];
      });
    });
    tctx.globalAlpha = 1;
  }
  function dealIn() {
    dealT0 = performance.now();
    return play(900 + (T ? T.regions.length * 26 : 0), () => { paintTerr(); });
  }

  // ---------------------------------------------------------------- rails: strands from chips into the hero
  function drawRails(progress = 1) {
    const rr = relay.getBoundingClientRect();
    rails.setAttribute("width", rr.width); rails.setAttribute("height", rr.height);
    const g = M.querySelector(".hg"); if (!g) return;
    const gr = g.getBoundingClientRect();
    const gx = gr.left - rr.left, gy = gr.top - rr.top + gr.height / 2;
    const words = [...M.querySelectorAll("h1,.lede,.facts")].map((e) => { const r = e.getBoundingClientRect(); const range = document.createRange(); range.selectNodeContents(e); const rs = range.getBoundingClientRect(); return Math.min(r.right, rs.right); });
    const gx2 = Math.max(gr.right, ...words) - rr.left + 10;
    let s = "";
    for (const el of relay.querySelectorAll(".chip[data-id]")) {
      const r = el.getBoundingClientRect();
      const left = el.classList.contains("left");
      const x1 = left ? r.right - rr.left + 6 : r.left - rr.left - 6, y1 = r.top - rr.top + r.height / 2;
      const x2 = left ? gx - 6 : gx2 + 6;
      const mx = (x1 + x2) / 2;
      const id = el.dataset.id, p = WD.byId.get(id);
      const heavy = el.querySelector(".fig.y");
      s += `<path class="${left ? "in" : "out"} ${heavy ? "y" : ""} ${el.classList.contains("from") ? "from" : ""}" data-id="${esc(id)}" d="M${x1} ${y1}C${mx} ${y1} ${mx} ${gy} ${x2} ${gy}" pathLength="1" style="stroke-dashoffset:${1 - progress}"/>`;
    }
    rails.innerHTML = s;
  }
  function hoverChip(p, el) {
    for (const path of rails.querySelectorAll("path")) path.classList.toggle("hot", !!p && path.dataset.id === (p && p.id));
    if (!p) { lit = from ? reach(P, from, VIA) : null; paintTerr(performance.now() + 1e6); return; }
    // Hovering a neighbour lights what it reaches of this package (dependents) — the question you'd hop to answer.
    const r = P.dependentsP.includes(p) ? reach(P, p, VIA) : null;
    lit = r || (from ? reach(P, from, VIA) : null);
    paintTerr(performance.now() + 1e6);
  }

  // ---------------------------------------------------------------- show, and hop
  function show(p, fromP = null, { animate = false } = {}) {
    P = p; from = fromP;
    fill(p, fromP); viaLine(p, fromP); layoutTerr(p);
    onChain && onChain(p, fromP);
    requestAnimationFrame(() => drawRails(1));
    if (animate) return dealIn();
    dealT0 = -1e6; paintTerr(); return Promise.resolve();
  }

  // The relay: FLIP every chip and the hero between the old layout and the new one.
  async function hop(Q) {
    const old = P;
    const down = old.depsP.includes(Q); // Q is something old rests on: the relay moves right
    const shift = down ? 1 : -1;
    const snap = new Map();
    for (const el of relay.querySelectorAll("[data-g]")) snap.set(el.dataset.g + (el.classList.contains("hg") ? ":hero" : ""), el.getBoundingClientRect());
    const nameSnap = new Map();
    for (const el of relay.querySelectorAll("[data-n]")) nameSnap.set(el.dataset.n + (el.tagName === "H1" ? ":hero" : ""), { r: el.getBoundingClientRect(), font: getComputedStyle(el).font, size: parseFloat(getComputedStyle(el).fontSize) });
    const oldChips = [...relay.querySelectorAll(".chip[data-id]")].map((el) => ({ id: el.dataset.id, side: el.classList.contains("left") ? "left" : "right", r: el.getBoundingClientRect(), html: el.outerHTML }));
    const oldHero = M.querySelector(".hero2").getBoundingClientRect();
    rails.innerHTML = "";
    // The page body leaves: the territory lifts away upward while the relay moves.
    const tc = host.querySelector(".terr");
    tc.style.transition = "opacity 140ms ease, transform 180ms ease"; tc.style.opacity = "0"; tc.style.transform = "translateY(-10px)";
    viaEl.style.opacity = "0"; howEl.style.opacity = "0";

    show(Q, old);
    // New positions.
    const ghost = document.createElement("div"); ghost.className = "ghosts"; document.body.appendChild(ghost);
    const flights = [];
    const hostR = host.getBoundingClientRect();
    for (const el of relay.querySelectorAll("[data-g]")) {
      const key = el.dataset.g + (el.classList.contains("hg") ? ":hero" : "");
      const to = el.getBoundingClientRect();
      // Where did this mark come from? The same package's mark in the old layout, hero or chip.
      const src = snap.get(key) || snap.get(el.dataset.g) || snap.get(el.dataset.g + ":hero");
      el.style.visibility = "hidden";
      if (src) flights.push({ el, kind: "gem", src, to, id: el.dataset.g, hero: el.classList.contains("hg") });
      else flights.push({ el, kind: "enter", to, id: el.dataset.g, side: el.closest(".lcol") ? "left" : el.closest(".rcol") ? "right" : "mid" });
    }
    const names = [];
    for (const el of relay.querySelectorAll("[data-n]")) {
      const hero = el.tagName === "H1";
      const src = nameSnap.get(el.dataset.n + (hero ? "" : ":hero")) || nameSnap.get(el.dataset.n) || nameSnap.get(el.dataset.n + ":hero");
      if (!src) continue;
      el.style.visibility = "hidden";
      names.push({ el, src, to: el.getBoundingClientRect(), toSize: parseFloat(getComputedStyle(el).fontSize), hero });
    }
    // Chips that were there before and are not now leave by their side (the side they were on, pushed by the shift).
    const nowIds = new Set([...relay.querySelectorAll(".chip[data-id]")].map((el) => el.dataset.id).concat([Q.id]));
    const leaving = oldChips.filter((c) => !nowIds.has(c.id) && c.id !== old.id);
    for (const c of leaving) { const d = document.createElement("div"); d.innerHTML = c.html; const n = d.firstElementChild; n.classList.add("fly"); Object.assign(n.style, { left: c.r.left + "px", top: c.r.top + "px", width: c.r.width + "px" }); ghost.appendChild(n); c.n = n; }
    // Chip bodies (everything but the travelling gem and name) unroll in place, staggered by row.
    const chipsNow = [...relay.querySelectorAll(".chip")];
    chipsNow.forEach((el) => { el.style.opacity = "0"; });
    M.querySelector(".lede").style.opacity = "0"; M.querySelector(".facts").style.opacity = "0";
    for (const f of flights) {
      const n = document.createElement("div"); n.className = "fly"; n.innerHTML = gem(56, getComputedStyle(f.el.querySelector("svg")).color, false); ghost.appendChild(n); f.n = n;
    }
    for (const nm of names) {
      const n = document.createElement("div"); n.className = "fly nm"; n.textContent = nm.el.textContent; ghost.appendChild(n); nm.n = n;
    }
    const dx = shift * 120;
    await play(620, (ms) => {
      const u = CARRY(ms), v = PLAY(ms - 40);
      for (const f of flights) {
        let x, y, s;
        if (f.kind === "gem") {
          x = lerp(f.src.left, f.to.left, u); y = lerp(f.src.top, f.to.top, u) - Math.sin(Math.PI * u) * (f.hero ? 18 : 6); s = lerp(f.src.width, f.to.width, u);
          f.n.style.opacity = 1;
        } else {
          const e = CARRY(ms - 120 - (f.to.top - hostR.top) * 0.5);
          x = f.to.left - (f.side === "left" ? dx * 0.6 : f.side === "right" ? -dx * 0.6 : 0) * (1 - e); y = f.to.top; s = f.to.width;
          f.n.style.opacity = e;
        }
        Object.assign(f.n.style, { left: x + "px", top: y + "px", width: s + "px", height: s + "px" });
      }
      for (const nm of names) {
        // The travelling name: re-set in the destination's face at the source's size on frame one,
        // then re-shaped at each frame's size (the title ruling), riding its gem.
        const size = lerp(nm.src.size, nm.toSize, u);
        Object.assign(nm.n.style, {
          left: lerp(nm.src.r.left, nm.to.left, u) + "px", top: lerp(nm.src.r.top, nm.to.top, u) + "px",
          font: nm.hero ? `700 ${size}px/1.1 "Bricolage Grotesque"` : `400 ${size}px/1.2 "Geist Mono"`,
          letterSpacing: nm.hero ? `${-0.03 * size}px` : "0", color: nm.hero ? "var(--ink0)" : "var(--ink1)",
        });
      }
      for (const c of leaving) { const e = ease.out(clamp(ms / 260)); c.n.style.transform = `translateX(${dx * e}px)`; c.n.style.opacity = 1 - e; }
      chipsNow.forEach((el, i) => { const e = clamp((ms - 220 - i * 22) / 180); el.style.opacity = e; });
      const e2 = clamp((ms - 300) / 220); M.querySelector(".lede").style.opacity = e2; M.querySelector(".facts").style.opacity = e2;
    });
    for (const el of relay.querySelectorAll("[data-g],[data-n]")) el.style.visibility = "";
    ghost.remove();
    tc.style.transition = "none"; tc.style.opacity = "1"; tc.style.transform = "none";
    viaEl.style.transition = "opacity 200ms"; viaEl.style.opacity = "1"; howEl.style.transition = "opacity 260ms"; howEl.style.opacity = "1";
    play(260, (ms) => drawRails(ease.out(ms / 260)));
    await dealIn();
  }

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && from && host.isConnected) { lit = null; viaEl.innerHTML = ""; howEl.innerHTML = ""; from = null; paintTerr(performance.now() + 1e6); }
  });
  return {
    show, hop, get P() { return P; }, get from() { return from; },
    terrRects() { // screen rects of every shingle on the page, for the Browse → page film
      const r = tcan.getBoundingClientRect(); const out = new Map();
      if (T) for (const reg of T.regions) for (const sh of reg.shingles) out.set(sh.item, { x: r.left + sh.x, y: r.top + sh.y, s: 10 });
      return out;
    },
    heroEls() { return { gem: M.querySelector(".hg"), name: M.querySelector("h1"), rest: [M.querySelector(".lede"), M.querySelector(".facts")] }; },
    paintTerr, dealIn, drawRails, pg,
  };
}
