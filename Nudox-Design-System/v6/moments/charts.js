// Small graphs that carry trust at a glance. The release comb: every release as a tick on a time line
// (tall = major, mid = minor, short = patch), yanked in coral, your pin in mint, the newest in amber, so
// age, cadence and churn read before any number does.
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
export const NOW = new Date("2026-09-28");
const day = 864e5;
export const ago = (d) => {
  if (!d) return "undated";
  const n = Math.round((NOW - new Date(d)) / day);
  if (n < 1) return "today";
  if (n < 45) return `${n} d ago`;
  if (n < 540) return `${Math.round(n / 30)} mo ago`;
  return `${(n / 365).toFixed(n < 3650 ? 1 : 0)} y ago`;
};
const semver = (v) => v.split(/[.+-]/).slice(0, 3).map((x) => parseInt(x, 10) || 0);
export function kindOf(prev, v) {
  if (!prev) return "major";
  const a = semver(prev), b = semver(v);
  if (b[0] !== a[0] || (b[0] === 0 && b[1] !== a[1])) return "major"; // 0.x minor is breaking
  if (b[1] !== a[1] || (b[0] === 0 && b[2] !== a[2] && false)) return "minor";
  return "patch";
}

// rel: [{v, t, y}] ascending. opts: { w, h, pin, latest, from (date), labels }
export function comb(rel, { w = 220, h = 26, pin = null, latest = null, labels = false, hot = null, id = "" } = {}) {
  const dated = rel.filter((r) => r.t);
  if (!dated.length) return `<svg class="comb" width="${w}" height="${h}"></svg>`;
  const t0 = +new Date(dated[0].t), t1 = +NOW;
  const x = (t) => 2 + ((+new Date(t) - t0) / Math.max(1, t1 - t0)) * (w - 4);
  let prev = null, s = `<svg class="comb" width="${w}" height="${h + (labels ? 14 : 0)}" viewBox="0 0 ${w} ${h + (labels ? 14 : 0)}">`;
  s += `<line x1="0" x2="${w}" y1="${h - 0.5}" y2="${h - 0.5}" class="base"/>`;
  if (labels) {
    const y0 = new Date(dated[0].t).getFullYear(), y1 = NOW.getFullYear();
    const step = y1 - y0 > 8 ? 2 : 1;
    for (let y = y0 + 1; y <= y1; y += step) { const xx = x(`${y}-01-01`); s += `<line x1="${xx}" x2="${xx}" y1="${h}" y2="${h + 3}" class="base"/><text x="${xx}" y="${h + 13}">${y}</text>`; }
  }
  for (const r of rel) {
    const k = kindOf(prev, r.v); prev = r.y ? prev : r.v;
    if (!r.t) continue;
    const xx = x(r.t), th = k === "major" ? h - 2 : k === "minor" ? h * 0.55 : h * 0.3;
    const cls = r.v === pin ? "pin" : r.v === latest ? "new" : r.y ? "yank" : k;
    s += `<line data-v="${esc(r.v)}" x1="${xx}" x2="${xx}" y1="${h - 1}" y2="${h - 1 - th}" class="${cls} ${hot === r.v ? "hot" : ""}"/>`;
  }
  // the pin and the newest drawn last, on top
  for (const r of rel) if (r.t && (r.v === pin || r.v === latest)) { const xx = x(r.t); s += `<circle cx="${xx}" cy="${3}" r="2.4" class="${r.v === pin ? "pin" : "new"}"/>`; }
  return s + `</svg>`;
}

// A bar split into what it brings: direct deps, and everything beneath them, scaled by lines of code.
export function weight(parts, w = 200) {
  const total = parts.reduce((s, p) => s + p.n, 0) || 1;
  let x = 0;
  return `<svg class="wbar" width="${w}" height="6">${parts.map((p) => { const ww = Math.max(1, (p.n / total) * w); const r = `<rect x="${x}" y="0" width="${ww - 1}" height="6" class="${p.cls}"/>`; x += ww; return r; }).join("")}</svg>`;
}

// Staleness across the library: one dot per direct dependency, x = how old your pinned release is
// (a square-root scale, so the last year gets room), size = how much your code uses it, amber when a
// newer release exists. The heavily used, far-behind ones sit big and amber at the right.
export function staleness(direct, TRUST, { w = 1000, h = 44 } = {}) {
  const pts = [];
  for (const p of direct) {
    const T = TRUST[p.id]; if (!T || !T.releases) continue;
    const r = T.releases.find((x) => x.v === p.version); if (!r || !r.t) continue;
    const age = Math.max(0, (NOW - new Date(r.t)) / 864e5 / 365);
    pts.push({ p, age, behind: p.newer ? p.newer.length : 0 });
  }
  const maxAge = Math.max(4, ...pts.map((d) => d.age));
  const X = (a) => 10 + Math.sqrt(a / maxAge) * (w - 20);
  let s = `<svg class="stale" width="${w}" height="${h + 16}" viewBox="0 0 ${w} ${h + 16}"><line x1="0" x2="${w}" y1="${h / 2}" y2="${h / 2}" class="ax"/>`;
  for (const yr of [0.25, 1, 2, 4, 8].filter((y) => y <= maxAge)) s += `<line x1="${X(yr)}" x2="${X(yr)}" y1="${h / 2 - 3}" y2="${h / 2 + 3}" class="ax"/><text x="${X(yr)}" y="${h + 12}">${yr < 1 ? "3 mo" : yr + " y"}</text>`;
  pts.sort((a, b) => b.p.sites - a.p.sites);
  const placed = [];
  for (const d of pts) {
    const r = 2.5 + Math.min(9, Math.sqrt(d.p.sites || 0) * 0.35);
    let y = h / 2, k = 0;
    while (placed.some((o) => Math.hypot(o.x - X(d.age), o.y - y) < o.r + r + 1) && k < 12) { k++; y = h / 2 + (k % 2 ? -1 : 1) * Math.ceil(k / 2) * 4; }
    placed.push({ x: X(d.age), y, r });
    s += `<circle cx="${X(d.age)}" cy="${y}" r="${r}" class="${d.behind ? "old" : "cur"}" data-pkg="${d.p.id}"><title>${d.p.name} ${d.p.version} · pinned ${d.age.toFixed(1)} y ago${d.behind ? ` · ${d.behind} newer` : ""}</title></circle>`;
  }
  return s + `</svg>`;
}
