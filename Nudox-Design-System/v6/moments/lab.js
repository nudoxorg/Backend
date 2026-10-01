// The symbol lab. Four instruments for the symbol page, each drawn from real data on this machine
// (extract_shapes.py) in five languages:
//   the plate   one language-neutral shape: ports in, ports out, exits for nothing / failure / later,
//               threads for generics, knobs for options that change the shape. Every fact says how we know
//               it (in its type, in its docs, in its code, at call sites) and shows the line it came from.
//   the family  the siblings arranged by what differs between them; hover one and the plate becomes it.
//   the ways    every call site as a double word tree: what callers do before, what they do after.
//   the crowd   who implements it, at a scale where lists stop working.
import { lerp, clamp } from "./motion.js";

const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const fmt = (n) => n >= 1e4 ? (n / 1e3).toFixed(0) + "k" : n >= 1e3 ? (n / 1e3).toFixed(1).replace(/\.0$/, "") + "k" : String(n);
const J = new Map();
export const shapeData = (id) => J.get(id) || J.set(id, fetch(`data/shapes/${id}.json`, { cache: "reload" }).then((r) => r.json())).get(id);

const HOW = { typed: "in its type", said: "in its docs", found: "in its code", seen: "at call sites" };
const THC = ["var(--th1)", "var(--th2)", "var(--th3)", "var(--th4)"];
const LANG = { rust: "RUST", go: "GO", python: "PYTHON", typescript: "TS", javascript: "JS" };

function ev(e, k = "") {
  if (!e) return "";
  return `<span class="lb-ev">${k ? `<span class="k">${k}</span>` : ""}<span class="f">${esc(e.f)}:${e.l}</span>${esc(e.t)}</span>`;
}
function how(h, e, k) { return `<span class="lb-how ${h}"><i></i>${HOW[h] || h}${ev(e, k)}</span>`; }

// ---------------------------------------------------------------- views: the words each language's facts are set in.
// {X} is a type parameter's pill; the thread runs through every pill of the same name.
const VIEW = {
  "rs-serde_json-from_str": { ins: { s: "text" }, out: "{T}", dial: "T" },
  "go-slices-binarysearch": { ins: { x: "a sorted list of {E}", target: "one {E}" }, out: "a position" },
  "go-yaml-unmarshal": { ins: { in: "bytes", out: "your value, by pointer" } },
  "ts-effect-map": { ins: { self: "an effect of {A} · fails {E} · needs {R}", f: "fn:{A}:{B}" }, out: "an effect of {B} · fails {E} · needs {R}" },
  "ts-zod-parse": { out: "{output<this>} what the schema describes" },
  "py-re-match": { ins: { string: "text or bytes {AnyStr}" }, out: "a Match of {AnyStr}" },
};

// Siblings and knobs, as changes to the shape. Each is read from the sibling's own declaration (the family
// cells carry its evidence line); here only the difference is written down.
const PATCH = {
  "rs-serde_json-from_str": {
    sib: {
      from_slice: (S) => { S.ins = [{ ...S.ins[0], n: "v", t: "&'a [u8]", w: "bytes", key: "in0" }]; },
      from_reader: (S) => { S.ins = [{ ...S.ins[0], n: "rdr", t: "R: io::Read", w: "a reader (io::Read)", key: "in0" }]; S.seams.fail.kinds = S.seams.fail.kinds.map((k) => ({ ...k, dim: false })); S.note = "Only from_reader can fail with <b>Io</b>: it reads while it parses."; },
      from_value: (S) => { S.ins = [{ ...S.ins[0], n: "value", t: "Value", w: "a Value (already parsed)", key: "in0" }]; S.seams.fail.kinds = S.seams.fail.kinds.filter((k) => k.n === "Data"); S.note = "From a Value there's no text left to be wrong: only <b>Data</b> can fail."; },
    },
    base: (S) => { S.seams.fail.kinds = S.seams.fail.kinds.map((k) => ({ ...k, dim: k.n === "Io" })); },
  },
  "ts-zod-parse": {
    sib: {
      safeParse: (S) => { delete S.seams.fail; S.out = { ...S.out, result: true, how: "typed" }; S.note = "safeParse never throws: the failure is <b>in its type</b> now, as { success: false, error: ZodError }."; },
      parseAsync: (S) => { S.out = { ...S.out, later: true }; S.seams.fail = { ...S.seams.fail, route: "rejected", ew: "rejects" }; S.seams.later = { how: "typed", w: "a Promise", ev: S.out.ev }; },
      safeParseAsync: (S) => { delete S.seams.fail; S.out = { ...S.out, later: true, result: true }; S.seams.later = { how: "typed", w: "a Promise", ev: S.out.ev }; },
    },
  },
  "js-which": {
    knob: {
      nothrow: (S, k) => { const f = S.seams.fail; delete S.seams.fail; S.seams.none = { how: "found", w: "null when it isn't on PATH", ev: k.ev, key: "fail" }; S.gone = f; },
      all: (S, k) => { S.out = { ...S.out, many: true, w: "every match: a list of paths", ev: k.ev }; },
    },
    sib: {
      "which.sync …nothrow": (S, P) => P.knob.nothrow(S, S.knobs.find((k) => k.n === "nothrow")),
      which: (S) => { S.out = { ...S.out, later: true, w: "a path (or a callback)" }; S.seams.fail = { ...S.seams.fail, route: "rejected", ew: "rejects" }; S.seams.later = { how: "found", w: "a Promise, or your callback", ev: S.seams.fail.ev }; S.note = "The async which has no nothrow: it always rejects."; },
    },
  },
  "py-subprocess-run": {
    base: (S, on) => {
      const f = S.seams.fail;
      f.kinds = f.kinds.map((k) => ({ ...k, dim: k.n === "CalledProcessError" && !on.check }));
      if (!on.check) { f.w = "FileNotFoundError, TimeoutExpired"; f.when = "the program is missing, or it runs past timeout"; }
      else { f.w = "CalledProcessError"; f.when = "a non-zero exit, now that check=True"; }
      S.out = { ...S.out, w: `a CompletedProcess${on.capture_output ? `, keeping stdout as ${on.text ? "text" : "bytes"}` : on.text ? " (text mode, nothing captured)" : ""}` };
    },
    knob: { check: () => {}, capture_output: () => {}, text: () => {} },
  },
  "py-re-match": {
    sib: {
      search: (S) => { S.where = "anywhere in the string"; },
      fullmatch: (S) => { S.where = "the whole string"; },
    },
    base: (S) => { S.where = S.where || "at the start"; },
  },
  "go-slices-binarysearch": {
    sib: {
      BinarySearchFunc: (S) => {
        S.ins = [S.ins[0], { n: "target", t: "T", w: "one {T}", how: "typed", key: "in1" }, { n: "cmp", t: "func(E, T) int", w: "fn:{E},{T}:order", how: "typed", key: "in2" }];
        S.threads = [{ ...S.threads[0], role: "through", needs: null, says: "anything: your function orders it" }, S.threads[1], { k: "T", role: "through", says: "the target can be another type", how: "typed", ev: S.threads[0].ev }];
      },
    },
  },
  "ts-effect-map": {
    sib: {
      mapError: (S) => { S.ins[1] = { ...S.ins[1], w: "fn:{E}:{E2}" }; S.out = { ...S.out, w: "an effect of {A} · fails {E2} · needs {R}" }; S.threads = [{ k: "A", role: "through", says: "its value passes through untouched" }, { k: "E", role: "turns", says: "its failure goes through your function" }, { k: "E2", role: "turns", says: "what your function gives becomes its failure" }, { k: "R", role: "through", says: "what it needs passes through" }]; },
      mapBoth: (S) => { S.ins[1] = { ...S.ins[1], n: "options", w: "fn:{A}:{B} and fn:{E}:{E2}" }; S.out = { ...S.out, w: "an effect of {B} · fails {E2} · needs {R}" }; S.threads = [{ k: "A", role: "turns", says: "value through one function" }, { k: "B", role: "turns", says: "becomes its value" }, { k: "E", role: "turns", says: "failure through the other" }, { k: "E2", role: "turns", says: "becomes its failure" }, { k: "R", role: "through", says: "passes through" }]; },
      flatMap: (S) => { S.ins[1] = { ...S.ins[1], w: "fn:{A}:effect of {B}, fails {E2}, needs {R2}" }; S.out = { ...S.out, w: "an effect of {B} · fails {E} or {E2} · needs {R} and {R2}" }; S.threads = [{ k: "A", role: "turns", says: "its value goes into your function" }, { k: "B", role: "turns", says: "your effect's value becomes its value" }, { k: "E", role: "through", says: "both failures can happen" }, { k: "E2", role: "through", says: "your effect's failure joins its own" }, { k: "R", role: "through", says: "both needs add up" }, { k: "R2", role: "through", says: "" }]; },
    },
  },
};

