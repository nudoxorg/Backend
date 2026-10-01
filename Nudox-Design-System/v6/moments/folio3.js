// The package page, third form. One idea per element, and every element answers inside itself:
//   hero        what it is and who made it
//   ticker      the only place versions live: scrub it to read the package at another release, and the
//               page changes colour while you're in the past
//   instruments licence (a stamp that unfolds), heads-up (icons that open into what they found),
//               weight (an iceberg: its own code above water, what it pulls in beneath)
//   territory   the shingles: every public name at a glance; click a module and its shingles unfurl
//               into cards, one per symbol, with badges instead of code
//   yours       how your code uses it, crate by crate; its features, as switches
import { C, canvas, cells, gem, text as ctext } from "./paint.js";
import { layout as terrLayout } from "../cohesion/territory.js";
import { ticker } from "./ticker.js";
import { iceberg } from "./iceberg.js";
import { crest, sheet } from "./crest.js";
import { featureBar } from "./features.js";
import { icon, read, tagHtml, KIND } from "./badges.js";
import { parse, verdict, YOURS } from "./license.js";
import { ago } from "./charts.js";
import { kLines, tint, link } from "./preview.js";
import { fmt, plural } from "./world.js";
import { CARRY, play, lerp, clamp, wait } from "./motion.js";

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const fileOf = (id) => id.replace(/[/+]/g, "_");
const cache = new Map();
// A module with more than this many names gets a page of its own rather than an inline unfurl.
const BIG = 14;
export async function detail(id) {
  if (cache.has(id)) return cache.get(id);
  let d = null;
  try { const r = await fetch(`data/pkg/${fileOf(id)}.json`, { cache: "reload" }); if (r.ok) d = await r.json(); } catch { d = null; }
  cache.set(id, d); return d;
}

// ---------------------------------------------------------------- the licence stamp
const PERM = { commercial: "sell with it", modify: "change it", distribute: "ship it", private: "use privately", patent: "patent grant" };
const ASK = { notice: "keep its notice", "notice-src": "keep its notice in source", changes: "say what you changed", disclose: "share your source", "disclose-file": "share changed files", same: "same licence for you", "same-file": "changed files keep it", "same-lib": "changes keep it", network: "network use counts" };
const NOPE = { liability: "no liability", warranty: "no warranty", trademark: "no trademark rights", endorse: "no endorsement", "patent-none": "no patent rights" };
function stamp(expr) {
  const v = verdict(expr), L = parse(expr);
  const terms = v.pick.filter((t) => t.t).map((t) => t.t);
  const union = (k) => [...new Set(terms.flatMap((t) => t[k]))];
  const opts = L.none ? "" : L.groups.map((g) => g.map((t) => `<span class="${v.pick.includes(t) ? "on" : ""}">${esc(t.id)}</span>`).join(`<i>or</i>`)).join(`<i class="and">and</i>`);
  const row = (k, map, cls) => union(k).map((x) => `<span class="st-r ${cls}">${icon(cls === "ok" ? "ctor" : cls === "ask" ? "marker" : "error")}${esc(map[x] || x)}</span>`).join("");
  return `<div class="stamp ${v.tone}" tabindex="0"><div class="st-face"><span class="st-seal">${icon(v.tone === "mint" ? "shield" : "unsafe")}</span><div><div class="st-v">${esc(v.word)}</div><div class="st-x">${opts || "no licence declared"}</div></div></div>
    <div class="st-more"><div class="st-line">${esc(v.line)}</div>${L.none ? "" : `<div class="st-rows">${row("p", PERM, "ok")}${row("c", ASK, "ask")}${row("l", NOPE, "no")}</div>`}</div></div>`;
}

