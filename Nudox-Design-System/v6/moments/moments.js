// The moments board: browse, add and hop through one app window, on this repository's real dependencies.
//   ?v=browse   the Library as strata; hover for the legit card, type to light, click unfurls into the page
//   ?v=package  the package page, legit-first; every package name is a preview; click one to go there
//   ?v=graph    the neighbourhood as a relay (what it rests on | it | who uses it); click to hop
//   ?v=add      the strata with a hand of registry cards; play one (click or drag it up)
// Forced states for stills: &hover=<name> &q=<query> &lens=versions &p=<name> &from=<name> &play=<name>
import { readTokens, gem, C } from "./paint.js";
import { world, below, why, fmt, plural } from "./world.js";
import { strata, shown } from "./strata.js";
import { pageView } from "./page.js";
import { folioView } from "./folio.js";
import { folio3 } from "./folio3.js";
import { previews, cardHtml } from "./preview.js";
import { staleness } from "./charts.js";
import { CARRY, play, lerp, clamp, ease, wait } from "./motion.js";

const Q = new URLSearchParams(location.search);
const view = Q.get("v") || "browse";
await document.fonts.ready;
await Promise.all(["700 40px 'Bricolage Grotesque'", "600 16px 'Bricolage Grotesque'", "500 11px 'Geist Mono'", "400 12px Geist", "italic 400 15px Newsreader"].map((f) => document.fonts.load(f)));
readTokens();
const WD = await world();
let VIA = {};
try { VIA = await (await fetch("data/via.json", { cache: "reload" })).json(); } catch { VIA = {}; }
let TRUST = {};
try { TRUST = await (await fetch("data/trust.json", { cache: "reload" })).json(); } catch { TRUST = {}; }
WD.trust = TRUST;
let LINES = {};
try { LINES = await (await fetch("data/via_lines.json", { cache: "reload" })).json(); } catch { LINES = {}; }
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
// A name can be several versions; prefer the one next to `near` (its dependency or user), else the direct one.
const find = (name, near = null) => {
  if (!name) return null;
  if (name.includes("@")) return WD.byId.get(name) || null;
  const all = WD.byName.get(name) || [];
  if (near) { const n = [...near.depsP, ...near.dependentsP].find((q) => q.name === name); if (n) return n; }
  return all.slice().sort((a, b) => a.depth - b.depth)[0] || null;
};
// What each direct dependency alone brings (removing it drops these), and each package's whole closure.
// (recomputed on use: adding a package changes it)
const only = { get: (id) => { const g = WD.groups.find((x) => x.via && (x.via.id || x.via) === id); return g ? g.pkgs : null; } };
const closure = new Map();
const closureOf = (p) => closure.get(p) || closure.set(p, below(p)).get(p);

