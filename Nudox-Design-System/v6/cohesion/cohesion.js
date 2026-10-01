// The cohesion board: one set of objects (package, module, item, relation) seen from each view.
// ?v=package|symbol|graph|find|compare|tree   forced states: &hover=<item> &region=<module> &q=<query>
import * as T from "./territory.js";
const SIDE = /^(side-|film-browse)/.test(new URLSearchParams(location.search).get("v") || "") ? await import("./sideviews.js") : null;

const q = new URLSearchParams(location.search);
const view = q.get("v") || "package";
const load = async (f) => (await fetch(f, { cache: "reload" })).json();
const P = await load("data/present.json");
P.items.forEach((it, i) => { it.gid = i; });
const MODS = P.modules.map((m) => ({ path: m.path || "lib", items: m.items.map((i) => P.items[i]) }));
const modOf = new Map(); MODS.forEach((m) => m.items.forEach((it) => modOf.set(it, m)));
const WRITTEN = new Set(["has", "takes", "gives", "is", "derives", "type", "impl"]);
const rels = new Map(); // item -> [{other, kind, dir}]
for (const [a, b, k] of P.rel) {
  const A = P.items[a], B = P.items[b];
  (rels.get(A) || rels.set(A, []).get(A)).push({ other: B, kind: k, dir: "out" });
  (rels.get(B) || rels.set(B, []).get(B)).push({ other: A, kind: k, dir: "in" });
}
const outs = new Map(); for (const [a, pkg, n] of P.out) (outs.get(P.items[a]) || outs.set(P.items[a], []).get(P.items[a])).push({ pkg: pkg.replace(/^backend-/, ""), n });
const byName = (n) => P.items.find((it) => it.n === n);
const fanIn = new Map(); for (const [, b] of P.rel) fanIn.set(P.items[b], (fanIn.get(P.items[b]) || 0) + 1);
const esc = T.esc;

const stage = document.getElementById("stage");
const note = document.getElementById("note");

// ---------------------------------------------------------------- chrome
const lights = `<div class="lights"><i></i><i></i><i></i></div>`;
function gem(size = 56, glyph = true) {
  const F = [[24,2,35,13,24,10,.4],[24,10,35,13,38,24,.3],[35,13,46,24,38,24,.34],[46,24,35,35,38,24,.14],[38,24,35,35,24,38,.08],[35,35,24,46,24,38,.11],[24,46,13,35,24,38,.22],[24,38,13,35,10,24,.2],[13,35,2,24,10,24,.3],[2,24,13,13,10,24,.56],[10,24,13,13,24,10,.5],[13,13,24,2,24,10,.66]];
  const c = "var(--k-ns)";
  return `<svg width="${size}" height="${size}" viewBox="0 0 48 48" style="flex:none;color:${c}">${F.map(([a,b,c2,d,e,f,t]) => `<polygon points="${a},${b} ${c2},${d} ${e},${f}" fill="currentColor" fill-opacity="${t}"/>`).join("")}<path d="M24 2 46 24 24 46 2 24z" fill="none" stroke="currentColor"/><path d="M24 10 38 24 24 38 10 24z" fill="none" stroke="currentColor" stroke-width=".7" stroke-opacity=".55"/>${glyph ? `<g transform="translate(17.4 17.4) scale(.55)" fill="none" stroke="currentColor" stroke-width="2.4"><path d="m12 3 8 4.5v9L12 21l-8-4.5v-9z"/><path d="m4 7.5 8 4.5 8-4.5M12 12v9"/></g>` : ""}</svg>`;
}
const win = (jump, body) => `<div class="win"><div class="tb">${lights}${jump}</div><div class="body">${body}</div></div>`;
const jumpbar = (crumbs) => `<div class="jump">${crumbs.map((c, i) => (i ? `<span class="sep">›</span>` : "") + (i === crumbs.length - 1 ? `<b>${esc(c)}</b>` : `<span>${esc(c)}</span>`)).join("")}</div>`;

// ---------------------------------------------------------------- the package folio
function shelfFor(state) {
  const rows = MODS.map((m) => `<div class="row ${state.hotRegion === m ? "hot" : ""}" data-m="${esc(m.path)}" data-share="shelf:${esc(m.path)}"><span>${esc(m.path)}</span><span class="ct">${m.items.length}</span></div>`).join("");
  return `<div class="shelf"><div class="up">‹ Library</div><div class="book">${gem(28, false)}<div><div class="bn">present</div><div class="bv">0.1.0</div></div></div>${rows}</div>`;
}

function strands(t, hot) {
  const a = T.center(t, hot); if (!a) return "";
  let s = `<svg class="strands" width="${t.w}" height="${t.h}">`;
  const seen = new Set();
  for (const r of rels.get(hot) || []) {
    if (seen.has(r.other)) continue; seen.add(r.other);
    const b = T.center(t, r.other); if (!b) continue;
    const dx = b.x - a.x, dy = b.y - a.y, d = Math.hypot(dx, dy) || 1;
    const bow = Math.min(60, d * 0.22), cx = (a.x + b.x) / 2 - (dy / d) * bow, cy = (a.y + b.y) / 2 + (dx / d) * bow * (dx < 0 ? -1 : 1) - bow * 0.3;
    s += `<path class="${WRITTEN.has(r.kind) ? "" : "arr"}" d="M${a.x} ${a.y}Q${cx} ${cy} ${b.x} ${b.y}"/>`;
  }
  const o = outs.get(hot) || [];
  o.forEach((x, i) => {
    const y = 14 + i * 20, x2 = t.w + 14;
    s += `<path class="out arr" d="M${a.x} ${a.y}C${a.x + 80} ${a.y} ${x2 - 60} ${y} ${x2} ${y}"/><text x="${x2 + 6}" y="${y + 4}">${esc(x.pkg)} <tspan class="n">×${x.n}</tspan></text>`;
  });
  return s + `</svg>`;
}