// ---------------------------------------------------------------- the model: facts + view + patches -> what to draw
function build(D, st) {
  const S = JSON.parse(JSON.stringify(D.shape));
  const V = VIEW[D.id] || {};
  S.ins = S.ins.map((j, i) => ({ ...j, key: "in" + i, w: (V.ins && V.ins[j.n]) || j.w }));
  S.out = { ...S.out, key: "out", w: V.out || S.out.w };
  S.threads = (S.threads || []).map((t) => ({ ...t }));
  const P = PATCH[D.id] || {};
  const on = st.knobs || {};
  if (P.base) P.base(S, on);
  for (const k of S.knobs || []) if (on[k.n] && P.knob && P.knob[k.n]) P.knob[k.n](S, k);
  if (st.sib && P.sib && P.sib[st.sib]) P.sib[st.sib](S, P);
  // thread colours follow first appearance, so a thread keeps its colour across a morph
  const seen = [];
  const collect = (w) => String(w || "").replace(/\{([\w.<>]+)\}/g, (_, k) => { if (!seen.includes(k)) seen.push(k); return ""; });
  S.ins.forEach((j) => collect(j.w)); collect(S.out.w);
  for (const t of S.threads) if (!seen.includes(t.k)) seen.push(t.k);
  S.tc = Object.fromEntries(seen.map((k, i) => [k, THC[i % THC.length]]));
  S.name = st.sib || D.name;
  return S;
}

function words(w, S) {
  const pill = (k) => `<span class="tp" data-t="${esc(k)}" style="--tc:${S.tc[k] || "var(--th1)"}">${esc(k.replace(/^output<this>$/, "out"))}</span>`;
  const fn = String(w).match(/^fn:(.+?):(.+?)(?: and fn:(.+?):(.+))?$/);
  if (fn) {
    const one = (a, b) => `<span class="fnp">${words(a, S)}<span class="ar">→</span>${words(b, S)}</span>`;
    return fn[3] ? `${one(fn[1], fn[2])}<span class="dflt">and</span>${one(fn[3], fn[4])}` : one(fn[1], fn[2]);
  }
  return esc(w).replace(/\{([\w.&;]+)\}/g, (_, k) => pill(k.replace(/&lt;/g, "<").replace(/&gt;/g, ">")));
}

function jack(j, S, side) {
  const cls = ["lb-jk", side, j.how, j.opt ? "opt" : "", j.rest ? "rest" : "", j.kw ? "kw" : "", j.fillsIn ? "fills" : "", j.many ? "many" : "", j.later ? "later" : "", j.result ? "result" : ""].filter(Boolean).join(" ");
  const full = j.t ? `<span class="full">${esc(j.t)}</span>` : "";
  const tip = ev(j.ev, `${HOW[j.how] || ""}${j.t ? ` · ${esc(j.t)}` : ""}`);
  if (side === "in") return `<div class="${cls}" data-key="${j.key}"><i class="port"></i><b class="jn">${esc(j.n)}</b><span class="jw">${words(j.w, S)}</span>${j.dflt ? `<span class="dflt">= ${esc(j.dflt)}</span>` : ""}${j.fillsIn ? `<span class="tag">it fills this</span>` : ""}${full}${tip}</div>`;
  let w = words(j.w, S);
  if (j.result) w = `<span class="ok">${w}</span><span class="or">or</span><span class="er">a ZodError</span>`;
  return `<div class="${cls}" data-key="${j.key}">${full}<span class="jw">${w}</span><i class="port"></i>${tip}</div>`;
}

const EXIT_ICON = {
  fail: `<svg class="mouth" viewBox="0 0 12 12"><path d="M1 1h10v6l-4 4H1z" fill="var(--coral)"/><path d="M4 4l4 4M8 4 4 8" stroke="var(--g0)" stroke-width="1.6"/></svg>`,
  none: `<svg class="mouth" viewBox="0 0 12 12"><circle cx="6" cy="6" r="4.4" fill="none" stroke="var(--ex-none)" stroke-width="1.6"/></svg>`,
  later: `<svg class="mouth" viewBox="0 0 12 12"><circle cx="6" cy="6" r="4.6" fill="none" stroke="var(--peri)" stroke-width="1.5" stroke-dasharray="2.2 1.6"/><path d="M6 3.4V6l1.8 1.2" stroke="var(--peri)" stroke-width="1.4" fill="none"/></svg>`,
  many: `<svg class="mouth" viewBox="0 0 12 12"><path d="M1 4h7v7H1zM4 1h7v7" fill="none" stroke="var(--th4)" stroke-width="1.4"/></svg>`,
};
const FAIL_WORD = { returned: "fails", thrown: "throws", raised: "raises", rejected: "rejects", carried: "fails inside", reports: "reports", panic: "panics" };

function exits(S, D) {
  const out = [];
  const sm = S.seams || {};
  const nSites = (D.sites || []).length;
  if (sm.none) out.push(`<div class="lb-ex none ${sm.none.how}" data-key="${sm.none.key || "none"}" data-x="none">${EXIT_ICON.none}<span class="ew">maybe nothing</span><span class="wh">${esc(sm.none.w)}</span>${how(sm.none.how, sm.none.ev)}${nSites && D.id === "py-re-match" ? `<span class="sites">· how ${fmt(nSites)} callers check it ›</span>` : ""}</div>`);
  if (sm.fail) {
    const f = sm.fail;
    const carried = f.route === "carried";
    const kinds = (f.kinds || []).map((k) => `<span style="${k.dim ? "opacity:.35;text-decoration:line-through" : ""}">${esc(k.n)}${ev(k.ev, esc(k.w))}</span>`).join("");
    out.push(`<div class="lb-ex fail ${carried ? "carried" : ""} ${f.how}" data-key="fail" data-x="fail">${EXIT_ICON.fail}<span class="ew">${esc(f.ew || FAIL_WORD[f.route] || "fails")}</span><b>${esc(f.w)}</b>${f.when ? `<span class="wh">when ${esc(f.when)}</span>` : ""}${kinds ? `<span class="kinds">${kinds}</span>` : ""}${how(f.how, f.ev, f.note ? esc(f.note) : "")}${f.said ? how("said", f.said) : ""}${nSites >= 5 && !carried && D.id !== "py-re-match" ? `<span class="sites">· what ${fmt(nSites)} callers do ›</span>` : ""}</div>`);
  }
  if (sm.later) out.push(`<div class="lb-ex later ${sm.later.how}" data-key="later" data-x="later">${EXIT_ICON.later}<span class="ew">later</span><span class="wh">${esc(sm.later.w)}</span>${how(sm.later.how, sm.later.ev)}</div>`);
  if (S.out.many) out.push(`<div class="lb-ex many found" data-key="many">${EXIT_ICON.many}<span class="ew">many</span><span class="wh">a list, not one</span>${how("found", S.out.ev)}</div>`);
  return out.join("");
}

const DIAL = `<svg width="18" height="18" viewBox="0 0 18 18"><circle cx="9" cy="9" r="7" fill="none" stroke="var(--th1)" stroke-width="1.5"/><path d="M9 9 9 3.4" stroke="var(--th1)" stroke-width="1.8" class="needle"/><circle cx="9" cy="9" r="1.6" fill="var(--th1)"/></svg>`;

export function fillsOf(D) {
  const by = new Map();
  for (const s of D.sites || []) {
    if (!s.fill || /^(inferred|a variable|a pointer variable|an expression)$/.test(s.fill)) continue;
    const k = s.fill.replace(/^serde_json::/, "");
    const e = by.get(k) || by.set(k, { n: k, sites: 0, crates: new Set(), y: 0 }).get(k);
    e.sites++; e.crates.add(s.p); if (s.y) e.y++;
  }
  return [...by.values()].sort((a, b) => b.crates.size - a.crates.size || b.sites - a.sites);
}