// ---------------------------------------------------------------- the window
const NOTES = {
  browse: `<b>Browse: the Library as strata.</b> Your crates on top; what they rest on, most-used first; then everything beneath, one layer per step further from your code, hung under its parents like roots. Each package is its shingles in miniature, mint where your code names it. <b>Hover</b> a package for its legit card (releases, licence, size, heads-up, what it is) and your relation to it; strands show what it rests on and who uses it, and anything deep draws one bright line up to the crate of yours that pulls it in. Over the dense layers a <b>lens</b> reads names for you. <b>Colour by</b> one question across all 1,350 (<kbd>1</kbd> use <kbd>2</kbd> newer releases <kbd>3</kbd> licence <kbd>4</kbd> heads-up). <b>Type</b> to light every matching name in the library. <b>Click</b> unfurls a package: its shingles fly to their words.`,
  package: `<b>The package page.</b> The shingles are its intro: every public name at a glance, mint where your code uses it. <b>Click a module</b> and its shingles unfurl into cards, one per symbol, with badges instead of code (hover a badge and it says what it means). Above: one <b>release ticker</b>, the only place versions live. Move along it and the bars widen apart under you; click or drag to travel, and the page reads the package at that release (warmer, with a banner saying what was different then). Three instruments answer inside themselves: the <b>licence</b> stamp unfolds into what it permits and asks of you; the <b>heads-up</b> icons open into what the source does (network, files, programs, unsafe, build scripts); the <b>weight</b> is an iceberg: its own code above water, everything it pulls in beneath, one layer per step (hover a block). Below: how your code uses it, crate by crate, and its features as switches.`,
  discover: `<b>Discover: the registry, by aisle, with Add where you are.</b> Your project is the dock at the top: the crates it rests on, as blocks. Each aisle is a category; at its head, what you already have in it. <b>Hover</b> a card and the dock lights what it would share with you and marks where it would land. Press <b>+</b>: the cost flashes on the card, it flies into the dock, the dock makes room and its count ticks as it lands. Type to search every crate on disk.`,
  graph: `<b>Graph: the relay.</b> A package page whose top is its neighbourhood: what it rests on at left (where it comes from), what uses it at right (where it goes). Hover a user to light what it reaches. Click a neighbour: the whole relay moves one column. The chip grows into the hero, and the package you left shrinks into the column its relation puts it in. Shared neighbours stay put. The page arrives <b>through the relation</b> ("toml reaches 12 items of toml_edit"). The jump bar keeps the trail as a sentence: <b>→</b> rests on, <b>←</b> is used by.`,
  symbol: `<b>The symbol page.</b> One name, drawn the page's way: where it comes from on the left, where it goes on the right, the spine down the middle. A function is a <b>pipe</b> (its inputs converge on a plate, what it gives leaves right, failure drops below in coral); a type shows what <b>makes one</b> and what it <b>does</b>, its cases hanging below; a trait is a <b>socket</b> with a notch for each member you write. Its <b>sigil</b> carries facts: one prong per input, a coral drop if it can fail, one bar per case, one notch per member to write, one mint dot per crate of yours that names it. Hover any name and it widens to say what it is; stacked groups spread on hover and open on click. The heart is <b>your code and it</b>: <b>scrub</b> the reach bar and the member you're on lights up in the diagram and in every crate's <b>deck</b> of call sites (hover a deck to fan it, click for all of them). Scrub its <b>history</b> to read it at another release. Click a name to go there; Esc goes back, and at the start folds the page back into its card.`,
  symbol6: `<b>The symbol page, simple.</b> docs.rs's reading order, drawn. <b>The call</b> is a rail: each input is a port on it, and the rail runs down into what it gives. Fallibility is the shape of the rail's end: an arrow gives, a dashed ring is <i>or nothing</i>, a coral block is <i>or fails</i>, a clock is <i>later</i>. Generic parameters are violet pills: <b>hover one</b> for what it must be and what your workspace chooses for it (click a type to see those places). An enum is a fork (one of), a struct a bracket (holds), and nesting loops back. Methods group by what they do with it, yours first. <b>In your workspace</b> lists every place your packages name it, filtered by package and by verb (makes, reads, changes, holds…); click a line to open it. Pages: <a href="?v=symbol6&id=rs-from_str">from_str</a> · <a href="?v=symbol6&id=rs-Value">Value</a> · <a href="?v=symbol6&id=rs-as_str">as_str</a> · <a href="?v=symbol6&id=rs-AllocationInfo">AllocationInfo</a> · <a href="?v=symbol6&id=rs-Serialize">Serialize</a> · <a href="?v=symbol6&id=py-re.match">re.match</a> · <a href="?v=symbol6&id=js-which.sync">which.sync</a>.`,
  symbol5: `<b>The symbol page, dense.</b> Where it comes from and where it goes stay the idea, but the fan becomes a <b>ledger</b>: two columns of wrapped tokens meeting at a spine, each group a row that grows only as tall as its words. Every member carries a mint bar for how much your code reaches it, so the reach bar is gone and the weight is on the words themselves. <b>How it's used</b> is a matrix: who (your crates, then other packages) × which part of it, a square per pair sized by use. Click a square for its lines; hover a member in the ledger and its column lights. Functions (<a href="?v=symbol5&id=rs-serde_json-from_str">from_str</a>, <a href="?v=symbol5&id=py-re-match">re.match</a>, <a href="?v=symbol5&id=js-which">which</a>, <a href="?v=symbol5&id=ts-effect-map">Effect.map</a>) get the lab's plate, family and ways.`,
  add: `<b>Add: play a card.</b> A registry search dealt as a hand. Each card's corners are its cost to you: <b>new</b> packages it would bring and <b>shared</b> ones you already have. Hover lifts a card and marks where it would land. Play it (click, or drag it up) and it flies to its place in the strata. The band makes room; what it brings settles in beneath it; what it shares lights up. It lands unread (dashed), and its territory develops as the index reads it.`,
};
document.body.innerHTML = `<div class="tabs">${["browse", "discover", "package", "symbol", "symbol6", "graph", "add"].map((v) => `<a href="?v=${v}" class="${v === view ? "on" : ""}">${v}</a>`).join("")}<a href="Lab.html">symbol lab ↗</a></div>
<div class="note">${NOTES[view] || ""}</div>
<div class="win"><div class="tb"><div class="lights"><i></i><i></i><i></i></div><div class="jump"></div></div>
<div class="body"><div class="side2"></div><div class="reader"></div></div></div>`;
const jump = document.querySelector(".jump"), side = document.querySelector(".side2"), reader = document.querySelector(".reader");
// ?win=1024x700: size the window, to check how a page reflows
if (Q.get("win")) { const [w, h] = Q.get("win").split("x").map(Number); const wn = document.querySelector(".win"); wn.style.width = w + "px"; wn.style.height = h + "px"; }
// The symbol page (symbol.js, W-Sym) registers window.__onSymbol, so a module card opens its symbol.
const SYM = (await import("./symbol.js")).mountSymbols({ WD, TRUST, LINES, reader, jump, side, find });

