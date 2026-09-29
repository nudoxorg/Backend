// Sidebar states and the reader beside each, on toml's real data (facet's release fixture):
// the 0.8.23 API with members, the diff to 1.1.6, and the 80 places this workspace uses toml.
import { esc, mark, layout, render } from "./territory.js";
import { sidebar } from "./sidebar.js";

const load = async (f) => (await fetch(f, { cache: "reload" })).json();
export const TOML = await load("data/toml.json");

// ---------------------------------------------------------------- the model: modules → items → members
const ITEMK = new Set(["struct", "enum", "trait", "function", "type", "macro", "const", "static", "union"]);
const byPath = new Map(TOML.items.map((i) => [i.path, i]));
const short = (p) => p.split("::").pop();
const modOf = (p) => { const parts = p.split("::"); return parts.length <= 2 ? "toml" : parts.slice(1, -1).join("::"); };
// README doctests and test helpers are not the package's API
const tops = TOML.items.filter((i) => ITEMK.has(i.k) && !/Doctests?$|^tests?$/i.test(i.path.split("::").pop()));
const members = (item) => TOML.items.filter((m) => m.path.startsWith(item.path + "::") && m.path.split("::").length === item.path.split("::").length + 1);
const usesOf = (i) => Object.values(i.uses || {}).reduce((a, b) => a + b, 0);
const deepUses = (item) => usesOf(item) + members(item).reduce((a, m) => a + usesOf(m), 0);
const changed = (i) => i.change === "changed" || i.change === "removed" || i.change === "deprecated";
const deepChanges = (item) => (changed(item) ? 1 : 0) + members(item).filter(changed).length;
const MODS = [...new Set(tops.map((i) => modOf(i.path)))].sort((a, b) => (a === "toml" ? -1 : b === "toml" ? 1 : a.localeCompare(b)));
const itemsIn = (m) => tops.filter((i) => modOf(i.path) === m).sort((a, b) => deepUses(b) - deepUses(a) || short(a.path).localeCompare(short(b.path)));
const crateOf = (file) => file.split("/")[1];
const CRATES = ["desktop", "local-service", "engine", "advisory"];

function itemRow(i, depth, o = {}) {
  const mem = members(i);
  return { id: i.path, depth, kind: mem.length && o.expandable !== false ? "group" : "item", open: !!o.open, f: i.f, name: short(i.path), uses: deepUses(i) || null, change: deepChanges(i) > 0, changeN: deepChanges(i) > 1 ? deepChanges(i) : null, gone: i.change === "removed" ? "gone in 1.1" : i.change === "deprecated" ? "deprecated" : null, count: mem.length || null, current: o.current, sel: o.sel, dim: o.dim, share: o.share };
}
function memberRows(item, depth, o = {}) {
  const mem = members(item).sort((a, b) => usesOf(b) - usesOf(a) || changed(b) - changed(a));
  const shown = o.all ? mem : mem.filter((m) => usesOf(m) || changed(m)).slice(0, o.max || 7);
  const rows = shown.map((m) => ({ id: m.path, depth, kind: "item", f: m.f, name: short(m.path), uses: usesOf(m) || null, change: changed(m) && m.change === "changed", gone: m.change === "removed" ? "gone in 1.1" : m.change === "deprecated" ? "deprecated" : null, sel: o.sel === m.path }));
  if (!o.all && mem.length > shown.length) rows.push({ id: item.path + "::more", depth, kind: "text", name: `${mem.length - shown.length} more`, sub: "" });
  return rows;
}

let SEL = null;
const counts = { contents: tops.length, versions: TOML.versions.length, rests: 5, users: 4 };
const book = `<span class="bn">toml</span><span class="bv">0.8.23</span><span class="pin">→ <b>1.1.6</b></span>`;
const HELD = [{ n: "Value", f: "type" }, { n: "from_str", f: "callable" }];
const TRAIL = ["serde_json › from_str", "toml_pin", "toml › Value"];

