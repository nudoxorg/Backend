// Weight as an iceberg: the package's own code is the tip above the waterline; everything it pulls in is
// the mass beneath, one layer per step down, each package a block as wide as the square root of its
// lines (the heavy tail stays on the page). Hover a block: it names itself in the berg's own caption and
// the path up to the surface lights. All of it happens inside the berg.
import { kLines } from "./preview.js";
import { fmt, plural } from "./world.js";

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

export function iceberg(host, p, TRUST, { w = 300, h = 132, onGo = null } = {}) {
  const sl = (q) => (TRUST[q.id] && TRUST[q.id].sloc) || 0;
  // Layers by first reach, breadth first, each package once.
  const seen = new Set([p]), layers = [], parent = new Map();
  let frontier = [p];
  while (frontier.length && layers.length < 7) {
    const next = [];
    for (const q of frontier) for (const d of q.depsP) if (!seen.has(d)) { seen.add(d); parent.set(d, q); next.push(d); }
    if (next.length) layers.push(next.sort((a, b) => sl(b) - sl(a)));
    frontier = next;
  }
  const own = sl(p), below = [...seen].filter((q) => q !== p).reduce((s, q) => s + sl(q), 0);
  const WL = 38; // waterline
  const rowH = 10, gap = 2, maxW = w - 20;
  const scale = (() => { let m = 0; for (const L of layers) m = Math.max(m, L.reduce((s, q) => s + Math.sqrt(Math.max(1, sl(q))), 0) + L.length * 1.5); return m ? maxW / m : 1; })();
  const blocks = [];
  layers.forEach((L, i) => {
    // Heaviest at the centre, the rest alternating outwards, so each layer bulges like ice does.
    const order = []; L.forEach((q, j) => (j % 2 ? order.push(q) : order.unshift(q)));
    const ws = order.map((q) => Math.max(2, Math.sqrt(Math.max(1, sl(q))) * scale));
    const tot = ws.reduce((a, b) => a + b, 0) + (order.length - 1) * 1.5;
    let x = w / 2 - tot / 2;
    order.forEach((q, j) => { blocks.push({ q, x, y: WL + 4 + i * (rowH + gap), w: ws[j], h: rowH, layer: i }); x += ws[j] + 1.5; });
  });
  const share = own / Math.max(1, own + below);
  const tipW = Math.max(14, Math.min(w * 0.6, Math.sqrt(own) * scale * 1.1)), tipH = Math.max(8, Math.min(WL - 8, 6 + share * 90));
  const tip = `M${w / 2 - tipW / 2} ${WL} L${w / 2 - tipW * 0.18} ${WL - tipH} L${w / 2 + tipW * 0.1} ${WL - tipH * 0.8} L${w / 2 + tipW / 2} ${WL} Z`;
  const H = Math.max(h, WL + 8 + layers.length * (rowH + gap) + 18);
  host.classList.add("berg");
  host.innerHTML = `<svg width="${w}" height="${H}" viewBox="0 0 ${w} ${H}">
    <rect class="sea" x="0" y="${WL}" width="${w}" height="${H - WL}"/>
    <path class="tip" d="${tip}"/>
    <line class="wl" x1="0" x2="${w}" y1="${WL}" y2="${WL}"/>
    ${blocks.map((b, i) => `<rect class="blk l${Math.min(b.layer, 4)}" data-i="${i}" x="${b.x}" y="${b.y}" width="${b.w}" height="${b.h}"/>`).join("")}
    <text class="own" x="${w / 2 + tipW / 2 + 6}" y="${WL - 6}">its own · ${kLines(own)}</text>
    </svg><div class="berg-cap"><b>${Math.round(share * 100)}%</b> above water · <b>${kLines(below)}</b> lines beneath in ${plural(seen.size - 1, "package")}</div>`;
  const cap = host.querySelector(".berg-cap");
  const base = cap.innerHTML;
  // Hover a block and it surfaces: a plate with its name and weight rises above the waterline over it,
  // and everything it carries beneath (its keel) glows while the rest of the ice goes quiet. The caption
  // says what share of the weight is its doing. Click goes there.
  const svg = host.querySelector("svg");
  const keel = (q) => { const out = new Set(); const go = (x) => { for (const d of x.depsP) if (seen.has(d) && !out.has(d) && d !== p) { out.add(d); go(d); } }; go(q); return out; };
  const surf = document.createElementNS("http://www.w3.org/2000/svg", "g"); surf.setAttribute("class", "surf"); svg.appendChild(surf);
  host.querySelectorAll(".blk").forEach((el) => {
    el.addEventListener("mouseenter", () => {
      const b = blocks[+el.dataset.i];
      const k = keel(b.q);
      svg.classList.add("sel");
      host.querySelectorAll(".blk").forEach((e2) => { const bb = blocks[+e2.dataset.i]; e2.classList.toggle("hot", bb === b); e2.classList.toggle("keel", k.has(bb.q)); });
      const carried = sl(b.q) + [...k].reduce((a, x) => a + sl(x), 0);
      const label = `${b.q.name} · ${kLines(sl(b.q))}`;
      const tw = Math.max(60, label.length * 6.6 + 16), px = Math.max(4, Math.min(w - tw - 4, b.x + b.w / 2 - tw / 2));
      surf.innerHTML = `<line class="rise" x1="${b.x + b.w / 2}" x2="${b.x + b.w / 2}" y1="${b.y}" y2="${WL - 4}"/><g class="plate-up" style="transform: translateY(${b.y - WL + 20}px)"><rect x="${px}" y="${WL - 26}" width="${tw}" height="20"/><text x="${px + 8}" y="${WL - 12}">${esc(label)}</text></g>`;
      requestAnimationFrame(() => { const g = surf.querySelector(".plate-up"); if (g) g.style.transform = "translateY(0)"; });
      const chain = []; for (let x = b.q; x && x !== p; x = parent.get(x)) chain.unshift(x.name);
      cap.innerHTML = `<b>${esc(b.q.name)}</b> carries <b>${Math.round((carried / Math.max(1, below)) * 100)}%</b> of the weight beneath (${kLines(carried)} lines${k.size ? `, ${plural(k.size, "package")} under it` : ""}) <span class="via">${chain.length > 1 ? "reached via " + chain.slice(0, -1).map(esc).join(" › ") : "it rests on it directly"}</span>${onGo ? `<span class="go">click to go there</span>` : ""}`;
    });
    el.addEventListener("mouseleave", () => { svg.classList.remove("sel"); host.querySelectorAll(".blk").forEach((e2) => e2.classList.remove("hot", "keel")); surf.innerHTML = ""; cap.innerHTML = base; });
    el.addEventListener("click", () => { const b = blocks[+el.dataset.i]; onGo && onGo(b.q); });
  });
  return { own, below, share, count: seen.size - 1 };
}