function peek(t, it) {
  const c = T.center(t, it); if (!c) return "";
  const rs = rels.get(it) || []; const kinds = new Set(rs.map((r) => r.other));
  // Below the map, under its shingle: the map (and the strands) stay in view.
  const left = Math.max(0, Math.min(784 - 360, c.x - 40));
  return `<i class="tick" style="left:${c.x}px;top:${c.y + 9}px;height:${t.h + 12 - c.y - 9}px"></i><div class="peek" style="left:${left}px;top:${t.h + 12}px">
    <div class="t">${T.mark(it.f, 12)}<span data-share="name:${esc(it.n)}" data-text="1">${esc(it.n)}</span><span class="k">${esc(it.k)} · ${esc(modOf.get(it).path)}</span></div>
    <div class="s">${esc(it.s)}</div>${it.d ? `<div class="d">${esc(it.d)}</div>` : ""}
    <div class="f"><span><b>${kinds.size}</b> related here</span>${it.members ? `<span><b>${it.members}</b> members</span>` : ""}${(outs.get(it) || []).length ? `<span>reaches <b>${outs.get(it).length}</b> packages</span>` : ""}<span style="margin-left:auto"><kbd>↵</kbd> open <kbd>G</kbd> graph</span></div></div>`;
}

function packageView(state) {
  const t = T.layout(MODS, 784);
  const lit = new Set();
  if (state.hot) for (const r of rels.get(state.hot) || []) lit.add(r.other);
  const starts = [...fanIn.entries()].sort((a, b) => b[1] - a[1]).slice(0, 3).map(([it]) => it);
  const gloss = { Identity: "one parsed row", Fault: "one typed failure", Theme: "colour and width", Language: "a source language" };
  const shareMap = new Map([[byName("RelationLabel"), "gem:RelationLabel"], [byName("relation_label"), "item:relation_label"], [byName("RelationDirection"), "item:RelationDirection"], [byName("RelationGroup"), "item:RelationGroup::label item:RelationGroup::new item:RelationGroup.label"]]);
  const map = T.render(t, { hot: state.hot, hotRegion: state.hotRegion, lit, dim: !!state.hot, share: shareMap });
  const folio = `<div class="folio">
    <div class="top">${gem(56)}<div><h1>present</h1><div class="lede">The one presentation model every product surface renders.</div></div></div>
    <div class="facts"><span class="y">yours</span><i>·</i><span>Rust</span><i>·</i><span class="v">0.1.0</span><i>·</i><span><span class="v">${P.items.length}</span> public items in <span class="v">${MODS.length}</span> modules</span><i>·</i><span>no I/O by construction</span></div>
    <div class="start"><span class="sh-h">Start here</span>${starts.map((it, i) => `${i ? `<span class="tl"></span>` : ""}<span class="stop ${state.hot === it ? "on" : ""}" data-i="${it.gid}">${T.mark(it.f, 11)}<b>${esc(it.n)}</b><em>${esc(gloss[it.n] || "")}</em></span>`).join("")}</div>
    <div class="map" id="map">${state.hot ? strands(t, state.hot) : ""}<div data-share="map" style="position:relative">${map}</div>${state.hot && state.peek ? peek(t, state.hot) : ""}</div>
    <div class="rows2"><span class="grp"><span class="h">Rests on</span><span class="dep">library</span><span class="dep">client</span><span class="dep">serde</span></span><span class="grp"><span class="h">Used by</span><span class="dep y">desktop</span><span class="dep y">cli</span><span class="dep y">mcp</span></span></div>
  </div>`;
  return { html: win(jumpbar(["present"]), `${shelfFor(state)}<div class="reader">${folio}</div>`), t };
}