// ---------------------------------------------------------------- heads-up icons
function headsUp(T) {
  if (!T) return "";
  const caps = T.caps || {};
  const items = [];
  const add = (k, word, n, ex, tone = "") => items.push(`<span class="hi ${tone}" tabindex="0">${icon(k)}<span class="hi-w"><b>${esc(word)}</b>${n != null ? `<em>${fmt(n)}</em>` : ""}${ex ? `<code>${esc(ex[0])}:${ex[1]}</code>` : ""}</span></span>`);
  if (T.build_rs) add("build", "runs code when you build", null, null, "amber");
  if (T.proc_macro) add("macroPkg", "runs inside your compiler", null, null, "amber");
  if (caps.process_n) add("process", "starts programs", caps.process_n, caps.process[0], "amber");
  if (caps.net_n) add("net", "network", caps.net_n, caps.net[0]);
  if (caps.fs_n) add("files", "files", caps.fs_n, caps.fs[0]);
  if (caps.env_n) add("env", "environment", caps.env_n, caps.env[0]);
  if (caps.ffi_n) add("ffi", "calls C", caps.ffi_n, caps.ffi[0], "amber");
  if (T.forbid_unsafe) add("shield", "forbids unsafe", null, null, "mint");
  else if (T.unsafe) add("unsafe", "unsafe blocks", T.unsafe, null, T.unsafe > 50 ? "amber" : "");
  return items.join("") || `<span class="hi mint quiet">${icon("shield")}<span class="hi-w"><b>nothing to flag</b></span></span>`;
}

