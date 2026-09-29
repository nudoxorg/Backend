// Previews, the way Wikipedia's page previews and Gwern's popups disclose a web of links: hover any
// package name (anything carrying data-pkg) and after a beat its card opens beside it — is it legit,
// what is it, what does it ask of you. The names inside a card are links too, so hovering one opens the
// next card beside the first: the tree beneath a package unfolds one step at a time, only along the path
// you're curious about. Moving back into an earlier card closes the later ones; leaving all of them
// closes the stack. A click goes there.
import { gem } from "./paint.js";
import { comb, ago } from "./charts.js";
import { chip, verdict } from "./license.js";
import { fmt, plural } from "./world.js";

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
export const tint = (p) => (p.kind === "yours" ? "var(--mint)" : "var(--k-ns)");
export const kLines = (n) => (n >= 1e6 ? (n / 1e6).toFixed(1) + "M" : n >= 1e4 ? Math.round(n / 1e3) + "K" : n >= 1e3 ? (n / 1e3).toFixed(1) + "K" : String(n || 0));
export const link = (p, text = null, cls = "") => `<a class="pk ${cls}" data-pkg="${esc(p.id)}">${esc(text || p.name)}</a>`;

// The heads-up pills: what it does to your build and your machine, from its own source.
export function heads(T, { full = false } = {}) {
  if (!T) return "";
  const out = [];
  if (T.proc_macro) out.push(`<span class="hu amber" data-hu="proc_macro">proc-macro</span>`);
  if (T.build_rs) out.push(`<span class="hu amber" data-hu="build_rs">build.rs</span>`);
  if (T.forbid_unsafe) out.push(`<span class="hu mint" data-hu="unsafe">forbids unsafe</span>`);
  else if (T.unsafe) out.push(`<span class="hu ${T.unsafe > 50 ? "amber" : ""}" data-hu="unsafe">unsafe ${fmt(T.unsafe)}</span>`);
  const caps = T.caps || {};
  for (const [k, w] of [["process", "runs programs"], ["net", "network"], ["fs", "files"], ["env", "env vars"], ["ffi", "C calls"]]) {
    const n = caps[k + "_n"] || 0;
    if (n) out.push(`<span class="hu ${k === "process" || k === "ffi" ? "amber" : ""}" data-hu="${k}">${w}${full ? ` <b>${fmt(n)}</b>` : ""}</span>`);
  }
  return out.join("");
}

export function cardHtml(p, TRUST) {
  const T = TRUST[p.id] || null;
  const rel = T && T.releases ? T.releases : [];
  const last = T && T.last, v = verdict(p.license);
  const deps = p.depsP.slice().sort((a, b) => b.items - a.items);
  const mine = p.dependentsP.filter((d) => d.kind === "yours");
  const others = p.dependentsP.length - mine.length;
  return `<div class="pv-t">${gem(20, tint(p))}<b>${esc(p.name)}</b><span class="v">${esc(p.version)}</span>${p.latest ? `<span class="am">→ ${esc(p.latest)}</span>` : ""}</div>
    ${p.lede ? `<div class="pv-d">${esc(p.lede)}</div>` : ""}
    <div class="pv-facts">
      <div class="pv-rel">${comb(rel, { w: 132, h: 18, pin: p.version, latest: p.latest })}<span>${T ? `${plural(T.n || rel.length, "release")} · last ${ago(last)}` : ""}</span></div>
      <div class="pv-row">${chip(p.license)}<span class="pv-v ${v.tone}">${v.word}</span><span class="pv-sz">${fmt(p.items)} items${T && T.sloc ? ` · ${kLines(T.sloc)} lines` : ""}</span></div>
      ${T ? `<div class="pv-heads">${heads(T)}</div>` : ""}
    </div>
    <div class="pv-links">${deps.length ? `<div><span class="k">Rests on</span>${deps.slice(0, 6).map((d) => link(d)).join(`<i>·</i>`)}${deps.length > 6 ? `<i>+${deps.length - 6}</i>` : ""}</div>` : `<div><span class="k">Rests on</span><em>nothing</em></div>`}
      <div><span class="k">Used by</span>${mine.slice(0, 3).map((d) => link(d, d.name.replace(/^backend-/, ""), "y")).join(`<i>·</i>`)}${others ? `${mine.length ? "<i>·</i>" : ""}<em>${fmt(others)} more in your library</em>` : ""}</div></div>`;
}