function plateHtml(D, S, st, { head = true } = {}) {
  const V = VIEW[D.id] || {};
  const knobs = (S.knobs || []).map((k) => {
    const ct = k.n && D.sites && D.sites.length && D.sites[0].kw ? D.sites.filter((s) => (s.kw || []).includes(k.n)).length : null;
    return `<button class="lb-knob ${st.knobs && st.knobs[k.n] ? "on" : ""}" data-knob="${esc(k.n)}"><span class="sw"></span>${esc(k.n)}<em>${esc(k.w)}</em>${ct != null ? `<span class="ct">${ct} of ${D.sites.length} calls</span>` : ""}</button>`;
  }).join("");
  const recv = S.recv ? `<div class="lb-recv"><div class="lb-jk in ${S.recv.how}" data-key="recv"><i class="port"></i><span class="jw">on ${words(S.recv.w, S)}</span>${ev(S.recv.ev, HOW[S.recv.how])}</div></div>` : "";
  let out = jack(S.out, S, "out");
  if (V.dial && !st.sib) {
    const F = fillsOf(D);
    out = out.replace(`<span class="jw">`, `<span class="jw"><span class="lb-dial" data-fills='${esc(JSON.stringify(F.slice(0, 14).map((f) => [f.n, f.crates.size, f.sites])))}'>${DIAL}<span class="spin">you choose<i>${F.length} types seen</i></span></span>`);
  }
  const untyped = S.untyped ? `<span class="untyped" title="${esc(S.untyped)}">no types: read from ${D.lang === "python" ? "its docs and code" : "its code"}</span>` : "";
  return `${head ? `<div class="lb-ph"><span class="lang ${D.lang}">${LANG[D.lang]}</span><span class="pk">${esc(D.pkg)}${D.module && D.module !== D.pkg ? ` · ${esc(D.module)}` : ""}</span><b class="nm">${esc(S.name)}</b><span class="kd">${esc(D.kind)}</span><span class="doc">${esc(D.doc)}</span>${untyped}</div>` : ""}
    ${knobs ? `<div class="lb-knobs">${knobs}</div>` : ""}
    <div class="lb-row">${recv ? `<div style="grid-column:1/-1;display:grid;grid-template-columns:subgrid">${recv}</div>` : ""}
      <div class="lb-ins">${S.ins.map((j) => jack(j, S, "in")).join("")}</div>
      <div class="lb-core" data-key="core">${coreGlyph(S)}<span>${esc(S.name)}</span>${S.where ? `<span class="where">${esc(S.where)}</span>` : ""}</div>
      <div class="lb-outs" style="grid-column:5">${out}</div>
    </div>
    <div class="lb-exits">${exits(S, D)}</div>
    ${S.note ? `<div class="lb-note">${S.note}</div>` : ""}
    <svg class="lb-ink"></svg>`;
}

function coreGlyph(S) {
  const n = Math.min(S.ins.length, 5), fails = !!(S.seams && S.seams.fail), none = !!(S.seams && S.seams.none);
  let s = `<path d="M7 5H19V15L16 18H4V8Z" fill="currentColor" fill-opacity=".22" stroke="currentColor" stroke-width="1.4"/>`;
  for (let i = 0; i < n; i++) { const y = n === 1 ? 11.5 : 7 + (9 * i) / (n - 1); s += `<path d="M0 ${y.toFixed(1)}H4" stroke="currentColor" stroke-width="1.3"/>`; }
  s += `<path d="M19 11.5H23" stroke="currentColor" stroke-width="1.3"/>`;
  if (fails) s += `<path d="M10 18V22H14" stroke="var(--coral)" stroke-width="1.4" fill="none"/>`;
  if (none) s += `<circle cx="16" cy="21" r="2" fill="none" stroke="var(--ex-none)" stroke-width="1.2"/>`;
  return `<svg width="24" height="24" viewBox="0 0 24 24" style="color:var(--k-ca);flex:none">${s}</svg>`;
}

// ---------------------------------------------------------------- the ink: wires, threads and their role tags
const ROLE = {
  choose: { w: "you choose", g: `<svg width="12" height="12" viewBox="0 0 12 12"><circle cx="6" cy="6" r="4.5" fill="none" stroke="currentColor" stroke-width="1.4"/><path d="M6 6V2.6" stroke="currentColor" stroke-width="1.6"/></svg>` },
  needs: { w: "must be", g: `<svg width="12" height="12" viewBox="0 0 12 12"><path d="M1.5 1.5h9v9h-9zM4 1.5v3h4v-3" fill="none" stroke="currentColor" stroke-width="1.4"/></svg>` },
  through: { w: "passes through", g: `<svg width="14" height="12" viewBox="0 0 14 12"><path d="M1 4h11M1 8h11M9.5 1.8 12 4 9.5 6.2" fill="none" stroke="currentColor" stroke-width="1.3"/></svg>` },
  turns: { w: "turns into", g: `<svg width="14" height="12" viewBox="0 0 14 12"><path d="M1 9C5 9 5 3 12 3M9.5 1 12 3 9.5 5" fill="none" stroke="currentColor" stroke-width="1.4"/></svg>` },
  sealed: { w: "the schema decides", g: `<svg width="12" height="12" viewBox="0 0 12 12"><path d="M1.5 4.5h9v6h-9zM3.5 4.5V3a2.5 2.5 0 0 1 5 0v1.5" fill="none" stroke="currentColor" stroke-width="1.4"/></svg>` },
};