// ---------------------------------------------------------------- the sidebar at library scope
function sidebar(S, hot) {
  const lenses = [["contents", "Contents", WD.all.length], ["versions", "Versions", WD.all.filter((p) => p.newer.length).length], ["rests", "Rests on", null], ["users", "Used by", WD.yours.length]];
  const row = (p) => {
    const g = S.lens === "versions" ? (p.latest ? `<span class="g am">→ ${esc(p.latest)}</span>` : `<span class="g"></span>`) : p.sites ? `<span class="g y">${fmt(p.sites)}</span>` : `<span class="g">${fmt(p.items)}</span>`;
    return `<div class="r ${hot === p ? "hot" : ""} ${p.depth === 1 && !p.sites && S.lens !== "versions" ? "dim" : ""}" data-id="${esc(p.id)}">${gem(12, p.kind === "yours" ? "var(--mint)" : "var(--k-ns)")}${esc(p.name)}${g}</div>`;
  };
  const list = S.lens === "versions" ? WD.direct.filter((p) => p.latest) : WD.direct;
  side.innerHTML = `<div class="scope">Library <span class="n">${fmt(WD.all.length)}</span></div>
    <div class="lens">${lenses.map(([k, w, n]) => `<span data-l="${k}" class="${S.lens === k ? "on" : ""}">${w}${S.lens === k && n != null ? `<i>${fmt(n)}</i>` : ""}</span>`).join("")}</div>
    <div class="hint">type to light · ${S.lens === "versions" ? "newer releases" : "most-used first"}</div>
    <div class="rows"><div class="hd">Yours <i>${WD.yours.length}</i></div><div class="hd">You rest on <i>${list.length}</i></div>${list.slice(0, 24).map(row).join("")}<div class="hd">Beneath <i>${fmt(WD.beneath.length)}</i></div></div>`;
}

// ---------------------------------------------------------------- the peek
function peekHtml(p) {
  const excl = only.get(p.id) || [];
  const all = closureOf(p).size;
  let use, cost;
  if (p.depth === 0) {
    use = `<div class="k">Its surface</div><div class="val">${fmt(p.items)}</div><div class="s">public items in ${plural(p.modules.length, "module")}</div>`;
    cost = `<div class="k">Rests on</div><div class="val">${fmt(p.depsP.length)}</div><div class="s">${fmt(all)} with everything beneath</div>`;
  } else if (p.depth === 1) {
    use = p.fresh
      ? `<div class="k">You use</div><div class="val">nothing yet</div><div class="s">just added · ${fmt(p.items)} public items to reach for</div>`
      : p.sites
      ? `<div class="k">You use</div><div class="val y">${fmt(p.used.size)} <span style="font:400 12px var(--ui);color:var(--ink3)">of ${fmt(p.items)}</span></div><div class="s">named ${plural(p.sites, "time")} by ${plural(Object.keys(p.uses.by || {}).length, "crate")}</div>`
      : `<div class="k">You use</div><div class="val co">never named</div><div class="s">maybe through a derive or macro</div>`;
    cost = `<div class="k">Only it brings</div><div class="val ${excl.length > 20 ? "co" : ""}">${fmt(excl.length)}</div><div class="s">${fmt(all)} beneath it in all</div>`;
  } else {
    const users = p.dependentsP.length;
    use = `<div class="k">Held up by</div><div class="val">${fmt(users)}</div><div class="s">package${users === 1 ? "" : "s"} rest directly on it</div>`;
    cost = `<div class="k">Rests on</div><div class="val">${fmt(all)}</div><div class="s">${fmt(p.items)} public items of its own</div>`;
  }
  const chain = p.depth > 0 ? why(p) : [];
  const whyHtml = chain.length > 1 ? `<div class="why">${chain.map((q, i) => `${i ? `<span class="a">→</span>` : ""}${i === chain.length - 1 ? `<b>${esc(shown(q))}</b>` : `<span class="${q.kind === "yours" ? "y" : ""}">${esc(shown(q))}</span>`}`).join("")}</div>` : "";
  return `<div class="t">${gem(18, p.kind === "yours" ? "var(--mint)" : "var(--k-ns)")}${esc(p.name)}<span class="v">${esc(p.version)}</span>${p.latest ? `<span class="am">→ ${esc(p.latest)}</span>` : ""}</div>
    ${p.lede ? `<div class="d">${esc(p.lede)}</div>` : ""}<div class="corners"><div class="corner">${use}</div><div class="corner">${cost}</div></div>${whyHtml}
    <div class="keys"><span><span class="kb">↵</span> open</span><span><span class="kb">space</span> keep</span>${p.dup ? `<span style="color:var(--amber)">two versions in the tree</span>` : ""}</div>`;
}

function mineLine(p) {
  const chain = p.depth > 0 ? why(p) : [];
  const g = p.depth === 1 ? WD.groups.find((x) => x.via === p) : null;
  const dies = g && g.pkgs.length ? `<span class="co">remove it and ${plural(g.pkgs.length, "package")} go with it</span>` : p.depth === 1 ? `<span>removing it drops nothing else</span>` : "";
  const use = p.depth === 1 ? (p.fresh ? `just added` : p.sites ? `<b class="y">you use ${fmt(p.used.size)} of ${fmt(p.items)}</b>, ${plural(p.sites, "time")}` : `<b class="co">never named in your code</b>`) : "";
  const whyHtml = chain.length > 2 ? `<span class="why">${chain.map((q, i) => `${i ? `<span class="a">→</span>` : ""}${i === chain.length - 1 ? `<b>${esc(shown(q))}</b>` : `<span class="${q.kind === "yours" ? "y" : ""}">${esc(shown(q))}</span>`}`).join("")}</span>` : "";
  return use || whyHtml || dies ? `<div class="pv-mine">${use}${dies}${whyHtml}</div>` : "";
}

