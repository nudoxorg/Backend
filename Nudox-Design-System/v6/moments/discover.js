// Discover: the registry, browsable by aisle (one row per category), with Add where you already are.
// Your project is the dock pinned at the top; hovering any card lights on the dock what that card would
// share with you and marks where it would land. Press + and it flies there.
import { gem } from "./paint.js";
import { icon } from "./badges.js";
import { verdict } from "./license.js";
import { kLines } from "./preview.js";
import { fmt, plural } from "./world.js";
import { PLAY, CARRY, play, lerp, clamp, ease, arc, wait } from "./motion.js";

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const NOW = new Date("2026-09-28");
const years = (d) => (d ? (NOW - new Date(d)) / 864e5 / 365 : null);

export async function discover({ WD, TRUST, reader, side, jump, Q }) {
  let REG = [], CATS = {};
  try { REG = await (await fetch("data/registry.json", { cache: "reload" })).json(); } catch { REG = []; }
  try { CATS = await (await fetch("data/categories.json", { cache: "reload" })).json(); } catch { CATS = {}; }
  if (!Array.isArray(REG)) REG = REG.crates || Object.values(REG);
  const byId = new Map(REG.map((r) => [r.id, r]));
  const into = WD.yours.find((p) => p.name === "backend-desktop") || WD.yours[0];
  const sl = (p) => (TRUST[p.id] && TRUST[p.id].sloc) || 0;
  jump.innerHTML = `<span class="c">Library</span><span class="rel">›</span><b>Discover</b>`;

  // ---------------------------------------------------------------- the dock: your project, pinned
  const dockDeps = () => into.depsP.slice().sort((a, b) => sl(b) - sl(a));
  function dockHtml() {
    const deps = dockDeps();
    return `<div class="dock"><div class="dk-h">${gem(18, "var(--mint)")}<b>${esc(into.name.replace(/^backend-/, ""))}</b><span>rests on <b class="dk-n">${deps.length}</b></span><span class="dk-say"></span></div>
      <div class="dk-row">${deps.map((p) => `<span class="dk-b ${p.fresh ? "fresh" : ""}" data-id="${esc(p.id)}" data-name="${esc(p.name)}" style="width:${Math.max(4, Math.min(46, Math.sqrt(sl(p)) / 4))}px"></span>`).join("")}<span class="dk-ghost"></span></div></div>`;
  }

  // ---------------------------------------------------------------- a registry card
  function card(r) {
    const v = verdict(r.license);
    const heads = [];
    if (r.build_rs) heads.push(["build", "amber"]); if (r.proc_macro) heads.push(["macroPkg", "amber"]);
    const caps = r.caps || {};
    if (caps.process || caps.process_n) heads.push(["process", "amber"]); if (caps.net || caps.net_n) heads.push(["net", ""]); if (caps.fs || caps.fs_n) heads.push(["files", ""]); if (caps.ffi || caps.ffi_n) heads.push(["ffi", "amber"]);
    if (r.forbid_unsafe) heads.push(["shield", "mint"]); else if (r.unsafe) heads.push(["unsafe", r.unsafe > 50 ? "amber" : ""]);
    const nw = r.brings ? (r.brings.new || []).length : 0, sh = r.brings ? (r.brings.shared || []).length : 0;
    const rel = r.releases || {};
    const age = years(rel.first), last = years(rel.last);
    const n = Math.min(r.items || 0, 60);
    return `<div class="rc" data-id="${esc(r.id)}" tabindex="0">
      <div class="rc-t">${gem(16, "var(--k-ns)", true)}<b>${esc(r.name)}</b><span class="rc-v">${esc(r.version)}</span><button class="rc-add" title="add to ${esc(into.name)}">${icon("ctor")}</button></div>
      <div class="rc-d">${esc(r.lede || "")}</div>
      <div class="rc-fp">${Array.from({ length: n }, (_, i) => `<i style="--k:${i}"></i>`).join("")}${r.items > 60 ? `<em>+${fmt(r.items - 60)}</em>` : ""}</div>
      <div class="rc-life"><span class="rl-bar"><i style="left:0;width:100%"></i><b style="left:${age ? clamp(1 - (last || 0) / Math.max(age, 0.01)) * 100 : 100}%"></b></span><span>${rel.n ? `${plural(rel.n, "release")} · ${last != null ? (last < 0.25 ? "active" : `last ${last.toFixed(1)} y ago`) : ""}` : ""}</span></div>
      <div class="rc-f"><span class="seal-s ${v.tone}">${icon(v.tone === "mint" ? "shield" : "unsafe")}</span><span class="rc-hs">${heads.map(([k, t], i) => `<span class="mchip ${t}" style="--i:${i}">${icon(k)}</span>`).join("")}</span>
        <span class="rc-cost">${nw ? `<b class="am">+${nw}</b> new` : `<b class="y">0</b> new`} · <b class="y">${sh}</b> shared</span></div>
      <div class="rc-more">${esc(r.readme || "")}${r.brings && (r.brings.new || []).length ? `<div class="rc-pull">pulls in ${(r.brings.new || []).slice(0, 6).map(esc).join(", ")}</div>` : ""}</div></div>`;
  }

  // ---------------------------------------------------------------- aisles
  const catNames = Object.keys(CATS).filter((k) => k !== "yours" && CATS[k] && CATS[k].crates && CATS[k].crates.length);
  catNames.sort((a, b) => (CATS[b].count || CATS[b].crates.length) - (CATS[a].count || CATS[a].crates.length));
  const pretty = (slug) => slug.replace(/::/g, " › ").replace(/-/g, " ");
  function aisle(slug, list, title = null) {
    const mine = ((CATS.yours || {})[slug] || []).map((id) => WD.byId.get(id)).filter(Boolean);
    return `<div class="aisle"><div class="ai-h"><b>${esc(title || pretty(slug))}</b><span>${plural(list.length, "crate")}</span>${mine.length ? `<span class="ai-y">you already have ${mine.slice(0, 4).map((p) => `<a class="pk y" data-pkg="${esc(p.id)}">${esc(p.name)}</a>`).join(" ")}${mine.length > 4 ? ` +${mine.length - 4}` : ""}</span>` : ""}</div>
      <div class="ai-row">${list.map(card).join("")}</div></div>`;
  }
  function aisles(q = "") {
    if (q) {
      const n = q.toLowerCase();
      const hits = REG.map((r) => ({ r, s: (r.name.toLowerCase() === n ? 100 : 0) + (r.name.toLowerCase().includes(n) ? 30 : 0) + ((r.keywords || []).some((k) => k.includes(n)) ? 10 : 0) + ((r.lede || "").toLowerCase().includes(n) ? 5 : 0) })).filter((x) => x.s > 0).sort((a, b) => b.s - a.s || (b.r.releases?.n || 0) - (a.r.releases?.n || 0)).slice(0, 24).map((x) => x.r);
      return aisle("", hits, `“${q}”`);
    }
    return catNames.slice(0, 7).map((slug) => aisle(slug, CATS[slug].crates.map((id) => byId.get(id)).filter(Boolean).slice(0, 16))).join("");
  }
  reader.innerHTML = `<div class="disc">${dockHtml()}<div class="ds-h"><h1>Discover</h1><div class="ds-q"><span class="qi">${icon("iter")}</span><input placeholder="search ${fmt(REG.length)} crates on disk" value="${esc(Q.get("q2") || "")}"></div></div><div class="aisles">${aisles(Q.get("q2") || "")}</div><div class="toast"></div></div>`;
  const disc = reader.querySelector(".disc"), dock = () => reader.querySelector(".dock");
  side.innerHTML = `<div class="scope"><span class="up">‹ Library</span></div><div class="scope" style="padding-top:4px">Discover</div><div class="hint">aisles by category</div><div class="rows">${catNames.slice(0, 20).map((c) => `<div class="r">${esc(pretty(c))}<span class="g">${fmt(CATS[c].count || CATS[c].crates.length)}</span></div>`).join("")}</div>`;
  const input = reader.querySelector(".ds-q input");
  input.oninput = () => { reader.querySelector(".aisles").innerHTML = aisles(input.value.trim()); wire(); };

  // ---------------------------------------------------------------- hover lights the dock; + plays the card
  function light(r) {
    const d = dock(); if (!d) return;
    const shared = new Set(r && r.brings ? r.brings.shared || [] : []);
    const names = new Set([...shared].map((id) => (WD.byId.get(id) || {}).name));
    d.querySelectorAll(".dk-b").forEach((b) => b.classList.toggle("sh", !!r && (shared.has(b.dataset.id) || names.has(b.dataset.name))));
    d.classList.toggle("armed", !!r);
    const g = d.querySelector(".dk-ghost");
    g.style.width = r ? Math.max(6, Math.min(46, Math.sqrt(r.sloc || 100) / 4)) + "px" : "0px";
    d.querySelector(".dk-say").innerHTML = r ? `<b>${esc(r.name)}</b> would share <b class="y">${shared.size}</b> with you and bring <b class="am">${(r.brings?.new || []).length}</b> new${r.brings?.new_total > (r.brings?.new || []).length ? ` (${r.brings.new_total} with everything under them)` : ""}` : "";
  }
  async function add(r, el) {
    const d = dock(), g = d.querySelector(".dk-ghost");
    light(r);
    const from = el.getBoundingClientRect(), to = g.getBoundingClientRect();
    const fly = el.cloneNode(true); fly.classList.add("flying2"); Object.assign(fly.style, { position: "fixed", left: from.left + "px", top: from.top + "px", width: from.width + "px", margin: 0, zIndex: 80 });
    document.body.appendChild(fly); el.style.visibility = "hidden";
    // The consequence flashes on the card while it is still big; then the flight.
    fly.querySelector(".rc-cost").classList.add("flash");
    await wait(260);
    const a = { x: from.left + from.width / 2, y: from.top + from.height / 2 }, b = { x: to.left + to.width / 2, y: to.top + to.height / 2 };
    await play(560, (ms) => {
      const u = PLAY(ms), v = clamp(ms / 560);
      const p = arc(a, b, clamp(u, 0, 1.05), 120);
      const s = lerp(1, Math.max(0.04, to.width / from.width), ease.inOut(v));
      fly.querySelectorAll(".rc-d,.rc-fp,.rc-life,.rc-f,.rc-more,.rc-t").forEach((n) => (n.style.opacity = 1 - clamp(v / 0.35)));
      fly.style.transform = `translate(${p.x - a.x}px, ${p.y - a.y}px) scale(${s}) rotate(${Math.sin(Math.PI * v) * -8}deg)`;
    });
    fly.remove();
    // Into the project: it lands where the ghost was; the count ticks as it lands.
    const P = { id: r.id, name: r.name, version: r.version, kind: "registry", depth: 1, fresh: true, depsP: [], dependentsP: [into], used: new Map(), items: r.items || 0, modules: [], newer: [] };
    into.depsP.unshift(P); TRUST[P.id] = TRUST[P.id] || { sloc: r.sloc || 0 };
    const n0 = into.depsP.length - 1;
    d.outerHTML = dockHtml();
    const d2 = dock(); const nb = d2.querySelector(`.dk-b[data-id="${CSS.escape(r.id)}"]`); nb.classList.add("landed");
    const cnt = d2.querySelector(".dk-n");
    await play(360, (ms) => { cnt.textContent = Math.round(lerp(n0, n0 + 1, clamp(ms / 360))); });
    const toast = reader.querySelector(".toast");
    toast.innerHTML = `Added <b>${esc(r.name)}</b> to ${esc(into.name.replace(/^backend-/, ""))} <span class="am">brought ${plural((r.brings?.new || []).length, "new package")}</span> <span class="y">shares ${(r.brings?.shared || []).length}</span><span class="u">Undo</span>`;
    toast.classList.add("on"); setTimeout(() => toast.classList.remove("on"), 4200);
    el.classList.add("added"); el.style.visibility = "";
    wire();
  }
  function wire() {
    reader.querySelectorAll(".rc").forEach((el) => {
      const r = byId.get(el.dataset.id);
      el.onmouseenter = () => light(r); el.onmouseleave = () => light(null);
      const b = el.querySelector(".rc-add"); if (b) b.onclick = (e) => { e.stopPropagation(); if (!el.classList.contains("added")) add(r, el); };
    });
  }
  wire();
  if (Q.get("hover2")) { const el = reader.querySelector(`.rc[data-id^="${CSS.escape(Q.get("hover2"))}@"]`); if (el) { el.classList.add("hov"); light(byId.get(el.dataset.id)); } }
  if (Q.get("add2")) { const el = reader.querySelector(`.rc[data-id^="${CSS.escape(Q.get("add2"))}@"]`); if (el) { await wait(600); add(byId.get(el.dataset.id), el); } }
}