// ---------------------------------------------------------------- browse: the library, as territories
const LIB = view === "package" ? [] : await load("data/library.json");
LIB.forEach((p) => { p.mods = p.modules.map((m) => ({ path: m.path, items: m.items })); p.all = p.mods.flatMap((m) => m.items); });
const lib = (n) => LIB.find((p) => p.name === n);
// toml_pin, the journeys' "your project": it pins toml 0.8.23 and reaches Value, from_str, de::Error.
const REACHED = { toml: [["value", "Value"], ["de", "from_str"], ["de", "Error"]] };
const reachedIn = (p) => new Set((REACHED[p.name] || []).flatMap(([m, n]) => p.mods.filter((x) => x.path === m).flatMap((x) => x.items.filter((i) => i.n === n))));
const TREE = { toml: "yours · direct", toml_edit: "via toml", toml_datetime: "via toml", serde_core: "via toml" };
const mini = (p, state, w, o = {}) => { const t = T.layout(p.mods, w, { pitch: o.pitch || 9, stone: o.stone || 6.5, pad: o.pad ?? 4, label: 0, rows: o.rows || 2 }); return T.render(t, state, { stone: o.stone || 6.5, cls: "mini", labels: false }); };
const plain = (d) => (d || "").replace(/\[([^\]]+)\]\([^)]*\)/g, "$1").replace(/\[([^\]]+)\]/g, "$1").replace(/`/g, "");
const gemS = (s) => gem(s, false);

// A signature, as its machine: what goes in, what comes out, what can fail.
function machine(sig, name, q) {
  const words = (t) => {
    t = (t || "").trim();
    if (/^&('\w+ )?(mut )?str$|^String$/.test(t)) return ["text", "in"];
    if (/^&('\w+ )?\[u8\]$|^Vec<u8>$/.test(t)) return ["bytes", "in"];
    if (/^&('\w+ )?(mut )?(\w+)$/.test(t)) return [t.replace(/^&('\w+ )?(mut )?/, ""), "nm"];
    return [t.replace(/<.*>/, ""), "nm"];
  };
  const m = sig.match(/\(([^)]*)\)\s*(?:->\s*(.*))?$/);
  if (!m) return `<div class="machine"><span class="fn">${esc(name)}</span></div>`;
  const params = m[1].split(",").map((x) => x.trim()).filter((x) => x && !/self$/.test(x));
  const ins = params.map((x) => words(x.split(":").slice(1).join(":")));
  let out = (m[2] || "").trim(), fail = null;
  const res = out.match(/^Result<(.+?)(?:,\s*(.+))?>$/);
  if (res) { out = res[1]; fail = (res[2] || "Error").replace(/^crate::|^Self::/, ""); }
  const [ow] = words(out);
  const hit = q && name === q ? `<u>${esc(name)}</u>` : esc(name);
  return `<div class="machine">${ins.map(([w, c]) => `<span class="${c}">${esc(w)}</span>`).join(`<span style="color:var(--ink4)">,&nbsp;</span>`)}${ins.length ? `<span class="wire"></span>` : ""}<span class="fn hit">${hit}</span>${ow ? `<span class="wire"></span><span class="out">${esc(ow)}</span>` : ""}${fail ? `<span class="fail">may fail · ${esc(fail)}</span>` : ""}</div>`;
}

function libraryShelf(counts, cur) {
  const rows = LIB.filter((p) => p.name !== "toml_pin" && p.name !== "rich_project").map((p) => { const n = counts ? counts.get(p) || 0 : null; return `<div class="row ${cur === p.name ? "cur" : ""} ${counts && !n ? "dimrow" : ""}"><span>${esc(p.name)}</span>${n ? `<span class="hits">${n}</span>` : `<span class="ct">${p.version || ""}</span>`}</div>`; }).join("");
  return `<div class="shelf"><div class="sec2" style="font:500 11px var(--ui);letter-spacing:.06em;text-transform:uppercase;color:var(--ink3);padding:12px 16px 6px">Yours</div><div class="row" style="color:var(--mint)"><span>toml_pin</span><span class="ct">1 dependency</span></div><div style="font:500 11px var(--ui);letter-spacing:.06em;text-transform:uppercase;color:var(--ink3);padding:16px 16px 6px">Library</div>${rows}</div>`;
}

function findView(q) {
  const hits = LIB.map((p) => ({ p, items: p.mods.flatMap((m) => m.items.filter((i) => i.n === q).map((i) => ({ i, m }))) })).filter((h) => h.items.length);
  const rank = (h) => (TREE[h.p.name] ? (h.p.name === "toml" ? 0 : 1) : 2);
  hits.sort((a, b) => rank(a) - rank(b));
  const counts = new Map(hits.map((h) => [h.p, h.items.length]));
  const answers = hits.map((h, k) => {
    const best = h.items.find((x) => x.i.f === "callable") || h.items[0];
    const lit = new Set(h.items.map((x) => x.i));
    const yours = reachedIn(h.p);
    const isToml = h.p.name === "toml";
    const REG = { value: "sec:value", de: "sec:de", ser: "sec:ser", "ser::ser_value": "sec:ser", map: "sec:map" };
    const shareRegion = isToml ? new Map(Object.entries(REG)) : null;
    const share = isToml ? new Map(h.p.mods.flatMap((m) => { const sec = (REG[m.path] || "").slice(4); return sec ? m.items.map((it) => [it, `row:${sec}/${it.n}`]) : []; })) : null;
    const tag = h.p.name === "toml" ? `<div class="tag">your code calls this</div>` : TREE[h.p.name] ? `<div class="tag q">in your tree, ${TREE[h.p.name]}</div>` : `<div class="tag q">not in your tree</div>`;
    return `<div class="ans ${k === 0 ? "on" : ""}">
      <div><div class="who"><span ${isToml ? 'data-share="gem:toml"' : ""}>${gemS(20)}</span><span class="nm" ${isToml ? 'data-share="name:toml" data-text="1"' : ""}>${esc(h.p.name)}</span><span class="vv">${h.p.version || ""}</span></div>${tag}</div>
      <div class="shape">${machine(best.i.s, best.i.n, q)}<div class="why"><span class="mono">${esc(h.p.name)}::${esc(best.m.path)}</span>${best.i.d ? ` · <span class="doc">${esc(plain(best.i.d))}</span>` : ""}</div>${h.items.length > 1 ? `<div class="also">also ${h.items.filter((x) => x !== best).map((x) => `<span class="mono">${esc(x.m.path)}</span>`).join("")}</div>` : ""}</div>
      <div class="where">${mini(h.p, { lit, yours, dim: true, share, shareRegion }, 250)}<span class="cap">${h.p.all.length} public items · ${h.p.mods.length} modules</span></div>
    </div>`;
  }).join("");
  const jump = `<div class="jump" style="width:620px;justify-content:flex-start;gap:10px"><svg width="14" height="14" viewBox="0 0 14 14"><circle cx="6" cy="6" r="4.5" fill="none" stroke="var(--ink3)" stroke-width="1.4"/><path d="M9.5 9.5L13 13" stroke="var(--ink3)" stroke-width="1.4"/></svg><b>${esc(q)}</b><span style="margin-left:auto;font:400 12px var(--ui);color:var(--ink3)">every answer, as a page</span></div>`;
  const page = `<div class="bpage"><div class="qhead"><span class="q">${esc(q)}</span><span class="n"><b>${hits.length}</b> packages answer · <span class="y">1 is yours to call</span></span></div><div class="answers">${answers}</div></div>`;
  return win(jump, `${libraryShelf(counts)}<div class="reader">${page}</div>`);
}

function shapeKey(it) { if (it.f !== "callable") return (it.s.match(/^pub (\w+)/) || [0, it.f])[1]; const m = (it.s || "").match(/\(([^)]*)\)\s*(?:->\s*(.*))?$/); return m ? m[1].split(",").length + "|" + (m[2] || "").replace(/crate::|Self::|<'\w+>|'\w+,?\s*/g, "").replace(/\s/g, "") : it.f; }
function compareView(names) {
  const ps = names.map(lib);
  const sets = ps.map((p) => new Map(p.all.map((i) => [i.n, i])));
  const all = new Map();
  sets.forEach((s2, k) => s2.forEach((i, n) => { (all.get(n) || all.set(n, []).get(n))[k] = i; }));
  const rows = [...all.entries()].map(([n, xs]) => ({ n, xs, c: xs.filter(Boolean).length })).filter((r) => r.c >= 2).sort((a, b) => b.c - a.c || (a.xs.find(Boolean).f > b.xs.find(Boolean).f ? 1 : -1) || a.n.localeCompare(b.n));
  const shared = new Set(rows.filter((r) => r.c === 3).map((r) => r.n));
  const focus = rows[0];
  const cols = ps.map((p, k) => {
    const lit = new Set(p.all.filter((i) => shared.has(i.n)));
    const cur = focus.xs[k];
    return `<div class="pk"><div class="hd">${gemS(22)}<span class="nm">${esc(p.name)}</span><span class="vv">${p.version}</span></div>
      <div class="fx"><b>${p.all.length}</b> public items · <b>${p.mods.length}</b> modules${TREE[p.name] ? ` · <span class="y">${TREE[p.name]}</span>` : ""}</div>
      ${mini(p, { lit, current: cur, dim: true }, 326, { pitch: 11, stone: 8, pad: 5 })}</div>`;
  }).join("");
  const brief = (i) => {
    if (i.f !== "callable") return `${esc((i.s.match(/^pub (\w+)/) || [, i.f])[1])}${i.members ? ` · ${i.members} members` : ""}`;
    const m = i.s.match(/\(([^)]*)\)\s*(?:->\s*(.*))?$/); if (!m) return "fn";
    const ins = m[1].split(",").map((x) => x.trim()).filter((x) => x && !/self$/.test(x)).map((x) => { const t2 = x.split(":").slice(1).join(":").trim(); return /str$|^String$/.test(t2) ? "text" : /\[u8\]$/.test(t2) ? "bytes" : /Read$|R$/.test(t2) ? "reader" : t2.replace(/^&('\w+ )?(mut )?/, "").replace(/<.*>/, ""); });
    const r = (m[2] || "").trim(); const res = r.match(/^Result<([^,>]+)/);
    return `${esc(ins.join(", ") || "()")} → ${esc(res ? res[1].replace(/<.*/, "") : r.replace(/<.*/, "") || "()")}${res ? ` <span style="color:var(--coral)">?</span>` : ""}`;
  };
  const cell = (i, ref) => !i ? `<div class="none">—</div>` : `<div class="${ref && shapeKey(ref) !== shapeKey(i) ? "diff" : "same"}">${T.mark(i.f, 10)}${brief(i)}</div>`;
  const ledger = `<div class="ledger"><div class="h">Name</div>${ps.map((p) => `<div class="h">${esc(p.name)}</div>`).join("")}${rows.slice(0, 13).map((r, k) => { const xs = Array.from({ length: 3 }, (_, j) => r.xs[j]); const ref = xs.find(Boolean); return `<div class="nm ${k === 0 ? "rowon" : ""}">${esc(r.n)}</div>${xs.map((i) => cell(i, ref)).join("")}`; }).join("")}</div>`;
  const page = `<div class="cmp"><h2>toml, toml_edit and basic-toml</h2><div class="sub"><b>${shared.size}</b> names in all three (lit) · ${rows.length - shared.size} in two · hover a row and its shingle rings in each map · amber = the same name, a different shape</div><div class="cols3">${cols}</div>${ledger}</div>`;
  return win(`<div class="jump"><b>compare</b><span class="sep">›</span><span>toml · toml_edit · basic-toml</span></div>`, `${libraryShelf(null, "toml")}<div class="reader">${page}</div>`);
}

function treeView() {
  const row = (p, label, yoursSet, w = 300) => `<div class="ans" style="grid-template-columns:230px 1fr 320px"><div><div class="who">${gemS(20)}<span class="nm">${esc(p.name)}</span><span class="vv">${p.version || ""}</span></div><div class="tag ${yoursSet.size ? "" : "q"}">${label}</div></div><div class="shape">${yoursSet.size ? `<div class="why">you reach <b style="color:var(--mint);font-weight:500">${yoursSet.size}</b> of its ${p.all.length}: ${[...yoursSet].map((i) => `<span class="mono">${esc(i.n)}</span>`).join(", ")}</div>` : `<div class="why">you don't reach it directly: toml does</div>`}</div><div class="where">${mini(p, { yours: yoursSet }, w)}</div></div>`;
  const ghost = (n, req) => `<div class="ans" style="grid-template-columns:230px 1fr 320px;opacity:.8"><div><div class="who"><svg width="20" height="20" viewBox="0 0 20 20"><path d="M10 1L19 10L10 19L1 10Z" fill="none" stroke="var(--ink4)" stroke-dasharray="2 2"/></svg><span class="nm" style="color:var(--ink2)">${esc(n)}</span><span class="vv">${esc(req)}</span></div><div class="tag q">via toml · not read yet</div></div><div class="why">its items appear here once it is indexed</div><div class="where"><svg width="300" height="26"><rect x=".5" y=".5" width="299" height="25" fill="none" stroke="var(--ink4)" stroke-dasharray="3 3"/></svg></div></div>`;
  const t = lib("toml");
  const page = `<div class="bpage"><div class="qhead"><span class="q" style="font-family:var(--display);font-size:34px;letter-spacing:-.02em">toml_pin</span><span class="n"><span class="y">yours</span> · rests on <b>1</b> package directly, <b>8</b> in all · you reach <b style="color:var(--mint)">3</b> items</span></div>
    <div style="margin-top:16px;font:500 11px var(--ui);letter-spacing:.06em;text-transform:uppercase;color:var(--ink3)">Speaks formats</div>
    <div class="answers" style="margin-top:6px">${row(t, "direct · 0.8.23", reachedIn(t))}${["toml_edit", "toml_datetime", "serde_core"].map((n) => row(lib(n), "via toml", new Set())).join("")}${ghost("indexmap", "2.0.0")}${ghost("winnow", "0.7")}</div></div>`;
  return win(`<div class="jump"><b>toml_pin</b><span class="sep">›</span><span>your tree</span></div>`, `${libraryShelf(null)}<div class="reader">${page}</div>`);
}


// ---------------------------------------------------------------- the symbol page and its graph: the same objects, the same sides
// Left = where it comes from (made by). Right = where it goes (taken by, held by). Down the spine = what it is made of.
const X0 = 196;
function kindGem(size, f = "type") {
  const hue = { type: "var(--k-ty)", callable: "var(--k-ca)", contract: "var(--k-co)", value: "var(--ink2)" }[f];
  return `<svg width="${size}" height="${size}" viewBox="0 0 48 48" style="display:block;color:${hue}"><path d="M24 2 46 24 24 46 2 24z" fill="currentColor" fill-opacity=".14" stroke="currentColor" stroke-width="1.6"/><path d="M24 11 37 24 24 37 11 24z" fill="currentColor" fill-opacity=".55"/></svg>`;
}
const R = {
  subject: byName("RelationLabel"),
  madeBy: [{ n: "relation_label", f: "callable", mod: "glyph", v: "gives one" }, { n: "RelationGroup::label", f: "callable", mod: "page", v: "reads it" }],
  goes: [{ n: "RelationGroup::new", f: "callable", mod: "page", v: "taken by" }, { n: "RelationGroup.label", f: "value", mod: "page", v: "held by" }],
  madeOf: [{ n: "SemanticLinkKind", f: "type", mod: "library", lib: true }, { n: "RelationDirection", f: "type", mod: "glyph" }],
};
const share = (key, extra = "") => `data-share="${esc(key)}" ${extra}`;
function locator(state, w = 236) {
  const t = T.layout(MODS, 784);
  const glyph = MODS.find((m) => m.path === "glyph");
  return `<div class="loc" style="left:${state.x}px;top:${state.y}px;width:${w}px"><div ${share("map")}>${T.render(t, { current: R.subject, hotRegion: glyph, lit: state.lit || new Set(), dim: true }, { cls: "mini locmap", labels: false, width: w })}</div><div class="cap">present › <span style="color:var(--ink1)">glyph</span></div></div>`;
}
function shelfSym(state) {
  const rows = MODS.map((m) => { const open = m.path === "glyph"; return `<div class="row ${open ? "hot" : ""}" ${share("shelf:" + m.path)}><span>${esc(m.path)}</span><span class="ct">${m.items.length}</span></div>` + (open ? m.items.map((it) => `<div class="row sub ${it === R.subject ? "cur" : ""}" ${share("shelfitem:" + it.n)}>${T.mark(it.f, 10)}<span style="margin-left:8px">${esc(it.n)}</span></div>`).join("") : ""); }).join("");
  return `<div class="shelf"><div class="up">‹ Library</div><div class="book">${gem(28, false)}<div><div class="bn">present</div><div class="bv">0.1.0</div></div></div>${rows}</div>`;
}
function symbolView(state = {}) {
  const S = R.subject, sx = X0 + 28;
  let ink = `<svg class="ink2" width="1176" height="850"><path class="sp" d="M${sx} 100V600"/>`;
  const cases = [["Typed", R.madeOf], ["Neighbourhood", []], ["Related", []]];
  cases.forEach((_, k) => { const y = 206 + k * 32; ink += `<path class="sp" d="M${sx} ${y - 14}q0 14 14 14H${X0 + 58}"/>`; });
  R.madeBy.forEach((r, k) => { const y = 402 + k * 36; ink += `<path class="rl" d="M40 ${y}H${sx - 8}"/><path d="M${sx - 14} ${y - 4}l6 4-6 4" fill="none" stroke="var(--k-ty)" stroke-width="1.2"/>`; });
  R.goes.forEach((r, k) => { const y = 520 + k * 36; ink += `<path class="rl ${k ? "arr" : ""}" d="M${sx} ${y}H${X0 + 58}"/><path class="rl" d="M${X0 + 360} ${y}H1130"/>`; });
  ink += `</svg>`;
  const html = `<div class="sym">${ink}
    <div class="gemT" ${share("gem:" + S.n)} style="left:${X0}px;top:44px">${kindGem(56)}</div>
    <div class="abs title" ${share("name:" + S.n, 'data-text="1"')} style="left:${X0 + 76}px;top:40px">${esc(S.n)}</div>
    <div class="abs lede" style="left:${X0 + 76}px;top:88px">${esc(S.d)}</div>
    <div class="abs marks2" style="left:${X0 + 76}px;top:124px"><span class="mono">present › glyph</span><span class="mono">glyph.rs:138</span><span>enum · 3 variants</span></div>
    ${locator({ x: X0 + 784 - 236, y: 44, lit: state.lit })}
    <div class="abs lbl" style="left:${X0 + 58}px;top:166px">one of <b>3</b></div>
    ${cases.map(([n, pay], k) => `<div class="abs case" style="left:${X0 + 66}px;top:${196 + k * 32}px"><span>${n}</span>${pay.length ? `<span class="pay">${pay.map((x) => `<span ${share("item:" + x.n)}>${T.mark(x.f, 10)}${esc(x.n)}${x.lib ? " <em>library</em>" : ""}</span>`).join("")}</span>` : ""}</div>`).join("")}
    <div class="abs caps" style="left:${X0 + 66}px;top:306px"><span>copies</span><span>compares</span><span>orders</span><span>hashes</span><span>prints</span></div>
    <div class="abs sech" style="left:${X0 + 58}px;top:360px">Getting one<span class="n">2 ways</span></div>
    ${R.madeBy.map((r, k) => `<div class="abs rowi" style="left:${X0 + 58}px;top:${392 + k * 36}px"><span class="step" ${share("item:" + r.n)}>${esc(r.n)}</span><span class="v">${r.v}</span><span class="mod">${r.mod}</span></div>`).join("")}
    <div class="abs sech" style="left:${X0 + 58}px;top:482px">Where it goes<span class="n">2</span></div>
    ${R.goes.map((r, k) => `<div class="abs rowi" style="left:${X0 + 66}px;top:${510 + k * 36}px"><span class="v" style="width:56px">${r.v}</span><span ${share("item:" + r.n)} style="display:inline-flex;gap:8px;align-items:center">${T.mark(r.f, 10)}${esc(r.n)}</span><span class="mod">${r.mod}</span></div>`).join("")}
  </div>`;
  return win(jumpbar(["present", "glyph", S.n]), `${shelfSym(state)}<div class="reader">${html}</div>`);
}
function graphView(state = {}) {
  const S = R.subject, C = { x: 588, y: 380 };
  const LEFT = 470, RIGHT = 712;
  const at = { "relation_label": ["l", 320], "RelationGroup::label": ["l", 440], "RelationGroup::new": ["r", 320], "RelationGroup.label": ["r", 440], "RelationDirection": ["d", 470], "SemanticLinkKind": ["d", 710] };
  let ink = `<svg class="ink2" width="1176" height="850">`;
  const all = [...R.madeBy, ...R.goes, ...R.madeOf];
  for (const r of all) {
    const [side, v] = at[r.n];
    let d;
    if (side === "l") d = `M${C.x - 26} ${C.y}C${C.x - 90} ${C.y} ${LEFT + 60} ${v} ${LEFT + 8} ${v}`;
    else if (side === "r") d = `M${C.x + 26} ${C.y}C${C.x + 90} ${C.y} ${RIGHT - 60} ${v} ${RIGHT - 8} ${v}`;
    else d = `M${C.x} ${C.y + 26}C${C.x} ${C.y + 90} ${v} ${C.y + 90} ${v} ${C.y + 150}`;
    ink += `<path class="ed on ${r.lib ? "lib" : ""}" d="${d}"/>`;
  }
  ink += `<text x="${LEFT}" y="${C.y - 110}" text-anchor="end">made by</text><text x="${RIGHT}" y="${C.y - 110}">where it goes</text><text x="${C.x}" y="${C.y + 208}" text-anchor="middle">made of</text></svg>`;
  const nodes = all.map((r) => {
    const [side, v] = at[r.n];
    const body = `<span ${share("item:" + r.n)} style="display:inline-flex;gap:8px;align-items:center">${T.mark(r.f, 11)}${esc(r.n)}</span><span class="mod">${r.mod}</span>`;
    if (side === "l") return `<div class="node" style="right:${1176 - LEFT}px;top:${v - 10}px">${body}</div>`;
    if (side === "r") return `<div class="node" style="left:${RIGHT}px;top:${v - 10}px">${body}</div>`;
    return `<div class="node ${r.lib ? "lib" : ""}" style="left:${v - 8}px;top:${C.y + 156}px">${body}</div>`;
  }).join("");
  const html = `<div class="sym">${ink}
    <div class="gemT" ${share("gem:" + S.n)} style="left:${C.x - 22}px;top:${C.y - 22}px">${kindGem(44)}</div>
    <div class="abs" ${share("name:" + S.n, 'data-text="1"')} style="left:${C.x - 80}px;top:${C.y - 50}px;width:160px;text-align:center;font:600 13px var(--mono);color:var(--ink0)">${esc(S.n)}</div>
    ${nodes}
    ${locator({ x: 40, y: 30 })}
  </div>`;
  return win(jumpbar(["present", "glyph", S.n, "graph"]), `${shelfSym(state)}<div class="reader">${html}</div>`);
}


// ---------------------------------------------------------------- transitions, filmed: shared elements travel, the rest unrolls
// One critically damped spring (CARRY, response 0.28 s) for everything that travels; text never scales
// (a name travels in its old face and swaps on landing); nothing fades: new content unrolls top-down.
const spring = (t) => { const w = (2 * Math.PI) / 0.28, s2 = t / 1000; return t <= 0 ? 0 : 1 - (1 + w * s2) * Math.exp(-w * s2); };
const lerp = (a, b, p) => a + (b - a) * p;
function measure(html) {
  const host = document.createElement("div");
  host.style.cssText = "position:absolute;left:-6000px;top:0;width:1440px";
  host.innerHTML = html; document.body.appendChild(host);
  const o = host.querySelector(".win").getBoundingClientRect();
  const m = new Map();
  host.querySelectorAll("[data-share]").forEach((el) => {
    const r = el.getBoundingClientRect();
    const ctx = [["shelf", "shelf"], ["sym", "sym"], ["folio", "folio"], ["peek", "peek"], ["map", "map"]].filter(([c]) => el.parentElement && el.parentElement.closest("." + c)).map(([, c]) => c);
    const open = ctx.map((c) => `<div class="${c} ctx">`).join(""), close = "</div>".repeat(ctx.length);
    // the root travels as itself, without its own placement (the overlay places it)
    let inner = el.outerHTML.replace(/data-share="[^"]*"/, "").replace(/^(<[^>]*?)style="[^"]*"/, "$1").replace(/^<(\w+)/, '<$1 data-root="1"');
    if (el instanceof SVGElement && el.tagName !== "svg") inner = `<svg class="terr" width="${r.width + 8}" height="${r.height + 8}" viewBox="${el.getBBox().x - 4} ${el.getBBox().y - 4} ${el.getBBox().width + 8} ${el.getBBox().height + 8}" style="margin:-4px">${inner}</svg>`;
    for (const key of el.dataset.share.split(" ")) m.set(key, { x: r.left - o.left, y: r.top - o.top, w: r.width, h: r.height, html: open + inner + close, text: !!el.dataset.text, svg: el instanceof SVGElement });
  });
  host.remove();
  return m;
}
function frame(A, B, htmlA, htmlB, t) {
  if (t <= 0) return htmlA;
  const p = spring(t), u = Math.min(1, Math.max(0, (t - 90) / 220));
  const unroll = 1 - (1 - u) ** 3;
  // Only marks and shapes travel: a shingle flies to its row's mark, a region's outline morphs
  // into its section's frame, a gem grows into a gem. Text never flies in crowds: rows and heads
  // unroll in place; a single name plate may travel, in its old face, and lands in its new one.
  const markish = (k) => k.startsWith("row:") || k.startsWith("item:");
  let overlay = "";
  const hide = new Set();
  for (const [key, b] of B) {
    const a = A.get(key); if (!a) continue;
    const x = lerp(a.x, b.x, p), y = lerp(a.y, b.y, p);
    if (markish(key) && a.svg && !b.svg) {
      const s2 = lerp(a.w, 11, p), tx = lerp(a.x, b.x - 2, p), ty = lerp(a.y, b.y + (b.h - 11) / 2, p);
      if (p < 0.97) overlay += `<svg class="fly" style="left:${tx}px;top:${ty}px" width="${s2}" height="${s2}" viewBox="0 0 10 10"><path d="${T.shinglePath(0, 0, 10)}" fill="var(--peri-hi)"/></svg>`;
    } else if (key.startsWith("sec:")) {
      const w = lerp(a.w, b.w, p), h = lerp(a.h, b.h, p);
      if (p < 0.97) overlay += `<div class="fly frameline" style="left:${x}px;top:${y}px;width:${w}px;height:${h}px"></div>`;
    } else if (key === "map") {
      const k = lerp(1, b.w / a.w, p);
      overlay += `<div class="fly nolabels" style="left:${x}px;top:${y}px;width:${a.w}px;transform:scale(${k});transform-origin:0 0">${a.html}</div>`; hide.add(key);
    } else if (b.text || a.text) {
      const land = p > 0.94;
      overlay += `<div class="fly" style="left:${land ? b.x : x}px;top:${land ? b.y : y}px">${land ? b.html : a.html}</div>`; hide.add(key);
    } else if (a.svg && !b.svg) {
      overlay += `<div class="fly" style="left:${x}px;top:${y}px">${b.html}</div>`; hide.add(key);
    } else {
      const k = lerp(a.w / b.w, 1, p);
      overlay += `<div class="fly" style="left:${x}px;top:${y}px;width:${b.w}px;height:${b.h}px;transform:scale(${k});transform-origin:0 0">${b.html}</div>`; hide.add(key);
    }
  }
  const base = htmlB.replace(/data-share="([^"]*)"/g, (m0, keys) => (keys.split(" ").some((k) => hide.has(k)) ? `${m0} data-hide="1"` : m0));
  const clip = `clip-path:inset(0 0 ${(1 - unroll) * 100}% 0)`;
  return base.replace('<div class="reader">', `<div class="reader" style="${clip}">`).replace('<div class="shelf', `<div style="${clip};display:contents"></div><div class="shelf`).replace(/<\/div><\/div>$/, `</div><div class="flylayer">${overlay}</div></div>`);
}

