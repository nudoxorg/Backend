// Canvas painting for territories at every distance: a package as a block of shingles in the library,
// and as its full territory (regions + labelled modules) on its page. One painter, so a block can grow
// into its page shingle by shingle. Mirrors facet::data::territory's grammar: family tints at rest,
// mint = your code reaches it, periwinkle = lit, amber = changes, dashed outline = unread.

export const C = {};
export function readTokens() {
  const cs = getComputedStyle(document.documentElement);
  for (const k of ["g0", "g1", "g2", "g3", "plate", "plate2", "plate3", "line1", "line2", "line3", "ink0", "ink1", "ink2", "ink3", "ink4",
    "k-ty", "k-ca", "k-co", "k-va", "k-ns", "mint", "peri", "peri-hi", "coral", "amber"]) C[k] = cs.getPropertyValue("--" + k).trim();
  C.fam = { type: C["k-ty"], callable: C["k-ca"], contract: C["k-co"], value: C.ink2 };
}

export function canvas(el, w, h) {
  const dpr = window.devicePixelRatio || 1;
  el.width = Math.round(w * dpr); el.height = Math.round(h * dpr);
  el.style.width = w + "px"; el.style.height = h + "px";
  const ctx = el.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return ctx;
}

// A package's block: its shingles row by row in module order (so a module is one run), `rows` high,
// capped at `cap` shingles. Local coordinates, top-left = 0,0. `more` = what the cap left out.
export function block(pkg, { pitch = 6, stone = 5, rows = 5, cap = 240 } = {}) {
  const cells = [];
  const list = [];
  for (const m of pkg.modules) for (const it of m.items) { if (list.length >= cap) break; list.push({ it, m }); }
  const n = list.length;
  const R = Math.max(1, Math.min(rows, n));
  const cols = Math.max(1, Math.ceil(n / R));
  // Column-major keeps a module's run as a vertical stripe that reads left to right.
  list.forEach((c, k) => cells.push({ it: c.it, m: c.m, x: Math.floor(k / R) * pitch, y: (k % R) * pitch }));
  const w = cols * pitch - (pitch - stone), h = R * pitch - (pitch - stone);
  return { w: Math.max(w, stone), h: Math.max(h, stone), cells, pitch, stone, more: Math.max(0, (pkg.items || 0) - n) };
}

const chamfer = (ctx, x, y, s) => {
  const c = Math.max(1.2, s * 0.3);
  ctx.moveTo(x + c, y); ctx.lineTo(x + s, y); ctx.lineTo(x + s, y + s - c); ctx.lineTo(x + s - c, y + s); ctx.lineTo(x, y + s); ctx.lineTo(x, y + c); ctx.closePath();
};
export function shingle(ctx, x, y, s) {
  if (s >= 5) chamfer(ctx, x, y, s); else ctx.rect(x, y, s, s);
}

// Paints cells grouped by colour in as few fills as possible. `tint(cell)` → [colour, alpha] or null.
export function cells(ctx, list, ox, oy, s, tint) {
  const groups = new Map();
  for (const c of list) {
    const t = tint(c); if (!t) continue;
    const key = t[0] + "|" + t[1];
    (groups.get(key) || groups.set(key, []).get(key)).push(c);
  }
  for (const [key, cs] of groups) {
    const [col, a] = key.split("|");
    ctx.globalAlpha = +a; ctx.fillStyle = col; ctx.beginPath();
    for (const c of cs) shingle(ctx, ox + (c.px ?? c.x), oy + (c.py ?? c.y), c.ps ?? s);
    ctx.fill();
  }
  ctx.globalAlpha = 1;
}

// A chamfered frame (the house plate shape: top-left and bottom-right corners cut).
export function frame(ctx, x, y, w, h, c = 4) {
  ctx.beginPath();
  ctx.moveTo(x + c, y); ctx.lineTo(x + w, y); ctx.lineTo(x + w, y + h - c); ctx.lineTo(x + w - c, y + h); ctx.lineTo(x, y + h); ctx.lineTo(x, y + c); ctx.closePath();
}

export function text(ctx, s, x, y, { font = "500 11px Geist Mono", color = C.ink3, align = "left", alpha = 1, base = "alphabetic" } = {}) {
  ctx.globalAlpha = alpha; ctx.font = font; ctx.fillStyle = color; ctx.textAlign = align; ctx.textBaseline = base;
  ctx.fillText(s, x, y); ctx.globalAlpha = 1;
}
export function measure(ctx, s, font) { ctx.font = font; return ctx.measureText(s).width; }

// The package gem (the hero mark), as an SVG string: twelve facets, tinted by ecosystem.
export function gem(size = 56, tint = "var(--k-ns)", dashed = false) {
  const F = [[24,2,35,13,24,10,.4],[24,10,35,13,38,24,.3],[35,13,46,24,38,24,.34],[46,24,35,35,38,24,.14],[38,24,35,35,24,38,.08],[35,35,24,46,24,38,.11],[24,46,13,35,24,38,.22],[24,38,13,35,10,24,.2],[13,35,2,24,10,24,.3],[2,24,13,13,10,24,.56],[10,24,13,13,24,10,.5],[13,13,24,2,24,10,.66]];
  const facets = dashed ? "" : F.map(([a, b, c, d, e, f, t]) => `<polygon points="${a},${b} ${c},${d} ${e},${f}" fill="currentColor" fill-opacity="${t}"/>`).join("");
  return `<svg class="gem" width="${size}" height="${size}" viewBox="0 0 48 48" style="color:${tint}">${facets}<path d="M24 2 46 24 24 46 2 24z" fill="none" stroke="currentColor" ${dashed ? 'stroke-dasharray="3 3"' : ""}/><path d="M24 10 38 24 24 38 10 24z" fill="none" stroke="currentColor" stroke-width=".7" stroke-opacity=".55"/></svg>`;
}
