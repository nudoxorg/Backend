// The release ticker: the one place a package's versions live. Every release is a bar on a time line
// (tall = breaking, mid = feature, short = fix; coral = yanked; mint = your pin; amber = newest). Move
// along it and the bars under the pointer widen apart (a fisheye), the nearest one lifts, and its label
// rides inside the ticker. Click or drag to travel: the page is read at that release.
import { kindOf, NOW, ago } from "./charts.js";
import { clamp, lerp, every } from "./motion.js";

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const H = 46, R = 110, D = 3.2;

export function ticker(host, rel, { pin, latest, at = null, local = [], onTravel, onPeek }) {
  const W = host.clientWidth;
  host.classList.add("tick");
  host.innerHTML = `<svg class="tk" width="${W}" height="${H + 18}"></svg><div class="tk-label"></div>`;
  const svg = host.querySelector("svg"), label = host.querySelector(".tk-label");
  const dated = rel.filter((r) => r.t);
  if (!dated.length) { label.textContent = "no dated releases"; return { set() {} }; }
  const t0 = +new Date(dated[0].t), t1 = +NOW;
  const X = (t) => 8 + ((+new Date(t) - t0) / Math.max(1, t1 - t0)) * (W - 16);
  let prev = null;
  const bars = [];
  for (const r of rel) { const k = kindOf(prev, r.v); if (!r.y) prev = r.v; if (r.t) bars.push({ r, k, x: X(r.t), local: local.includes(r.v) }); }
  const years = [];
  for (let y = new Date(dated[0].t).getFullYear() + 1; y <= NOW.getFullYear(); y++) years.push({ y, x: X(`${y}-01-01`) });
  let px = null, hot = null, reading = at, dragging = false;
  const fish = (x) => {
    if (px == null) return x;
    const d = x - px, r = Math.abs(d);
    if (r >= R) return x;
    const u = r / R; return px + Math.sign(d) * R * (((D + 1) * u) / (D * u + 1));
  };
  function draw() {
    let s = `<line class="base" x1="0" x2="${W}" y1="${H}" y2="${H}"/>`;
    for (const y of years) s += `<line class="yr" x1="${fish(y.x)}" x2="${fish(y.x)}" y1="${H}" y2="${H + 4}"/><text class="yr" x="${fish(y.x)}" y="${H + 15}">${y.y}</text>`;
    const cur = reading || pin;
    for (const b of bars) {
      const x = fish(b.x), near = px == null ? 0 : clamp(1 - Math.abs(b.x - px) / R);
      const w = 1.4 + 4.6 * near * near;
      const h = (b.k === "major" ? 34 : b.k === "minor" ? 20 : 11) * (b === hot ? 1.18 : 1);
      const cls = [b.k, b.r.y ? "yank" : "", b.r.v === pin ? "pin" : "", b.r.v === latest ? "new" : "", b.r.v === cur && cur !== pin ? "at" : "", b === hot ? "hot" : "", b.local ? "read" : ""].join(" ");
      s += `<rect class="${cls}" x="${x - w / 2}" y="${H - h}" width="${w}" height="${h}"/>`;
    }
    // Your pin and the newest wear small flags that stay put; the release being read wears a caret.
    const flag = (v, cls, word) => { const b = bars.find((x) => x.r.v === v); if (!b) return ""; const x = fish(b.x); return `<g class="flag ${cls}"><line x1="${x}" x2="${x}" y1="${H - 40}" y2="${H - 36}"/><text x="${x}" y="${H - 41}" text-anchor="${x > W - 80 ? "end" : x < 80 ? "start" : "middle"}">${esc(word)}</text></g>`; };
    s += flag(pin, "pin", `your pin ${pin}`);
    if (latest && latest !== pin) s += flag(latest, "new", `newest ${latest}`);
    if (reading && reading !== pin) { const b = bars.find((x) => x.r.v === reading); if (b) { const x = fish(b.x); s += `<path class="caret" d="M${x - 5} ${H + 1}L${x} ${H - 5}L${x + 5} ${H + 1}Z"/>`; } }
    svg.innerHTML = s;
    if (hot) {
      const x = fish(hot.x);
      const words = [`<b>${esc(hot.r.v)}</b>`, esc(hot.r.t || ""), hot.r.y ? `<span class="co">yanked</span>` : hot.k === "major" ? "breaking" : hot.k === "minor" ? "features" : "fixes", hot.r.v === pin ? `<span class="y">your pin</span>` : hot.r.v === latest ? `<span class="am">newest</span>` : ago(hot.r.t)];
      label.innerHTML = words.join(`<i>·</i>`) + (hot.local ? `<span class="rd">read</span>` : `<span class="nr">not read yet</span>`) + (onPeek ? `<span class="pk2"></span>` : "");
      label.style.left = clamp(x - label.offsetWidth / 2, 0, W - label.offsetWidth) + "px";
      label.classList.add("on");
      if (onPeek) onPeek(hot.r.v, label.querySelector(".pk2"));
    } else label.classList.remove("on");
  }
  let raf = 0;
  const kick = () => { if (!raf) raf = requestAnimationFrame(() => { raf = 0; draw(); }); };
  const nearest = (x) => { let best = null, bd = 1e9; for (const b of bars) { const d = Math.abs(fish(b.x) - x); if (d < bd) { bd = d; best = b; } } return bd < 24 ? best : null; };
  host.addEventListener("mousemove", (e) => {
    const r = svg.getBoundingClientRect(); const x = e.clientX - r.left;
    px = x; hot = nearest(x); kick();
    if (dragging && hot && hot.r.v !== reading) { reading = hot.r.v; onTravel && onTravel(reading, true); }
  });
  host.addEventListener("mouseleave", () => { px = null; hot = null; dragging = false; kick(); });
  host.addEventListener("mousedown", () => { dragging = true; if (hot) { reading = hot.r.v; onTravel && onTravel(reading, true); } });
  window.addEventListener("mouseup", () => { if (dragging) { dragging = false; if (reading) onTravel && onTravel(reading, false); } });
  draw();
  return {
    set(v) { reading = v; kick(); },
    hover(v) { const b = bars.find((x) => x.r.v === v); if (b) { px = b.x; hot = b; } else { px = null; hot = null; } draw(); },
  };
}
