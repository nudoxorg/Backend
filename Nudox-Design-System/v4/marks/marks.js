// FACET v4 identity marks. Each fact about a package is its own component:
// rest = a glyph plus at most one word; depth waits on hover, press and ⌥.
//
// Stable API (D-Browse imports this; do not rename):
//   ecosystemMark(eco, opts)             -> html  eco: 'crates'|'npm'|'pypi'|'go'|'maven'|'nuget'|'cpp'
//   licenseMark(spdx, yourLicense, opts) -> html  spdx: SPDX expression, or null (no license field)
//   versionComb(history, pin, opts)      -> html  history: [{v, at?, yanked?}], pin: version | null
//   depLink(dep, opts)                   -> html  dep: one row of data.json packages[*].deps
// Helpers (also stable):
//   depList(deps, opts) -> html   normal deps as links; dev/build deps wait under ⌥
//   packageGem(px, opts) -> html  the hero gem
//   install(root)                 wires hover, press-to-copy, scrub and ⌥ (runs once on import)
//   openMark(id), readSpdx(spdx), semverReading(history, pin)
//
// Importing this module adds marks.css next to it. All hover behaviour is delegated from
// document, so the returned strings work wherever they are inserted.

const HERE = new URL(".", import.meta.url);
const SPRING = "cubic-bezier(.34,1.56,.64,1)";