function ink(el, S) {
  const svg = el.querySelector("svg.lb-ink");
  el.querySelectorAll(".lb-role").forEach((r) => r.remove());
  const o = el.getBoundingClientRect();
  const R = (e) => { const r = e.getBoundingClientRect(); return { x: r.left - o.left, y: r.top - o.top, w: r.width, h: r.height, r: r.right - o.left, b: r.bottom - o.top, cx: r.left - o.left + r.width / 2, cy: r.top - o.top + r.height / 2 }; };
  const core = el.querySelector(".lb-core"); if (!core) return;
  const C = R(core);
  let p = "";
  // wires: every input converges on the core's left, the output leaves its right
  const ins = [...el.querySelectorAll(".lb-ins .lb-jk")];
  ins.forEach((j, i) => {
    const a = R(j), port = R(j.querySelector(".port"));
    const x0 = a.r + 8, y0 = a.cy, x1 = C.x, y1 = C.cy + (ins.length > 1 ? (i - (ins.length - 1) / 2) * 5 : 0);
    const mx = (x0 + x1) / 2;
    p += `<path d="M${x0} ${y0}C${mx} ${y0} ${mx} ${y1} ${x1} ${y1}" stroke="var(--wire)" stroke-width="1.3" fill="none"/>`;
  });
  el.querySelectorAll(".lb-outs .lb-jk").forEach((j) => {
    const a = R(j), port = R(j.querySelector(".port"));
    const x0 = C.r, y0 = C.cy, x1 = a.x - 8, y1 = a.cy, mx = (x0 + x1) / 2;
    p += `<path d="M${x0} ${y0}C${mx} ${y0} ${mx} ${y1} ${x1} ${y1}" stroke="var(--wire)" stroke-width="1.3" fill="none"/><path d="M${x1 - 6} ${y1 - 4} ${x1} ${y1} ${x1 - 6} ${y1 + 4}" stroke="var(--wire)" stroke-width="1.3" fill="none"/>`;
  });
  const rv = el.querySelector(".lb-recv .lb-jk");
  if (rv) { const a = R(rv); p += `<path d="M${a.cx} ${a.b + 2}V${C.y}" stroke="var(--wire)" stroke-width="1.3"/>`; }
  // threads: each type parameter's pills joined along a lane under the row; its role tag sits on the lane
  const row = R(el.querySelector(".lb-row"));
  const byT = new Map();
  el.querySelectorAll(".lb-row .tp").forEach((t) => { const k = t.dataset.t; (byT.get(k) || byT.set(k, []).get(k)).push(R(t)); });
  let lane = 0;
  const lanes = [];
  const mid = C.cx;
  const shared = new Map();   // through-threads over the same span ride one lane, side by side
  for (const t of S.threads || []) {
    const pills = (byT.get(t.k) || []).sort((a, b) => a.cx - b.cx);
    const col = S.tc[t.k] || "var(--th1)";
    if (!pills.length) continue;
    if (t.role === "choose" && el.querySelector(".lb-dial")) { if (t.needs) lanes.push({ t: { ...t, role: "needs" }, x: pills[0].cx, y: pills[0].b + 14, col, anchor: "right" }); continue; }
    const spans = pills[0].cx < mid && pills[pills.length - 1].cx > mid;
    const sk = t.role === "through" && spans ? "through" : null;
    let L = sk && shared.get(sk);
    const slot = L ? L.n++ : 0;
    if (!L) { L = { y: row.b + 12 + lane * 16, n: 1, ts: [] }; lane++; if (sk) shared.set(sk, L); }
    const y = L.y + slot * 3.5;
    let x0 = pills[0].cx, x1 = pills[pills.length - 1].cx;
    if (x1 - x0 < 60) { if (x0 > mid) x0 = x1 - 60; else x1 = x0 + 60; }
    for (const q of pills) p += `<path d="M${q.cx} ${q.b + 1}V${y}" stroke="${col}" stroke-width="1.5" stroke-opacity=".75" fill="none"/>`;
    p += `<path d="M${x0} ${y}H${x1}" stroke="${col}" stroke-width="1.5" stroke-opacity=".75" fill="none"/>`;
    if (L.ts.length) { L.ts.push(t); continue; }
    L.ts.push(t);
    lanes.push({ t, x: (x0 + x1) / 2, y: L.y, col, L });
  }
  svg.innerHTML = p;
  // role tags on their lanes
  for (const L of lanes) {
    const t = L.t, r = ROLE[t.role] || ROLE.through;
    const tag = document.createElement("span");
    tag.className = "lb-role"; tag.style.setProperty("--tc", L.col);
    const many = L.L && L.L.ts.length > 1 ? L.L.ts : null;
    const word = many ? `${many.map((x) => `<b style="color:${S.tc[x.k]}">${esc(x.k)}</b>`).join(" and ")} pass through untouched`
      : t.role === "needs" ? `must be <b>${esc(t.needs)}</b>` : t.role === "choose" ? `you choose <b>${esc(t.k)}</b>${t.needs ? ` · must be <b>${esc(t.needs)}</b>` : ""}` : t.role === "turns" && t.into ? `<b>${esc(t.k)}</b> turns into <b>${esc(t.into)}</b>` : t.role === "sealed" ? `the schema decides` : `<b>${esc(t.k)}</b> ${r.w}`;
    const says = many ? many.map((x) => `${x.k}: ${x.says || ""}`).join("  ·  ") : t.says || "";
    tag.innerHTML = `<span style="color:${L.col};display:inline-flex">${r.g}</span>${word}${ev(t.ev, esc(says))}`;
    tag.style.left = L.x + "px"; tag.style.top = L.y + "px";
    if (L.anchor === "right") { tag.style.transform = "translate(-100%, -50%)"; tag.style.left = (L.x + 10) + "px"; }
    el.appendChild(tag);
    if (L.anchor === "right") lane = Math.max(lane, 1);
  }
  // exits hang under the core, and leave room for the lanes
  const ex = el.querySelector(".lb-exits");
  ex.style.marginTop = (lane ? 12 + lane * 16 + 12 : 16) + "px";
  ex.style.paddingLeft = "0px";
  const need = Math.max(0, ...[...ex.querySelectorAll(".lb-ex")].map((e) => e.getBoundingClientRect().width));
  ex.style.paddingLeft = Math.max(0, Math.min(C.x - 18, ex.clientWidth - need - 4)) + "px";
  const exs = [...ex.querySelectorAll(".lb-ex")];
  if (exs.length) {
    const E0 = R(exs[0]);
    const railY = E0.y - 8;
    let q = `<path d="M${C.x + 14} ${C.b}V${railY}H${R(exs[exs.length - 1]).x + 14}" stroke="var(--wire)" stroke-width="1.3" fill="none"/>`;
    for (const e of exs) { const a = R(e); const c = getComputedStyle(e).getPropertyValue("--exc").trim() || "var(--wire)"; q += `<path d="M${a.x + 14} ${railY}V${a.y}" stroke="${c}" stroke-width="1.6"/>`; }
    svg.insertAdjacentHTML("beforeend", q);
  }
}

// ---------------------------------------------------------------- the plate element, with its knobs, dial, family morphs and exits
export async function plate(id, { head = true, family = true, bowtie = null, knobs = {}, sib = null } = {}) {
  const D = await shapeData(id);
  const wrap = document.createElement("div");
  wrap.className = family && D.family ? "lb-duo" : "";
  const el = document.createElement("section");
  el.className = "lb-plate"; el.dataset.id = id;
  wrap.appendChild(el);
  const st = { knobs: { ...knobs }, sib, pinned: sib, bt: bowtie };
  const draw = (animate = false) => {
    const before = animate ? snap(el) : null;
    const S = build(D, st);
    el.innerHTML = plateHtml(D, S, st, { head });
    if (st.bt) el.appendChild(bowTie(D, S, st.bt));
    requestAnimationFrame(() => { ink(el, S); if (before) flip(el, before); });
    el._S = S;
  };
  el.addEventListener("click", (e) => {
    const k = e.target.closest(".lb-knob");
    if (k) { st.knobs[k.dataset.knob] = !st.knobs[k.dataset.knob]; draw(true); const f = wrap.querySelector(".lb-fam"); if (f) f.parentElement.innerHTML = familyHtml(D, st); return; }
    const x = e.target.closest(".lb-ex[data-x]");
    if (x && (D.sites || []).length && x.dataset.x !== "later") { st.bt = st.bt === x.dataset.x ? null : x.dataset.x; draw(false); return; }
    const c = e.target.closest(".lb-chip[data-ctx]");
    if (c) { st.ctx = c.dataset.ctx; draw(false); }
  });
  // the dial spins through the types callers chose, while the pointer is on it
  let spinT = null;
  el.addEventListener("mouseover", (e) => {
    const d = e.target.closest(".lb-dial"); if (!d || spinT) return;
    const F = JSON.parse(d.dataset.fills || "[]"); let i = 0;
    const spin = d.querySelector(".spin"), needle = d.querySelector(".needle");
    const step = () => { const f = F[i % F.length]; spin.innerHTML = `${esc(f[0])}<i>${f[1]} crate${f[1] > 1 ? "s" : ""} · ${f[2]} call${f[2] > 1 ? "s" : ""}</i>`; needle.setAttribute("transform", `rotate(${(i * 360) / F.length} 9 9)`); i++; };
    step(); spinT = setInterval(step, 650);
  });
  el.addEventListener("mouseout", (e) => { const d = e.target.closest(".lb-dial"); if (d && !d.contains(e.relatedTarget)) { clearInterval(spinT); spinT = null; draw(false); } });
  if (family && D.family) {
    const fam = document.createElement("div");
    fam.innerHTML = familyHtml(D, st);
    wrap.appendChild(fam);
    fam.addEventListener("mouseover", (e) => { const c = e.target.closest(".lb-cell[data-n]"); if (!c) return; const n = c.dataset.n === D.name ? null : c.dataset.n; if (n === st.sib) return; st.sib = n; draw(true); });
    fam.addEventListener("mouseleave", () => { if (st.sib !== st.pinned) { st.sib = st.pinned; draw(true); } });
    fam.addEventListener("click", (e) => { const c = e.target.closest(".lb-cell[data-n]"); if (!c) return; st.pinned = c.dataset.n === D.name ? null : c.dataset.n; st.sib = st.pinned; fam.innerHTML = familyHtml(D, st); draw(false); });
  }
  draw(false);
  if (window.ResizeObserver) new ResizeObserver(() => el._S && ink(el, el._S)).observe(el);
  wrap._st = st; wrap._draw = draw;
  return wrap;
}

function snap(el) { const m = new Map(); el.querySelectorAll("[data-key]").forEach((e) => m.set(e.dataset.key, e.getBoundingClientRect())); return m; }
function flip(el, before) {
  const core = el.querySelector(".lb-core"); const c = core ? core.getBoundingClientRect() : null;
  el.querySelectorAll("[data-key]").forEach((e) => {
    const a = before.get(e.dataset.key), b = e.getBoundingClientRect();
    if (a) { const dx = a.left - b.left, dy = a.top - b.top; if (Math.abs(dx) + Math.abs(dy) > 0.5 || Math.abs(a.width - b.width) > 1) e.animate([{ transform: `translate(${dx}px,${dy}px)`, opacity: 0.55 }, { transform: "none", opacity: 1 }], { duration: 300, easing: "cubic-bezier(.2,.9,.3,1)" }); }
    else if (c) e.animate([{ transform: `translate(${c.left - b.left}px,${c.top - b.top}px) scale(.6)`, opacity: 0 }, { transform: "none", opacity: 1 }], { duration: 320, easing: "cubic-bezier(.2,.9,.3,1)" });
  });
  const svg = el.querySelector("svg.lb-ink"); svg.animate([{ opacity: 0 }, { opacity: 0 }, { opacity: 1 }], { duration: 360 });
  el.querySelectorAll(".lb-role").forEach((r) => r.animate([{ opacity: 0 }, { opacity: 0 }, { opacity: 1 }], { duration: 380 }));
}

