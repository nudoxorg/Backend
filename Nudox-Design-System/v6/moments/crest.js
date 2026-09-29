// The crest: the hero's own instruments, compact, each opening inside itself.
//   licence     a stamp; hover unfolds what it permits and asks of you
//   heads-up    icons stacked like a hand of chips; hover fans them out with their words; click lays
//               every finding out on one sheet, with the lines in its source that show it
//   weight      an iceberg glyph with the lines it pulls in cut into it; click opens the full berg
//   advisories  what the advisory feeds say about this release (honest when no feed is configured)
import { icon } from "./badges.js";
import { verdict, parse } from "./license.js";
import { kLines } from "./preview.js";
import { below, fmt, plural } from "./world.js";

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

// Everything the heads-up can say, most consequential first.
export function findings(T) {
  if (!T) return [];
  const caps = T.caps || {}, out = [];
  if (T.build_rs) out.push({ k: "build", tone: "amber", word: "Runs code when you build", why: "It has a build script (build.rs): code that runs on your machine at compile time.", n: null, ex: [] });
  if (T.proc_macro) out.push({ k: "macroPkg", tone: "amber", word: "Runs inside your compiler", why: "It's a procedural macro: its code runs in rustc while your code compiles.", n: null, ex: [] });
  if (caps.process_n) out.push({ k: "process", tone: "amber", word: "Starts programs", why: "It spawns other programs.", n: caps.process_n, ex: caps.process });
  if (caps.ffi_n) out.push({ k: "ffi", tone: "amber", word: "Calls C", why: "It crosses into C code, where Rust's guarantees stop.", n: caps.ffi_n, ex: caps.ffi });
  if (caps.net_n) out.push({ k: "net", tone: "", word: "Opens network connections", why: "It names sockets or HTTP clients.", n: caps.net_n, ex: caps.net });
  if (caps.fs_n) out.push({ k: "files", tone: "", word: "Touches files", why: "It reads or writes the file system.", n: caps.fs_n, ex: caps.fs });
  if (caps.env_n) out.push({ k: "env", tone: "", word: "Reads environment variables", why: "Its behaviour can change with your environment.", n: caps.env_n, ex: caps.env });
  if (T.forbid_unsafe) out.push({ k: "shield", tone: "mint", word: "Forbids unsafe code", why: "#![forbid(unsafe_code)]: the compiler guarantees it has none.", n: null, ex: [] });
  else if (T.unsafe) out.push({ k: "unsafe", tone: T.unsafe > 50 ? "amber" : "", word: "Unsafe blocks", why: "Places where it promises what the compiler can't check.", n: T.unsafe, ex: [] });
  return out;
}

// The iceberg glyph: its own lines above the waterline, what it pulls in cut into the mass below.
function bergGlyph(own, deep) {
  const share = own / Math.max(1, own + deep);
  const tipH = 5 + Math.round(share * 22);
  return `<svg class="bg" viewBox="0 0 76 64" width="76" height="64">
    <path class="bg-tip" d="M26 24 L36 ${24 - tipH} L41 ${24 - tipH * 0.75} L50 24Z"/>
    <line class="bg-wl" x1="2" x2="74" y1="24" y2="24"/>
    <path class="bg-mass" d="M14 24 L62 24 L70 36 L60 52 L44 61 L28 60 L12 50 L6 36Z"/>
    <text class="bg-n" x="38" y="44">${kLines(deep)}</text>
    <text class="bg-own" x="${52 + 2}" y="${Math.max(9, 24 - tipH + 4)}">${kLines(own)}</text></svg>`;
}

export function crest(p, T, TRUST) {
  const v = verdict(p.license), L = parse(p.license);
  const f = findings(T);
  const all = below(p);
  const sl = (q) => (TRUST[q.id] && TRUST[q.id].sloc) || 0;
  const own = sl(p), deep = [...all].reduce((s, q) => s + sl(q), 0);
  const opts = L.none ? "no licence" : L.groups.map((g) => g.map((t) => `<span class="${v.pick.includes(t) ? "on" : ""}">${esc(t.id)}</span>`).join(`<i>or</i>`)).join(`<i class="and">and</i>`);
  const stack = f.map((x, i) => `<span class="chip-i ${x.tone}" style="--i:${i}" data-k="${x.k}">${icon(x.k)}<span class="ci-w"><b>${esc(x.word)}</b>${x.n != null ? `<em>${fmt(x.n)}</em>` : ""}</span></span>`).join("");
  return `<div class="crest">
    <div class="cr lic ${v.tone}" tabindex="0"><span class="cr-k">Licence</span><div class="cr-face"><span class="seal">${icon(v.tone === "mint" ? "shield" : "unsafe")}</span><span class="lw">${esc(v.word)}</span></div><div class="cr-x">${opts}</div>
      <div class="cr-more">${esc(v.line)}</div></div>
    <div class="cr heads" tabindex="0"><span class="cr-k">Heads-up <b>${f.filter((x) => x.tone === "amber").length || ""}</b></span><div class="stack" style="--n:${f.length}">${stack || `<span class="chip-i mint" style="--i:0">${icon("shield")}<span class="ci-w"><b>Nothing to flag</b></span></span>`}</div></div>
    <div class="cr weight" tabindex="0" title="open the iceberg"><span class="cr-k">Weight</span>${bergGlyph(own, deep)}<div class="cr-x">${plural(all.size, "package")} beneath</div></div>
    <div class="cr adv" tabindex="0"><span class="cr-k">Advisories</span><div class="cr-face"><span class="seal quiet">${icon("shield")}</span><span class="lw quiet">no feed</span></div><div class="cr-x">RustSec, OSV and GHSA can be read; none configured</div></div>
  </div>`;
}

// The sheet every heads-up finding opens onto, with the lines that show it.
export function sheet(p, T) {
  const f = findings(T);
  return `<div class="sheet"><div class="sh-h"><b>${esc(p.name)}</b> ${esc(p.version)}, read from its own source<span class="x">${icon("error")}</span></div>
    ${f.map((x) => `<div class="sh-r ${x.tone}"><span class="sh-i">${icon(x.k)}</span><div><div class="sh-w">${esc(x.word)}${x.n != null ? ` <em>${fmt(x.n)} ${x.n === 1 ? "place" : "places"}</em>` : ""}</div><div class="sh-why">${esc(x.why)}</div>
      ${(x.ex || []).map(([file, line, text]) => `<div class="sh-l"><span>${esc(file)}:${line}</span><code>${esc(text)}</code></div>`).join("")}</div></div>`).join("") || `<div class="sh-r mint">Nothing it does needs a heads-up.</div>`}</div>`;
}
