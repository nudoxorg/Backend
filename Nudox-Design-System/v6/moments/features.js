// Features as a bar of switches you can scroll along and flip. Turning one on switches on everything it
// needs: those snap on too and lock (a small padlock closes on each), held by the one that asked for them,
// and the optional packages it pulls in appear at the end of the bar. Trying to turn off a locked one
// shakes it and names what holds it.
import { icon } from "./badges.js";
import { kLines } from "./preview.js";

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const LOCK = `<svg class="lk" viewBox="0 0 12 12" width="12" height="12"><path class="lk-s" d="M4 5.5V3.9a2 2 0 0 1 4 0v1.6" fill="none"/><rect x="2.8" y="5.5" width="6.4" height="4.6"/></svg>`;

export function featureBar(host, T) {
  if (!T || !T.features || !T.features.all.length) { host.innerHTML = ""; return; }
  const G = T.feature_graph || {};
  const names = [...new Set([...T.features.all, ...Object.keys(G)])].filter((n) => n !== "default");
  const needs = (f) => (G[f] && G[f].enables) || [];
  const deps = (f) => (G[f] && G[f].deps) || [];
  const on = new Set();
  const heldBy = new Map(); // feature → set of features holding it on
  function close(f, by, acc = new Set()) { for (const x of needs(f)) { if (!heldBy.has(x)) heldBy.set(x, new Set()); heldBy.get(x).add(by); if (!acc.has(x)) { acc.add(x); close(x, by, acc); } } return acc; }
  const chosen = new Set(T.features.default.filter((f) => f !== "default"));
  function recompute() {
    on.clear(); heldBy.clear();
    for (const f of chosen) { on.add(f); for (const x of close(f, f)) on.add(x); }
  }
  recompute();
  const size = T.feature_deps_size || {};
  function draw(flash = new Set(), shake = null) {
    const pulled = [...new Set([...on].flatMap(deps))];
    const lines = pulled.reduce((s, d) => s + (size[d] || 0), 0);
    host.innerHTML = `<div class="fb-k">Features <b>${on.size}</b> of ${names.length} on${pulled.length ? ` · pulls in <b>${pulled.length}</b>${lines ? ` (${kLines(lines)} lines)` : ""}` : ""}</div>
      <div class="fb-row">${names.map((f) => {
        const locked = !chosen.has(f) && on.has(f);
        const by = heldBy.has(f) ? [...heldBy.get(f)].filter((x) => chosen.has(x)) : [];
        return `<span class="fb ${on.has(f) ? "on" : ""} ${locked ? "locked" : ""} ${flash.has(f) ? "flash" : ""} ${shake === f ? "shake" : ""} ${T.features.default.includes(f) ? "def" : ""}" data-f="${esc(f)}" title="${locked ? `held on by ${by.join(", ")}` : needs(f).length ? `switches on ${needs(f).join(", ")}` : ""}">
          <i class="sw"></i><b>${esc(f)}</b>${locked ? LOCK : needs(f).length ? `<em>+${needs(f).length}</em>` : ""}</span>`;
      }).join("")}${pulled.length ? `<span class="fb-deps">${icon("ctor")}${pulled.slice(0, 8).map(esc).join(" · ")}</span>` : ""}</div>`;
    host.querySelectorAll(".fb").forEach((el) => (el.onclick = () => flip(el.dataset.f)));
  }
  function flip(f) {
    const before = new Set(on);
    if (on.has(f) && !chosen.has(f)) { draw(new Set(), f); return; } // locked: shake, say why
    if (chosen.has(f)) chosen.delete(f); else chosen.add(f);
    recompute();
    draw(new Set([...on].filter((x) => !before.has(x))));
  }
  draw();
}