function familyHtml(D, st) {
  const F = D.family; const cols = F.axes[0], rows = F.axes[1];
  const cur = st.pinned || (st.knobs && st.knobs.nothrow && D.id === "js-which" ? "which.sync …nothrow" : D.name);
  const at = new Map(F.cells.map((c) => [c.at.join(","), c]));
  const glyphs = (n) => {
    const S = build(D, { knobs: {}, sib: n === D.name ? null : n });
    const g = [];
    if (S.seams.fail) g.push(`<i class="fail"></i><em>${esc(S.seams.fail.ew || FAIL_WORD[S.seams.fail.route] || "fails")}</em>`);
    if (S.out.result) g.push(`<i class="result"></i><em>result</em>`);
    if (S.seams.none) g.push(`<i class="none"></i><em>null</em>`);
    if (S.seams.later) g.push(`<i class="later"></i><em>later</em>`);
    if (S.out.many) g.push(`<i class="many"></i>`);
    return g.join("");
  };
  let h = `<div class="fam-h">Its family · <b>${esc(F.says)}</b></div><div class="lb-fam" style="grid-template-columns:${rows.length > 1 ? "max-content " : ""}repeat(${cols.length}, minmax(0,1fr))">`;
  if (rows.length > 1) h += `<span></span>`;
  h += cols.map((c) => `<span class="ax">${esc(c)}</span>`).join("");
  rows.forEach((r, ri) => {
    if (rows.length > 1) h += `<span class="ax r">${esc(r)}</span>`;
    cols.forEach((_, ci) => {
      const c = at.get(`${ci},${ri}`);
      h += c ? `<div class="lb-cell ${c.n === cur ? "cur" : ""}" data-n="${esc(c.n)}"><b>${esc(c.n).replace(/_/g, "_<wbr>").replace(/\./g, ".<wbr>")}</b><span class="gl">${glyphs(c.n)}</span>${ev(c.ev)}</div>` : `<div class="lb-cell empty"><span>no such form</span></div>`;
    });
  });
  return h + `</div>`;
}

// ---------------------------------------------------------------- what callers do: the words and colours of a way
export const WAYS = {
  propagate: ["passes it up", "?"], convert: ["converts it", ".map_err"], wrap: ["adds context", ".context"], handle: ["handles it", "match / if let"], crash: ["crashes", "unwrap / expect"],
  discard: ["drops the error", ".ok()"], fallback: ["falls back", "unwrap_or"], test: ["checks it in a test", ""], chain: ["chains on", ".map / .and_then"], pass: ["returns it", ""], other: ["something else", ""],
  unchecked: ["reads it unchecked", ".group() on None"], cond: ["tests it in a condition", "if re.match(…):"], checked: ["checks it later", "if m:"], skip: ["skips on error", "continue"],
  "raises (check=True)": ["raises on failure", "check=True"], "reads returncode": ["reads the exit code", ".returncode"], "ignores the exit": ["ignores the exit", ""],
  "expected first": ["expected first", "literal, value"], "two values": ["two values", ""], "reversed?": ["reversed?", "value, literal"],
  "data-first": ["data-first", "map(fx, f)"], "data-last": ["data-last", "pipe(fx, map(f))"],
};
const WAYC = { cond: "cond", crash: "crash", unchecked: "unchecked", propagate: "propagate", convert: "convert", wrap: "wrap", pass: "pass", handle: "handle", checked: "checked", discard: "discard", fallback: "fallback", test: "test", other: "other",
  "raises (check=True)": "propagate", "reads returncode": "handle", "ignores the exit": "discard", skip: "handle", "expected first": "handle", "two values": "test", "reversed?": "crash", "data-first": "pass", "data-last": "handle", chain: "pass" };
const wayCls = (w) => "way-" + (WAYC[w] || "other");

function bowTie(D, S, which) {
  const el = document.createElement("div");
  const sites = D.sites.filter((s) => s.x !== "test");   // what callers do in code; a test's unwrap is not a crash
  const src = sites.length ? sites : D.sites;
  const byW = new Map(); for (const s of src) byW.set(s.way, (byW.get(s.way) || 0) + 1);
  const rows = [...byW.entries()].sort((a, b) => b[1] - a[1]);
  const max = rows.length ? rows[0][1] : 1;
  const x = which === "none" ? S.seams.none : S.seams.fail;
  const kinds = which === "none" ? [{ n: "no match", w: "the pattern doesn't match at the start", ev: x.ev }] : (x.kinds || []).filter((k) => !k.dim);
  el.className = "lb-bt";
  el.innerHTML = `<div class="why">${kinds.map((k) => `<div class="k"><span>${esc(k.w)}</span><b>${esc(k.n)}</b>${ev(k.ev)}</div>`).join("")}</div>
    <div class="knot">${which === "none" ? "gives None" : esc(FAIL_WORD[x.route] || "fails")}<br><span style="color:var(--ink3);font:400 11px var(--ui);text-transform:none;letter-spacing:0">${fmt(src.length)} calls ${src === D.sites ? "" : "in code"}</span></div>
    <div class="do">${rows.map(([w, n]) => `<div class="k ${wayCls(w)}"><span class="pc">${Math.round((100 * n) / src.length)}%</span><span class="bar" style="width:${Math.max(3, (160 * n) / max)}px;background:color-mix(in srgb, var(--bc) 70%, transparent)"></span><b style="color:color-mix(in srgb, var(--bc) 80%, white)">${esc((WAYS[w] || [w])[0])}</b><span style="font:400 11px var(--mono);color:var(--ink3)">${esc((WAYS[w] || ["", ""])[1])} · ${n}</span></div>`).join("")}</div>
    <svg></svg>`;
  requestAnimationFrame(() => {
    const o = el.getBoundingClientRect(), svg = el.querySelector("svg"); if (!o.width) return;
    const knot = el.querySelector(".knot").getBoundingClientRect();
    const kx = knot.left - o.left, kr = knot.right - o.left, ky = knot.top - o.top + knot.height / 2;
    let p = "";
    el.querySelectorAll(".why .k").forEach((k) => { const r = k.getBoundingClientRect(); const y = r.top - o.top + r.height / 2, x = r.right - o.left + 6; p += `<path d="M${x} ${y}C${(x + kx) / 2} ${y} ${(x + kx) / 2} ${ky} ${kx - 4} ${ky}" stroke="var(--coral)" stroke-opacity=".5" stroke-width="1.3" fill="none"/>`; });
    el.querySelectorAll(".do .k").forEach((k) => { const r = k.getBoundingClientRect(); const y = r.top - o.top + r.height / 2, x = r.left - o.left - 6; const c = getComputedStyle(k).getPropertyValue("--bc"); p += `<path d="M${kr + 4} ${ky}C${(x + kr) / 2} ${ky} ${(x + kr) / 2} ${y} ${x} ${y}" stroke="${c}" stroke-opacity=".6" stroke-width="1.3" fill="none"/>`; });
    svg.innerHTML = p;
  });
  return el;
}