function filmView(fromHtml, toHtml, times, caption) {
  const A = measure(fromHtml), B = measure(toHtml);
  return `<div class="film">${times.map((t) => `<div class="fr"><div class="t">${t} ms</div><div class="scaler">${frame(A, B, fromHtml, toHtml, t)}</div></div>`).join("")}</div>`;
}

// ---------------------------------------------------------------- mount + live hover (the same grammar everywhere)
const hatch = `<svg width="0" height="0" style="position:absolute"><defs><pattern id="hatch" width="4" height="4" patternTransform="rotate(45)" patternUnits="userSpaceOnUse"><rect width="2" height="4" fill="var(--coral)"/></pattern></defs></svg>`;
const state = {
  hot: q.get("hover") ? byName(q.get("hover")) : null,
  hotRegion: q.get("region") ? MODS.find((m) => m.path === q.get("region")) : null,
  peek: !!q.get("hover"),
};
if (state.hot && !state.hotRegion) state.hotRegion = modOf.get(state.hot);
let timer = null;
function draw() {
  if (view === "package") {
    note.innerHTML = `<b>The package page is its territory.</b> One region per module, one shingle per public item, tinted by kind (teal types, periwinkle callables). Hover a shingle: it lifts, its relations draw as strands to the shingles they reach (solid = written: has, takes, gives; dashed = calls), everything else steps back, and the peek opens after 280 ms. Relations that leave the package exit right, with a count. Hover a region or its shelf row: both answer. The Start line lifts its shingle. No tabs, no outline list: the shelf lists the modules, the map shows what they hold.`;
    stage.innerHTML = hatch + packageView(state).html;
  }
  if (view === "symbol") {
    note.innerHTML = `<b>The symbol page keeps the package in view.</b> The territory has folded into the locator (top right: the same regions and shingles at a fifth of the pitch, glyph outlined, RelationLabel ringed). The drawn page keeps the one geometry: makers enter from the left along rails, what it goes into leaves right, what it is made of hangs off the spine. Hovering any related name rings its shingle in the locator.`;
    stage.innerHTML = symbolView(state);
  }
  if (view === "graph") {
    note.innerHTML = `<b>The graph is the page, zoomed out: same sides, same marks.</b> Made-by on the left (where the page's rails come from), where-it-goes on the right, made-of below the node (where the fork hangs). The locator stays as the minimap. Every node's mark is the row mark from the page and the shingle's kind from the map.`;
    stage.innerHTML = graphView(state);
  }
  if (view === "film-browse") {
    const a = findView("from_str"), b = SIDE.sideView("side-plain", win, jumpbar);
    note.innerHTML = `<b>Browse → package: the shingles open into the package.</b> Choosing toml's answer in Find: its small map expands into the page. Each region travels to its module's section head, each shingle to its item's row (a shingle is an overview; the row is the thing you can read), the answer's gem and name become the hero, and the Library shelf becomes toml's sidebar. The rest unrolls behind them. Nothing fades; text travels at its own size and lands in its new face.`;
    stage.innerHTML = q.get("t") ? frame(measure(a), measure(b), a, b, +q.get("t")) : filmView(a, b, [0, 40, 90, 140, 200, 340], "Browse → package");
  }
  if (view === "film-open" || view === "film-graph") {
    const pkg = hatch + packageView({ hot: byName("RelationLabel"), hotRegion: MODS.find((m) => m.path === "glyph"), peek: true }).html;
    const sym = hatch + symbolView({}), gr = hatch + graphView({});
    const [a, b, cap] = view === "film-open" ? [pkg, sym, "Package → Symbol: click RelationLabel's shingle"] : [sym, gr, "Symbol → Graph: G"];
    note.innerHTML = `<b>${cap}.</b> ${view === "film-open" ? "The shingle becomes the gem and rises to the hero; the name in the peek travels to the title line and lands in its title face; the territory folds, as itself, into the locator (the same layout at a third); RelationLabel's relatives fly from their shingles to their rows (made by on the left rails, where it goes on the right, made of on the fork); the shelf's glyph row opens where it was. Everything else unrolls top-down behind them. Frames on the CARRY spring (0.28 s)." : "The gem settles into the node; each row travels to its node on the same side (rails left → made-by left; where-it-goes right; the fork's parts below); the locator moves to the minimap corner; the page unrolls away as the graph unrolls in."}`;
    stage.innerHTML = q.get("t") ? frame(measure(a), measure(b), a, b, +q.get("t")) : filmView(a, b, [0, 30, 70, 120, 180, 320], cap);
  }
  if (SIDE && view.startsWith("side-")) {
    const notes = {
      "side-contents": `<b>Contents: the package's outline, with state on every row.</b> Scope is one package (the crumbs are menus: click <i>toml</i> for its siblings). Rows carry what matters without opening anything: <span style="color:var(--mint)">mint</span> = how often your code uses it, <span style="color:var(--amber)">amber</span> = it changes in the release you're looking at (1.1.6), coral = gone or deprecated there. Modules roll their counts up. Arrow keys move a selection and a peek follows it beside the sidebar (here on <i>as_str</i>: who uses it, and what 1.1.6 does to it); ↵ opens, → shows members, H holds. Held items sit above the lenses in every scope; the trail at the bottom is where you've been. The reader is the package opened up: modules as sections, items with their shape, members on demand, used ones first.`,
      "side-narrow": `<b>Narrow: just type.</b> No field to click: typing while the sidebar has focus narrows the current scope in place, keeping each match's parents as context. The count says how much of the scope matches; Esc restores. The jump bar is for going anywhere; this is for going somewhere nearby.`,
      "side-users": `<b>Used by: who rests on this package, and how.</b> Your crates first, with how often each uses toml and how many of its items. Selecting one (desktop) answers the question on the right with its actual call sites, grouped by item, and narrows the Contents lens to only what desktop uses. "How much of this do we actually use" becomes a look, not a search.`,
      "side-versions": `<b>Versions: compare releases of one package.</b> The lens lists releases (pin in mint, the one you're looking at in periwinkle). The reader leads with what changes <i>for you</i>: the one API your code touches that changes (from_str: the lifetime is named now), each of your call sites, and the verdict. Then everything else, by module. Everywhere else in the app, rows now show amber against this target.`,
      "side-rests": `<b>Rests on: why each dependency is here.</b> Each one with what toml uses it for, its size when indexed, dashed when not read yet; duplicates in your tree called out. (Draft: the "what toml uses of it" footprint needs the index's cross-package references.)`,
      "side-item": `<b>The same four lenses at every scope.</b> On an item, Contents is its members, Versions its history across releases, Rests on what it is made of, Used by who calls it. Here: Value, Used by: your code's 68 uses by crate, desktop opened to what it calls. The reader is the drawn page (W-Page2's, embedded from the v5 board), so the sidebar and the page answer different questions about one thing: the page says what Value is; the lens says who leans on it. Selecting a use row opens that call site in Source.`,
      "side-library": `<b>The Library scope: what you have, by how it relates to you.</b> The sidebar groups packages as Yours, You rest on, Beneath them, Also in the library; each row carries your uses (mint) and a newer release (amber "→ 1.1.6"). The reader is where browsing lives: each package as a card with its shingle overview (mint: what you use; amber: what the newer release changes), so the question "which of my dependencies need attention" is answered at a glance. Opening a card is the Browse → package film.`,
      "side-jump": `<b>The path is the jump bar, and each segment is a menu of its siblings.</b> Click <i>value</i> (or focus the bar and press ↓): the modules beside it, each with its rolled-up state, filterable by typing. The sidebar no longer repeats the path; it only offers the step out (‹ Library). (Xcode's jump bar, VS Code's breadcrumbs.)`,
      "side-hoist": `<b>Hoist: scope into one thing.</b> → on Value (or double-click) makes it the sidebar's scope: the crumbs grow (Library › toml › value › Value), its members fill the list, all of them, used ones first. ← or a crumb pops back out. The reader doesn't move: browsing the sidebar is not navigating.`,
    };
    note.innerHTML = notes[view] || "";
    stage.innerHTML = SIDE.sideView(view, win, jumpbar);
  }
  if (view === "find") {
    note.innerHTML = `<b>Find: every answer is an item, and every item shows where it lives.</b> One answer per package, yours first. The left says who and whether it is in your tree; the middle is the call's machine (what goes in, what comes out, what can fail), with the match underlined; the right is the package's own territory at a quarter pitch with the matching shingles lit (and mint where your code already reaches). The shelf answers too: packages with matches carry the count, the rest step back. ↑↓ walks answers; ↵ opens the item, and its shingle is the door (the same transition as the package page).`;
    stage.innerHTML = findView(q.get("q") || "from_str");
  }
  if (view === "compare") {
    note.innerHTML = `<b>Compare: three territories, one set of names.</b> The names all three share are lit in each map, so you see <i>where</i> each package keeps its common surface and how much else it carries. The ledger lists the shared names; a cell is that package's shape for the name, amber where the same name has a different shape (the rule from the browse rulings: same name ≠ same thing). Hovering a row rings its shingle in all three maps (here: the first row).`;
    stage.innerHTML = compareView(["toml", "toml_edit", "basic-toml"]);
  }
  if (view === "tree") {
    note.innerHTML = `<b>Your tree: what you rest on, and how much of it you use.</b> Each dependency is a row with its territory; mint shingles are the items your code reaches (toml_pin reaches Value, from_str and de::Error: 3 of toml's 34). What comes with it sits beneath, quiet; unread packages are dashed until indexed. The question "how much of this do I actually use?" is answered by looking, not by reading.`;
    stage.innerHTML = treeView();
  }
}
draw();
stage.addEventListener("mouseover", (e) => {
  const sh = e.target.closest("[data-i]"), rg = e.target.closest("[data-m]");
  const it = sh ? P.items[+sh.dataset.i] : null;
  const m = rg ? MODS.find((x) => x.path === rg.dataset.m) : it ? modOf.get(it) : null;
  if (it === state.hot && m === state.hotRegion) return;
  state.hot = it; state.hotRegion = m; state.peek = false; clearTimeout(timer);
  if (it) timer = setTimeout(() => { state.peek = true; draw(); }, 280);
  draw();
});