function contentsRows(sel) {
  const rows = [];
  for (const m of MODS) {
    const items = itemsIn(m);
    const open = m === "value" || m === "de";
    if (m === "toml") { for (const i of items) rows.push(itemRow(i, 0, { expandable: false })); continue; }
    const u = items.reduce((a, i) => a + deepUses(i), 0), c = items.reduce((a, i) => a + deepChanges(i), 0);
    rows.push({ id: m, depth: 0, kind: "group", open, f: "value", gem: mark("contract", 10).replace("km fco", "km fns"), name: m, uses: u || null, change: c > 0, changeN: c || null, count: items.length });
    if (!open) continue;
    for (const i of items) {
      const isValue = short(i.path) === "Value" && m === "value";
      rows.push(itemRow(i, 1, { open: isValue, current: isValue }));
      if (isValue) rows.push(...memberRows(i, 2, { sel }));
    }
  }
  return rows;
}

// ---------------------------------------------------------------- the peek beside the sidebar
function siblingMenu() {
  // the modules beside `value`, each with its state rolled up; type to filter, ↵ goes
  return `<div class="jmenu"><div class="jq">type to filter · ↑↓ · ↵</div>${MODS.filter((m) => m !== "toml").map((m) => { const items = itemsIn(m); const u = items.reduce((a, i) => a + deepUses(i), 0), c = items.reduce((a, i) => a + deepChanges(i), 0); return `<div class="jr ${m === "value" ? "on" : ""}">${mark("contract", 10).replace("km fco", "km fns")}<span class="jn">${esc(m)}</span><span class="jc">${items.length}</span><span class="gl">${u ? `<span class="g use">${u}</span>` : ""}${c ? `<span class="g ch">${c}</span>` : ""}</span></div>`; }).join("")}</div>`;
}
function speek(path, top) {
  const i = byPath.get(path);
  const u = i.uses || {}; const max = Math.max(1, ...Object.values(u));
  const diff = TOML.diff.changed.find((c) => c.path === path);
  return `<div class="speek" style="left:292px;top:${top}px">
    <div class="t">${mark(i.f, 12)}${esc(short(path))}<span class="k">${esc(i.k)} · ${esc(modOf(path))}</span></div>
    <div class="s">${esc(i.sig)}</div>
    ${Object.keys(u).length ? `<div class="u">${CRATES.filter((c) => u[c]).map((c) => `<div class="ur"><span>${c}</span><span class="bar" style="width:${(u[c] / max) * 100}%"></span><span class="n">${u[c]}</span></div>`).join("")}</div>` : ""}
    ${diff ? `<div class="chg"><div class="lb">in 1.1.6</div><div class="was">${esc(diff.before)}</div><div class="now">${esc(diff.after)}</div></div>` : ""}
    <div class="f"><span><kbd>↵</kbd> open</span><span><kbd>H</kbd> hold</span><span><kbd>→</kbd> members</span><span><kbd>G</kbd> graph</span></div></div>`;
}