// ---------------------------------------------------------------- browse
async function browse() {
  jump.innerHTML = `<b>Library</b>`;
  reader.innerHTML = `<div class="lib-h"><h1>Library</h1><div class="facts"></div><div class="stale-w"><span class="k">How old your pins are</span>${staleness(WD.direct, TRUST, { w: 900, h: 40 })}</div><div class="overs"></div></div><div class="narrowq"><span class="q"></span><span class="caret"></span><span class="n"></span></div><div class="peek2"></div>`;
  const newer = WD.all.filter((p) => p.newer.length).length, dups = new Set(WD.all.filter((p) => p.dup).map((p) => p.name)).size;
  const unnamed = WD.direct.filter((p) => !p.sites).length;
  reader.querySelector(".facts").innerHTML = [`<b>${fmt(WD.all.length)}</b> packages`, `<b>${WD.yours.length}</b> yours`, `<b>${WD.direct.length}</b> you rest on`, `<b>${fmt(WD.beneath.length)}</b> beneath`,
    `<span class="am"><b style="color:inherit">${fmt(newer)}</b> have newer releases</span>`, `<b>${dups}</b> in two versions`, unnamed ? `<span class="co"><b style="color:inherit">${unnamed}</b> you never name</span>` : ""].filter(Boolean).join(`<i>·</i>`);
  const peek = reader.querySelector(".peek2"), nq = reader.querySelector(".narrowq");
  let peekTimer = 0;
  const S0 = { q: Q.get("q") || "", lens: Q.get("lens") || "contents", scroll: +(Q.get("scroll") || 0) };
  if (Q.get("at")) { const [ax, ay] = Q.get("at").split(",").map(Number); S0.at = { x: ax, y: ay }; }
  let st = null;
  st = strata(reader, WD, {
    state: S0,
    onHover(p, r) {
      clearTimeout(peekTimer); peek.classList.remove("on");
      sidebar(st ? st.S : S0, p);
      if (!p) return;
      peekTimer = setTimeout(() => {
        peek.innerHTML = cardHtml(p, TRUST) + mineLine(p);
        const rr = reader.getBoundingClientRect();
        let x = r.x - rr.left + r.w + 18, y = r.top - rr.top - 8;
        if (x + 350 > rr.width) x = r.x - rr.left - 358;
        if (x < 8) x = 8;
        Object.assign(peek.style, { left: x + "px", top: "0px" });
        const ph = peek.getBoundingClientRect().height;
        y = Math.max(8, Math.min(rr.height - ph - 8, y));
        peek.style.top = y + "px";
        peek.classList.add("on");
      }, 280);
    },
    onOpen(p, api) { openPackage(p, api); },
    onScroll(y) { const h = reader.querySelector(".lib-h"); if (h) h.style.transform = `translateY(${-y}px)`; },
  });
  sidebar(st.S, null);
  const narrow = () => {
    nq.classList.toggle("on", !!st.S.q);
    nq.querySelector(".q").textContent = st.S.q;
    const h = st.hits();
    nq.querySelector(".n").innerHTML = h.items ? `<b>${fmt(h.items)}</b> items in <b>${fmt(h.pkgs)}</b> packages · esc` : "nothing · esc";
  };
  narrow();
  // Colour the whole library by one question: 1 use · 2 releases · 3 licence · 4 heads-up.
  const OV = [["contents", "what you use"], ["versions", "newer releases"], ["licence", "licence"], ["heads", "heads-up"]];
  const overs = reader.querySelector(".overs");
  const drawOvers = () => {
    const c = st.S.lens === "licence" ? st.count("licence") : st.S.lens === "heads" ? st.count("heads") : null;
    const legend = st.S.lens === "licence" ? `<span class="lg"><i class="co"></i>${fmt(c.get("coral") || 0)} copyleft or unlicensed<i class="am"></i>${fmt(c.get("amber") || 0)} weak copyleft or unrecognised</span>`
      : st.S.lens === "heads" ? `<span class="lg"><i class="am"></i>${fmt(c.get("build") || 0)} run code when you build<i class="pe"></i>${fmt(c.get("reach") || 0)} reach the network or start programs</span>`
      : st.S.lens === "versions" ? `<span class="lg"><i class="am"></i>amber: a newer release exists; bright: you use a name in it</span>` : `<span class="lg"><i class="mi"></i>mint: names your code uses</span>`;
    overs.innerHTML = `<span class="k">Colour by</span>${OV.map(([k, w], i) => `<b data-o="${k}" class="${st.S.lens === k ? "on" : ""}"><kbd>${i + 1}</kbd>${w}</b>`).join("")}${legend}`;
    overs.querySelectorAll("[data-o]").forEach((b) => (b.onclick = () => { st.set({ lens: b.dataset.o }); drawOvers(); sidebar(st.S, null); }));
  };
  drawOvers();
  side.addEventListener("mouseover", (e) => { const r = e.target.closest(".r"); if (r) { const p = WD.byId.get(r.dataset.id); st.reveal(p); st.hover(p); } });
  side.addEventListener("click", (e) => {
    const l = e.target.closest("[data-l]"); if (l && (l.dataset.l === "contents" || l.dataset.l === "versions")) { st.set({ lens: l.dataset.l }); sidebar(st.S, null); drawOvers(); }
    const r = e.target.closest(".r"); if (r) openPackage(WD.byId.get(r.dataset.id), st);
  });
  document.onkeydown = (e) => {
    if (e.metaKey || e.ctrlKey) return;
    if (e.key === "Escape") { st.set({ q: "" }); narrow(); return; }
    if (e.key === "Backspace") { st.set({ q: st.S.q.slice(0, -1) }); narrow(); return; }
    if (!st.S.q && /^[1-4]$/.test(e.key)) { st.set({ lens: OV[+e.key - 1][0] }); drawOvers(); sidebar(st.S, null); return; }
    if (e.key.length === 1 && /[\w:]/.test(e.key)) { st.set({ q: st.S.q + e.key }); narrow(); }
  };
  const h = find(Q.get("hover"));
  if (h) { st.reveal(h); st.hover(h); }
  const o = find(Q.get("open"));
  if (o) { st.reveal(o); st.hover(o); await wait(+(Q.get("after") || 700)); openPackage(o, st); }
  return st;
}