export function previews({ WD, TRUST, onGo, root = document.body }) {
  const layer = document.createElement("div"); layer.className = "pv-layer"; root.appendChild(layer);
  const lines = document.createElementNS("http://www.w3.org/2000/svg", "svg"); lines.classList.add("pv-lines"); layer.appendChild(lines);
  const stack = []; // [{ el, anchor, p }]
  let openTimer = 0, closeTimer = 0, pending = null;

  function place(el, anchor, depth) {
    const a = anchor.getBoundingClientRect(), r = el.getBoundingClientRect();
    let x, y;
    if (depth === 0) { x = a.left; y = a.bottom + 8; if (y + r.height > innerHeight - 8) y = a.top - r.height - 8; }
    else { const parent = stack[depth - 1].el.getBoundingClientRect(); x = parent.right + 14; y = Math.max(8, a.top - 20); if (x + r.width > innerWidth - 8) x = parent.left - r.width - 14; }
    x = Math.max(8, Math.min(innerWidth - r.width - 8, x)); y = Math.max(8, Math.min(innerHeight - r.height - 8, y));
    el.style.left = x + "px"; el.style.top = y + "px";
  }
  function drawLines() {
    lines.setAttribute("width", innerWidth); lines.setAttribute("height", innerHeight);
    lines.innerHTML = stack.map((s, i) => {
      if (i === 0) return "";
      const a = s.anchor.getBoundingClientRect(), c = s.el.getBoundingClientRect();
      const x1 = a.right + 2, y1 = a.top + a.height / 2, x2 = c.left < a.left ? c.right : c.left, y2 = c.top + 18;
      return `<path d="M${x1} ${y1}C${(x1 + x2) / 2} ${y1} ${(x1 + x2) / 2} ${y2} ${x2} ${y2}"/>`;
    }).join("");
  }
  function open(anchor, depth) {
    const p = WD.byId.get(anchor.dataset.pkg); if (!p) return;
    truncate(depth);
    const el = document.createElement("div"); el.className = "pv"; el.dataset.depth = depth;
    el.innerHTML = cardHtml(p, TRUST); layer.appendChild(el);
    place(el, anchor, depth);
    stack.push({ el, anchor, p });
    anchor.classList.add("pv-on");
    requestAnimationFrame(() => el.classList.add("on"));
    drawLines();
  }
  function truncate(depth) {
    while (stack.length > depth) { const s = stack.pop(); s.anchor.classList.remove("pv-on"); s.el.classList.remove("on"); const e = s.el; setTimeout(() => e.remove(), 140); }
    drawLines();
  }
  const closeAll = () => truncate(0);
  document.addEventListener("mouseover", (e) => {
    const a = e.target.closest("[data-pkg]");
    const card = e.target.closest(".pv");
    clearTimeout(closeTimer);
    if (a) {
      const depth = card ? +card.dataset.depth + 1 : 0;
      if (stack[depth] && stack[depth].anchor === a) return;
      clearTimeout(openTimer); pending = a;
      openTimer = setTimeout(() => { if (pending === a) open(a, depth); }, depth ? 200 : 300);
      return;
    }
    pending = null; clearTimeout(openTimer);
    if (card) { const d = +card.dataset.depth; if (stack.length > d + 1) closeTimer = setTimeout(() => truncate(d + 1), 320); return; }
    closeTimer = setTimeout(closeAll, 300);
  });
  document.addEventListener("click", (e) => {
    const a = e.target.closest("[data-pkg]"); if (!a) return;
    const p = WD.byId.get(a.dataset.pkg); if (!p || !onGo) return;
    e.preventDefault(); closeAll(); onGo(p, a);
  });
  return { closeAll, open: (a, depth = 0) => open(a, depth), get stack() { return stack; } };
}