// ---------------------------------------------------------------- the readers
const machine = (sig, name) => {
  const m = (sig || "").match(/\(([^)]*)\)\s*(?:->\s*(.*))?$/);
  if (!m) return `<span class="kw">${esc((sig.match(/^(?:pub )?(\w+)/) || [0, ""])[1])}</span>`;
  const ins = m[1].split(",").map((x) => x.trim()).filter((x) => x && !/self$/.test(x)).map((x) => { const t = x.split(":").slice(1).join(":").trim(); return /str$/.test(t) ? "text" : /\[u8\]$/.test(t) ? "bytes" : t.replace(/^&('\w+ )?(mut )?/, "").replace(/<.*>/, "").replace(/.*::/, ""); });
  const r = (m[2] || "").trim(); const res = r.match(/^Result<([^,>]+)/); const opt = r.match(/^Option<(.+)>$/);
  const out = res ? res[1] : opt ? opt[1] : r;
  return `<span class="mach">${ins.length ? `<span class="in">${esc(ins.join(", "))}</span><i class="w"></i>` : ""}<b>${esc(name)}</b>${out ? `<i class="w"></i><span class="out">${esc(out.replace(/<.*>/, "").replace(/.*::/, "").replace(/^&('\w+ )?/, ""))}</span>` : ""}${res ? `<span class="fail">may fail</span>` : ""}${opt ? `<span class="maybe">or nothing</span>` : ""}</span>`;
};

function packageReader() {
  // the package opened: each module a section; items with their shape, uses and changes; members on demand
  const sec = (m) => {
    const items = itemsIn(m); const u = items.reduce((a, i) => a + deepUses(i), 0), c = items.reduce((a, i) => a + deepChanges(i), 0);
    const kinds = {}; items.forEach((i) => { kinds[i.k] = (kinds[i.k] || 0) + 1; });
    const rows = items.slice(0, m === "value" ? 3 : 4).map((i) => {
      const mem = members(i); const used = mem.filter((x) => usesOf(x)).sort((a, b) => usesOf(b) - usesOf(a));
      const open = short(i.path) === "Value";
      return `<div class="pi ${open ? "open" : ""}"><div class="pr" data-share="row:${esc(m)}/${esc(short(i.path))}">${mark(i.f, 11)}<span class="pn">${esc(short(i.path))}</span><span class="ps">${machine(i.sig, short(i.path))}</span><span class="pg">${deepUses(i) ? `<span class="g use">${deepUses(i)}</span>` : ""}${deepChanges(i) ? `<span class="g ch">${deepChanges(i)}</span>` : ""}</span></div>
        ${open ? `<div class="pmem">${used.map((x) => `<span class="pm ${x.path === SEL ? "twin" : ""}">${mark(x.f, 9)}${esc(short(x.path))}<i>${usesOf(x)}</i></span>`).join("")}<span class="pmore">+ ${mem.length - used.length} more · ${mem.filter(changed).length} change in 1.1.6</span></div>` : mem.length ? `<div class="pmem q">${mem.length} members</div>` : ""}</div>`;
    }).join("");
    return `<div class="psec"><div class="ph" data-share="sec:${esc(m)}"><span class="pmod">${esc(m === "toml" ? "toml (root)" : m)}</span><span class="pk">${Object.entries(kinds).map(([k, n]) => `${n} ${k}${n > 1 ? "s" : ""}`).join(" · ")}</span><span class="pg">${u ? `<span class="g use">${u} uses</span>` : ""}${c ? `<span class="g ch">${c} change</span>` : ""}</span></div>${rows}${items.length > (m === "value" ? 3 : 4) ? `<div class="pmore2">${items.length - (m === "value" ? 3 : 4)} more in ${esc(m)}</div>` : ""}</div>`;
  };
  return `<div class="pkg2"><div class="top"><span data-share="gem:toml">${gemMini(48)}</span><div><h1 data-share="name:toml" data-text="1">toml</h1><div class="lede">A native Rust encoder and decoder of TOML-formatted files and streams.</div></div></div>
    <div class="facts2"><span class="v">0.8.23</span><i>·</i><span>crates.io</span><i>·</i><span>MIT / Apache-2.0</span><i>·</i><span><b>30</b> items in 6 modules</span><i>·</i><span class="y">your code uses 7 of them, 80 times</span></div>
    <div class="secs">${["value", "de", "ser", "map"].map(sec).join("")}</div></div>`;
}
function gemMini(s) { return `<svg width="${s}" height="${s}" viewBox="0 0 48 48" style="flex:none;color:var(--k-ns)"><path d="M24 2 46 24 24 46 2 24z" fill="currentColor" fill-opacity=".12" stroke="currentColor"/><path d="M24 10 38 24 24 38 10 24z" fill="none" stroke="currentColor" stroke-opacity=".55" stroke-width=".7"/></svg>`; }

function usersReader(crate) {
  const sites = TOML.uses.filter((u) => crateOf(u.file) === crate);
  const by = new Map(); sites.forEach((u) => (by.get(u.decl) || by.set(u.decl, []).get(u.decl)).push(u));
  const groups = [...by.entries()].sort((a, b) => b[1].length - a[1].length).map(([decl, us]) => {
    const n = short(decl); const i = byPath.get(decl);
    return `<div class="ug"><div class="uh">${mark(i ? i.f : "callable", 11)}<span class="un">${esc(decl.replace(/^toml::value::/, "").replace(/^toml::de::/, ""))}</span><span class="uc">${us.length} ${us.length === 1 ? "place" : "places"}</span>${i && changed(i) ? `<span class="g ch">changes in 1.1.6</span>` : ""}</div>
      ${us.slice(0, 3).map((u) => `<div class="us"><span class="uf">${esc(u.file.replace(/^apps\/desktop\/src\//, "").replace(/^crates\//, ""))}:${u.line}</span><code>${esc(u.text).replace(new RegExp(esc(n).replace(/[.*+?^${}()|[\]\\]/g, "\\$&")), `<u>${esc(n)}</u>`)}</code></div>`).join("")}${us.length > 3 ? `<div class="umore">${us.length - 3} more</div>` : ""}</div>`;
  }).join("");
  return `<div class="pkg2"><div class="uhead"><span class="ut">How <b>${crate}</b> uses toml</span><span class="us2"><span class="y">${sites.length}</span> places · ${by.size} items · the rest of toml (${tops.length - by.size} items) it never touches</span></div><div class="ugs">${groups}</div></div>`;
}

function versionsReader() {
  const vs = TOML.versions; const pin = vs.indexOf("0.8.23"), tgt = vs.indexOf("1.1.6");
  const ticks = vs.map((v, k) => `<i class="${k === pin ? "pin" : ""} ${k === tgt ? "tgt" : ""} ${k > pin && k < tgt ? "between" : ""} ${TOML.yanked.includes(v) ? "yk" : ""}" style="height:${/\.0$/.test(v) ? 16 : 8}px"></i>`).join("");
  const imp = TOML.impact;
  const d = TOML.diff;
  // a change belongs to the module it is declared in: the first lower-case segment after the crate
  const modFor = (p) => { const seg = p.split("::").slice(1); const m = seg.findIndex((x) => /^[A-Z]/.test(x)); const mods = seg.slice(0, m < 0 ? seg.length - 1 : m); return mods.join("::") || "toml"; };
  const perMod = {}; for (const k of ["added", "changed", "removed", "deprecated"]) for (const c of d[k]) { if (TOML.reexports[c.path.split("::").slice(0, 2).join("::")] && c.path.split("::").length > 1 && /^[A-Z]/.test(c.path.split("::")[1])) continue; const m = modFor(c.path); (perMod[m] ||= { added: 0, changed: 0, removed: 0, deprecated: 0 })[k]++; }
  const site = (u) => `<div class="us"><span class="uf">${esc(u.file.replace(/^apps\/desktop\/src\//, "desktop/").replace(/^crates\//, ""))}:${u.line}</span><code>${esc(u.text)}</code></div>`;
  return `<div class="pkg2"><div class="vhead"><span class="vt">toml <span class="mono">0.8.23</span> <span class="arr">→</span> <span class="mono peri">1.1.6</span></span><span class="vs">${tgt - pin} releases later · a major version</span></div>
    <div class="verdict"><span class="vk major">major</span><span class="vg">In general: <b>${d.changed.length}</b> changed, <b>${d.removed.length}</b> removed, <b>${d.deprecated.length}</b> deprecated.</span><span class="vk ok">for you: compatible</span><span class="vg">The one API you use that changed (<span class="mono">from_str</span>) keeps working at all ${imp.length} of your call sites.</span></div>
    <div class="comb2">${ticks}</div><div class="combcap"><span class="mono">${vs[0]}</span><span><span class="mint">▍</span>your pin 0.8.23 <span class="peri">▍</span>looking at 1.1.6 · click a release to look at it, shift-click for a range</span><span class="mono">${vs[vs.length - 1]}</span></div>
    <div class="vsec"><div class="vh">What changes for you <span class="amber">${imp.length} of your places touch it</span></div>
      <div class="vitem"><div class="uh">${mark("callable", 11)}<span class="un">from_str</span><span class="uc">signature changed</span></div>
        <div class="sigdiff"><div><span class="was">pub fn from_str&lt;T&gt;(s: &amp;'_ str) -&gt; Result&lt;T, Error&gt;</span></div><div><span class="now">pub fn from_str&lt;<ins>'a, </ins>T&gt;(s: &amp;<ins>'a</ins> str) -&gt; Result&lt;T, Error&gt;</span></div></div>
        <div class="vnote">The input's lifetime is now named: <code>T</code> may borrow from the text. Calls that parse into an owned value, like all four of yours, compile unchanged.</div>
        ${imp.map(site).join("")}</div>
      <div class="vitem q"><div class="uh">${mark("type", 11)}<span class="un">Value</span><span class="uc">29 places · unchanged</span></div><div class="uh">${mark("callable", 11)}<span class="un">as_str, as_table, as_bool, as_array</span><span class="uc">39 places · unchanged</span></div></div>
    </div>
    <div class="vsec"><div class="vh">Everything else in 1.1.6 <span class="q"><span class="mint">+${d.added.length}</span> · <span class="amber">~${d.changed.length}</span> · <span class="coral">−${d.removed.length}</span> · ${d.deprecated.length} deprecated</span></div>
      ${Object.entries(perMod).sort((a, b) => (b[1].added + b[1].changed) - (a[1].added + a[1].changed)).slice(0, 5).map(([m, c]) => `<div class="vrow"><span class="vm">${esc(m)}</span><span class="vb"><i class="a" style="width:${c.added * 3}px"></i><i class="c" style="width:${c.changed * 3}px"></i><i class="r" style="width:${(c.removed + c.deprecated) * 3}px"></i></span><span class="vn"><span class="mint">+${c.added}</span> <span class="amber">~${c.changed}</span>${c.removed ? ` <span class="coral">−${c.removed}</span>` : ""}${c.deprecated ? ` <span class="coral">${c.deprecated} deprecated</span>` : ""}</span></div>`).join("")}
    </div></div>`;
}

function restsReader(hot = "winnow") {
  // Strata: what you stand on, as layers. Width is size (public items; unread packages get a
  // nominal width, dashed). Hovering a block lights the chain from your code down to it: why it's here.
  const L = [
    [["desktop", 0, "yours"], ["local-service", 0, "yours"], ["engine", 0, "yours"], ["advisory", 0, "yours"]],
    [["toml", 30, ""]],
    [["toml_edit", 80, ""], ["serde_core", 59, ""], ["toml_datetime", 9, ""], ["serde_spanned", 0, "rough"], ["indexmap", 0, "rough"]],
    [["winnow", 0, "rough"], ["toml_write", 0, "rough"], ["hashbrown", 0, "rough"], ["equivalent", 0, "rough"]],
  ];
  const parent = { toml: ["desktop", "local-service", "engine", "advisory"], toml_edit: ["toml"], serde_core: ["toml", "toml_edit", "toml_datetime"], toml_datetime: ["toml", "toml_edit"], serde_spanned: ["toml", "toml_edit"], indexmap: ["toml", "toml_edit"], winnow: ["toml_edit"], toml_write: ["toml_edit"], hashbrown: ["indexmap"], equivalent: ["indexmap"] };
  const uses = { desktop: 33, "local-service": 20, engine: 16, advisory: 11 };
  const W = 800, gap = 10, rowH = 46, pitch = 96;
  const pos = {};
  L.forEach((layer, li) => {
    // every block holds its name; what's left is shared by size
    const mins = layer.map(([name]) => name.length * 7.8 + 24);
    const wts = layer.map(([, n, k]) => (k === "yours" ? 1 : n ? Math.sqrt(n) : 0));
    const free = Math.max(0, W - gap * (layer.length - 1) - mins.reduce((a, b) => a + b, 0));
    const total = wts.reduce((a, b) => a + b, 0) || 1;
    let x = 0; layer.forEach(([name, n, k], i) => { const w = mins[i] + (free * wts[i]) / total; pos[name] = { x, y: li * pitch, w, h: rowH, n, k, li }; x += w + gap; });
  });
  const chain = new Set([hot]); const up = (n) => (parent[n] || []).forEach((p) => { if (!chain.has(p)) { chain.add(p); up(p); } }); up(hot);
  let svg = `<svg class="strata" width="${W}" height="${L.length * pitch}" viewBox="0 0 ${W} ${L.length * pitch}">`;
  for (const [child, ps] of Object.entries(parent)) for (const p of ps) {
    const a = pos[p], b = pos[child]; if (!a || !b) continue;
    const on = chain.has(child) && chain.has(p);
    const x1 = a.x + a.w / 2, y1 = a.y + a.h, x2 = b.x + b.w / 2, y2 = b.y;
    svg += `<path class="${on ? "on" : ""}" d="M${x1} ${y1}C${x1} ${y1 + 30} ${x2} ${y2 - 30} ${x2} ${y2}"/>`;
  }
  for (const [name, q] of Object.entries(pos)) {
    const on = chain.has(name);
    svg += `<g class="blk ${q.k} ${on ? "on" : ""} ${name === hot ? "hot" : ""}"><rect x="${q.x}" y="${q.y}" width="${q.w}" height="${q.h}"/><text x="${q.x + 10}" y="${q.y + 20}">${esc(name)}</text><text class="n" x="${q.x + 10}" y="${q.y + 36}">${q.k === "yours" ? `uses toml ${uses[name]}×` : q.k === "rough" ? "not read yet" : `${q.n} public items`}</text></g>`;
  }
  svg += `</svg>`;
  return `<div class="pkg2"><div class="uhead"><span class="ut">What toml rests on</span><span class="us2">5 directly · 9 in all · 6 not read yet · hover a layer to see why it's here</span></div>
    <div class="why"><span class="wq">Why is <b>winnow</b> here?</span> <span class="wa">desktop, local-service, engine and advisory use <span class="mono">toml</span> → toml reads through <span class="mono">toml_edit</span> → toml_edit parses with <span class="mono">winnow</span>.</span></div>
    <div class="strata-wrap">${svg}<div class="lbls"><span>your code</span><span>toml</span><span>directly</span><span>beneath</span></div></div>
    <div class="dnote">Also in your tree twice: <span class="mono">toml</span> 0.8.23 and 1.1.6, <span class="mono">toml_datetime</span> 0.6.11 and 1.1.1. Removing <span class="mono">toml</span> would drop 8 of these 9 with it.</div></div>`;
}

// ---------------------------------------------------------------- the states
const await_lib = await (async () => { const d = await load("data/library.json"); d.forEach((p) => { p.mods = p.modules.map((m) => ({ path: m.path, items: m.items })); p.all = p.mods.flatMap((m) => m.items); }); return d; })();
export function sideView(which, win, jumpbar) {
  const base = { crumbs: ["Library", "toml"], menu: true, book, held: HELD, counts, trail: TRAIL };
  // the jump bar is the path, and each segment opens its siblings (here: the modules beside value)
  const jb = (crumbs, open) => open ? `<div class="jump jmenu-host">${crumbs.map((c, i) => (i ? `<span class="sep">›</span>` : "") + `<span class="${i === crumbs.length - 1 ? "b" : ""} ${c === open ? "open" : ""}">${esc(c)}</span>`).join("")}</div>${siblingMenu()}` : jumpbar(crumbs);
  if (which === "side-plain") return win(jumpbar(["toml"]), sidebar({ ...base, lens: "contents", rows: contentsRows(null) }) + `<div class="reader">${packageReader()}</div>`);
  if (which === "side-contents") {
    SEL = "toml::value::Value::as_str";
    const rows = contentsRows("toml::value::Value::as_str");
    const selIndex = rows.findIndex((r) => r.sel);
    return win(jumpbar(["toml", "value", "Value"]), sidebar({ ...base, lens: "contents", rows }) + `<div class="reader">${packageReader()}</div>` + speek("toml::value::Value::as_str", 178 + 28 * selIndex));
  }
  if (which === "side-narrow") {
    const q = "as_";
    const value = tops.find((i) => short(i.path) === "Value");
    const hits = members(value).filter((m) => short(m.path).startsWith(q)).sort((a, b) => usesOf(b) - usesOf(a));
    const rows = [{ id: "value", depth: 0, kind: "group", open: true, gem: mark("contract", 10).replace("km fco", "km fns"), name: "value", count: null }, itemRow(value, 1, { open: true }), ...hits.map((m, k) => ({ id: m.path, depth: 2, kind: "item", f: m.f, name: short(m.path), hit: [0, q.length], uses: usesOf(m) || null, change: m.change === "changed", sel: k === 0 }))];
    return win(jumpbar(["toml", "value", "Value"]), sidebar({ ...base, lens: "contents", rows, narrow: q, narrowN: hits.length, narrowOf: TOML.items.length }) + `<div class="reader">${packageReader()}</div>`);
  }
  if (which === "side-users") {
    const rows = [{ kind: "head", name: "Yours", note: "this workspace" }, ...["desktop", "local-service", "engine", "advisory"].map((c) => { const n = TOML.uses.filter((u) => crateOf(u.file) === c).length; return { id: c, depth: 0, kind: "item", gem: gemMini(12), name: c, uses: n, sel: c === "desktop", sub: `${new Set(TOML.uses.filter((u) => crateOf(u.file) === c).map((u) => u.decl)).size} items` }; }), { kind: "head", name: "In the library", note: "indexed" }, { id: "toml_pin", depth: 0, kind: "item", gem: gemMini(12), name: "toml_pin", uses: 3, sub: "3 items" }];
    return win(jumpbar(["toml", "used by", "desktop"]), sidebar({ ...base, lens: "users", rows }) + `<div class="reader">${usersReader("desktop")}</div>`);
  }
  if (which === "side-versions") {
    const vs = TOML.versions; const pin = vs.indexOf("0.8.23");
    const pick = [...vs.slice(-8).reverse(), "…", ...vs.slice(pin - 1, pin + 3).reverse()];
    const rows = [{ kind: "head", name: "Releases", note: "newest first" }, ...pick.map((v) => v === "…" ? { id: "gap", depth: 0, kind: "text", name: `${vs.indexOf("1.1.6") - 8 - pin} releases between` } : { id: v, depth: 0, kind: "item", gem: `<i class="vt ${v === "0.8.23" ? "pin" : v === "1.1.6" ? "tgt" : ""}"></i>`, name: v, sub: v === "0.8.23" ? "your pin" : v === "1.1.6" ? "looking at" : "", sel: v === "1.1.6", current: v === "0.8.23", change: v === "1.1.6", changeN: v === "1.1.6" ? 187 : null })];
    return win(jumpbar(["toml", "0.8.23 → 1.1.6"]), sidebar({ ...base, lens: "versions", rows }) + `<div class="reader">${versionsReader()}</div>`);
  }
  if (which === "side-rests") {
    const rows = [{ kind: "head", name: "Directly", note: "5" }, ...[["toml_edit", "0.22.27", 80], ["serde_core", "1.0.145", 59], ["toml_datetime", "0.6.11", 9], ["serde_spanned", "0.6.9", null], ["indexmap", "2.0.0", null]].map(([n, v, c], k) => ({ id: n, depth: 0, kind: c ? "group" : "item", gem: gemMini(12), name: n, sub: v, count: c, dim: !c, sel: k === 0 })), { id: "we", depth: 1, kind: "item", gem: gemMini(10), name: "winnow", sub: "0.7", dim: true }, { id: "tw", depth: 1, kind: "item", gem: gemMini(10), name: "toml_write", sub: "0.1", dim: true }];
    return win(jumpbar(["toml", "rests on"]), sidebar({ ...base, lens: "rests", rows }) + `<div class="reader">${restsReader()}</div>`);
  }
  if (which === "side-hoist") {
    const value = tops.find((i) => short(i.path) === "Value");
    const rows = [itemRow(value, 0, { open: true, current: true }), ...memberRows(value, 1, { all: true, sel: "toml::value::Value::as_str" })];
    return win(jumpbar(["toml", "value", "Value"]), sidebar({ ...base, crumbs: ["Library", "toml", "value", "Value"], scopeTitle: `${mark("type", 11)}<b>Value</b><span class="bv">enum · ${members(value).length} members</span>`, book: null, lens: "contents", rows, counts: { ...counts, contents: members(value).length } }) + `<div class="reader">${packageReader()}</div>`);
  }
  if (which === "side-item") {
    // the same four lenses at item scope: Contents = members, Versions = its history,
    // Rests on = what it is made of, Used by = who calls it. The reader is the drawn page.
    const value = tops.find((i) => short(i.path) === "Value");
    // one rule for every count: a use counts toward the item it resolves to, when that item is in the API
    const users = TOML.uses.filter((u) => (u.decl === value.path || u.decl.startsWith(value.path + "::")) && byPath.has(u.decl));
    const rows = [{ kind: "head", name: "Your code", note: `${users.length} places` }, ...CRATES.map((c) => { const n = users.filter((u) => crateOf(u.file) === c).length; return n ? { id: c, depth: 0, kind: "group", open: c === "desktop", gem: gemMini(12), name: c, uses: n } : null; }).filter(Boolean)];
    const di = rows.findIndex((r) => r.id === "desktop");
    const dsites = users.filter((u) => crateOf(u.file) === "desktop");
    const byItem = new Map(); dsites.forEach((u) => byItem.set(u.decl, (byItem.get(u.decl) || 0) + 1));
    rows.splice(di + 1, 0, ...[...byItem.entries()].sort((a, b) => b[1] - a[1]).map(([d, n], k) => ({ id: d, depth: 1, kind: "item", f: byPath.get(d)?.f || "callable", name: d === "toml::value::Value" ? "Value (the type)" : "." + short(d), uses: n, sel: k === 0 })));
    const reader = `<div class="reader"><iframe class="pageframe" src="../../v5/page/Page.html?p=rs-value&embed=1"></iframe></div>`;
    return win(jumpbar(["toml", "value", "Value"]), sidebar({ ...base, crumbs: ["toml", "Value"], scopeTitle: `${mark("type", 11)}<b>Value</b><span class="bv">enum</span>`, book: null, lens: "users", rows, counts: { contents: members(value).length, versions: 1, rests: 3, users: users.length } }) + reader);
  }
  if (which === "side-library") {
    const LIBR = await_lib;
    const rel = { toml: ["direct", "0.8.23", "1.1.6"], toml_edit: ["beneath", "0.22.27"], toml_datetime: ["beneath", "0.6.11", "1.1.1"], serde_core: ["beneath", "1.0.229"], smallvec: ["loose", "1.16.0", "2.0.0-beta.1"] };
    const group = (p) => (p.name === "toml_pin" || p.name === "rich_project" ? "yours" : rel[p.name] ? rel[p.name][0] : "loose");
    const G = [["yours", "Yours"], ["direct", "You rest on"], ["beneath", "Beneath them"], ["loose", "Also in the library"]];
    const rows = [];
    for (const [g, title] of G) {
      const ps = LIBR.filter((p) => group(p) === g);
      if (!ps.length) continue;
      rows.push({ kind: "head", name: title, note: String(ps.length) });
      for (const p of ps) { const r = rel[p.name]; rows.push({ id: p.name, depth: 0, kind: "item", gem: gemMini(12), name: p.name, sub: r && r[2] ? "" : p.version || "", uses: p.name === "toml" ? 80 : p.name === "smallvec" ? 8 : null, change: r && r[2] ? true : false, changeN: r && r[2] ? `${(p.version || "").split(".").slice(0, 2).join(".")} → ${r[2].replace(/-beta\.\d+$/, "β")}` : null, sel: p.name === "toml" }); }
    }
    const card = (p) => {
      const r = rel[p.name];
      const yours = p.name === "toml" ? new Set(p.mods.flatMap((m) => m.items.filter((i) => ["Value", "from_str", "Error"].includes(i.n) && ["value", "de"].includes(m.path)))) : new Set();
      const changed = p.name === "toml" ? new Map(p.mods.flatMap((m) => m.items.filter((i) => ["from_str", "Error", "Deserializer", "ValueDeserializer", "Serializer", "Map"].includes(i.n) && !yours.has(i)).map((i) => [i, "changed"]))) : new Map();
      const t = layout(p.mods, 300, { pitch: 9, stone: 6.5, pad: 4, label: 0, rows: 2 });
      return `<div class="lcard ${p.name === "toml" ? "on" : ""}"><div class="lh">${gemMini(18)}<span class="ln2">${esc(p.name)}</span><span class="lv">${esc(p.version || "")}</span>${r && r[2] ? `<span class="lup">→ ${r[2]}</span>` : ""}</div>
        <div class="lf">${p.all.length} public items${p.name === "toml" ? ` · <span class="mint">you use 3, 80 times</span>` : ""}${r && r[2] && p.name === "toml" ? ` · <span class="amber">1.1.6 changes 1 you use</span>` : ""}</div>
        ${render(t, { yours, changed, dim: yours.size > 0 || changed.size > 0 }, { stone: 6.5, cls: "mini", labels: false })}</div>`;
    };
    const shown = ["toml", "toml_edit", "serde_core", "toml_datetime", "serde_json", "smallvec"].map((n) => LIBR.find((p) => p.name === n)).filter(Boolean);
    const reader = `<div class="reader"><div class="pkg2" style="width:960px;left:80px"><div class="uhead"><span class="ut">Library</span><span class="us2">13 packages · 3 languages · 2 have newer releases · 1 of them changes something you use</span></div><div class="lcards">${shown.map(card).join("")}</div></div></div>`;
    return win(jumpbar(["Library"]), sidebar({ ...base, crumbs: ["Library"], book: `<span class="bn">Library</span><span class="bv">13 packages</span>`, lens: "contents", rows, counts: { contents: 13, versions: 3, rests: null, users: 2 } }) + reader);
  }
  if (which === "side-jump") {
    const rows = contentsRows(null);
    return win(jb(["toml", "value", "Value"], "value"), sidebar({ ...base, lens: "contents", rows }) + `<div class="reader">${packageReader()}</div>`);
  }
  return "";
}