// ---------------------------------------------------------------- Browse → package: the shingles unfurl into words
// Each shingle flies from the block to its own word's mark on the page, then its word unrolls from the
// mark. The name plate grows into the title. Facts and prose unroll in reading order behind them.
async function openPackage(p, st) {
  const src = st.cellsOf(p);
  const lr = st.rectOf(p);
  document.onkeydown = null;
  reader.querySelector(".peek2").classList.remove("on");
  const host = document.createElement("div"); host.style.cssText = "position:absolute;inset:0;opacity:0"; reader.appendChild(host);
  const fv = makeFolio(host);
  await fv.render(p);
  const words = new Map();
  host.querySelectorAll(".f-contents .mrow").forEach((row) => { const mod = row.querySelector(".mp").firstChild.textContent; row.querySelectorAll(".w").forEach((w) => words.set(mod + "::" + w.dataset.it, w)); });
  const title = host.querySelector(".f-hero h1"), g = host.querySelector(".f-hero .hg");
  const hr = title.getBoundingClientRect(), gr = g.getBoundingClientRect();
  const ov = document.createElement("canvas"); ov.style.cssText = "position:fixed;left:0;top:0;pointer-events:none;z-index:40";
  document.body.appendChild(ov);
  const ctx = ov.getContext("2d"); const dpr = devicePixelRatio || 1;
  ov.width = innerWidth * dpr; ov.height = innerHeight * dpr; ov.style.width = innerWidth + "px"; ov.style.height = innerHeight + "px"; ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  const cx = lr.x + lr.w / 2, cy = lr.y + lr.h / 2;
  const fl = src.map((s2) => { const w = words.get(s2.c.m.path + "::" + s2.c.it.n); const m = w && w.querySelector("i"); const r = m && m.getBoundingClientRect(); return r && r.top < innerHeight + 40 ? { s: s2, d: { x: r.left, y: r.top, s: r.width }, w, delay: Math.hypot(s2.x - cx, s2.y - cy) * 0.3 } : null; }).filter(Boolean);
  const flying = new Set(fl.map((f) => f.w));
  const plate = document.createElement("div"); plate.className = "ghosts"; plate.innerHTML = `<div class="fly nm">${esc(p.name)}</div><div class="fly">${gem(56, p.kind === "yours" ? "var(--mint)" : "var(--k-ns)")}</div>`;
  document.body.appendChild(plate);
  const [nm, gm] = plate.children;
  const unroll = [...host.querySelectorAll(".f-hero .lede, .f-hero .f-by, .f-ver, .fact, .f-about, .f-contents .f-sh, .f-comb, .mrow .mp, .f-rel")];
  unroll.forEach((el) => (el.style.opacity = "0"));
  const allWords = [...words.values()];
  allWords.forEach((w) => { w.style.opacity = "0"; });
  title.style.visibility = "hidden"; g.style.visibility = "hidden";
  host.style.opacity = "1";
  await play(1100, (ms) => {
    st.fade(1 - clamp(ms / 200));
    const lh = reader.querySelector(".lib-h"); if (lh) lh.style.opacity = 1 - clamp(ms / 160);
    ctx.clearRect(0, 0, innerWidth, innerHeight);
    const groups = new Map();
    for (const f of fl) {
      const u = CARRY(ms - f.delay);
      if (u > 0.985) { if (!f.done) { f.done = true; f.t = ms; } continue; }
      const sz = lerp(f.s.s, f.d.s, u), x = lerp(f.s.x, f.d.x, u), y = lerp(f.s.y, f.d.y, u) - Math.sin(Math.PI * u) * 16;
      const mine = p.used.has(f.s.c.it.n);
      const key = (mine ? C.mint : C.fam[f.s.c.it.f] || C.ink2) + "|" + lerp(f.s.tint ? f.s.tint[1] : 0.3, 1, u).toFixed(2);
      (groups.get(key) || groups.set(key, []).get(key)).push([x, y, sz]);
    }
    for (const [key, list] of groups) {
      const [col, a] = key.split("|"); ctx.fillStyle = col; ctx.globalAlpha = +a; ctx.beginPath();
      for (const [x, y, sz] of list) { const c = Math.max(1.2, sz * 0.3); ctx.moveTo(x + c, y); ctx.lineTo(x + sz, y); ctx.lineTo(x + sz, y + sz - c); ctx.lineTo(x + sz - c, y + sz); ctx.lineTo(x, y + sz); ctx.lineTo(x, y + c); ctx.closePath(); }
      ctx.fill();
    }
    ctx.globalAlpha = 1;
    // A landed shingle becomes its word's mark; the word unrolls to its right.
    for (const f of fl) if (f.done) { const e = clamp((ms - f.t) / 180); f.w.style.opacity = 1; f.w.style.clipPath = `inset(0 ${(1 - e) * 100}% 0 0)`; }
    for (const w of allWords) if (!flying.has(w)) w.style.opacity = clamp((ms - 520) / 260);
    const u = CARRY(ms - 40);
    const size = lerp(11, 40, u);
    Object.assign(nm.style, { left: lerp(lr.x, hr.left, u) + "px", top: lerp(lr.top - 2, hr.top, u) + "px", font: `700 ${size}px/1.1 "Bricolage Grotesque"`, letterSpacing: `${-0.03 * size}px`, color: "var(--ink0)" });
    const gs = lerp(12, 56, u);
    Object.assign(gm.style, { left: lerp(lr.x - 16, gr.left, u) + "px", top: lerp(lr.top - 2, gr.top, u) + "px", width: gs + "px", height: gs + "px", opacity: clamp(ms / 120) });
    unroll.forEach((el) => { const r = el.getBoundingClientRect(); el.style.opacity = clamp((ms - 200 - Math.max(0, r.top - hr.top) * 0.45) / 220); });
  });
  title.style.visibility = ""; g.style.visibility = "";
  unroll.forEach((el) => (el.style.opacity = "")); allWords.forEach((w) => { w.style.opacity = ""; w.style.clipPath = ""; });
  plate.remove(); ov.remove(); st.el.remove();
  for (const el of [...reader.children]) if (el !== host) el.remove();
  host.style.cssText = "position:absolute;inset:0";
  trail = [{ p, rel: null }]; drawTrail2(fv); folioSide(p); activeFolio = fv;
  document.onkeydown = (e) => { if (e.key === "Escape") location.href = "?v=browse&hover=" + encodeURIComponent(p.name); };
}