// ---------------------------------------------------------------- the ways: a double word tree and its concordance
export async function ways(id, { title = true, rows = 13, initial = null, ctx0 = null } = {}) {
  const D = await shapeData(id);
  const el = document.createElement("section");
  el.className = "lb-ways";
  const hasCtx = D.sites.some((s) => s.x === "test");
  const st = { ctx: ctx0 || (hasCtx && D.sites.filter((s) => s.x !== "test").length >= 60 && D.sites.length >= 12 ? "code" : "all"), yours: false, hot: initial, pin: initial };
  const kwName = D.name.startsWith(D.module) ? D.name : D.lang === "rust" ? `${D.module}::${D.name}` : `${D.module}.${D.name}`;
  const filtered = () => D.sites.filter((s) => (st.ctx === "all" || (st.ctx === "code" ? s.x !== "test" : s.x === st.ctx)) && (!st.yours || s.y));
  const draw = () => {
    const S = filtered();
    const crates = new Set(S.map((s) => s.p)).size;
    const nY = D.sites.filter((s) => s.y && (st.ctx === "all" || (st.ctx === "code" ? s.x !== "test" : s.x === st.ctx))).length;
    const cnt = (f) => D.sites.filter(f).length;
    const L = group(S, (s) => s.left[0]);
    const R1 = group(S, (s) => s.way);
    const R2 = new Map();
    for (const [w, ss] of R1) R2.set(w, group(ss, (s) => r2of(D, s)));
    const top = (m, n) => { const a = [...m.entries()].sort((x, y) => y[1].length - x[1].length); return a.length > n ? [...a.slice(0, n - 1), ["…", a.slice(n - 1).flatMap((x) => x[1]), a.length - n + 1]] : a; };
    const Ls = top(L, 9), R1s = top(R1, 8);
    const max = Math.max(1, ...Ls.map((x) => x[1].length), ...R1s.map((x) => x[1].length));
    const fs = (n) => Math.round(11 + 11 * Math.sqrt(n / max));
    const br = (side, k, ss, more) => {
      const ny = ss.filter((s) => s.y).length;
      const key = `${side}:${k}`;
      const sem = side === "r1";
      const label = k === "…" ? `${more} other ${side === "l" ? "contexts" : "ways"}` : sem ? (WAYS[k] || [k])[0] : k;
      return `<span class="lb-br ${side} ${sem ? wayCls(k) : ""} ${k === "…" ? "more" : ""} ${st.pin === key ? "pin" : ""}" data-b="${esc(key)}"><span class="w ${sem ? "sem" : ""}" style="font-size:${fs(ss.length)}px;--fs:${fs(ss.length)}px">${esc(label)}</span><span class="c">${ss.length}${ny && !st.yours ? ` <span class="y">${ny} yours</span>` : ""}</span></span>`;
    };
    let r2 = "";
    for (const [w, ss] of R1s) {
      if (w === "…") continue;
      const sub = top(R2.get(w) || new Map(), 3).filter((x) => x[0] && x[0] !== "…" && x[0] !== "·");
      r2 += `<div class="grp" data-w="${esc(w)}" style="display:flex;flex-direction:column;gap:0">${sub.map(([k, s2]) => `<span class="lb-br r2 ${wayCls(w)} ${st.pin === `r2:${w}:${k}` ? "pin" : ""}" data-b="r2:${esc(w)}:${esc(k)}"><span class="w" style="font-size:${Math.max(11, fs(s2.length) - 3)}px">${esc(k)}</span><span class="c">${s2.length}</span></span>`).join("")}</div>`;
    }
    const chips = [
      hasCtx ? `<span class="lb-chip ${st.ctx === "code" ? "on" : ""}" data-ctx="code">in code <i>${cnt((s) => s.x !== "test")}</i></span><span class="lb-chip ${st.ctx === "test" ? "on" : ""}" data-ctx="test">in tests <i>${cnt((s) => s.x === "test")}</i></span><span class="lb-chip ${st.ctx === "all" ? "on" : ""}" data-ctx="all">all <i>${D.sites.length}</i></span>` : "",
      nY ? `<span class="lb-chip y ${st.yours ? "on" : ""}" data-yours="1">yours <i>${nY}</i></span>` : "",
    ].join("");
    if (S.length < 12 && st.ctx === "all") {
      el.innerHTML = `<div class="lb-wh"><span class="cnt"><b>${S.length}</b> call site${S.length === 1 ? "" : "s"} on this machine: too few for a tree, so here they are</span></div><div class="lb-conc short"></div>`;
      requestAnimationFrame(() => conc(el, D, S, st, rows));
      return;
    }
    el.innerHTML = `<div class="lb-wh">${title ? `<span class="t"><span class="pk">${LANG[D.lang]} · ${esc(D.pkg)}</span>${esc(D.name)}</span>` : ""}<span class="cnt"><b>${fmt(S.length)}</b> call sites in <b>${crates}</b> ${D.lang === "rust" ? "crates" : D.lang === "go" ? "modules" : "packages"}</span><span class="lb-chips">${chips}</span></div>
      <div class="lb-tree"><svg class="ink"></svg><div class="cols">
        <div class="col l">${Ls.map(([k, ss, m]) => br("l", k, ss, m)).join("")}</div>
        <div class="mid"><span class="kw">${esc(kwName)}<span class="a">(…)</span></span></div>
        <div class="col r1">${R1s.map(([k, ss, m]) => br("r1", k, ss, m)).join("")}</div>
        <div class="col r2">${r2}</div>
      </div></div>
      ${fillsHtml(D, S)}
      <div class="lb-conc"></div>`;
    requestAnimationFrame(() => { treeInk(el, st); conc(el, D, S, st, rows); });
  };
  el.addEventListener("mouseover", (e) => { const b = e.target.closest(".lb-br"); if (!b) return; st.hot = b.dataset.b; mark(el, st); conc(el, D, filtered(), st, rows); });
  el.addEventListener("mouseleave", () => { st.hot = st.pin; mark(el, st); conc(el, D, filtered(), st, rows); });
  el.addEventListener("click", (e) => {
    const b = e.target.closest(".lb-br"); if (b) { st.pin = st.pin === b.dataset.b ? null : b.dataset.b; st.hot = st.pin; draw(); return; }
    const c = e.target.closest(".lb-chip[data-ctx]"); if (c) { st.ctx = c.dataset.ctx; st.pin = st.hot = null; draw(); return; }
    const y = e.target.closest(".lb-chip[data-yours]"); if (y) { st.yours = !st.yours; st.pin = st.hot = null; draw(); }
  });
  draw();
  if (window.ResizeObserver) new ResizeObserver(() => treeInk(el, st)).observe(el);
  return el;
}
function group(ss, f) { const m = new Map(); for (const s of ss) { const k = f(s); (m.get(k) || m.set(k, []).get(k)).push(s); } return m; }
function r2of(D, s) {
  if (D.lang === "rust") { const t = (s.right || []).find((x) => x.length > 1 || "?;{)".includes(x)) || "·"; return t === "{" ? "{ … }" : t; }
  if (D.id === "py-re-match") return (s.right[0] || "·").replace(/^\)$/, ")");
  if (D.id === "go-testify-assert-equal") return `${s.es} , ${s.as}`;
  if (D.id === "py-subprocess-run") return (s.kw || []).filter((k) => ["check", "capture_output", "text", "stdout", "cwd", "env", "timeout"].includes(k)).slice(0, 2).join(" ") || "no keywords";
  return s.right[0] || "·";
}
function inBranch(D, s, key) {
  if (!key) return true;
  const [side, a, b] = key.split(":");
  if (side === "l") return a === "…" ? true : s.left[0] === a;
  if (side === "r1") return a === "…" ? true : s.way === a;
  if (side === "r2") return s.way === a && r2of(D, s) === key.slice(4 + a.length);
  return true;
}
function mark(el, st) {
  const tree = el.querySelector(".lb-tree");
  tree.classList.toggle("focus", !!st.hot);
  const [side, a] = (st.hot || "").split(":");
  el.querySelectorAll(".lb-br").forEach((b) => {
    const k = b.dataset.b; let hot = k === st.hot;
    if (side === "r1" && k.startsWith(`r2:${a}:`)) hot = true;
    if (side === "r2" && k === `r1:${a}`) hot = true;
    b.classList.toggle("hot", hot);
  });
  treeInk(el, st);
}
function treeInk(el, st) {
  const tree = el.querySelector(".lb-tree"); if (!tree) return;
  const svg = tree.querySelector("svg.ink"), o = tree.getBoundingClientRect();
  const kw = tree.querySelector(".mid .kw"); if (!kw || !o.width) return;
  const K = kw.getBoundingClientRect();
  const kl = K.left - o.left, kr = K.right - o.left, ky = K.top - o.top + K.height / 2;
  const n = (b) => +(b.querySelector(".c").textContent.match(/\d+/) || [1])[0];
  const all = [...tree.querySelectorAll(".lb-br")];
  const max = Math.max(1, ...all.map(n));
  let p = "";
  const band = (x0, y0, x1, y1, w, col, op) => { const mx = (x0 + x1) / 2; p += `<path d="M${x0} ${y0}C${mx} ${y0} ${mx} ${y1} ${x1} ${y1}" stroke="${col}" stroke-opacity="${op}" stroke-width="${w.toFixed(1)}" fill="none" stroke-linecap="round"/>`; };
  const focus = !!st.hot;
  for (const b of tree.querySelectorAll(".col.l .lb-br")) { const r = b.getBoundingClientRect(); band(r.right - o.left + 4, r.top - o.top + r.height / 2, kl - 3, ky, 1 + 9 * (n(b) / max), "var(--ink3)", focus && !b.classList.contains("hot") ? 0.08 : 0.28); }
  const r1 = new Map();
  for (const b of tree.querySelectorAll(".col.r1 .lb-br")) { const r = b.getBoundingClientRect(); const c = getComputedStyle(b).getPropertyValue("--bc") || "var(--ink3)"; r1.set(b.dataset.b.slice(3), { r, c }); band(kr + 3, ky, r.left - o.left - 4, r.top - o.top + r.height / 2, 1 + 9 * (n(b) / max), c, focus && !b.classList.contains("hot") ? 0.1 : 0.42); }
  for (const b of tree.querySelectorAll(".col.r2 .lb-br")) {
    const w = b.dataset.b.split(":")[1]; const A = r1.get(w); if (!A) continue;
    const r = b.getBoundingClientRect(); if (!r.width) continue; const aw = A.r.right - o.left + 4;
    band(aw, A.r.top - o.top + A.r.height / 2, r.left - o.left - 4, r.top - o.top + r.height / 2, 1 + 6 * (n(b) / max), A.c, focus && !b.classList.contains("hot") ? 0.08 : 0.3);
  }
  svg.innerHTML = p;
}
function conc(el, D, S, st, rows) {
  const box = el.querySelector(".lb-conc"); if (!box) return;
  const key = st.hot || st.pin;
  const pick = S.filter((s) => inBranch(D, s, key));
  // yours first, then one line per package before repeats, so the concordance shows spread, not one crate's tests
  const seenP = new Set(), first = [], rest = [];
  for (const s of pick.slice().sort((a, b) => (b.y - a.y) || (a.x === "test") - (b.x === "test"))) (seenP.has(s.p) ? rest : (seenP.add(s.p), first)).push(s);
  const list = [...first, ...rest].slice(0, rows);
  const label = key ? branchWord(key) : "every way";
  requestAnimationFrame(() => box.classList.toggle("over", box.scrollHeight > box.clientHeight + 2));
  box.innerHTML = `<div class="hd"><span><b>${pick.length}</b> ${pick.length === 1 ? "line" : "lines"} · ${esc(label)}</span><span>aligned on the call; one per ${D.lang === "rust" ? "crate" : "package"} first</span></div>` +
    list.map((s) => {
      const call = s.call, i = call.indexOf("(");
      const name = i > 0 ? call.slice(0, i) : call, args = i > 0 ? call.slice(i) : "";
      const post = esc(s.post).replace(/^(\s*)([?]|\.[a-z_]+\([^)]*\)?|\.[a-z_]+)/, (m0, sp, h) => `${sp}<span class="h">${h}</span>`);
      return `<div class="lb-kw ${wayCls(s.way)}"><span class="src">${s.y ? `<span class="y">${esc(s.p)}</span>` : esc(s.p)} ${esc(s.f === "." ? s.p.split("/").pop() + (D.lang === "python" ? ".py" : "") : s.f.split("/").slice(-1)[0])}:${s.l}${s.x === "test" ? ` <span class="t">test</span>` : ""}</span><span class="pre"><span>${esc(s.pre)}</span></span><span class="call">${esc(name)}<span class="args">${esc(args.length > 48 ? args.slice(0, 46) + "…)" : args)}</span></span><span class="post">${post}</span></div>`;
    }).join("");
}
function branchWord(key) { const [side, a, ...b] = key.split(":"); return side === "l" ? `after “${a}”` : side === "r1" ? (WAYS[a] || [a])[0] : `${(WAYS[a] || [a])[0]} · ${b.join(":")}`; }
function fillsHtml(D, S) {
  const F = fillsOf({ sites: S });
  if (!F.length) return "";
  const word = D.lang === "go" ? "What callers point it at" : "What callers choose for T";
  return `<div class="lb-fills"><span class="lbl">${DIAL}${word}</span>${F.slice(0, 12).map((f) => `<span class="lb-fill">${esc(f.n)}<i>${f.crates.size} ${D.lang === "go" ? "mod" : "crate"}${f.crates.size > 1 ? "s" : ""}${f.sites > f.crates.size ? ` · ${f.sites}` : ""}</i></span>`).join("")}${F.length > 12 ? `<span>+${F.length - 12} more</span>` : ""}</div>`;
}

