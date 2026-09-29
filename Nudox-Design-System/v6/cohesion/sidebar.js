// The sidebar: one scope at a time, four lenses on it, and a small set of primitives that work the
// same in every lens. Scope (hoist: Library › package › module › type, the crumbs are menus),
// Lens (Contents · Versions · Rests on · Used by), Narrow (just type), State (glyphs on rows: your
// uses in mint, changes to the target release in amber, gone/deprecated in coral), Peek (arrows
// move a peek beside the sidebar; ↵ opens), Hold (items you keep at hand, across scopes), Trail.
import { esc, mark } from "./territory.js";

export const LENSES = [["contents", "Contents"], ["versions", "Versions"], ["rests", "Rests on"], ["users", "Used by"]];

const chev = (open) => `<svg class="chev ${open ? "open" : ""}" width="10" height="10" viewBox="0 0 10 10"><path d="M3.5 2l3 3-3 3" fill="none" stroke="currentColor" stroke-width="1.4"/></svg>`;
const glyphs = (r) => {
  let g = "";
  if (r.gone) g += `<span class="g gone">${esc(r.gone)}</span>`;
  else if (r.change) g += `<span class="g ch" title="changes in the target release">${r.changeN ? r.changeN : ""}</span>`;
  if (r.uses) g += `<span class="g use">${r.uses}</span>`;
  if (r.count != null && !r.uses && !r.change) g += `<span class="g ct">${r.count}</span>`;
  return g;
};

// rows: [{ id, depth, kind: 'group'|'item'|'text', f, name, open, current, sel, uses, change, changeN, gone, count, dim, hit:[a,b], note }]
export function sidebar(s) {
  const up = s.crumbs.length > 1 ? s.crumbs[s.crumbs.length - 2] : null;
  const crumbs = up ? `<span class="up2">‹ ${esc(up)}</span>${s.scopeTitle ? `<span class="scopet">${s.scopeTitle}</span>` : ""}` : "";
  const held = s.held && s.held.length ? `<div class="held">${s.held.map((h, i) => `<span class="hc">${mark(h.f, 9)}${esc(h.n)}<kbd>⌘${i + 1}</kbd></span>`).join("")}</div>` : "";
  const lens = `<div class="lens">${LENSES.map(([k, w]) => `<span class="ln ${s.lens === k ? "on" : ""}">${w}${s.lens === k && s.counts && s.counts[k] != null ? `<i>${s.counts[k]}</i>` : ""}</span>`).join("")}</div>`;
  const narrow = s.narrow ? `<div class="narrow on"><span class="q">${esc(s.narrow)}</span><span class="caret"></span><span class="nn">${s.narrowN} of ${s.narrowOf}</span></div>`
    : s.via ? `<div class="narrow via"><span>only what <b>${esc(s.via)}</b> uses</span><span class="x">esc</span></div>`
    : `<div class="narrow"><span class="ph">type to narrow</span></div>`;
  const rows = s.rows.map((r) => {
    if (r.kind === "head") return `<div class="srow head">${esc(r.name)}${r.note ? `<i>${esc(r.note)}</i>` : ""}</div>`;
    const name = r.hit ? `${esc(r.name.slice(0, r.hit[0]))}<u>${esc(r.name.slice(r.hit[0], r.hit[1]))}</u>${esc(r.name.slice(r.hit[1]))}` : esc(r.name);
    const lead = r.kind === "group" ? chev(r.open) : `<span class="nochev"></span>`;
    const m = r.kind === "text" ? "" : r.gem ? r.gem : mark(r.f || "value", 10);
    return `<div class="srow ${r.kind} ${r.current ? "cur" : ""} ${r.sel ? "sel" : ""} ${r.dim ? "dim" : ""} ${r.gone ? "struck" : ""}" style="padding-left:${14 + r.depth * 16}px" ${r.share ? `data-share="${r.share}"` : ""}>${lead}${m}<span class="nm">${name}</span>${r.sub ? `<span class="sub">${esc(r.sub)}</span>` : ""}<span class="gl">${glyphs(r)}</span></div>`;
  }).join("");
  const widen = s.narrow ? `<div class="widen"><span>${esc(s.narrow)}</span> in the whole library <kbd>↵</kbd></div>` : "";
  const sticky = s.sticky ? `<div class="sticky">${s.sticky.map((t, i) => `<div class="srow group stick" style="padding-left:${14 + i * 16}px">${chev(true)}${t}</div>`).join("")}</div>` : "";
  const trail = s.trail ? `<div class="trail"><span class="th">Trail</span>${s.trail.map((t) => `<span class="tt">${esc(t)}</span>`).join(`<span class="ts">‹</span>`)}</div>` : "";
  return `<div class="shelf side"><div class="scope">${crumbs}</div>${s.book ? `<div class="book2">${s.book}</div>` : ""}${held}${lens}${narrow}<div class="srows">${sticky}${rows}${widen}</div>${trail}</div>`;
}
