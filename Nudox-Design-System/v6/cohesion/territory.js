// The territory (the owner's "shingles"): one region per module, one shingle per public item.
// Mirrors facet::data::territory: squarified regions, 10 px chamfered shingles, quiet mono labels
// drawn only where they fit. Shared by every view on the cohesion board.

export const STONE = 10, PITCH = 14, PAD = 8, LABEL = 20, GAP = 3;
export const FAM = { type: "fty", callable: "fca", contract: "fco", value: "fva" };

export function squarify(values, w, h) {
  const out = values.map(() => ({ x: 0, y: 0, w: 0, h: 0 }));
  const total = values.reduce((s, v) => s + Math.max(0, v), 0);
  if (total <= 0) return out;
  const scale = (w * h) / total;
  const order = values.map((_, i) => i).sort((a, b) => values[b] - values[a]);
  const area = (i) => Math.max(0, values[i]) * scale;
  const worst = (row, side) => { const s = row.reduce((t, i) => t + area(i), 0); return Math.max(...row.map((i) => { const a = Math.max(area(i), 1e-6); return Math.max((side * side * a) / (s * s), (s * s) / (side * side * a)); })); };
  let x = 0, y = 0, W = w, H = h, row = [];
  const lay = () => {
    const s = row.reduce((t, i) => t + area(i), 0);
    if (W >= H) { const cw = s / H; let cy = y; for (const i of row) { const ch = area(i) / cw; out[i] = { x, y: cy, w: cw, h: ch }; cy += ch; } x += cw; W -= cw; }
    else { const ch = s / W; let cx = x; for (const i of row) { const cw = area(i) / ch; out[i] = { x: cx, y, w: cw, h: ch }; cx += cw; } y += ch; H -= ch; }
    row = [];
  };
  for (const i of order) { const side = Math.min(W, H); if (!row.length || worst([...row, i], side) <= worst(row, side)) row.push(i); else { lay(); row.push(i); } }
  if (row.length) lay();
  return out;
}

// Lays a territory out at width `w` as justified rows of regions, in the shelf's order.
// Every region holds its label (never dropped, never clipped) and its shingles on one 14 px grid;
// a row's regions share its height, and the row is justified to the full width by widening its
// regions in proportion to what they hold. The height follows from the rows.
export function layout(modules, w, opts = {}) {
  const pitch = opts.pitch || PITCH, stone = opts.stone || STONE, pad = opts.pad ?? PAD, label = opts.label ?? LABEL;
  const R = opts.rows || 3; // shingle rows per layout row, before justification
  const charW = opts.charW || 7.1, gap = GAP;
  const want = modules.map((m) => {
    const cols = Math.max(1, Math.ceil(m.items.length / R));
    return Math.max(label ? m.path.length * charW : 0, cols * pitch - (pitch - stone)) + 2 * pad;
  });
  const rows = []; let cur = [], used = 0;
  modules.forEach((m, i) => {
    if (cur.length && used + want[i] + gap > w) { rows.push(cur); cur = []; used = 0; }
    cur.push(i); used += want[i] + (cur.length > 1 ? gap : 0);
  });
  if (cur.length) rows.push(cur);
  const regions = []; let y = 0;
  rows.forEach((row, ri) => {
    const base = row.reduce((s2, i) => s2 + want[i], 0) + gap * (row.length - 1);
    const last = ri === rows.length - 1 && base < w * 0.7;
    const extra = last ? 0 : w - base;
    const weight = row.reduce((s2, i) => s2 + modules[i].items.length, 0) || 1;
    let x = 0; const placed = [];
    for (const i of row) {
      const rw = want[i] + (extra * modules[i].items.length) / weight;
      const cols = Math.max(1, Math.floor((rw - 2 * pad + (pitch - stone)) / pitch));
      const n = modules[i].items.length;
      placed.push({ i, x, rw, cols, rowsN: Math.ceil(n / cols) });
      x += rw + gap;
    }
    const rh = label + 2 * pad + Math.max(...placed.map((p) => p.rowsN)) * pitch - (pitch - stone);
    for (const p of placed) {
      const m = modules[p.i];
      const rect = { x: p.x, y, w: p.rw, h: rh };
      const top = y + pad + label;
      regions[p.i] = { module: m, rect, labelled: label > 0, fits: true, shingles: m.items.map((it, k) => ({ item: it, x: p.x + pad + (k % p.cols) * pitch, y: top + Math.floor(k / p.cols) * pitch })) };
    }
    y += rh + gap;
  });
  return { w, h: Math.ceil(y - gap), regions };
}