// ---------------------------------------------------------------- the crowd: implementors as a territory
export async function crowd(id) {
  const D = await shapeData(id);
  const el = document.createElement("section");
  el.className = "lb-crowd";
  const rows = D.crowd;
  const total = rows.reduce((s, r) => s + r[1] + r[2], 0), der = rows.reduce((s, r) => s + r[1], 0), yours = rows.filter((r) => r[3]);
  const ny = yours.reduce((s, r) => s + r[1] + r[2], 0);
  el.innerHTML = `<div class="lb-wh"><span class="t"><span class="pk">RUST · serde</span>Serialize</span><span class="cnt"><b>${fmt(total)}</b> types implement it in <b>${rows.length}</b> crates · <b>${Math.round((100 * der) / total)}%</b> by derive · yours <b style="color:var(--mint)">${fmt(ny)}</b> in ${yours.length}</span>
    <span class="lb-legend" style="margin-left:auto"><span><i class="dv"></i>#[derive(Serialize)]</span><span><i class="hd"></i>impl Serialize for … (by hand)</span><span><i class="y"></i>your crates</span></span></div>
    <div class="lb-map"></div><div class="lb-peek"></div>`;
  const map = el.querySelector(".lb-map"), peek = el.querySelector(".lb-peek");
  const lay = () => {
    const W = map.clientWidth, H = map.clientHeight; if (!W) return;
    // your crates first, as one region; then everyone else
    const mine = rows.filter((r) => r[3]).map((r) => ({ r, v: r[1] + r[2] })), rest = rows.filter((r) => !r[3]).map((r) => ({ r, v: r[1] + r[2] }));
    const sum = (a) => a.reduce((x, y) => x + y.v, 0);
    const wy = Math.max(120, Math.round((W * sum(mine)) / (sum(mine) + sum(rest))));
    const rects = [...(mine.length ? squarify(mine, 0, 0, wy, H) : []), ...squarify(rest, mine.length ? wy + 4 : 0, 0, W - (mine.length ? wy + 4 : 0), H)];
    map.style.setProperty("--wy", wy + "px");
    map.innerHTML = rects.map(({ it, x, y, w, h }, i) => {
      const [n, d, hd, y0] = it.r; const fd = d / (d + hd);
      const lab = w > 54 && h > 18 ? `<span class="l">${esc(n)}${h > 34 && w > 60 ? `<i>${fmt(d + hd)}${hd ? ` · ${hd} by hand` : ""}</i>` : ""}</span>` : "";
      return `<div class="cr ${y0 ? "y" : ""}" data-i="${i}" style="left:${x.toFixed(1)}px;top:${y.toFixed(1)}px;width:${Math.max(0, w).toFixed(1)}px;height:${Math.max(0, h).toFixed(1)}px"><span class="dv" style="width:${(fd * 100).toFixed(1)}%"></span><span class="hd" style="width:${((1 - fd) * 100).toFixed(1)}%"></span>${lab}</div>`;
    }).join("");
    map._rects = rects;
  };
  map.addEventListener("mousemove", (e) => {
    const c = e.target.closest(".cr"); if (!c) return;
    const R = map._rects[+c.dataset.i], [n, d, hd, y0, names] = R.it.r;
    map.querySelectorAll(".cr.hot").forEach((x) => x.classList.remove("hot")); c.classList.add("hot");
    peek.innerHTML = `<div class="t ${y0 ? "y" : ""}">${esc(n)}</div><div class="s">${d} derived · ${hd} by hand${y0 ? " · yours" : ""}</div><div class="n">${names.slice(0, 24).map((x) => `<span class="${x[1] === "hand" ? "h" : ""}">${esc(x[0])}</span>`).join("")}${names.length > 24 || d + hd > names.length ? `<span>… ${d + hd - Math.min(24, names.length)} more</span>` : ""}</div>`;
    const o = el.getBoundingClientRect(); let x = e.clientX - o.left + 14, y = e.clientY - o.top + 14;
    if (x + 370 > o.width) x = e.clientX - o.left - 374; peek.style.left = x + "px"; peek.style.top = y + "px"; peek.classList.add("on");
  });
  map.addEventListener("mouseleave", () => { peek.classList.remove("on"); map.querySelectorAll(".cr.hot").forEach((x) => x.classList.remove("hot")); });
  requestAnimationFrame(lay);
  if (window.ResizeObserver) new ResizeObserver(lay).observe(map);
  return el;
}
function squarify(items, x, y, w, h) {
  const total = items.reduce((s, i) => s + i.v, 0); const out = [];
  const scale = (w * h) / total; let rest = items.map((i) => ({ it: i, a: i.v * scale }));
  while (rest.length) {
    const short = Math.min(w, h); let row = [], best = Infinity;
    for (const r of rest) {
      const t = [...row, r]; const s = t.reduce((a, b) => a + b.a, 0);
      const worst = Math.max(...t.map((q) => Math.max((short * short * q.a) / (s * s), (s * s) / (short * short * q.a))));
      if (worst > best) break; best = worst; row = t;
    }
    const s = row.reduce((a, b) => a + b.a, 0);
    if (w >= h) { const cw = s / h; let cy = y; for (const r of row) { const ch = r.a / cw; out.push({ it: r.it.it ? r.it.it : r.it, x, y: cy, w: cw, h: ch }); cy += ch; } x += cw; w -= cw; }
    else { const chh = s / w; let cx = x; for (const r of row) { const cw = r.a / chh; out.push({ it: r.it.it ? r.it.it : r.it, x: cx, y, w: cw, h: chh }); cx += cw; } y += chh; h -= chh; }
    rest = rest.slice(row.length);
  }
  return out.map((o) => ({ ...o, it: o.it.r ? o.it : o.it }));
}