// ---------------------------------------------------------------- the folio: every package name goes somewhere
let trail2 = [];
function drawTrail2(fv) {
  jump.innerHTML = [`<span class="c" data-lib="1">Library</span><span class="rel">›</span>`].concat(trail.map((t, i) => `${i ? `<span class="rel ${t.rel === "→" ? "d" : ""}">${t.rel}</span>` : ""}${i === trail.length - 1 ? `<b>${esc(t.p.name)}</b>` : `<span class="c" data-i="${i}">${esc(t.p.name)}</span>`}`)).join(" ");
  jump.onclick = async (e) => {
    const c = e.target.closest(".c"); if (!c) return;
    if (c.dataset.lib) { location.href = "?v=browse&hover=" + encodeURIComponent(trail[trail.length - 1].p.name); return; }
    const i = +c.dataset.i; const target = trail[i].p;
    trail = trail.slice(0, i + 1);
    await fv.hop(target, c); drawTrail2(fv); folioSide(target);
  };
}
function folioSide(p) {
  const T = TRUST[p.id];
  const rel = T && T.releases ? T.releases.slice().reverse().slice(0, 8) : [];
  side.innerHTML = `<div class="scope"><span class="up">‹ Library</span></div>
    <div class="scope" style="padding-top:4px">${gem(22, p.kind === "yours" ? "var(--mint)" : "var(--k-ns)")}${esc(p.name)}</div>
    <div class="lens"><span class="on">Contents<i>at ${esc(p.version)}</i></span><span>Rests on<i>${p.depsP.length}</i></span><span>Used by</span></div>
    <div class="hint">the contents at a release · type to narrow</div>
    <div class="rows"><div class="hd">Releases <i>${T ? T.n || T.releases.length : ""}</i></div>${rel.map((r) => `<div class="r ${r.v === p.version ? "cur" : ""} ${r.y ? "dim" : ""}"><span class="km2 value" style="transform:scale(.7)"></span>${esc(r.v)}<span class="g ${r.v === p.version ? "y" : r.v === p.latest ? "am" : ""}">${r.v === p.version ? "your pin" : r.y ? "yanked" : (r.t || "").slice(0, 7)}</span></div>`).join("")}
    <div class="hd">Modules <i>${p.modules.length}</i></div>${p.modules.slice(0, 14).map((m) => `<div class="r"><span class="km2 ${m.items[0] ? m.items[0].f : "value"}" style="transform:scale(.8)"></span>${esc(m.path)}<span class="g">${m.items.length}</span></div>`).join("")}</div>`;
}
function makeFolio(host) {
  const fv = folioView(host, WD, { TRUST, onGraph: (p) => { location.href = "?v=graph&p=" + encodeURIComponent(p.id); } });
  return fv;
}
async function packagePage3() {
  reader.innerHTML = "";
  const host = document.createElement("div"); host.style.cssText = "position:absolute;inset:0"; reader.appendChild(host);
  const fv = folio3(host, WD, { TRUST, LINES,
    onSymbol: (p, it, el, mod) => { window.__onSymbol && window.__onSymbol(p, it, el, mod); },
    onContext: (c) => side4(c),
    onGo: (q) => { location.href = "?v=package&p=" + encodeURIComponent(q.id); } });
  const p = find(Q.get("p")) || find("toml");
  await fv.render(p);
  trail = [{ p, rel: null }]; jump.innerHTML = `<span class="c">Library</span><span class="rel">›</span><b>${esc(p.name)}</b>`;
  if (Q.get("at")) await fv.travel(Q.get("at"), false);
  if (Q.get("mod")) await fv.unfurl(Q.get("mod"));
  if (Q.get("tick")) fv.hoverTick(Q.get("tick"));
  if (Q.get("feat")) { const el = host.querySelector(`.fb[data-f="${CSS.escape(Q.get("feat"))}"]`); if (el) el.click(); }
  if (Q.get("scrollto")) { const el = host.querySelector(Q.get("scrollto")); if (el) host.querySelector(".f3").scrollTop = el.offsetTop - 40; }
  return fv;
}
// The sidebar follows what the page is showing instead of repeating it. On the package's intro the page
// already shows its modules, so the sidebar holds what the page doesn't: where you came from, what you
// hold, and the crates of yours that lean on it. With a module open it lists that module's symbols, the
// way you'd walk them with the keyboard.
function side4(c) {
  const p = c.p;
  const head = `<div class="scope"><span class="up">‹ Library</span></div><div class="scope" style="padding-top:4px">${gem(22, p.kind === "yours" ? "var(--mint)" : "var(--k-ns)")}${esc(p.name)}${c.mode === "module" ? `<span class="n">:: ${esc(c.mod.path)}</span>` : ""}</div>`;
  if (c.mode === "module") {
    side.innerHTML = head + `<div class="hint">${c.mod.items.length} symbols · ↑↓ to walk · ↵ to open</div><div class="rows">${c.mod.items.slice(0, 30).map((it) => `<div class="r"><span class="km2 ${it.f}" style="transform:scale(.8)"></span>${esc(it.n)}${p.used.has(it.n) ? `<span class="g y">${fmt(p.used.get(it.n))}</span>` : ""}</div>`).join("")}</div>`;
    return;
  }
  const by = p.uses && p.uses.by ? Object.entries(p.uses.by).sort((a, b) => b[1] - a[1]) : [];
  side.innerHTML = head + `<div class="hint">what the page doesn't show</div><div class="rows">
    <div class="hd">Your crates on it <i>${by.length}</i></div>${by.slice(0, 8).map(([k, n]) => `<div class="r">${gem(12, "var(--mint)")}${esc(k.replace(/^backend-/, ""))}<span class="g y">${fmt(n)}</span></div>`).join("") || `<div class="r dim">none name it directly</div>`}
    <div class="hd">Came in through <i>${p.dependentsP.length}</i></div>${p.dependentsP.slice(0, 6).map((d) => `<div class="r"><a class="pk" data-pkg="${esc(d.id)}">${esc(d.name.replace(/^backend-/, ""))}</a><span class="g">${esc(d.version)}</span></div>`).join("")}
    <div class="hd">Trail</div><div class="r dim">Library › ${esc(p.name)}</div></div>`;
}
// The package's sidebar, without repeating the page: its modules (the shingle regions), nothing else.
function side3(p) {
  side.innerHTML = `<div class="scope"><span class="up">‹ Library</span></div>
    <div class="scope" style="padding-top:4px">${gem(22, p.kind === "yours" ? "var(--mint)" : "var(--k-ns)")}${esc(p.name)}</div>
    <div class="lens"><span class="on">Contents</span><span>Rests on<i>${p.depsP.length}</i></span><span>Used by</span></div>
    <div class="hint">type to narrow</div>
    <div class="rows">${p.modules.slice(0, 24).map((m) => { const n = m.items.filter((it) => p.used.has(it.n)).length; return `<div class="r"><span class="km2 ${m.items[0] ? m.items[0].f : "value"}" style="transform:scale(.8)"></span>${esc(m.path)}<span class="g ${n ? "y" : ""}">${n ? `${n} of ` : ""}${m.items.length}</span></div>`; }).join("")}</div>`;
}
async function packagePage() {
  reader.innerHTML = "";
  const host = document.createElement("div"); host.style.cssText = "position:absolute;inset:0"; reader.appendChild(host);
  const fv = makeFolio(host);
  const p = find(Q.get("p")) || find("toml");
  await fv.render(p);
  trail = [{ p, rel: null }]; drawTrail2(fv); folioSide(p);
  if (Q.get("at")) fv.showAt(Q.get("at"));
  if (Q.get("fact")) { const f = host.querySelector(`[data-fact="${Q.get("fact")}"]`); f && f.onmouseenter(); }
  if (Q.get("map")) host.querySelectorAll(".f-toggle b")[1].click();
  if (Q.get("scrollto")) { const el = host.querySelector(Q.get("scrollto")); if (el) host.querySelector(".folio2").scrollTop = el.offsetTop - 60; }
  // &pv=a,b,c opens a chain of previews: a from the page, b from a's card, c from b's.
  if (Q.get("pv")) {
    let scope = host;
    for (const [i, name] of Q.get("pv").split(",").entries()) {
      const a = [...scope.querySelectorAll("a.pk")].find((x) => x.textContent === name); if (!a) break;
      if (i === 0) { const pg = host.querySelector(".folio2"); pg.scrollTop = Math.max(0, a.offsetTop - 200); }
      PV.open(a, i); scope = PV.stack[i].el;
    }
  }
  return fv;
}
let activeFolio = null;
const PV = previews({ WD, TRUST, onGo: async (p, anchor) => {
  const fv = activeFolio; if (!fv || !fv.P || fv.busy) return;
  fv.busy = true;
  const cur = fv.P;
  trail.push({ p, rel: cur.depsP.includes(p) ? "→" : cur.dependentsP.includes(p) ? "←" : "·" });
  await fv.hop(p, anchor); drawTrail2(fv); folioSide(p);
  fv.busy = false;
} });