const cut = (x, y, s, c) => `M${x + c} ${y}H${x + s}V${y + s - c}L${x + s - c} ${y + s}H${x}V${y + c}Z`;
export const shinglePath = (x, y, s = STONE) => cut(x, y, s, Math.max(1.5, s * 0.3));

// Renders a laid-out territory as one SVG. `state` = { lit:Set(item), yours:Set, current:item,
// hot:item, hotRegion:module, dim:bool, changed:Map(item->'new'|'gone'|'changed') }.
export function render(t, state = {}, opts = {}) {
  const stone = opts.stone || STONE;
  const labels = opts.labels !== false && t.regions.some((r) => r.labelled);
  const dim = state.dim || (state.lit && state.lit.size > 0);
  const W = opts.width || t.w, H = (t.h * W) / t.w;
  let s = `<svg class="terr ${opts.cls || ""}" width="${W}" height="${H}" viewBox="0 0 ${t.w} ${t.h}" style="overflow:visible">`;
  for (const r of t.regions) {
    const hot = state.hotRegion === r.module;
    const rk = state.shareRegion && state.shareRegion.get(r.module.path);
    s += `<rect class="reg ${hot ? "hot" : ""}" ${rk ? `data-share="${rk}"` : ""} data-m="${esc(r.module.path)}" x="${r.rect.x}" y="${r.rect.y}" width="${r.rect.w}" height="${r.rect.h}"/>`;
    if (labels && r.labelled) s += `<text class="rl ${hot ? "hot" : ""}" x="${r.rect.x + PAD}" y="${r.rect.y + PAD + 11}">${esc(r.module.path)}</text>`;
    for (const sh of r.shingles) {
      const it = sh.item;
      const cls = ["sh", FAM[it.f] || "fva"];
      if (state.lit && state.lit.has(it)) cls.push("lit");
      if (state.yours && state.yours.has(it)) cls.push("y");
      if (state.current === it) cls.push("cur");
      if (state.hot === it) cls.push("hot");
      const ch = state.changed && state.changed.get(it); if (ch) cls.push(ch);
      if (dim && !cls.includes("lit") && !cls.includes("hot") && !cls.includes("cur") && !ch) cls.push("dim");
      const sk = state.share && state.share.get(it);
      s += `<path class="${cls.join(" ")}" data-i="${it.gid}" ${sk ? `data-share="${sk}"` : ""} d="${shinglePath(sh.x, sh.y, stone)}"/>`;
    }
  }
  return s + `</svg>`;
}

export function center(t, item) {
  for (const r of t.regions) for (const sh of r.shingles) if (sh.item === item) return { x: sh.x + STONE / 2, y: sh.y + STONE / 2, region: r };
  return null;
}

export const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

// A kind mark at size s (the graph's node marks and the pages' row marks: one shape per family).
export function mark(f, s = 11, cls = "") {
  const h = s / 2;
  const shape = {
    type: `<path d="M${h} 0.5L${s - 0.5} ${h}L${h} ${s - 0.5}L0.5 ${h}Z" class="fill"/>`,
    contract: `<path d="M${h} 1L${s - 1} ${h}L${h} ${s - 1}L1 ${h}Z" class="line"/>`,
    callable: `<path d="${cut(1, 1, s - 2, 2.5)}" class="fill"/>`,
    value: `<rect x="${s * 0.25}" y="${s * 0.25}" width="${s * 0.5}" height="${s * 0.5}" class="fill"/>`,
  }[f] || "";
  return `<svg class="km ${FAM[f] || "fva"} ${cls}" width="${s}" height="${s}" viewBox="0 0 ${s} ${s}">${shape}</svg>`;
}