// ---------------------------------------------------------------- the lab page
export async function lab(root) {
  const Q = new URLSearchParams(location.search);
  const only = Q.get("only");
  root.innerHTML = `<h1>Symbol lab</h1>
    <p class="lede">Four instruments for the symbol page, on real code from this machine: the Rust registry and this repository, the Go module cache, the Python 3.14 stdlib with pip's vendored libraries, and two node_modules trees. <b>The plate</b> draws any function in any language the same way: ports in, ports out, exits below for nothing, failure and later. <b>Threads</b> tie generics together, and <b>knobs</b> are options that change the shape. Every fact says how we know it: <b>in its type</b> (solid), <b>in its docs</b> (dashed), <b>in its code</b> (dotted) or <b>at call sites</b> (beaded). Hover it for the line it came from. <b>The family</b> lays out the siblings by what differs; hover one and the plate becomes it. <b>The ways</b> shows every call site as a double word tree: what callers do before the call and after it. <b>The crowd</b> shows who implements it, at a scale where lists stop working.</p>`;
  const sec = (h, n, sub) => { const s = document.createElement("div"); s.innerHTML = `<h2>${h}<span class="n">${n}</span></h2><p class="sub">${sub}</p>`; root.appendChild(s); return s; };
  if (!only || only === "plate") {
    const s = sec("The plate", "one shape, five languages", "Rust says failure in the type; Go returns an <b>error</b>; TypeScript can't say it throws, so zod has a sibling that returns the failure instead; plain JavaScript says nothing, so every fact is read from its code; Python's stdlib has no type hints, so its docs carry the weight. Generics are threads: <b>you choose</b> (from_str's T), <b>must be</b> (BinarySearch's E), <b>passes through</b> (Effect's E and R), <b>turns into</b> (map's A into B). Click a knob and the shape changes; click an exit to see what callers do about it.");
    const force = (id) => { const o = {}; for (const v of Q.getAll("k")) { const [i, ks] = v.split(":"); if (i === id) o.knobs = Object.fromEntries(ks.split(",").map((k) => [k, true])); } for (const v of Q.getAll("s")) { const [i, n] = v.split(":"); if (i === id) o.sib = n; } for (const v of Q.getAll("b")) { const [i, n] = v.split(":"); if (i === id) o.bowtie = n === "none" ? null : n; } return o; };
    for (const [id, o0] of [["rs-serde_json-from_str", { bowtie: "fail" }], ["go-slices-binarysearch", {}], ["go-yaml-unmarshal", {}], ["ts-effect-map", {}], ["ts-zod-parse", {}], ["js-which", {}], ["py-re-match", { bowtie: "none" }], ["py-subprocess-run", {}]]) { if (Q.get("id") && Q.get("id") !== id) continue; s.appendChild(await plate(id, { ...o0, ...force(id) })); }
  }
  if (!only || only === "ways") {
    const s = sec("The ways", "every call site, as a double word tree", "For a name used hundreds of times, relation lists stop scaling. The call sites don't: they fold into a <b>word tree</b>. Left is the context callers call it from; right is what they do with the result, coloured by consequence (coral crashes, peri passes it on, mint handles it, amber drops it). Branch words grow with use. Hover a branch and the <b>concordance</b> below keeps only those lines, aligned on the call like a linguist's KWIC. <b>in code / in tests</b> matters: an unwrap in a test is fine, one in a library isn't.");
    for (const id of ["rs-serde_json-from_str", "py-re-match", "go-testify-assert-equal", "ts-effect-map", "py-subprocess-run"]) s.appendChild(await ways(id));
  }
  if (!only || only === "crowd") {
    const s = sec("The crowd", "8.5k implementors, one territory", "A trait implemented thousands of times can't be a list. Each crate is a region sized by how many of its types implement Serialize, cut into what it derives (teal) and what it writes by hand (hatched amber). Your crates are mint. Hover a region for its types.");
    s.appendChild(await crowd("rs-serde-serialize"));
  }
}

// ---------------------------------------------------------------- the evidence: every fact, and the line it was read from
export function evidence(D) {
  const S = D.shape, rows = [];
  const add = (fact, h, e) => { if (e && !rows.some((r) => r.e.f === e.f && r.e.l === e.l && r.fact === fact)) rows.push({ fact, h, e }); };
  for (const j of S.ins) add(`takes ${j.n}: ${j.w.replace(/\{|\}/g, "")}`, j.how, j.ev);
  if (S.recv) add(`is called on ${S.recv.w}`, S.recv.how, S.recv.ev);
  add(`gives ${String(S.out.w).replace(/\{|\}/g, "")}`, S.out.how, S.out.ev);
  for (const t of S.threads || []) add(`${t.k}: ${t.says}`, t.how, t.ev);
  const sm = S.seams || {};
  if (sm.none) add(`may give nothing: ${sm.none.w}`, sm.none.how, sm.none.ev);
  if (sm.fail) { add(`${FAIL_WORD[sm.fail.route] || "fails"} ${sm.fail.w}`, sm.fail.how, sm.fail.ev); for (const k of sm.fail.kinds || []) add(`${k.n}: ${k.w}`, k.how || sm.fail.how, k.ev); if (sm.fail.said) add("its docs on failing", "said", sm.fail.said); }
  if (sm.later) add(`later: ${sm.later.w}`, sm.later.how, sm.later.ev);
  for (const k of S.knobs || []) add(`knob ${k.n}: ${k.w}`, k.how || "found", k.ev);
  const byF = new Map(); for (const r of rows) (byF.get(r.e.f) || byF.set(r.e.f, []).get(r.e.f)).push(r);
  const el = document.createElement("section");
  el.className = "lb-evid";
  el.innerHTML = `<div class="lb-wh"><span class="t">Where each fact comes from</span><span class="cnt">${rows.length} facts from ${byF.size} file${byF.size > 1 ? "s" : ""} of ${esc(D.pkg)} ${esc(D.version)}${S.untyped ? ` · <b style="color:var(--amber)">${esc(S.untyped)}</b>` : ""}</span></div>` +
    [...byF.entries()].map(([f, rs]) => `<div class="fh">${esc(f)}</div>${rs.sort((a, b) => a.e.l - b.e.l).map((r) => `<div class="er"><span class="fa">${esc(r.fact)}</span>${how(r.h, null)}<span class="ln">${r.e.l}</span><code>${esc(r.e.t)}</code></div>`).join("")}`).join("");
  return el;
}