// ---------------------------------------------------------------- the page + trail
let trail = [];
function drawTrail(pv) {
  jump.innerHTML = trail.map((t, i) => `${i ? `<span class="rel ${t.rel === "→" ? "d" : ""}">${t.rel}</span>` : `<span class="c" data-lib="1">Library</span><span class="rel">›</span>`}${i === trail.length - 1 ? `<b>${esc(t.p.name)}</b>` : `<span class="c" data-i="${i}">${esc(t.p.name)}</span>`}`).join(" ");
  jump.onclick = (e) => {
    const c = e.target.closest(".c"); if (!c) return;
    if (c.dataset.lib) { location.href = "?v=browse&hover=" + encodeURIComponent(trail[trail.length - 1].p.name); return; }
    const i = +c.dataset.i; const target = trail[i].p;
    trail = trail.slice(0, i + 1);
    const cur = pv.P;
    if (cur.depsP.includes(target) || cur.dependentsP.includes(target)) { trail.pop(); pv.hop(target).then(() => {}); }
    else { pv.show(target, trail[i - 1] ? trail[i - 1].p : null, { animate: true }); drawTrail(pv); }
  };
}
function bookSide(p, fromP) {
  const lit = fromP && VIA ? new Set(VIA[(p.dependentsP.includes(fromP) ? fromP.id + ">" + p.id : "")] || (fromP.kind === "yours" ? [...p.used.keys()] : [])) : new Set();
  side.innerHTML = `<div class="scope"><span class="up">‹ Library</span></div>
    <div class="scope" style="padding-top:4px">${gem(22, p.kind === "yours" ? "var(--mint)" : "var(--k-ns)")}${esc(p.name)} <span class="n">${esc(p.version)}</span></div>
    <div class="lens"><span class="on">Contents<i>${fmt(p.modules.length)}</i></span><span>Versions</span><span>Rests on</span><span>Used by</span></div>
    <div class="hint">${lit.size ? `${fmt(lit.size)} reached by ${esc(fromP.short || fromP.name)} · type to narrow` : "type to narrow"}</div>
    <div class="rows">${p.modules.slice(0, 26).map((m) => { const n = m.items.filter((it) => lit.has(it.n)).length; return `<div class="r ${n ? "" : lit.size ? "dim" : ""}"><span class="km2 ${m.items[0] ? m.items[0].f : "value"}" style="transform:scale(.8)"></span>${esc(m.path)}<span class="g ${n ? "y" : ""}" style="${n ? "color:var(--peri-hi)" : ""}">${n ? n + " of " : ""}${m.items.length}</span></div>`; }).join("")}</div>`;
}
function makePage(host) {
  const pv = pageView(host, WD, {
    VIA, LINES,
    onChain: (p, fromP) => bookSide(p, fromP),
    onHop: async (Qp) => {
      if (pv.busy) return; pv.busy = true;
      const cur = pv.P;
      const rel = cur.depsP.includes(Qp) ? "→" : "←";
      trail.push({ p: Qp, rel });
      drawTrail(pv);
      await pv.hop(Qp);
      pv.busy = false;
    },
  });
  return pv;
}
function pageKeys(pv) {
  document.onkeydown = (e) => { if (e.key === "Escape" && !pv.from && trail.length <= 1) location.href = "?v=browse&hover=" + encodeURIComponent(pv.P.name); };
}
async function hop() {
  reader.innerHTML = "";
  const host = document.createElement("div"); host.style.cssText = "position:absolute;inset:0"; reader.appendChild(host);
  const pv = makePage(host);
  const p = find(Q.get("p")) || find("toml") || WD.direct[0];
  // Arrive from the crate of yours that uses it most, so the page opens through that relation.
  const byUse = p.uses && p.uses.by ? Object.entries(p.uses.by).sort((a, b) => b[1] - a[1]) : [];
  const from = find(Q.get("from"), p) || (byUse.length ? WD.yours.find((y) => y.name === byUse[0][0]) : null);
  trail = from ? [{ p: from, rel: null }, { p, rel: "→" }] : [{ p, rel: null }];
  await pv.show(p, from, { animate: true });
  drawTrail(pv);
  pageKeys(pv);
  const next = find(Q.get("to"), p);
  if (next) { await wait(+(Q.get("after") || 1200)); trail.push({ p: next, rel: p.depsP.includes(next) ? "→" : "←" }); drawTrail(pv); await pv.hop(next); }
}

if (view === "browse") await browse();
else if (view === "package") await packagePage3();
else if (view === "discover") { const { discover } = await import("./discover.js"); await discover({ WD, TRUST, reader, side, jump, Q }); }
else if (view === "symbol") await SYM.route(Q);
else if (view === "symbol6") { const { symbol6 } = await import("./symbol6.js"); await symbol6({ reader, jump, side, Q }); window.addEventListener("popstate", () => location.reload()); }
else if (view === "symbol5") { const { symbol5 } = await import("./symbol5.js"); await symbol5({ SYM, reader, jump, side, Q }); }
else if (view === "package2") activeFolio = await packagePage();
else if (view === "graph" || view === "hop") await hop();
else if (view === "add") { const { add } = await import("./hand.js"); const st = await browse(); await add({ WD, reader, st, Q, closureOf }); }