// --------------------------------------------------------------------------- small utilities
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
let seq = 0;
const uid = (p) => `${p}-${++seq}`;
const WORDS = ["no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];
const word = (n) => (n >= 0 && n <= 10 ? WORDS[n] : String(n));
const plural = (n, one, many = one + "s") => `${n.toLocaleString("en")} ${n === 1 ? one : many}`;
const cap = (s) => (s ? s[0].toUpperCase() + s.slice(1) : s);
const list = (xs, conj = "and") => (xs.length <= 1 ? xs.join("") : xs.slice(0, -1).join(", ") + ` ${conj} ` + xs.at(-1));
const now = () => (globalThis.MARKS_NOW ? Date.parse(globalThis.MARKS_NOW) : Date.now());

function ago(iso, t = now()) {
  if (!iso) return null;
  const d = (t - Date.parse(iso)) / 864e5;
  if (d < 1) return "today";
  if (d < 45) return plural(Math.round(d), "day");
  if (d < 365 * 2) return plural(Math.round(d / 30.44), "month");
  return plural(Math.round(d / 365.25), "year");
}

// --------------------------------------------------------------------------- 1. ecosystem marks
// Each registry is a different cut of stone; C/C++ has no registry, so its stone is uncut.
const ECO = {
  crates: { word: "crates.io", kind: "Rust crate", registry: "crates.io", install: (p) => `cargo add ${p}` },
  npm: { word: "npm", kind: "npm package", registry: "npmjs.com", install: (p) => `npm i ${p}` },
  pypi: { word: "PyPI", kind: "Python package", registry: "PyPI", install: (p) => `pip install ${p}` },
  go: { word: "Go", kind: "Go module", registry: "proxy.golang.org", install: (p, v) => `go get ${p}${v ? "@" + v : ""}` },
  maven: { word: "Maven", kind: "Java library", registry: "Maven Central", install: (p, v) => `implementation("${p}${v ? ":" + v : ""}")` },
  nuget: { word: "NuGet", kind: ".NET package", registry: "nuget.org", install: (p, v) => `dotnet add package ${p}${v ? " --version " + v : ""}` },
  cpp: { word: "source", kind: "C/C++ library", registry: "no registry", install: (p) => `git clone ${p}` },
};
const ECO_ALIAS = { cargo: "crates", rust: "crates", "crates.io": "crates", typescript: "npm", javascript: "npm", python: "pypi",
  golang: "go", java: "maven", csharp: "nuget", "c#": "nuget", dotnet: "nuget", c: "cpp", "c++": "cpp", clang: "cpp" };

const scale = (pts, k, c = [12, 12]) => pts.map(([x, y]) => [c[0] + (x - c[0]) * k, c[1] + (y - c[1]) * k]);
const hexagon = [0, 1, 2, 3, 4, 5].map((i) => { const a = (-90 + 60 * i) * Math.PI / 180; return [12 + 10 * Math.cos(a), 12.2 + 10 * Math.sin(a)]; });
const CUTS = {
  // outer polygon (clockwise, screen coords), table scale, rotation step and facet shift for the press "tick"
  crates: { stones: [{ o: [[6.5, 4], [17.5, 4], [21.5, 8], [21.5, 16], [17.5, 20], [6.5, 20], [2.5, 16], [2.5, 8]], k: 0.5 }], step: 180, shift: 4 },
  npm: { stones: [{ o: [[3.5, 3.5], [20.5, 3.5], [20.5, 20.5], [3.5, 20.5]], k: 0.46 }], step: 90, shift: 1 },
  pypi: { stones: [{ o: [[14.5, 7.5], [21.5, 14.5], [14.5, 21.5], [7.5, 14.5]], k: 0.42, c: [14.5, 14.5], back: true },
                   { o: [[9.5, 2.5], [16.5, 9.5], [9.5, 16.5], [2.5, 9.5]], k: 0.42, c: [9.5, 9.5] }], step: 90, shift: 1, own: true },
  go: { stones: [{ o: [[8, 5], [22, 5], [16, 19], [2, 19]], k: 0.45 }], step: 180, shift: 2 },
  maven: { stones: [{ o: [[12, 1.5], [18.5, 12], [12, 22.5], [5.5, 12]], k: 0.42 }], step: 180, shift: 2 },
  nuget: { stones: [{ o: hexagon, k: 0.5 }], step: 60, shift: 1 },
  cpp: { stones: [{ o: [[5, 8.5], [12.5, 2.5], [21, 8], [18.5, 19.5], [6.5, 20]], apex: [11, 11.5] }], step: 0, shift: 0 },
};
const LIGHT = [-0.6, -0.8]; // light from the top-left, a little more from the top

function facetLight(outer) {
  const n = outer.length;
  const dots = outer.map((a, i) => {
    const b = outer[(i + 1) % n];
    const nx = b[1] - a[1], ny = -(b[0] - a[0]);
    const l = Math.hypot(nx, ny) || 1;
    return (nx * LIGHT[0] + ny * LIGHT[1]) / l;
  });
  const lit = dots.indexOf(Math.max(...dots));
  return dots.map((d, i) => (i === lit ? 0.95 : +(0.1 + 0.42 * Math.max(0, d) ** 1.4).toFixed(3)));
}

const pts = (a) => a.map(([x, y]) => `${x.toFixed(2)},${y.toFixed(2)}`).join(" ");

function stoneSvg(eco, px = 16, { local = false } = {}) {
  const cut = CUTS[eco] || CUTS.crates;
  let g = "";
  for (const s of cut.stones) {
    const c = s.c || [12, 12];
    const inner = s.apex ? null : scale(s.o, s.k, c);
    const op = facetLight(s.o);
    const n = s.o.length;
    let f = "";
    for (let i = 0; i < n; i++) {
      const a = s.o[i], b = s.o[(i + 1) % n];
      const poly = inner ? [a, b, inner[(i + 1) % n], inner[i]] : [a, b, s.apex];
      const prev = op[(i - cut.shift + n) % n];
      f += `<polygon class="mk-fc" points="${pts(poly)}" style="--o:${op[i]};--p:${prev}"/>`;
    }
    const table = inner ? `<polygon class="mk-tb${local ? " mk-mine" : ""}" points="${pts(inner)}"/>` : "";
    const origin = cut.own ? ` style="transform-origin:${c[0]}px ${c[1]}px"` : "";
    g += `<g class="mk-st${s.back ? " mk-back" : ""}"${origin}>${f}${table}<polygon class="mk-ol" points="${pts(s.o)}"/></g>`;
  }
  return `<svg class="mk-stone mk-${eco}${cut.step ? "" : " mk-rough"}" viewBox="0 0 24 24" width="${px}" height="${px}" style="--step:${cut.step}deg" aria-hidden="true">${g}</svg>`;
}

// an install line breaks only between tokens, never inside `--version` or a module path
const toks = (line) => String(line).split(/(\s+)/).map((t) => (/^\s+$/.test(t) ? t : `<span class="mk-tok">${esc(t)}</span>`)).join("");

/**
 * ecosystemMark(eco, opts)
 *   opts.pkg       package name for the install line (default none: the card shows the registry only)
 *   opts.version   version for ecosystems whose install line names one (go, maven, nuget)
 *   opts.install   an exact install line (overrides the template; data.json has one per package)
 *   opts.local     {path} a workspace package: no registry; the copy line becomes the path dependency
 *   opts.word      false to draw the glyph alone (rows); default true (hero)
 *   opts.say       one sentence for the card (serif), when the registry needs explaining
 *   opts.card      false for the stone alone, no card (titlebars, dense lists)
 *   opts.id        id for ?hover= stills
 */
export function ecosystemMark(eco, opts = {}) {
  const key = ECO[eco] ? eco : ECO_ALIAS[String(eco).toLowerCase()] || "crates";
  const e = ECO[key];
  const local = opts.local;
  const line = opts.install || (opts.pkg ? e.install(opts.pkg, opts.version) : null);
  const w = opts.word === false ? "" : `<span class="mk-w">${esc(local ? "local" : e.word)}</span>`;
  const kind = local ? `${e.kind.split(" ")[0]} package` : e.kind;
  const where = local ? `in this workspace${local.path ? ` · ${local.path}` : ""}` : e.registry;
  let say = opts.say;
  if (!say && local) say = "Never published: other packages reach it by path.";
  if (!say && key === "cpp") say = "No registry to install from.";
  const copy = line
    ? `<span class="mk-well"><button class="mk-line" type="button" data-mk-copy="${esc(line)}"><span class="mk-txt">${toks(line)}</span>`
      + `<span class="mk-hint">copy</span><span class="mk-done">copied</span></button>`
      + `<span class="mk-tray"><span class="mk-ghost">${toks(line)}</span></span></span>`
    : "";
  const card = opts.card === false ? "" : `<span class="mk-card mk-card-eco" role="tooltip">`
    + `<span class="mk-eh">${stoneSvg(key, 30, { local: !!local })}<span class="mk-et"><b>${esc(kind)}</b><i>${esc(where)}</i></span></span>`
    + (say ? `<span class="mk-say">${esc(say)}</span>` : "") + copy + `</span>`;
  return `<span class="mk mk-m mk-eco${local ? " is-local" : ""}" tabindex="0" data-mk="eco" data-mk-id="${esc(opts.id || uid("eco"))}"`
    + ` aria-label="${esc(kind + ", " + where)}"><span class="mk-g">${stoneSvg(key, opts.px || 16, { local: !!local })}</span>${w}${card}</span>`;
}

// --------------------------------------------------------------------------- 2. license marks
// Shape encodes the family: permissive = open ring, weak copyleft = ring closed by one thin arc,
// strong copyleft = closed ring, public domain = dotted ring, unknown or custom = dashed ring.
const P = { commercial: "commercial use", modify: "modification", distribute: "distribution", patent: "patent use", private: "private use" };
const C = { notice: "keep the notice", changes: "mark your changes", source: "share the source", same: "same license", samefile: "same license, per file",
  samelib: "same license, for the library", network: "share source over a network" };
const L = { liability: "no liability", warranty: "no warranty", trademark: "no trademark rights", patent: "no patent rights" };
const base4 = ["commercial", "modify", "distribute", "private"];
const LIC = {
  "MIT": { name: "MIT License", fam: "permissive", p: base4, c: ["notice"], l: ["liability", "warranty"] },
  "MIT-0": { name: "MIT No Attribution", fam: "permissive", p: base4, c: [], l: ["liability", "warranty"] },
  "Apache-2.0": { name: "Apache License 2.0", fam: "permissive", p: [...base4.slice(0, 3), "patent", "private"], c: ["notice", "changes"], l: ["trademark", "liability", "warranty"] },
  "BSD-2-Clause": { name: "BSD 2-Clause", fam: "permissive", p: base4, c: ["notice"], l: ["liability", "warranty"] },
  "BSD-3-Clause": { name: "BSD 3-Clause", fam: "permissive", p: base4, c: ["notice"], l: ["liability", "warranty"] },
  "BSD-1-Clause": { name: "BSD 1-Clause", fam: "permissive", p: base4, c: ["notice"], l: ["liability", "warranty"] },
  "0BSD": { name: "Zero-Clause BSD", fam: "permissive", p: base4, c: [], l: ["liability", "warranty"] },
  "ISC": { name: "ISC License", fam: "permissive", p: base4, c: ["notice"], l: ["liability", "warranty"] },
  "Zlib": { name: "zlib License", fam: "permissive", p: base4, c: ["notice", "changes"], l: ["liability", "warranty"] },
  "zlib-acknowledgement": { name: "zlib/libpng with acknowledgement", fam: "permissive", p: base4, c: ["notice", "changes"], l: ["liability", "warranty"] },
  "BSL-1.0": { name: "Boost Software License 1.0", fam: "permissive", p: base4, c: ["notice"], l: ["liability", "warranty"] },
  "Unicode-3.0": { name: "Unicode License v3", fam: "permissive", p: base4, c: ["notice"], l: ["liability", "warranty"] },
  "Unicode-DFS-2016": { name: "Unicode License 2016", fam: "permissive", p: base4, c: ["notice"], l: ["liability", "warranty"] },
  "NCSA": { name: "University of Illinois/NCSA", fam: "permissive", p: base4, c: ["notice"], l: ["liability", "warranty"] },
  "CDLA-Permissive-2.0": { name: "Community Data License, Permissive 2.0", fam: "permissive", p: base4, c: ["notice"], l: ["liability", "warranty"] },
  "MPL-2.0": { name: "Mozilla Public License 2.0", fam: "weak", p: [...base4.slice(0, 3), "patent", "private"], c: ["source", "notice", "samefile"], l: ["trademark", "liability", "warranty"] },
  "LGPL-2.1": { name: "GNU LGPL v2.1", fam: "weak", p: base4, c: ["source", "notice", "samelib", "changes"], l: ["liability", "warranty"] },
  "LGPL-3.0": { name: "GNU LGPL v3.0", fam: "weak", p: [...base4.slice(0, 3), "patent", "private"], c: ["source", "notice", "samelib", "changes"], l: ["liability", "warranty"] },
  "EPL-2.0": { name: "Eclipse Public License 2.0", fam: "weak", p: [...base4.slice(0, 3), "patent", "private"], c: ["source", "notice", "samefile"], l: ["trademark", "liability", "warranty"] },
  "GPL-2.0": { name: "GNU GPL v2.0", fam: "strong", p: base4, c: ["source", "notice", "same", "changes"], l: ["liability", "warranty"] },
  "GPL-3.0": { name: "GNU GPL v3.0", fam: "strong", p: [...base4.slice(0, 3), "patent", "private"], c: ["source", "notice", "same", "changes"], l: ["liability", "warranty"] },
  "AGPL-3.0": { name: "GNU AGPL v3.0", fam: "strong", p: [...base4.slice(0, 3), "patent", "private"], c: ["source", "notice", "same", "changes", "network"], l: ["liability", "warranty"] },
  "Unlicense": { name: "The Unlicense", fam: "public", p: base4, c: [], l: ["liability", "warranty"] },
  "CC0-1.0": { name: "Creative Commons Zero", fam: "public", p: base4, c: [], l: ["liability", "trademark", "patent", "warranty"] },
};
const FAM_WORD = { permissive: "permissive", weak: "weak copyleft", strong: "strong copyleft", public: "public domain", unknown: "unknown terms" };
const FAM_RANK = { public: 0, permissive: 1, weak: 2, strong: 3, unknown: 4 };
const FAM_TERMS = { weak: "weak copyleft terms", strong: "strong copyleft terms", unknown: "custom terms", permissive: "permissive terms", public: "no terms" };

function baseId(id) {
  return id.replace(/-only$|-or-later$|\+$/, "");
}
function licInfo(id) {
  const b = baseId(id);
  return LIC[id] || LIC[b] || { name: id, fam: "unknown", p: [], c: [], l: [] };
}

/** Parse an SPDX expression (also the legacy "MIT/Apache-2.0" form). AND binds tighter than OR. */
export function readSpdx(spdx) {
  if (spdx == null || spdx === "") return null;
  const toks = String(spdx).replace(/\s*\/\s*/g, " OR ").match(/\(|\)|[^\s()]+/g) || [];
  let i = 0;
  const peek = () => toks[i];
  function atom() {
    if (peek() === "(") { i++; const e = or(); i++; return e; }
    let id = toks[i++];
    if (peek() === "WITH") { i++; id += ` WITH ${toks[i++]}`; }
    return { id };
  }
  function and() { const a = [atom()]; while (peek() === "AND") { i++; a.push(atom()); } return a.length > 1 ? { op: "and", args: a } : a[0]; }
  function or() { const a = [and()]; while (peek() === "OR") { i++; a.push(and()); } return a.length > 1 ? { op: "or", args: a } : a[0]; }
  return or();
}
const idsOf = (t) => (!t ? [] : t.id ? [t.id] : t.args.flatMap(idsOf));
const shortId = (id) => id.replace(/ WITH .*/, "").replace(/-only$|-or-later$/, "");
function restWord(t) {
  if (!t) return "none";
  if (t.id) return shortId(t.id);
  const parts = t.args.map((a) => (a.id ? shortId(a.id) : a.op === "or" ? a.args.map(restWord).join("/") : restWord(a)));
  if (t.op === "or") return parts.length > 2 ? `${parts.slice(0, 2).join("/")} +${parts.length - 2}` : parts.join("/");
  return `${parts[0]} +${parts.length - 1}`;
}

// An option is a list of ids that all apply (one branch of the OR, with its ANDs).
function options(t) {
  if (!t) return [];
  if (t.id) return [[t.id]];
  if (t.op === "or") return t.args.flatMap(options);
  return t.args.map(options).reduce((acc, opts) => acc.flatMap((a) => opts.map((b) => [...a, ...b])), [[]]);
}

/** Does `ids` (all applying) fit a project licensed `yours`?
 *  -> {level: 0 fits | 1 fits with a condition | 2 does not fit | 3 unknown, words (a full sentence), short (a clause after its name)} */
function fitOne(ids, yours, project) {
  const yourIds = new Set(idsOf(readSpdx(yours)).map(baseId));
  let level = 0, words = null, short = null;
  const worse = (l, w, s) => { if (l >= level) { level = l; words = w; short = s; } };
  for (const id of ids) {
    const inf = licInfo(id), b = baseId(id);
    if (inf.fam === "unknown") {
      worse(3, id === "LICENSE-FILE" ? "Its terms are in its own LICENSE file: read them before you ship it" : `${id} is not a license this knows: read it before you ship it`, "has terms nobody has read yet");
    } else if (inf.fam === "strong") {
      if (!yourIds.has(b)) worse(2, `Shipping it would make ${project} ${shortId(id)}: anyone you ship to could ask for all of its source`, `would make ${project} ${shortId(id)}`);
    } else if (inf.fam === "weak") {
      worse(1, b === "MPL-2.0" || b === "EPL-2.0"
        ? `Fits, with one duty: keep its own files under ${shortId(id)} and publish your changes to them; the rest of ${project} stays yours`
        : `Fits if people can swap in their own build of it; Rust links statically, so that means shipping your object files`,
        b === "MPL-2.0" || b === "EPL-2.0" ? "asks you to publish changes to its files" : "asks that people can relink it");
    } else if (b === "Apache-2.0" && yourIds.has("GPL-2.0") && !yourIds.has("GPL-3.0")) {
      worse(2, "Apache-2.0's patent terms do not mix with GPL-2.0", "does not mix with GPL-2.0");
    }
  }
  return { level, words, short };
}

function licenseFit(tree, yours, project) {
  const yourWord = restWord(readSpdx(yours));
  if (!tree) return { level: 3, line: `No license at all: by default nobody may copy it. Ask the author before you ship it.` };
  const rated = options(tree).map((ids) => ({ ids, ...fitOne(ids, yours, project) }));
  const nm = (r) => r.ids.map(shortId).join(" + ");
  if (rated.length === 1) {
    const r = rated[0];
    const all = r.ids.length > 1;
    if (r.level === 0) return { level: 0, line: all ? `All of it applies, and all of it fits your ${yourWord} project.` : `Fits your ${yourWord} project.`, choice: r.ids };
    return { level: r.level, line: r.words + ".", choice: r.ids };
  }
  const best = Math.min(...rated.map((r) => r.level));
  const good = rated.filter((r) => r.level === best);
  const bad = rated.filter((r) => r.level > best);
  if (best === 0 && !bad.length && tree.op === "and") {
    const always = rated[0].ids.filter((i) => rated.every((r) => r.ids.includes(i)));
    const rest = [...new Set(rated.flatMap((r) => r.ids.filter((i) => !always.includes(i))))];
    return { level: 0, line: `${list(always.map(shortId))} always ${always.length > 1 ? "apply" : "applies"}; for the rest, ${rest.length > 1 ? `either of ${list(rest.map(shortId), "or")}` : shortId(rest[0])}. All of it fits your ${yourWord} project.`, choice: rated[0].ids };
  }
  if (best === 0 && !bad.length) {
    const pat = good.find((r) => r.ids.some((i) => licInfo(i).p.includes("patent")));
    return { level: 0, line: `Either fits your ${yourWord} project${pat ? `; ${nm(pat)} adds a patent grant` : ""}.`, choice: (pat || good[0]).ids };
  }
  const pick = good[0];
  const lead = best === 0 ? `Choose ${nm(pick)}: it fits your ${yourWord} project.` : `Choose ${nm(pick)}. ${pick.words}.`;
  return { level: best, line: `${lead} ${bad.map((r) => `${nm(r)} ${r.short}`).join("; ")}.`, choice: pick.ids };
}

function ringSvg(fam, px = 16, extra = "") {
  // octagonal ring; eight edges drawn separately so a family can open, thin, dot or dash them
  const R = 9, c = 12;
  const v = [...Array(8)].map((_, i) => { const a = (-112.5 + 45 * i) * Math.PI / 180; return [c + R * Math.cos(a), c + R * Math.sin(a)]; });
  let s = "";
  for (let i = 0; i < 8; i++) {
    const a = v[i], b = v[(i + 1) % 8];
    let cls = "mk-e";
    if (fam === "permissive" && i === 1) cls = "mk-e mk-gap";
    if (fam === "weak" && i === 1) cls = "mk-e mk-gap";
    s += `<line class="${cls}" x1="${a[0].toFixed(2)}" y1="${a[1].toFixed(2)}" x2="${b[0].toFixed(2)}" y2="${b[1].toFixed(2)}"/>`;
  }
  if (fam === "public") s = v.map(([x, y]) => `<rect class="mk-d" x="${(x - 1.1).toFixed(2)}" y="${(y - 1.1).toFixed(2)}" width="2.2" height="2.2" transform="rotate(45 ${x.toFixed(2)} ${y.toFixed(2)})"/>`).join("");
  const core = fam === "strong" ? `<polygon class="mk-core" points="${pts(scale(v, 0.52))}"/>`
    : fam === "weak" ? `<rect class="mk-core mk-weakc" x="9.6" y="9.6" width="4.8" height="4.8" transform="rotate(45 12 12)"/>` : "";
  return `<svg class="mk-ring mk-${fam}" viewBox="0 0 24 24" width="${px}" height="${px}" aria-hidden="true"${extra}>${core}${s}</svg>`;
}

function ringsFor(tree, px, fitIds) {
  if (!tree) return `<span class="mk-rings">${ringSvg("unknown", px)}</span>`;
  const one = (id) => ringSvg(licInfo(id).fam, px, fitIds && fitIds.includes(id) ? ` data-fit="1"` : "");
  if (tree.id) return `<span class="mk-rings">${one(tree.id)}</span>`;
  const inner = (t) => (t.id ? one(t.id) : `<span class="mk-rings mk-${t.op}">${t.args.map(inner).join("")}</span>`);
  return `<span class="mk-rings mk-${tree.op}">${tree.args.map(inner).join("")}</span>`;
}

function columns(id) {
  const inf = licInfo(id);
  const col = (h, keys, dict) => `<span class="mk-col"><span class="mk-h">${h}</span>${keys.length ? keys.map((k) => `<span>${esc(dict[k])}</span>`).join("") : `<span class="mk-none">none</span>`}</span>`;
  return `<span class="mk-cols" data-lic="${esc(id)}">${col("Permissions", inf.p, P)}${col("Conditions", inf.c, C)}${col("Limitations", inf.l, L)}</span>`;
}

/**
 * licenseMark(spdx, yourLicense, opts)
 *   spdx         the package's license expression; null/undefined = no license field
 *   yourLicense  the reader's project license (fit line), e.g. "MIT OR Apache-2.0"
 *   opts.file    the license-file name when there is no expression ("LICENSE")
 *   opts.project the reader's project name used in consequence words (default "your project")
 *   opts.tree    {families, notable:[{crate, license, fam, via}], total, local} from data.json tree.licenses
 *   opts.self    true on the project's own hero: the card summarises the whole tree
 *   opts.id, opts.word (false = glyph only)
 */
export function licenseMark(spdx, yourLicense, opts = {}) {
  const project = opts.project || "your project";
  const noField = spdx == null || spdx === "";
  const tree = noField ? (opts.file ? { id: "LICENSE-FILE" } : null) : readSpdx(spdx);
  const fit = opts.self
    ? { level: 0, line: `Your project's own license: every package in your tree is checked against it.`, choice: null }
    : licenseFit(tree, yourLicense || "MIT", project);
  const ids = idsOf(tree);
  const w = opts.word === false ? "" : `<span class="mk-w">${esc(noField ? (opts.file ? "custom" : "none") : restWord(tree))}</span>`;
  const head = noField
    ? `<b>${opts.file ? `Custom terms` : `No license`}</b><i>${opts.file ? `license-file = "${esc(opts.file)}"` : "no license field"}</i>`
    : `<b>${esc(ids.length === 1 ? licInfo(ids[0]).name : spdx)}</b><i>${esc(ids.length === 1 ? FAM_WORD[licInfo(ids[0]).fam] : tree.op === "or" ? "your choice of " + word(ids.length) : "all of them apply")}</i>`;
  // the choice tabs: one per distinct id; the fitting one starts selected
  const pickId = (fit.choice || ids)[0];
  const tabs = ids.length > 1 ? `<span class="mk-tabs">${ids.map((id) => `<button type="button" class="mk-tab${id === pickId ? " is-on" : ""}" data-mk-lic="${esc(id)}">${ringSvg(licInfo(id).fam, 13)}${esc(shortId(id))}</button>`).join("")}</span>` : "";
  const cols = ids.filter((id) => id !== "LICENSE-FILE").map((id) => columns(id).replace('class="mk-cols"', `class="mk-cols${id === pickId ? " is-on" : ""}"`)).join("");
  // what the mark knows about your whole tree
  let treeLine = "";
  const t = opts.tree;
  if (t && opts.self) {
    const ns = (t.notable || []).filter((n) => n.fam === "weak" || n.fam === "strong" || n.fam === "unknown");
    const byLic = {};
    for (const n of ns) (byLic[n.license] ||= []).push(n);
    const parts = Object.entries(byLic).map(([lic, xs]) => {
      const run = xs.filter((x) => !x.buildOnly).map((x) => x.crate), bo = xs.filter((x) => x.buildOnly).map((x) => x.crate);
      const tail = bo.length ? ` (${list(bo)} too, at build time only)` : "";
      return lic === "custom" ? `custom terms in ${list(run.length ? run : bo)}` : `${lic} in ${list(run)}${tail}`;
    });
    const f = t.families || {};
    treeLine = `<span class="mk-tree"><span class="mk-h">Your tree</span>${plural(t.total, "package")}: permissive throughout${parts.length ? `, plus ${list(parts)}` : ""}.`
      + (f.unread ? ` <span class="mk-dim">${f.unread.toLocaleString("en")} are not unpacked here, so unread.</span>` : "") + `</span>`;
  } else if (t && ids.length && tree) {
    // one more line, only when it matters: terms of a kind this tree does not carry yet
    const eff = Math.min(...options(tree).map((o) => Math.max(...o.map((i) => FAM_RANK[licInfo(i).fam]))));
    const famName = Object.keys(FAM_RANK).find((k) => FAM_RANK[k] === eff);
    if (eff >= 2) {
      const self = opts.pkg;
      const all = (t.notable || []).filter((n) => n.fam === famName);
      const carried = all.filter((n) => n.crate !== self);
      if (self && carried.length < all.length) {
        const bo = (n) => n.buildOnly ? " (build-time only)" : "";
        treeLine = carried.length
          ? `<span class="mk-tree">One of ${word(all.length)} packages in your tree with ${esc(FAM_TERMS[famName])}; the others are ${esc(list(carried.map((n) => n.crate + bo(n))))}.</span>`
          : `<span class="mk-tree">The only package in your tree with ${esc(FAM_TERMS[famName])}.</span>`;
      } else {
        const lic = ids.map(shortId).filter((i) => FAM_RANK[licInfo(i).fam] >= 2).join(", ") || "these terms";
        treeLine = carried.length
          ? `<span class="mk-tree">Your tree already has ${esc(FAM_TERMS[famName])} in ${esc(list(carried.slice(0, 3).map((n) => n.crate)))}; this adds no new kind.</span>`
          : `<span class="mk-tree mk-new">Adding it brings ${esc(lic)} into a tree with no ${esc(FAM_TERMS[famName])} today.</span>`;
      }
    }
  }
  const fitCls = ["mk-fits", "mk-cond", "mk-no", "mk-unk"][fit.level];
  const card = `<span class="mk-card mk-card-lic" role="tooltip"><span class="mk-lh">${ringsFor(tree, 26, fit.level <= 1 ? fit.choice : null)}<span class="mk-et">${head}</span></span>`
    + tabs + cols + `<span class="mk-fit ${fitCls}">${ringSvg("permissive", 12)}<span>${esc(fit.line)}</span></span>` + treeLine + `</span>`;
  return `<span class="mk mk-m mk-lic" tabindex="0" data-mk="lic" data-mk-id="${esc(opts.id || uid("lic"))}" aria-label="${esc(spdx || "no license")}">`
    + `<span class="mk-g">${ringsFor(tree, opts.px || 16)}</span>${w}${card}</span>`;
}

// --------------------------------------------------------------------------- 3. version comb
function parseV(v) {
  const [core, build = ""] = String(v).split("+");
  const dash = core.indexOf("-");
  const nums = dash < 0 ? core : core.slice(0, dash);
  const pre = dash < 0 ? "" : core.slice(dash + 1);
  const [M = 0, m = 0, p = 0] = nums.replace(/^v/, "").split(".").map((n) => parseInt(n, 10) || 0);
  return { M, m, p, pre, build, raw: v };
}
function cmpV(a, b) {
  const x = parseV(a), y = parseV(b);
  return x.M - y.M || x.m - y.m || x.p - y.p || (x.pre ? (y.pre ? x.pre.localeCompare(y.pre) : -1) : y.pre ? 1 : 0);
}
const caretClass = (v) => { const s = parseV(v); return s.M > 0 ? `${s.M}` : s.m > 0 ? `0.${s.m}` : `0.0.${s.p}`; };
const shortV = (v) => String(v).split("+")[0];

/** The semver reading in words. */
export function semverReading(history, pin, t = now()) {
  const rel = history.filter((r) => !r.yanked && !parseV(r.v).pre).sort((a, b) => cmpV(a.v, b.v));
  const latest = rel.at(-1);
  if (!pin) return { behind: null, latest, words: latest ? `Not in your tree · ${plural(history.length, "release")} · the newest ${ago(latest.at, t)} ago` : `No releases` };
  const after = rel.filter((r) => cmpV(r.v, pin) > 0);
  const classes = new Set(after.map((r) => caretClass(r.v)));
  classes.delete(caretClass(pin));
  const breaking = classes.size;
  const firstAfter = after.filter((r) => r.at).sort((a, b) => Date.parse(a.at) - Date.parse(b.at))[0];
  if (!after.length) return { behind: 0, breaking: 0, latest, words: "Up to date · the newest release" };
  const age = firstAfter ? ago(firstAfter.at, t) : null;
  const words = `${plural(after.length, "release")} behind · ${breaking ? `${word(breaking)} of them breaking` : "none of them breaking"}${age ? ` · ${age}` : ""}`;
  return { behind: after.length, breaking, latest, age, words };
}

function tickKind(list, i) {
  const r = list[i], s = parseV(r.v);
  if (s.pre) return "pre";
  const prev = list.slice(0, i).reverse().find((x) => !parseV(x.v).pre);
  if (!prev) return "major";
  if (caretClass(prev.v) !== caretClass(r.v)) return "major";
  const q = parseV(prev.v);
  return q.M === s.M && q.m === s.m ? "patch" : "minor";
}

function numberHtml(v, from = null, t = 0.7) {
  // odometer: each semver segment is its own wheel; only changed wheels roll.
  // `from` + `t` draw one frozen frame of the roll (stills).
  const parts = String(v).split(/([.+-])/);
  const old = from ? String(from).split(/([.+-])/) : null;
  const dir = from && cmpV(v, from) > 0 ? 1 : -1;
  return parts.map((p, i) => {
    if (/^[.+-]$/.test(p)) return `<span class="mk-sep">${esc(p)}</span>`;
    if (old && old.length === parts.length && old[i] !== p) {
      const a = dir > 0 ? -t * 100 : t * 100, b = dir > 0 ? (1 - t) * 100 : -(1 - t) * 100;
      const w = Math.max(p.length, old[i].length);
      return `<span class="mk-wh" data-i="${i}" style="width:${w}ch"><span class="mk-rl mk-in" style="transform:translateY(${b}%)">${esc(p)}</span><span class="mk-rl mk-out" style="transform:translateY(${a}%)">${esc(old[i])}</span></span>`;
    }
    return `<span class="mk-wh" data-i="${i}"><span class="mk-rl">${esc(p)}</span></span>`;
  }).join("");
}

/**
 * versionComb(history, pin, opts) — the number rides the comb.
 *   history      [{v, at?, yanked?}] any order; every release the registry knows
 *   pin          your locked version, or null when the package is not in your tree
 *   opts.also    [{v, via:[names]}] other versions of this package in your tree (duplicates)
 *   opts.yours   your packages that pin `pin` (names), for the hover
 *   opts.measured data.json packages[*].measured (API diffs from releases.json), optional
 *   opts.local   {path} a workspace package that was never published
 *   opts.variant 'rider' (default) | 'baseline' | 'terrace' | 'time'
 *   opts.view    a version to show scrubbed to (stills); opts.name the package name for sentences
 *   opts.id, opts.latest (false hides the faint latest label)
 */
export function versionComb(history, pin, opts = {}) {
  const variant = opts.variant || "rider";
  const hist = [...(history || [])].sort((a, b) => cmpV(a.v, b.v));
  const name = opts.name || "it";
  const id = opts.id || uid("ver");
  const t0 = now();
  if (opts.local || !hist.length) {
    const v = pin || "0.0.0";
    const card = `<span class="mk-card mk-card-ver" role="tooltip"><span class="mk-vh"><b>${esc(v)}</b><i>never published</i></span>`
      + `<span class="mk-say">${esc(opts.local ? `Its version comes from the workspace, and publishing is off: nothing outside this repository can depend on it.` : `No release history is known for ${name}.`)}</span></span>`;
    return `<span class="mk mk-ver mk-solo" data-mk="ver" data-mk-id="${esc(id)}" style="--nw:${String(v).length + 1.4}ch">`
      + `<span class="mk-comb"><span class="mk-m mk-num mk-pin" tabindex="0" data-mk-id="${esc(id)}-num" style="left:0">${numberHtml(v)}${card}</span>`
      + `<i class="mk-unpub"></i></span></span>`;
  }
  const also = (opts.also || []).filter((a) => a.v !== pin);
  const pinIdx = pin ? hist.findIndex((r) => r.v === pin) : hist.length - 1;
  const viewV = opts.view && hist.some((r) => r.v === opts.view) ? opts.view : null;
  const anchorIdx = viewV ? hist.findIndex((r) => r.v === viewV) : pinIdx;
  const shown = hist[anchorIdx]?.v || pin;
  const n = hist.length;
  // x along the comb, 0..1: release order, or time for the 'time' variant
  let frac;
  if (variant === "time" && hist.every((r) => r.at)) {
    const a = Date.parse(hist[0].at), b = Math.max(...hist.map((r) => Date.parse(r.at)));
    frac = hist.map((r) => (Date.parse(r.at) - a) / (b - a || 1));
  } else frac = hist.map((_, i) => (n === 1 ? 0 : i / (n - 1)));
  const dense = n > (opts.width || 600) / 3.2;
  const reading = semverReading(hist, pin, t0);
  const kinds = hist.map((_, i) => tickKind(hist, i));
  let climb = 0, lastClass = pin ? caretClass(pin) : null;
  const floors = [];
  const alsoSet = new Set(also.map((a) => a.v));
  let ticks = "";
  hist.forEach((r, i) => {
    const after = pin && i > pinIdx;
    const k = kinds[i];
    if (variant === "terrace" && after && !parseV(r.v).pre && caretClass(r.v) !== lastClass) { climb++; lastClass = caretClass(r.v); }
    if (variant === "terrace" && after) {
      const f = floors.at(-1);
      if (f && f.lift === climb) f.b = frac[i]; else floors.push({ a: frac[i], b: frac[i], lift: climb });
    }
    const cls = ["mk-t", `mk-${k}`, r.yanked ? "mk-yanked" : "", after ? "mk-after" : "", after && k === "major" ? "mk-brk" : "", alsoSet.has(r.v) ? "mk-alsot" : "", i === pinIdx && pin ? "mk-pin" : ""].filter(Boolean).join(" ");
    const shift = i > anchorIdx ? " + var(--nw)" : "";
    const fade = after ? `;--f:${(0.62 - 0.4 * ((i - pinIdx) / Math.max(1, n - 1 - pinIdx))).toFixed(2)}` : "";
    const lift = variant === "terrace" && climb ? `;--lift:${climb * 4}px` : "";
    if (i === anchorIdx && variant !== "baseline") return; // the number is this tooth
    ticks += `<i class="${cls}" data-v="${esc(r.v)}" style="left:calc((100% - var(--nw)) * ${frac[i].toFixed(4)}${shift})${fade}${lift}"></i>`;
  });
  for (const f of floors) ticks += `<i class="mk-floor" style="left:calc((100% - var(--nw)) * ${f.a.toFixed(4)} + var(--nw) - 2px);width:calc((100% - var(--nw)) * ${(f.b - f.a).toFixed(4)} + 4px);bottom:${3 + f.lift * 4}px"></i>`;
  // the pin tooth stays home while you look elsewhere
  if (viewV && pin && viewV !== pin) {
    ticks += `<i class="mk-t mk-pinhome" style="left:calc((100% - var(--nw)) * ${frac[pinIdx].toFixed(4)}${pinIdx > anchorIdx ? " + var(--nw)" : ""})"></i>`;
  }
  // duplicates: other versions of this package your lockfile also holds
  let alsoMarks = "";
  for (const a of also) {
    const j = hist.findIndex((r) => r.v === a.v);
    if (j < 0) continue;
    const m = opts.measured?.diffs?.[a.v];
    const via = (x) => ((x || []).length > 3 ? `${x.slice(0, 3).join(", ")} and ${x.length - 3} more` : list(x || []));
    const pinSide = opts.yours?.length ? `your ${plural(opts.yours.length, "package")} pin ${esc(shortV(pin))}` : opts.pinVia ? `${esc(shortV(pin))} through ${esc(via(opts.pinVia))}` : `you pin ${esc(shortV(pin))}`;
    const older = cmpV(a.v, pin) < 0 ? a : { v: pin, via: opts.pinVia };
    const notYours = !opts.yours?.length && opts.pinVia;
    const move = notYours ? `Neither is yours to move: ${plural((older.via || []).length, "crate")} still ask for ${esc(shortV(older.v).split(".")[0])}.x.` : m ? (m.yourSitesChanged === 0 ? `Moving yours to ${shortV(a.v)} drops a copy; none of your ${m && opts.measured.yourUses} uses change.` : `Moving yours to ${shortV(a.v)} drops a copy; ${plural(m.yourSitesChanged, "of your uses", "of your uses")} change.`) : `Moving yours to ${shortV(a.v)} would drop a copy.`;
    const card = `<span class="mk-card mk-card-ver" role="tooltip"><span class="mk-vh"><b>${esc(shortV(a.v))}</b><i>also in your tree</i></span>`
      + `<span class="mk-say">Two copies of ${esc(name)} compile. ${esc(shortV(a.v))} comes through ${esc(via(a.via))}; ${pinSide}.</span>`
      + `<span class="mk-fact">${esc(move)}</span></span>`;
    alsoMarks += `<span class="mk-m mk-also${frac[j] > 0.55 ? " mk-flip" : ""}"${frac[j] > 0.55 ? ` data-side="left"` : ""} tabindex="0" data-mk-id="${esc(id)}-also-${esc(shortV(a.v))}" style="left:calc((100% - var(--nw)) * ${frac[j].toFixed(4)}${j > anchorIdx ? " + var(--nw)" : ""})"><i></i>${card}</span>`;
  }
  // the reading, on the number
  const lat = reading.latest;
  const m = opts.measured && lat ? opts.measured.diffs?.[lat.v] : null;
  let yoursSay = "";
  if (m && opts.measured.yourUses != null) {
    const resp = m.yourItemsRespelled || [];
    yoursSay = m.yourSitesChanged === 0
      ? `None of your ${opts.measured.yourUses} uses change${resp.length ? `; ${plural(m.yourSitesTouched, "call")} to ${resp.map((p) => p.split("::").pop()).join(", ")} ${m.yourSitesTouched === 1 ? "is" : "are"} respelled, not changed` : ""}.`
      : `${plural(m.yourSitesChanged, "of your uses", "of your uses")} change.`;
  }
  const dup = also.length ? `<span class="mk-fact">Your tree also holds ${esc(also.map((a) => shortV(a.v)).join(", "))}.</span>` : "";
  const head = pin
    ? (reading.behind ? `<b>${esc(shortV(pin))}</b><span class="mk-to">→</span><span class="mk-lat">${esc(shortV(lat.v))}</span>` : `<b>${esc(shortV(pin))}</b>`)
    : `<b>${esc(shortV(lat?.v || ""))}</b>`;
  const numCard = `<span class="mk-card mk-card-ver" role="tooltip"><span class="mk-vh">${head}</span>`
    + `<span class="mk-read">${esc(reading.words)}</span>` + (yoursSay ? `<span class="mk-say">${esc(yoursSay)}</span>` : "") + dup + `</span>`;
  const numCls = ["mk-m", "mk-num", pin && !viewV ? "mk-pin" : "", viewV && viewV !== pin ? "mk-view" : "", !pin ? "mk-free" : "", frac[anchorIdx] > 0.55 && variant !== "baseline" ? "mk-flip" : ""].filter(Boolean).join(" ");
  const numLeft = variant === "baseline" ? "0" : `calc((100% - var(--nw)) * ${frac[anchorIdx].toFixed(4)})`;
  const side = numCls.includes("mk-flip") ? ` data-side="left"` : "";
  const num = `<span class="${numCls}"${side} tabindex="0" data-mk-id="${esc(id)}-num" style="left:${numLeft}">${numberHtml(shortV(shown), opts.rollFrom ? shortV(opts.rollFrom) : null)}${numCard}</span>`;
  const latestLbl = opts.latest !== false && lat && pin && cmpV(lat.v, pin) > 0 && variant !== "baseline" ? `<span class="mk-latest">${esc(shortV(lat.v))}</span>` : "";
  const line = viewV && pin && viewV !== pin
    ? `<span class="mk-vline">viewing <b>${esc(shortV(viewV))}</b><i>·</i>you pin <span>${esc(shortV(pin))}</span><kbd>esc</kbd></span>` : "";
  const nw = `${shortV(shown).length + 1.4}ch`;
  return `<span class="mk mk-ver mk-${variant}${dense ? " mk-dense" : ""}" data-mk="ver" data-mk-id="${esc(id)}" data-pin="${esc(pin || "")}" style="--nw:${nw}"`
    + ` data-hist='${esc(JSON.stringify(hist.map((r) => [r.v, r.at || null, r.yanked ? 1 : 0])))}' data-frac='${esc(JSON.stringify(frac.map((f) => +f.toFixed(4))))}'>`
    + `<span class="mk-comb">${variant === "baseline" ? num + `<span class="mk-bcomb">${ticks}</span>` : ticks + alsoMarks + num}<span class="mk-tip"></span></span>${latestLbl}${line}</span>`;
}

// --------------------------------------------------------------------------- 4. depends on
/**
 * depLink(dep, opts) — a hyperlinked name whose peek says why this package uses it.
 *   dep: {name, kind, req, resolved, optional, features, on:{default,tree}, uses, files, items:[[item,n]],
 *         purpose, inTree:{versions, yours, via, local}, treeNote, local, newest}
 *   opts.parent  the package that depends on it (for sentences); opts.href; opts.id
 */
export function depLink(dep, opts = {}) {
  const parent = opts.parent || "it";
  const cls = ["mk", "mk-m", "mk-dep", dep.optional ? "mk-opt" : "", dep.optional && dep.on && dep.on.tree === false ? "mk-off" : "", dep.kind !== "normal" ? "mk-dev" : "", dep.local || dep.inTree?.local ? "mk-mine" : ""].filter(Boolean).join(" ");
  const say = dep.purpose ? cap(dep.purpose) + "." : dep.uses === null ? `${cap(dep.kind)}-only: its uses are in files the package does not ship.` : "Declared, but no use of it was found.";
  const top = (dep.items || [])[0];
  const more = (dep.items || []).length > 1 && top[1] < dep.uses;
  const uses = dep.uses == null ? `<span class="mk-uses mk-unk">uses unknown: its tests are not shipped</span>`
    : `<span class="mk-uses"><b>${dep.uses.toLocaleString("en")}</b> ${dep.uses === 1 ? "use" : "uses"}${top ? `<i>·</i>${more ? "most through " : ""}<code>${esc(top[0])}</code>${more ? ` ${top[1]}` : ""}` : ""}</span>`;
  const rows = [];
  if (dep.kind !== "normal") rows.push(["kind", `${dep.kind}-dependency`]);
  if (dep.optional) {
    const feats = (dep.features || []).filter((f) => f !== dep.name && f !== dep.alias).map((f) => `<code>${esc(f)}</code>`);
    const fl = feats.length ? feats.join(", ") : `<code>${esc(dep.name)}</code>`;
    const state = dep.on?.tree === false ? "off in your tree" : dep.on?.default ? "on by default" : "off by default";
    rows.push(["feature", `optional · ${state} · ${fl}`]);
  }
  if (dep.req || dep.resolved) {
    const req = dep.req ? `<code>${esc(/^[\d]/.test(dep.req) ? "^" + dep.req : dep.req)}</code>` : "";
    const got = dep.resolved ? ` → <code>${esc(shortV(dep.resolved))}</code>` : dep.newest ? ` · newest <code>${esc(dep.newest)}</code>` : "";
    rows.push(["version", req + got]);
  }
  const it = dep.inTree;
  let tree = "";
  if (dep.local || it?.local) tree = `<span class="mk-mint">yours</span>, in this workspace`;
  else if (!it) tree = dep.treeNote ? esc(dep.treeNote) : `not in your tree`;
  else if (dep.optional && dep.on?.tree === false) {
    const f = (dep.features || []).find((x) => x !== dep.name && x !== dep.alias) || dep.name;
    tree = `turning on <code>${esc(f)}</code> <span class="mk-mint">costs nothing</span>: ${esc(dep.name)} ${esc(shortV(it.versions.at(-1)))} is already here`;
  } else if (it.yours?.length) tree = `${plural(it.yours.length, "of your packages", "of your packages")} use it too`;
  else if (it.versions.length > 1) tree = `${word(it.versions.length)} versions: ${it.versions.map((v) => `<code>${esc(shortV(v))}</code>`).join(", ")}`;
  else {
    const others = (it.via || []).filter((x) => x !== parent && x !== opts.parentCrate);
    const n = (it.viaCount ?? (it.via || []).length) - ((it.via || []).length - others.length);
    const who = n > 2 ? `${others.slice(0, 2).join(", ")} and ${n - 2} more` : list(others.slice(0, 2));
    tree = others.length ? `also through ${esc(who)}: it stays if ${esc(parent)} goes` : `only through ${esc(parent)}: it leaves with it`;
  }
  rows.push(["your tree", tree]);
  const card = `<span class="mk-card mk-card-dep" role="tooltip"><span class="mk-dh">${depMark(dep)}<b>${esc(dep.name)}</b>${dep.resolved ? `<span class="mk-v">${esc(shortV(dep.resolved))}</span>` : ""}</span>`
    + `<span class="mk-say">${esc(say)}</span>${uses}<span class="mk-kv">${rows.map(([k, v]) => `<span class="mk-k">${k}</span><span class="mk-v">${v}</span>`).join("")}</span></span>`;
  const href = opts.href ? ` href="${esc(opts.href)}"` : "";
  return `<span class="${cls}" tabindex="0" data-mk="dep" data-mk-id="${esc(opts.id || uid("dep"))}">${depMark(dep)}<a class="mk-dn"${href}>${esc(dep.name)}</a>${card}</span>`;
}

function depMark(dep) {
  return `<i class="mk-dd${dep.optional ? " mk-hollow" : ""}${dep.local || dep.inTree?.local ? " mk-mine" : ""}"></i>`;
}

/** depList(deps, opts): normal deps as links; dev and build deps wait under ⌥. */
export function depList(deps, opts = {}) {
  const norm = deps.filter((d) => d.kind === "normal");
  const rest = deps.filter((d) => d.kind !== "normal");
  const ids = opts.ids || {};
  const one = (d) => depLink(d, { ...opts, id: ids[d.name] || (opts.idPrefix ? `${opts.idPrefix}-${d.name}` : undefined) });
  return `<span class="mk-deps">${norm.map(one).join("")}${rest.length ? `<span class="mk-alt-only">${rest.map(one).join("")}</span>` : ""}</span>`;
}

// --------------------------------------------------------------------------- the hero gem
const GEM_FACETS = ["24,2 35,13 24,10", "24,10 35,13 38,24", "35,13 46,24 38,24", "46,24 35,35 38,24", "38,24 35,35 24,38", "35,35 24,46 24,38",
  "24,46 13,35 24,38", "24,38 13,35 10,24", "13,35 2,24 10,24", "2,24 13,13 10,24", "10,24 13,13 24,10", "13,13 24,2 24,10"];
const GEM_LIT = [0.4, 0.3, 0.34, 0.14, 0.08, 0.11, 0.22, 0.2, 0.3, 0.56, 0.5, 0.66];
const PKG_GLYPH = '<path class="f" d="m12 3 8 4.5-8 4.5-8-4.5z"></path><path d="m12 3 8 4.5v9L12 21l-8-4.5v-9z"></path><path d="m4 7.5 8 4.5 8-4.5M12 12v9"></path>';
export function packageGem(px = 64, opts = {}) {
  const f = GEM_FACETS.map((p, i) => `<polygon class="mk-gfc" style="--t:${GEM_LIT[i]};--i:${i}" points="${p}"></polygon>`).join("");
  return `<svg class="mk-gem${opts.local ? " mk-mine" : ""}" width="${px}" height="${px}" viewBox="0 0 48 48" aria-hidden="true">${f}`
    + `<path d="M24 2 46 24 24 46 2 24z" fill="none" stroke="currentColor" stroke-width="1"></path>`
    + `<path class="mk-gtb" d="M24 10 38 24 24 38 10 24z" stroke="currentColor" stroke-width=".7" stroke-opacity=".55"></path>`
    + `<g transform="translate(17.4 17.4) scale(.55)" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="square" stroke-linejoin="miter">${PKG_GLYPH.replace('class="f"', 'fill="currentColor" stroke="none" opacity=".5"')}</g></svg>`;
}

// --------------------------------------------------------------------------- behaviour
function ensureCss() {
  if (typeof document === "undefined") return;
  const href = new URL("marks.css", HERE).href;
  if ([...document.querySelectorAll("link[rel=stylesheet]")].some((l) => l.href === href)) return;
  const l = document.createElement("link");
  l.rel = "stylesheet";
  l.href = href;
  document.head.appendChild(l);
}

function placeCard(m) {
  const card = m.querySelector(":scope > .mk-card");
  if (!card) return;
  m.classList.remove("mk-flip", "mk-up");
  if (m.dataset.side === "left") m.classList.add("mk-flip");
  const r = card.getBoundingClientRect();
  const W = document.documentElement.clientWidth;
  if (r.right > W - 8) m.classList.add("mk-flip");
  else if (r.left < 8) m.classList.remove("mk-flip");
  if (r.bottom > window.innerHeight - 8 && r.height < m.getBoundingClientRect().top - 8) m.classList.add("mk-up");
}

export function openMark(id, root = document) {
  const m = root.querySelector(`[data-mk-id="${CSS.escape(id)}"]`);
  if (!m) return null;
  m.classList.add("is-open");
  (m.closest(".mk-m") || m).classList.add("is-open");
  requestAnimationFrame(() => placeCard(m));
  return m;
}

function nearestTick(ver, clientX) {
  const comb = ver.querySelector(".mk-comb");
  const r = comb.getBoundingClientRect();
  const frac = JSON.parse(ver.dataset.frac || "[]");
  const nw = ver.querySelector(".mk-num")?.getBoundingClientRect().width || 0;
  const x = (clientX - r.left) / Math.max(1, r.width - nw);
  let best = 0, bd = 9;
  frac.forEach((f, i) => { const d = Math.abs(f - x); if (d < bd) { bd = d; best = i; } });
  return best;
}

function rollNumber(numEl, from, to, dir) {
  const a = String(from).split(/([.+-])/), b = String(to).split(/([.+-])/);
  if (a.length !== b.length) { numEl.innerHTML = numberHtml(to) + (numEl.querySelector(".mk-card")?.outerHTML || ""); return; }
  numEl.querySelectorAll(".mk-wh").forEach((wh) => {
    const i = +wh.dataset.i;
    if (a[i] === b[i]) return;
    const old = wh.querySelector(".mk-rl");
    const nu = document.createElement("span");
    nu.className = "mk-rl mk-in " + (dir > 0 ? "mk-up" : "mk-down");
    nu.textContent = b[i];
    old.classList.add("mk-out", dir > 0 ? "mk-up" : "mk-down");
    wh.appendChild(nu);
    setTimeout(() => old.remove(), 260);
  });
}

function scrubTo(ver, i) {
  const hist = JSON.parse(ver.dataset.hist || "[]");
  const frac = JSON.parse(ver.dataset.frac || "[]");
  const pin = ver.dataset.pin;
  const num = ver.querySelector(".mk-num");
  const cur = ver.dataset.at ? +ver.dataset.at : hist.findIndex((h) => h[0] === pin);
  if (i === cur || !hist[i]) return;
  const v = hist[i][0];
  rollNumber(num, shortV(hist[cur]?.[0] || v), shortV(v), i > cur ? 1 : -1);
  ver.dataset.at = i;
  num.style.left = `calc((100% - var(--nw)) * ${frac[i]})`;
  num.classList.toggle("mk-view", v !== pin);
  num.classList.toggle("mk-pin", v === pin);
  ver.querySelectorAll(".mk-comb > i.mk-t").forEach((t) => {
    const j = hist.findIndex((h) => h[0] === t.dataset.v);
    if (j < 0) return;
    t.style.left = `calc((100% - var(--nw)) * ${frac[j]}${j > i ? " + var(--nw)" : ""})`;
    t.classList.toggle("mk-hid", j === i);
  });
  let line = ver.querySelector(".mk-vline");
  if (v !== pin && pin) {
    if (!line) { line = document.createElement("span"); line.className = "mk-vline"; ver.appendChild(line); }
    line.innerHTML = `viewing <b>${esc(shortV(v))}</b><i>·</i>you pin <span>${esc(shortV(pin))}</span><kbd>esc</kbd>`;
  } else line?.remove();
}

let installed = false;
export function install(root = typeof document !== "undefined" ? document : null) {
  if (!root || installed) return;
  installed = true;
  ensureCss();
  root.addEventListener("pointerover", (e) => {
    const m = e.target.closest?.(".mk-m");
    if (m) placeCard(m);
  });
  root.addEventListener("click", (e) => {
    const b = e.target.closest?.("[data-mk-copy]");
    if (b) {
      const mark = b.closest(".mk-eco");
      navigator.clipboard?.writeText(b.dataset.mkCopy).catch(() => {});
      mark.classList.remove("is-copied");
      void mark.offsetWidth;
      mark.classList.add("is-copied");
      setTimeout(() => mark.classList.remove("is-copied"), 1500);
      return;
    }
    const eco = e.target.closest?.(".mk-eco > .mk-g");
    if (eco) eco.parentElement.querySelector("[data-mk-copy]")?.click();
    const tab = e.target.closest?.("[data-mk-lic]");
    if (tab) chooseLicense(tab);
  });
  root.addEventListener("pointerover", (e) => {
    const tab = e.target.closest?.("[data-mk-lic]");
    if (tab) chooseLicense(tab);
  });
  // comb: hover tip, drag to scrub
  let drag = null;
  root.addEventListener("pointermove", (e) => {
    const ver = e.target.closest?.(".mk-ver:not(.mk-solo)");
    if (!ver) return;
    const hist = JSON.parse(ver.dataset.hist || "[]");
    const i = nearestTick(ver, e.clientX);
    const tip = ver.querySelector(".mk-tip");
    const h = hist[i];
    if (tip && h && !e.target.closest(".mk-card")) {
      const t = ver.querySelector(`i.mk-t[data-v="${CSS.escape(h[0])}"]`);
      tip.innerHTML = `<b>${esc(shortV(h[0]))}</b>${h[1] ? ` · ${esc(ago(h[1]))} ago` : ""}${h[2] ? " · yanked" : ""}`;
      tip.style.left = t ? t.style.left : "";
      tip.classList.add("is-on");
    }
    if (drag === ver) scrubTo(ver, i);
  });
  root.addEventListener("pointerout", (e) => { const v = e.target.closest?.(".mk-ver"); if (v && !v.contains(e.relatedTarget)) v.querySelector(".mk-tip")?.classList.remove("is-on"); });
  root.addEventListener("pointerdown", (e) => {
    const ver = e.target.closest?.(".mk-ver:not(.mk-solo)");
    if (!ver || e.target.closest(".mk-card")) return;
    drag = ver;
    scrubTo(ver, nearestTick(ver, e.clientX));
  });
  root.addEventListener("pointerup", () => { drag = null; });
  root.addEventListener("keydown", (e) => {
    if (e.key === "Alt") document.documentElement.classList.add("mk-alt");
    const ver = e.target.closest?.(".mk-ver");
    if (!ver) return;
    const hist = JSON.parse(ver.dataset.hist || "[]");
    const cur = ver.dataset.at ? +ver.dataset.at : hist.findIndex((h) => h[0] === ver.dataset.pin);
    if (e.key === "ArrowLeft") scrubTo(ver, Math.max(0, cur - 1));
    if (e.key === "ArrowRight") scrubTo(ver, Math.min(hist.length - 1, cur + 1));
    if (e.key === "Escape") scrubTo(ver, hist.findIndex((h) => h[0] === ver.dataset.pin));
  });
  root.addEventListener("keyup", (e) => { if (e.key === "Alt") document.documentElement.classList.remove("mk-alt"); });
  window.addEventListener("blur", () => document.documentElement.classList.remove("mk-alt"));
}

function chooseLicense(tab) {
  const card = tab.closest(".mk-card");
  card.querySelectorAll(".mk-tab").forEach((t) => t.classList.toggle("is-on", t === tab));
  card.querySelectorAll(".mk-cols").forEach((c) => c.classList.toggle("is-on", c.dataset.lic === tab.dataset.mkLic));
}

if (typeof document !== "undefined") install(document);

export const _internals = { parseV, cmpV, caretClass, licInfo, licenseFit, options, restWord, stoneSvg, ringSvg, SPRING };