export function folio3(host, WD, { TRUST, LINES, onSymbol, onContext, onGo }) {
  let P = null, T = null, D = null, at = null, AT = null, tk = null, terr = null, open = null;

  function hero(p) {
    const by = T && T.authors && T.authors.length ? `by ${T.authors.slice(0, 2).map(esc).join(", ")}` : "";
    const repo = T && T.repository ? `<span class="url">${esc(T.repository.replace(/^https?:\/\/(www\.)?/, "").replace(/\.git$/, ""))}</span>` : "";
    const cats = T && T.categories && T.categories.length ? `${T.categories.slice(0, 2).map((c) => esc(c.replace(/::/g, " › "))).join(", ")}` : "";
    const ed = T && T.edition ? `Rust ${esc(T.edition)}${T.rust_version ? `, needs ${esc(T.rust_version)}` : ""}` : "";
    return `<div class="h3"><span class="hg" data-g="${esc(p.id)}">${gem(56, tint(p))}</span><div class="h3-t">
      <h1 data-n="${esc(p.id)}">${esc(p.name)}</h1><div class="lede">${esc(p.lede || "")}</div>
      <div class="by">${[by, repo, cats, ed].filter(Boolean).join(`<i>·</i>`)}</div>${crest(p, T, TRUST)}</div></div>`;
  }
  function yours(p) {
    const by = p.uses && p.uses.by ? Object.entries(p.uses.by).sort((a, b) => b[1] - a[1]) : [];
    const max = by.length ? by[0][1] : 1;
    const rows = by.slice(0, 6).map(([crate, n]) => {
      const ex = LINES[`${crate}@0.1.0>${p.id}`] || {};
      const names = Object.keys(ex);
      return `<div class="yr" data-crate="${esc(crate)}"><span class="yc">${gem(12, "var(--mint)")}${esc(crate.replace(/^backend-/, ""))}</span><span class="yb"><i style="width:${(n / max) * 100}%"></i></span><span class="yn">${plural(n, "place")}</span>
        <span class="yw">${names.slice(0, 5).map((nm) => `<span class="ywc" tabindex="0">${esc(nm)}<span class="ywl">${ex[nm].map(([f, l, t]) => `<span><em>${esc(f)}:${l}</em>${esc(t)}</span>`).join("")}</span></span>`).join("")}</span></div>`;
    }).join("");
    return `<div class="more3">${rows ? `<div class="m3"><div class="m3h">Your code and it</div>${rows}</div>` : `<div class="m3"><div class="m3h">Your code and it</div><div class="quiet">None of your crates name it directly${p.dependentsP.length ? `; it comes in under ${p.dependentsP.slice(0, 3).map((d) => link(d)).join(", ")}` : ""}.</div></div>`}
</div>`;
  }

  // ---------------------------------------------------------------- the territory (shingles), and its states
  const state = new Map(); // item name → "new" | "chg" | "gone" | "absent" while in the past
  function modsOf() { return (D ? D.modules : P.modules).filter((m) => m.items.length && !m.private).slice().sort((a, b) => (b.path === "lib") - (a.path === "lib") || b.items.length - a.items.length); }
  function paintTerr(hot = null) {
    const { ctx, L, w } = terr;
    ctx.clearRect(0, 0, w, L.h + 30);
    for (const r of L.regions) {
      const past = at && at !== P.version;
      ctx.fillStyle = past ? "rgba(244,187,106,.05)" : terr.hotRegion === r ? "rgba(158,176,255,.07)" : "rgba(158,176,255,.035)"; ctx.fillRect(r.rect.x, r.rect.y, r.rect.w, r.rect.h);
      ctx.strokeStyle = terr.hotRegion === r ? C["peri-hi"] : C.line3; ctx.lineWidth = terr.hotRegion === r ? 1.5 : 1; ctx.strokeRect(r.rect.x + 0.5, r.rect.y + 0.5, r.rect.w - 1, r.rect.h - 1);
      ctext(ctx, r.module.path, r.rect.x + 8, r.rect.y + 19, { font: "500 11.5px 'Geist Mono'", color: terr.hotRegion === r ? C.ink0 : C.ink2 });
      // What the region is, said by the region: how much of it you use (a mint count in its corner)
      // and its mix of kinds (a thin bar along its foot: types, functions, traits, values).
      const mod = modsOf().find((m) => m.path === r.module.path);
      const mine = mod ? mod.items.filter((it) => P.used.has(it.n)).length : 0;
      ctx.font = "500 11.5px 'Geist Mono'"; const lw = ctx.measureText(r.module.path).width;
      if (mine && r.rect.w > lw + 40) { ctx.font = "600 10.5px 'Geist Mono'"; const t = String(mine), tw = ctx.measureText(t).width; ctx.fillStyle = C.mint; ctx.globalAlpha = 0.18; ctx.fillRect(r.rect.x + r.rect.w - tw - 12, r.rect.y + 6, tw + 8, 16); ctx.globalAlpha = 1; ctext(ctx, t, r.rect.x + r.rect.w - tw - 8, r.rect.y + 18, { font: "600 10.5px 'Geist Mono'", color: C.mint }); }
      if (mod) {
        const mix = { type: 0, callable: 0, contract: 0, value: 0 }; for (const it of mod.items) mix[it.f] = (mix[it.f] || 0) + 1;
        let x = r.rect.x + 1; const w = r.rect.w - 2, n = mod.items.length || 1;
        for (const [k, c] of Object.entries(mix)) { if (!c) continue; const ww = (c / n) * w; ctx.fillStyle = C.fam[k] || C.ink2; ctx.globalAlpha = 0.7; ctx.fillRect(x, r.rect.y + r.rect.h - 3, ww, 2); x += ww; }
        ctx.globalAlpha = 1;
      }
      cells(ctx, r.shingles.map((sh) => ({ px: sh.x, py: sh.y, ps: 10, it: sh.item })), 0, 0, 10, (c) => {
        const st = state.get(c.it.n);
        if (st === "gone") return [C.coral, 0.9];
        if (st === "chg") return [C.amber, 1];
        if (st === "absent") return [C.ink4, 0.25];
        if (P.used.has(c.it.n)) return [C.mint, 1];
        return [C.fam[c.it.f] || C.ink2, past ? 0.3 : 0.62];
      });
    }
    if (hot) {
      const { sh } = hot, s = 15, x = sh.x - 2.5, y = sh.y - 4;
      ctx.fillStyle = P.used.has(sh.item.n) ? C.mint : C.fam[sh.item.f] || C.ink1; ctx.beginPath(); const c = 4.5; ctx.moveTo(x + c, y); ctx.lineTo(x + s, y); ctx.lineTo(x + s, y + s - c); ctx.lineTo(x + s - c, y + s); ctx.lineTo(x, y + s); ctx.lineTo(x, y + c); ctx.closePath(); ctx.fill();
      // The name rides on the shingle itself, on a small plate.
      ctx.font = "500 12px 'Geist Mono'"; const tw = ctx.measureText(sh.item.n).width;
      let lx = sh.x + 16, ly = sh.y - 6; if (lx + tw + 14 > w) lx = sh.x - tw - 20;
      ctx.fillStyle = C.plate3; ctx.fillRect(lx, ly, tw + 12, 20); ctx.strokeStyle = C.line3; ctx.strokeRect(lx + 0.5, ly + 0.5, tw + 11, 19);
      ctext(ctx, sh.item.n, lx + 6, ly + 14, { font: "500 12px 'Geist Mono'", color: C.ink0 });
    }
  }
  function mountTerr() {
    const el = host.querySelector(".terr3 canvas");
    const w = host.querySelector(".terr3").clientWidth;
    const L = terrLayout(modsOf().map((m) => ({ path: m.path, items: m.items })), w, { rows: 3 });
    const ctx = canvas(el, w, L.h + 30);
    terr = { el, ctx, L, w, hotRegion: null };
    paintTerr();
    const hit = (e) => { const r = el.getBoundingClientRect(), x = e.clientX - r.left, y = e.clientY - r.top; for (const reg of L.regions) if (x >= reg.rect.x && x <= reg.rect.x + reg.rect.w && y >= reg.rect.y && y <= reg.rect.y + reg.rect.h) { const sh = reg.shingles.find((s) => x >= s.x - 2 && x <= s.x + 12 && y >= s.y - 2 && y <= s.y + 12); return { reg, sh }; } return null; };
    el.onmousemove = (e) => { const h = hit(e); terr.hotRegion = h ? h.reg : null; el.style.cursor = h ? "pointer" : ""; paintTerr(h && h.sh ? h : null); modDoc(h ? h.reg : null); };
    el.onmouseleave = () => { terr.hotRegion = null; paintTerr(); modDoc(null); };
    el.onclick = (e) => { const h = hit(e); if (!h) return; if (h.reg.module.items.length > BIG) moduleView(h.reg, h.sh ? h.sh.item : null); else unfurl(h.reg, h.sh ? h.sh.item : null); };
  }

  // The module under the pointer reads itself at the territory's foot: its own words, and what it holds.
  function modDoc(reg) {
    const box = host.querySelector(".mdoc"); if (!box) return;
    if (!reg) { box.classList.remove("on"); return; }
    const m = modsOf().find((x) => x.path === reg.module.path);
    const kinds = {}; for (const it of m.items) { const k = read(it).kind; kinds[k] = (kinds[k] || 0) + 1; }
    const mine = m.items.filter((it) => P.used.has(it.n)).length;
    box.innerHTML = `<b>${esc(m.path)}</b>${m.doc ? `<span class="md">${esc(m.doc)}</span>` : ""}<span class="mk">${Object.entries(kinds).sort((a, b) => b[1] - a[1]).map(([k, n]) => `${n} ${esc(KIND[k] || k)}${n > 1 && !/s$/.test(k) ? "s" : ""}`).join(" · ")}</span>${mine ? `<span class="y">you use ${mine}</span>` : ""}<span class="go">${m.items.length > BIG ? "click for its own page" : "click to open here"}</span>`;
    box.classList.add("on");
  }

  // A big module is a page of its own: its words at the top, its symbols as cards grouped by what
  // they are (types, then functions, then traits, then values), the rest of the package a rail above.
  async function moduleView(reg, focusItem) {
    const mods = modsOf();
    const mod = mods.find((m) => m.path === reg.module.path);
    open = mod.path;
    const groups = new Map();
    for (const it of mod.items) { const k = read(it).kind; const g = k === "fn" || k === "macro" ? "Functions" : k === "trait" ? "Traits" : k === "const" || k === "static" ? "Values" : "Types"; (groups.get(g) || groups.set(g, []).get(g)).push(it); }
    const order = ["Types", "Functions", "Traits", "Values"].filter((g) => groups.has(g));
    const box = host.querySelector(".mod3");
    const mine = mod.items.filter((it) => P.used.has(it.n)).length;
    box.innerHTML = `<div class="rail3">${mods.map((m) => `<span class="rt ${m.path === mod.path ? "on" : ""}" data-m="${esc(m.path)}">${esc(m.path)}<i>${m.items.length}</i></span>`).join("")}<span class="rt close">${icon("error")}back to ${esc(P.name)}</span></div>
      <div class="mhead"><div class="mh-path"><span class="mh-pkg">${esc(P.name)}</span><span class="mh-sep">::</span>${esc(mod.path)}</div>
      ${mod.doc_full || mod.doc ? `<div class="mh-doc">${esc(mod.doc_full || mod.doc)}</div>` : `<div class="mh-doc quiet">This module has no description of its own.</div>`}
      <div class="mh-stats">${order.map((g) => `<span><b>${groups.get(g).length}</b> ${g.toLowerCase()}</span>`).join("")}${mine ? `<span class="y"><b>${mine}</b> you use</span>` : ""}</div></div>
      ${order.map((g) => `<div class="mgrp"><div class="mg-h">${g}</div><div class="cards3">${groups.get(g).map((it) => card(it, mod.path)).join("")}</div></div>`).join("")}`;
    box.classList.add("on", "page");
    host.querySelector(".terr3").classList.add("folded"); host.querySelector(".fbar").classList.add("folded");
    box.querySelectorAll(".rt[data-m]").forEach((t) => (t.onclick = () => { const r2 = terr.L.regions.find((x) => x.module.path === t.dataset.m); if (!r2) return; if (r2.module.items.length > BIG) moduleView(r2, null); else { box.classList.remove("page"); unfurl(r2, null); } }));
    box.querySelector(".rt.close").onclick = fold;
    box.querySelectorAll(".card3").forEach((c) => (c.onclick = () => onSymbol && onSymbol(P, mod.items.find((x) => x.n === c.dataset.it), c, mod)));
    host.querySelector(".f3").scrollTo({ top: box.offsetTop - 60, behavior: "instant" });
    const cards = [...box.querySelectorAll(".card3, .mhead, .mg-h")];
    cards.forEach((c, i) => { c.style.opacity = 0; c.style.transform = "translateY(6px)"; setTimeout(() => { c.style.transition = "opacity 200ms, transform 260ms cubic-bezier(.2,1.2,.4,1)"; c.style.opacity = 1; c.style.transform = "none"; }, 20 + i * 12); });
    if (focusItem) { const c = box.querySelector(`.card3[data-it="${CSS.escape(focusItem.n)}"]`); if (c) { c.classList.add("lit"); setTimeout(() => c.classList.remove("lit"), 900); } }
    onContext && onContext({ mode: "module", p: P, mod, D });
  }

  // ---------------------------------------------------------------- a module unfurls into cards
  function card(it, mod) {
    const r = read(it);
    const st = state.get(it.n);
    const you = P.used.has(it.n) ? `<span class="you">${icon("you")}${fmt(P.used.get(it.n))}</span>` : "";
    const tags = r.tags.map(tagHtml).join("");
    return `<div class="card3 ${it.f} ${st || ""}" data-it="${esc(it.n)}" data-mod="${esc(mod)}" tabindex="0">
      <div class="c3t"><span class="km3 ${it.f}"></span><b>${esc(it.n)}</b><span class="c3k">${KIND[r.kind] || r.kind}</span>${you}</div>
      <div class="c3d">${it.d ? esc(it.d) : `<span class="undoc">${icon("undoc")}undocumented</span>`}</div>
      <div class="c3b">${tags}${st === "gone" ? `<span class="bdg coral">${icon("error")}<b>gone at ${esc(at)}</b></span>` : st === "chg" ? `<span class="bdg amber">${icon("mutates")}<b>changed at ${esc(at)}</b></span>` : st === "new" ? `<span class="bdg mint">${icon("ctor")}<b>new at ${esc(at)}</b></span>` : ""}</div></div>`;
  }
  async function unfurl(reg, focusItem) {
    const mods = modsOf();
    const mod = mods.find((m) => m.path === reg.module.path);
    const box = host.querySelector(".mod3");
    open = mod.path;
    const src = reg.shingles.map((sh) => { const r = terr.el.getBoundingClientRect(); return { it: sh.item, x: r.left + sh.x, y: r.top + sh.y }; });
    box.innerHTML = `<div class="rail3">${mods.map((m) => `<span class="rt ${m.path === mod.path ? "on" : ""}" data-m="${esc(m.path)}">${esc(m.path)}<i>${m.items.length}</i></span>`).join("")}<span class="rt close">${icon("error")}fold</span></div>
      <div class="cards3">${mod.items.map((it) => card(it, mod.path)).join("")}</div>`;
    box.classList.add("on");
    host.querySelector(".terr3").classList.add("folded");
    box.querySelectorAll(".rt[data-m]").forEach((t) => (t.onclick = () => { const r2 = terr.L.regions.find((x) => x.module.path === t.dataset.m); if (r2) unfurl(r2, null); }));
    box.querySelector(".rt.close").onclick = fold;
    box.querySelectorAll(".card3").forEach((c) => (c.onclick = () => onSymbol && onSymbol(P, mod.items.find((x) => x.n === c.dataset.it), c, mod)));
    // Each shingle flies to its card's mark, then its card opens out of it.
    const cardsEls = [...box.querySelectorAll(".card3")];
    cardsEls.forEach((c) => (c.style.opacity = 0));
    const pg = host.querySelector(".f3"); const top = box.offsetTop - 70; pg.scrollTo({ top, behavior: "instant" });
    await wait(16);
    const ov = document.createElement("canvas"); ov.style.cssText = "position:fixed;left:0;top:0;pointer-events:none;z-index:40"; document.body.appendChild(ov);
    const g = canvas(ov, innerWidth, innerHeight);
    const pr = terr.el.getBoundingClientRect();
    const flights = cardsEls.map((c, i) => { const m = c.querySelector(".km3").getBoundingClientRect(); const s = src.find((x) => x.it.n === c.dataset.it) || src[0]; return { c, from: { x: s.x - (pr.top - terr.el.getBoundingClientRect().top), y: s.y }, to: { x: m.left, y: m.top, s: m.width }, it: s.it, delay: i * 14 }; });
    await play(620, (ms) => {
      g.clearRect(0, 0, innerWidth, innerHeight);
      for (const f of flights) {
        const u = CARRY(ms - f.delay);
        if (u > 0.97) { f.c.style.opacity = 1; f.c.style.clipPath = `inset(0 ${(1 - clamp((ms - f.delay - 260) / 200)) * 100}% 0 0)`; continue; }
        const x = lerp(f.from.x, f.to.x, u), y = lerp(f.from.y, f.to.y, u) - Math.sin(Math.PI * u) * 12, s = lerp(10, f.to.s, u);
        cells(g, [{ px: x, py: y, ps: s, it: f.it }], 0, 0, s, (c) => [P.used.has(c.it.n) ? C.mint : C.fam[c.it.f] || C.ink2, lerp(0.5, 1, u)]);
      }
    });
    cardsEls.forEach((c) => { c.style.opacity = ""; c.style.clipPath = ""; });
    ov.remove();
    if (focusItem) { const c = box.querySelector(`.card3[data-it="${CSS.escape(focusItem.n)}"]`); if (c) { c.classList.add("lit"); setTimeout(() => c.classList.remove("lit"), 900); } }
    onContext && onContext({ mode: "module", p: P, mod, D });
  }
  function fold() {
    open = null;
    const box = host.querySelector(".mod3");
    box.classList.remove("on", "page"); box.innerHTML = "";
    host.querySelector(".terr3").classList.remove("folded"); host.querySelector(".fbar").classList.remove("folded");
    onContext && onContext({ mode: "intro", p: P, D });
  }

  // ---------------------------------------------------------------- travelling in time
  async function travel(v, live) {
    at = v === P.version ? null : v;
    tk && tk.set(at);
    const pg = host.querySelector(".f3");
    pg.classList.toggle("past", !!at);
    const banner = host.querySelector(".when");
    state.clear();
    if (!at) { banner.innerHTML = ""; paintTerr(); refreshCards(); return; }
    const rel = T.releases.find((r) => r.v === at), pinR = T.releases.find((r) => r.v === P.version);
    const before = rel && pinR && rel.t && pinR.t ? new Date(rel.t) < new Date(pinR.t) : true;
    const dist = rel && pinR ? ago(rel.t).replace(" ago", "") : "";
    AT = await detail(`${P.name}@${at}`);
    if (!AT) { banner.innerHTML = `<span class="w1">${icon("marker")}Reading <b>${esc(at)}</b></span><span class="w2">not read yet: its names aren't in the index</span><span class="back">back to your pin <kbd>esc</kbd></span>`; paintTerr(); return; }
    const a = new Map(), b = new Map();
    for (const m of D.modules) for (const it of m.items) a.set(m.path + "::" + it.n, it);
    for (const m of AT.modules) for (const it of m.items) b.set(m.path + "::" + it.n, it);
    let n = 0, c = 0, g = 0, hit = 0;
    for (const [k, it] of a) { const y = b.get(k); if (!y) { state.set(it.n, before ? "absent" : "gone"); g++; if (P.used.has(it.n)) hit++; } else if (y.s !== it.s) { state.set(it.n, "chg"); c++; if (P.used.has(it.n)) hit++; } }
    for (const [k] of b) if (!a.has(k)) n++;
    const words = before ? [`${fmt(g)} of today's names didn't exist yet`, `${fmt(c)} looked different`, `${fmt(n)} have since gone`] : [`${fmt(n)} new names`, `${fmt(c)} changed`, `${fmt(g)} gone`];
    banner.innerHTML = `<span class="w1">${icon("marker")}Reading <b>${esc(at)}</b>, ${before ? `${esc(dist)} before` : "after"} your pin</span><span class="w2">${words.join(" · ")}</span>${hit ? `<span class="w3">${plural(hit, "name")} you use ${before ? "were different then" : "change"}</span>` : P.sites ? `<span class="w4">nothing you use changes</span>` : ""}<span class="back">back to your pin <kbd>esc</kbd></span>`;
    banner.querySelector(".back").onclick = () => travel(P.version, false);
    paintTerr(); refreshCards();
  }
  function refreshCards() {
    const box = host.querySelector(".mod3"); if (!open) return;
    const mod = modsOf().find((m) => m.path === open); if (!mod) return;
    box.querySelector(".cards3").innerHTML = mod.items.map((it) => card(it, mod.path)).join("");
    box.querySelectorAll(".card3").forEach((c) => (c.onclick = () => onSymbol && onSymbol(P, mod.items.find((x) => x.n === c.dataset.it), c, mod)));
  }

  async function render(p) {
    P = p; T = TRUST[p.id] || null; at = null; state.clear(); open = null;
    D = await detail(p.id);
    const mods = (D ? D.modules : p.modules).filter((m) => !m.private);
    const names = mods.reduce((s, m) => s + m.items.length, 0);
    const docd = D ? D.modules.flatMap((m) => m.items).filter((i) => i.d).length : 0;
    const allN = D ? D.modules.reduce((s, m) => s + m.items.length, 0) : 0;
    host.innerHTML = `<div class="f3">${hero(p)}
      <div class="berg-panel"><div class="berg-host"></div></div>
      <div class="tick3"><div class="tk-host"></div><div class="when"></div></div>
      <div class="fbar"></div>
      <div class="terr3"><div class="t3h"><b>${fmt(names)}</b> public names in <b>${plural(mods.length, "module")}</b>${p.sites ? ` · <span class="y">you use ${fmt(p.used.size)}</span>` : ""}${allN ? ` · documented ${Math.round((docd / allN) * 100)}%` : ""}${T && T.examples ? ` · ${plural(T.examples, "example")}` : ""}<span class="hint">hover a module to read it · click to open it</span></div><canvas></canvas><div class="mdoc"></div></div>
      <div class="mod3"></div>
      ${yours(p)}</div>`;
    if (T && T.releases) tk = ticker(host.querySelector(".tk-host"), T.releases, { pin: p.version, latest: p.latest || p.version, local: T.local_versions || [p.version], onTravel: (v, live) => { if (!live || (T.local_versions || []).includes(v)) travel(v, live); } });
    featureBar(host.querySelector(".fbar"), T);
    // The crest's instruments open inside themselves; the weight glyph opens the full berg under the hero.
    const w = host.querySelector(".cr.weight");
    w.onclick = () => {
      const panel = host.querySelector(".berg-panel");
      if (panel.classList.toggle("on")) iceberg(host.querySelector(".berg-host"), p, TRUST, { w: 1060, h: 150, onGo });
    };
    host.querySelector(".cr.heads").onclick = () => {
      const m = document.createElement("div"); m.className = "modal"; m.innerHTML = sheet(p, T); document.body.appendChild(m);
      requestAnimationFrame(() => m.classList.add("on"));
      const close = () => { m.classList.remove("on"); setTimeout(() => m.remove(), 200); };
      m.onclick = (e) => { if (e.target === m || e.target.closest(".x")) close(); };
      document.addEventListener("keydown", function k(e) { if (e.key === "Escape") { close(); document.removeEventListener("keydown", k); } });
    };
    onContext && onContext({ mode: "intro", p, D });
    mountTerr();
  }
  document.addEventListener("keydown", (e) => { if (e.key === "Escape" && host.isConnected) { if (at) travel(P.version, false); else if (open) fold(); } });
  return { render, travel, unfurl: (path) => { const r = terr.L.regions.find((x) => x.module.path === path); if (r) return r.module.items.length > BIG ? moduleView(r, null) : unfurl(r, null); }, get P() { return P; }, hoverTick: (v) => tk && tk.hover(v) };
}
