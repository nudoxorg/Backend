// The symbol page, simple form. docs.rs's reading order, drawn:
//   header          kind, path, name, what it is
//   the call        a rail: each input is a port on it, the rail runs down into what it gives; fallibility is
//                   the shape of the rail's end: an arrow (gives), a hollow ring (or nothing), a coral block
//                   (or fails), a clock (later). Generic parameters are violet pills; hover one for its card.
//   docs            the doc comment, examples folded
//   if it fails     the error, what kinds it tells you, when (from its docs)
//   what it is      an enum is a fork (one of), a struct a bracket (holds); nesting loops back
//   what you can do methods grouped by what they do with it (makes, reads, changes, uses up), yours first
//   your workspace  every place your packages name it, filtered by package and by verb; click opens the line
// The rail at the right is context: source, which of your packages use it and how, siblings, releases.
// Data: data/page6/<id>.json (extract_page6.py), the page model the native page is built from.

const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const PAGES = { Value: "rs-Value", as_str: "rs-as_str", from_str: "rs-from_str", AllocationInfo: "rs-AllocationInfo", Serialize: "rs-Serialize", match: "py-re.match", "which.sync": "js-which.sync" };
const get = (id) => fetch(`data/page6/${id}.json`, { cache: "reload" }).then((r) => r.json());

// ---------------------------------------------------------------- icons: one small drawn vocabulary
const sv = (w, body, cls = "") => `<svg class="${cls}" width="${w}" height="${w}" viewBox="0 0 14 14" fill="none" stroke-width="1.5">${body}</svg>`;
const I = {
  // what a place does with it (the verbs, used as tags everywhere)
  makes: sv(14, `<rect x="1.5" y="1.5" width="11" height="11" stroke="var(--make)"/><path d="M7 4v6M4 7h6" stroke="var(--make)"/>`),
  reads: sv(14, `<circle cx="7" cy="7" r="4.6" stroke="var(--ink2)"/>`),
  changes: sv(14, `<circle cx="7" cy="7" r="4.6" stroke="var(--chg)"/><circle cx="7" cy="7" r="1.8" fill="var(--chg)" stroke="none"/>`),
  "uses up": sv(14, `<circle cx="7" cy="7" r="5" fill="var(--use)" stroke="none"/><path d="M4.5 7h5M7.5 5l2 2-2 2" stroke="var(--g0)"/>`),
  holds: sv(14, `<path d="M4.5 2H2.5v10h2M9.5 2h2v10h-2" stroke="var(--ink2)"/>`),
  matches: sv(14, `<path d="M2 7h3M5 7l4-4h3M5 7l4 4h3" stroke="var(--make)"/>`),
  names: sv(14, `<circle cx="7" cy="7" r="1.8" fill="var(--ink3)" stroke="none"/>`),
  imports: sv(14, `<path d="M7 2v7M4 6l3 3 3-3M3 12h8" stroke="var(--ink4)"/>`),
  calls: sv(14, `<path d="M1.5 7h7M6 4.5 8.5 7 6 9.5" stroke="var(--peri)"/><rect x="9.5" y="3.5" width="3" height="7" stroke="var(--peri)"/>`),
  derives: sv(14, `<path d="M3 11 11 3M7 2.5v3M5.5 4h3M10 8v3M8.5 9.5h3" stroke="var(--make)"/>`),
  implements: sv(14, `<path d="M2.5 2.5h9v9h-9zM5 2.5v3.5h4V2.5" stroke="var(--k-co)"/>`),
  "asks for": sv(14, `<circle cx="5" cy="7" r="2.8" stroke="var(--k-co)"/><path d="M7.8 7h4.7M11 7v2" stroke="var(--k-co)"/>`),
  // outcomes, small, for rows and siblings
  ofail: sv(11, `<rect x="1" y="1" width="12" height="12" fill="var(--fail)" stroke="none"/><path d="M4.5 4.5l5 5M9.5 4.5l-5 5" stroke="var(--g0)" stroke-width="1.8"/>`),
  onone: sv(11, `<circle cx="7" cy="7" r="5" stroke="var(--none)" stroke-width="1.8" stroke-dasharray="2.4 1.8"/>`),
  olater: sv(11, `<circle cx="7" cy="7" r="5.4" stroke="var(--peri)"/><path d="M7 4v3.3l2 1.3" stroke="var(--peri)"/>`),
  omany: sv(11, `<path d="M2 4h10M2 7h10M2 10h10" stroke="var(--ink2)"/>`),
  open: `<svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.4"><path d="M5 2H2v8h8V7M7 2h3v3M10 2 5.5 6.5"/></svg>`,
  loop: `<svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M11 7a4 4 0 1 1-1.2-2.9M11 2.5v2.4H8.6"/></svg>`,
};
const KIND = {
  function: sv(18, `<path d="M4 3h8v8l-2 2H4z" stroke="var(--k-ca)" fill="rgba(143,166,255,.18)"/><path d="M0.5 8H4M12 8h1.5" stroke="var(--k-ca)"/>`),
  method: sv(18, `<path d="M4 4h8v7l-2 2H4z" stroke="var(--k-ca)" fill="rgba(143,166,255,.18)"/><path d="M8 0.5V4" stroke="var(--k-ca)"/>`),
  enum: sv(18, `<path d="M7 1.5 12.5 7 7 12.5 1.5 7z" stroke="var(--k-ty)" fill="rgba(70,210,220,.16)"/><path d="M4.5 6h5M4.5 8h5" stroke="var(--k-ty)"/>`),
  struct: sv(18, `<path d="M7 1.5 12.5 7 7 12.5 1.5 7z" stroke="var(--k-ty)" fill="rgba(70,210,220,.16)"/><path d="M5.8 5v4M8.2 5v4" stroke="var(--k-ty)"/>`),
  trait: sv(18, `<path d="M3 2h8v10H3zM5.5 2v3h3V2" stroke="var(--k-co)"/>`),
};
// joints on the rail
const J = {
  in: `<svg width="12" height="12" viewBox="0 0 12 12"><circle cx="6" cy="6" r="4" fill="var(--ink1)"/></svg>`,
  opt: `<svg width="12" height="12" viewBox="0 0 12 12"><circle cx="6" cy="6" r="3.8" fill="var(--g1)" stroke="var(--ink2)" stroke-width="1.5"/></svg>`,
  rest: `<svg width="14" height="12" viewBox="0 0 14 12"><circle cx="4.5" cy="6" r="3" fill="var(--ink1)"/><circle cx="10" cy="6" r="3" fill="var(--g1)" stroke="var(--ink1)" stroke-width="1.3"/></svg>`,
  recv: (how) => `<svg width="12" height="12" viewBox="0 0 12 12"><rect x="1.5" y="1.5" width="9" height="9" fill="${how === "changes" ? "var(--chg)" : how === "uses up" ? "var(--use)" : "var(--ink2)"}"/></svg>`,
  out: `<svg width="14" height="14" viewBox="0 0 14 14"><path d="M2 3.5 12 7 2 10.5z" fill="var(--peri-hi)"/></svg>`,
  many: `<svg width="16" height="14" viewBox="0 0 16 14"><path d="M1 3.5 8 7 1 10.5zM7 3.5 14 7 7 10.5z" fill="var(--peri-hi)"/></svg>`,
  fail: `<svg width="14" height="14" viewBox="0 0 14 14"><rect x="1" y="1" width="12" height="12" fill="var(--fail)"/><path d="M4.4 4.4l5.2 5.2M9.6 4.4l-5.2 5.2" stroke="var(--g0)" stroke-width="1.9"/></svg>`,
  none: `<svg width="14" height="14" viewBox="0 0 14 14"><circle cx="7" cy="7" r="5.2" fill="var(--g1)" stroke="var(--none)" stroke-width="1.8" stroke-dasharray="2.6 1.9"/></svg>`,
  later: `<svg width="16" height="16" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.2" fill="var(--g1)" stroke="var(--peri)" stroke-width="1.5"/><path d="M8 4.6V8l2.3 1.5" stroke="var(--peri)" stroke-width="1.5" fill="none"/></svg>`,
  case: `<svg width="12" height="12" viewBox="0 0 12 12"><path d="M6 1.5 10.5 6 6 10.5 1.5 6z" fill="var(--g1)" stroke="var(--make)" stroke-width="1.5"/></svg>`,
  field: `<svg width="10" height="10" viewBox="0 0 10 10"><rect x="1" y="1" width="8" height="8" fill="var(--ink2)"/></svg>`,
};
const CAP = { Clone: ["copies", `<rect x="1.5" y="3.5" width="7" height="7"/><rect x="5" y="1.5" width="7" height="7"/>`], Copy: ["copies by assignment", `<rect x="1.5" y="3.5" width="7" height="7"/><rect x="5" y="1.5" width="7" height="7"/>`], PartialEq: ["compares with ==", `<path d="M2.5 5h9M2.5 9h9"/>`], Eq: null, Hash: ["can be a map key", `<path d="M5 1.5 4 12.5M10 1.5 9 12.5M2 5h10.5M1.5 9H12"/>`], Debug: ["prints for debugging", `<path d="M4.5 2C3 2 3.5 7 2 7c1.5 0 1 5 2.5 5M9.5 2c1.5 0 1 5 2.5 5-1.5 0-1 5-2.5 5"/>`], Display: ["prints", `<path d="M2 4h10M2 7h10M2 10h6"/>`], Default: ["has a default", `<circle cx="7" cy="7" r="4.5"/><circle cx="7" cy="7" r="1" fill="currentColor"/>`], Serialize: ["serde can write it", `<rect x="1.5" y="3.5" width="6" height="7"/><path d="M5 7h7M10 5l2 2-2 2"/>`], Deserialize: ["serde can read one", `<rect x="6.5" y="3.5" width="6" height="7"/><path d="M1.5 7h7M6.5 5l2 2-2 2"/>`], FromStr: ["parses from text", `<path d="M2 3.5h5M2 7h5M2 10.5h3M9 7h3.5M11 5l2 2-2 2"/>`], Index: ["v[i] works", `<path d="M4 2H2v10h2M10 2h2v10h-2M7 4.5v5"/>`], IntoDeserializer: null, Deserializer: ["is itself a reader of serde data", `<rect x="3" y="2.5" width="8" height="9"/><path d="M5 5.5h4M5 8.5h4"/>`], StdError: null, Error: ["is an error", `<rect x="2" y="2" width="10" height="10"/><path d="M5 5l4 4M9 5 5 9"/>`] };
const capHtml = (c) => { const k = CAP[c.name]; if (k === null) return ""; const [w, g] = k || [c.name, `<path d="M3 2.5h8v9H3zM5.5 2.5v3h3v-3"/>`]; return `<span class="cap" title="${esc(c.name)}${c.how === "derive" ? " (derived)" : ""}"><svg width="14" height="14" viewBox="0 0 14 14" fill="none" stroke="currentColor" stroke-width="1.4">${g}</svg>${esc(w)}</span>`; };

// ---------------------------------------------------------------- small pieces
function md(t) {
  return esc(t).replace(/\[`?([\w:.()]+?)`?\](?![(\[])/g, "`$1`").replace(/`([^`]+)`/g, (_, c) => `<code>${c}</code>`).replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>").replace(/\[([^\]]+)\]\[[^\]]*\]/g, "$1").replace(/\[([^\]]+)\]\([^)]*\)/g, "$1");
}
function typ(t, { err = false, g = null } = {}) {
  if (!t) return "";
  const word = t.word || t.text;
  const gen = t.gen ? `<span class="s6-g" data-card="gen:${esc(t.gen)}">${esc(t.gen)}</span>` : "";
  const link = t.link && PAGES[t.link] ? ` link" data-go="${PAGES[t.link]}` : "";
  const known = t.known ? ` ${t.known}" title="Not declared: read from its ${t.known}` : "";
  const loops = t.loops ? `<span class="loop" title="It holds more of itself: it nests">${I.loop}</span>` : "";
  const shown = t.gen && word === t.gen ? "" : `<span class="w${link}${known}"${t.link && !PAGES[t.link] && err ? ` data-card="err"` : ""}>${esc(word)}</span>`;
  const x = t.text && t.text !== word && !t.gen ? `<span class="x">${esc(t.text)}</span>` : "";
  return `<span class="s6-t${err ? " err" : ""}">${gen}${shown}${loops}${x}</span>`;
}
const outcomeGlyphs = (sig) => { if (!sig) return ""; const o = sig.out || {}; return [o.fails ? I.ofail : "", o.none ? I.onone : "", o.later ? I.olater : "", o.many ? I.omany : ""].join(""); };
function row(cls, joint, name, val, { lbl = false } = {}) { return `<div class="s6-r ${cls}"><span class="j"><i class="ln t"></i><i class="ln b"></i><span class="gl">${joint}</span></span><span class="nm${lbl ? " lbl" : ""}">${name}</span><span class="val">${val}</span></div>`; }

// ---------------------------------------------------------------- the call
function callHtml(M) {
  const S = M.sig; const rows = [];
  if (S.recv) rows.push(row("recv", J.recv(S.recv.how), "self", `${typ(S.recv.type || { word: M.path[M.path.length - 1] })}<span class="pd">${esc(S.recv.how === "reads" ? "reads it" : S.recv.how === "changes" ? "changes it" : "uses it up")}</span>`));
  for (const p of S.ins) {
    const j = p.rest ? J.rest : p.opt ? J.opt : J.in;
    const gen = p.type.gen && p.type.word !== p.type.gen ? "" : "";
    rows.push(row(`in${p.fields ? " has-sub" : ""}`, j, esc(p.name), `${typ(p.type)}${gen}${p.dflt ? `<span class="dflt">= ${esc(p.dflt)}</span>` : p.opt ? `<span class="dflt">optional</span>` : ""}${p.doc ? `<span class="pd">${esc(p.doc)}</span>` : ""}${p.fields ? `<span class="caret" data-sub="${esc(p.name)}">${p.fields.length} options${p.fields.some((f) => f.changes) ? `, ${p.fields.filter((f) => f.changes).length} change what it gives` : ""} ▸</span>` : ""}`));
    if (p.fields) for (const f of p.fields) rows.push(row(`sub sub-${esc(p.name)}${OPEN_SUB.has(p.name) ? "" : " hide"}`, "", "", "").replace(`<span class="nm"></span><span class="val"></span>`, `<span></span><span class="nm">${esc(f.name)}</span><span class="val"><span class="s6-t"><span class="w">${esc(f.word)}</span></span><span class="pd">${esc(f.doc)}</span>${f.changes ? `<span class="pd" style="display:inline-flex;gap:4px;align-items:center">${f.changes.startsWith("fails") ? `${I.ofail}→${I.onone}` : `→ ${I.omany}`}</span>` : ""}</span>`));
  }
  const o = S.out;
  if (o.later) rows.push(row("later", J.later, "later", `<span class="when">${esc(M.lang === "javascript" || M.lang === "typescript" ? "a Promise: it answers after you await it" : "it answers when you await it")}</span>`, { lbl: true }));
  const gives = o.type && o.type.word !== "nothing" ? typ(o.type) : `<span class="when">nothing</span>`;
  rows.push(row("out", o.many ? J.many : J.out, o.many ? "gives each" : "gives", `${gives}${S.generics && S.generics.length && o.type && o.type.gen ? `<span class="s6-role">${esc(roleWord(S.generics.find((g) => g.name === o.type.gen)))}</span>` : ""}`, { lbl: true }));
  if (o.none) rows.push(row("none", J.none, "or nothing", `<span class="when">${esc(o.none.when || (M.lang === "rust" ? "None" : "null"))}</span>`, { lbl: true }));
  if (o.fails) rows.push(row("fail", J.fail, M.lang === "python" ? "or raises" : M.lang === "javascript" || M.lang === "typescript" ? "or throws" : "or fails", `<span data-card="err" style="cursor:help">${typ(o.fails.type, { err: true })}</span><span class="when">${esc(o.fails.short || shortWhen(o.fails.when))}</span>`, { lbl: true }));
  const html = rows.join("");
  return `<div class="s6-call">${html}</div>`;
}
const OPEN_SUB = new Set();
const roleWord = (g) => !g ? "" : g.role === "choose" ? "you choose it" : g.role === "through" ? "the same kind you gave" : "any that fits";
function shortWhen(t) { if (!t) return ""; const m = String(t).match(/^(.+?[.!?])(\s|$)/); return (m ? m[1] : t).replace(/^This conversion can fail if /, "if "); }

// ---------------------------------------------------------------- docs
function docsHtml(M) {
  const out = []; let skip = false;
  for (const b of M.docs || []) {
    if (b.k === "h") { skip = /^errors$/i.test(b.t) && !!(M.sig && M.sig.out.fails); if (!/^examples?$/i.test(b.t) && !skip) out.push(`<div class="hh">${md(b.t)}</div>`); continue; }
    if (skip && b.k === "p") continue;
    if (b.k === "p") out.push(`<p>${md(b.t)}</p>`);
    else if (b.k === "li") out.push(`<li>${md(b.t)}</li>`);
    else if (b.k === "code") out.push(`<details class="s6-ex"><summary>Example</summary><pre>${esc(b.t)}</pre></details>`);
  }
  const rest = out.slice(1);   // the first paragraph is the lede
  if (rest.length <= 2) return rest.join("");
  return rest.slice(0, 2).join("") + `<details class="s6-ex s6-more-docs"><summary>${rest.length - 2} more paragraph${rest.length - 2 > 1 ? "s" : ""}</summary><div style="margin-top:12px">${rest.slice(2).join("")}</div></details>`;
}

// ---------------------------------------------------------------- if it fails
function failsHtml(M) {
  const f = M.sig && M.sig.out.fails; if (!f) return "";
  if (!(f.kinds || []).length && (!f.when || f.when.length < 90)) return "";
  const kinds = (f.kinds || []).map((k) => `<span class="k${k.name === "Io" && M.name === "from_str" ? " dim" : ""}">${I.ofail}<b>${esc(k.name)}</b>${esc(k.doc.replace(/^The error was caused by /, "").replace(/\.$/, ""))}</span>`).join("");
  const verb = M.lang === "python" ? "raises" : M.lang === "javascript" ? "throws" : "returns";
  return `<section class="s6-sec"><div class="s6-h">If it fails</div><div class="s6-fails"><div class="top">${typ(f.type, { err: true })}<p>${md(f.when ? f.when : `It ${verb} ${f.type.word}.`)}</p></div>${kinds ? `<div class="kinds">${kinds}</div><div class="src">${esc(f.type.word)} tells you which with ${esc(f.kinds_from || "its kind")}.${M.name === "from_str" ? " Io can't happen here: the text is already in memory." : ""}</div>` : ""}</div></section>`;
}

// ---------------------------------------------------------------- what it is
function shapeHtml(M) {
  const S = M.shape; if (!S) return "";
  if (S.kind === "one of") {
    const rows = S.cases.map((c, i) => `<div class="s6-c ${i === 0 ? "first" : ""} ${i === S.cases.length - 1 ? "last" : ""}" data-i="${i}"><span class="j"><i class="ln t"></i><i class="ln b"></i><span class="gl">${J.case}</span></span><span class="nm">${esc(c.name)}</span><span class="ty">${c.type ? typ({ ...c.type, loops: c.loops }) : `<span class="doc">nothing inside</span>`}</span><span class="doc">${esc(c.doc.replace(/^Represents (a |an )?/, "").replace(/\.$/, ""))}</span><span></span>${c.more ? `<div class="more">${md(c.more)}</div>` : ""}</div>`).join("");
    return `<section class="s6-sec"><div class="s6-h">What it is<i>one of ${S.cases.length}</i><span class="aside">Exactly one of these at a time${S.cases.some((c) => c.loops) ? ` · ${I.loop.replace('stroke="currentColor"', 'stroke="var(--make)"')} holds more of itself` : ""}</span></div><div class="s6-shape oneof">${rows}</div></section>`;
  }
  if (S.kind === "holds") {
    const rows = S.fields.map((f, i) => `<div class="s6-c ${i === 0 ? "first" : ""} ${i === S.fields.length - 1 ? "last" : ""}"><span class="j"><i class="ln t"></i><i class="ln b"></i><span class="gl">${J.field}</span></span><span class="nm">${esc(f.name)}</span><span class="ty">${typ(f.type)}</span><span class="doc">${md(f.doc.replace(/\.$/, ""))}</span>${f.yours ? `<span class="yu">you read it · ${f.yours}</span>` : "<span></span>"}</div>`).join("");
    return `<section class="s6-sec"><div class="s6-h">What it is<i>holds ${S.fields.length}</i><span class="aside">All of these, together</span></div><div class="s6-shape holds">${rows}</div></section>`;
  }
  return "";
}

// ---------------------------------------------------------------- what you can do with it
const VERB = { makes: "Makes one", reads: "Reads it", changes: "Changes it", "uses up": "Uses it up" };
function verbsHtml(M) {
  if (!M.groups || !M.groups.length) return "";
  const g = M.groups.map((G) => {
    const items = G.items;
    const rows = items.map((it, i) => {
      const s = it.sig;
      const ins = it.froms ? it.froms.map(esc).join(", ") : it.from ? esc(it.from.word) : s ? s.ins.map((p) => esc(p.type.word)).join(", ") : "";
      const out = s ? (s.out.type && s.out.type.word !== "nothing" ? esc(s.out.type.word) : "") : "";
      const o = it.fails ? I.ofail : outcomeGlyphs(s);
      return `<div class="s6-m${i >= 6 ? " extra" : ""}" ${PAGES[it.name] ? `data-go="${PAGES[it.name]}"` : ""} title="${esc(it.doc)}"><span class="n">${esc(it.name)}</span><span class="s">${ins ? `(${ins})` : "()"}${out ? ` → ${out}` : ""}<span class="o">${o}</span></span>${it.yours ? `<span class="yu">${it.yours}</span>` : "<span></span>"}</div>`;
    }).join("");
    return `<div class="s6-vg ${items.length > 6 ? "shut" : ""}"><div class="vh">${I[G.verb] || ""}${VERB[G.verb] || G.verb}<i>${items.length}</i></div>${rows}${items.length > 6 ? `<span class="fold">${items.length - 6} more</span>` : ""}</div>`;
  }).join("");
  return `<section class="s6-sec"><div class="s6-h">What you can do with it<span class="aside"><span style="display:inline-flex;align-items:center;gap:6px"><i style="width:6px;height:6px;border-radius:50%;background:var(--mint);display:inline-block"></i>your workspace uses it</span></span></div><div class="s6-verbs">${g}</div></section>`;
}

// ---------------------------------------------------------------- in your workspace
const TAG_ORDER = ["calls", "makes", "reads", "changes", "uses up", "matches", "holds", "derives", "implements", "asks for", "names", "imports"];
function usesHtml(M, st) {
  const all = M.uses || [];
  if (!all.length) return `<section class="s6-sec" id="s6-uses"><div class="s6-h">In your workspace</div><p class="s6-none">${M.workspace ? `Your workspace has no ${esc(M.lang)}. These are its uses in ${esc(M.workspace)}.` : "Nothing in your workspace names it."}</p></section>`;
  const vis = all.filter((u) => (st.imports || u.tag !== "imports") && (!st.pkg || u.pkg === st.pkg) && (!st.tag || u.tag === st.tag) && (!st.fill || u.fill === st.fill) && (st.tests || u.ctx !== "test"));
  const inCtx = (u) => st.tests || u.ctx !== "test";
  const pk = new Map(); for (const u of all) if ((u.tag !== "imports" || st.imports) && inCtx(u)) pk.set(u.pkg, (pk.get(u.pkg) || 0) + 1);
  const tg = new Map(); for (const u of all) if ((!st.pkg || u.pkg === st.pkg) && inCtx(u)) tg.set(u.tag, (tg.get(u.tag) || 0) + 1);
  const pks = [...pk.entries()].sort((a, b) => b[1] - a[1]);
  const pkChips = `<span class="s6-pick"><span class="s6-chip ${st.pkg ? "pk on" : ""}" data-menu="1">${st.pkg ? esc(st.pkg) : `all ${pks.length} package${pks.length === 1 ? "" : "s"}`} <i>${st.pkg ? pk.get(st.pkg) : [...pk.values()].reduce((a, b) => a + b, 0)}</i> ▾</span>${st.pkg ? `<span class="s6-chip" data-pkg="">× all</span>` : ""}<span class="s6-menu ${st.menu ? "on" : ""}">${pks.map(([p, n]) => `<span class="mi ${st.pkg === p ? "on" : ""}" data-pkg="${esc(p)}"><b>${esc(p)}</b><span class="tags">${[...new Set(all.filter((u) => u.pkg === p && u.tag !== "imports").map((u) => u.tag))].sort((a, b) => TAG_ORDER.indexOf(a) - TAG_ORDER.indexOf(b)).map((t) => I[t]).join("")}</span><i>${n}</i></span>`).join("")}</span></span>`;
  const tagChips = TAG_ORDER.filter((t) => tg.has(t)).map((t) => `<span class="s6-chip ${st.tag === t ? "on" : ""} ${t === "imports" && !st.imports ? "dim" : ""}" data-tag="${esc(t)}">${I[t]}${esc(t)} <i>${tg.get(t)}</i></span>`).join("");
  const groups = new Map(); for (const u of vis) (groups.get(u.pkg) || groups.set(u.pkg, []).get(u.pkg)).push(u);
  const hl = (t) => { const n = M.name.split(".").pop(); const m = esc(t).replace(new RegExp(`\\b(${n.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})\\b`), "<mark>$1</mark>"); return m; };
  const body = [...groups.entries()].sort((a, b) => b[1].length - a[1].length).map(([p, us]) => {
    const tags = [...new Set(us.map((u) => u.tag))].sort((a, b) => TAG_ORDER.indexOf(a) - TAG_ORDER.indexOf(b));
    const cap = st.open.has(p) ? us.length : 5;
    return `<div class="s6-pg"><div class="ph"><b>${esc(p)}</b><i>${us.length}</i><span class="tags">${tags.map((t) => `<span title="${esc(t)}">${I[t]}</span>`).join("")}</span></div>${us.slice(0, cap).map((u) => `<div class="s6-u${u.approx ? " approx" : ""}" data-open="${esc((u.dir ? u.dir + "/" : "") + u.file)}:${u.line}"><span class="tg" title="${esc(u.tag)}${u.approx ? " (matched by name)" : ""}">${I[u.tag]}</span><span class="loc"><span>${esc(u.file)}:${u.line}</span></span><span class="code">${hl(u.text)}</span><span class="end">${u.fill ? `<span class="fill" title="T is ${esc(u.fill)} here">T = ${esc(u.fill)}</span>` : ""}<span class="open">${I.open} open</span></span></div>`).join("")}${us.length > cap ? `<span class="more" data-more="${esc(p)}">${us.length - cap} more in ${esc(p)}</span>` : ""}</div>`;
  }).join("");
  const ctx = all.some((u) => u.ctx === "test") ? `<span class="s6-chip ${st.tests ? "on" : ""}" data-tests="1">include tests <i>${all.filter((u) => u.ctx === "test").length}</i></span>` : "";
  const approx = all.some((u) => u.approx) ? `<div class="s6-foot">Method calls and field reads are matched by name in the files that import it; the index resolves them exactly.</div>` : "";
  return `<section class="s6-sec" id="s6-uses"><div class="s6-h">In your workspace<i>${vis.length} place${vis.length === 1 ? "" : "s"}</i>${st.fill ? `<span class="aside">where T is <b style="color:var(--gen);font-family:var(--mono)">${esc(st.fill)}</b> · <span data-fill="" style="color:var(--peri);cursor:pointer">clear</span></span>` : ""}</div>
    <div class="s6-f">${pkChips}<span class="s6-sep"></span>${tagChips}${ctx}</div><div class="s6-list">${body || `<p class="s6-none">No place matches.</p>`}</div>${approx}</section>`;
}

// ---------------------------------------------------------------- the rail
function railHtml(M, st) {
  const uses = (M.uses || []).filter((u) => u.tag !== "imports");
  const by = new Map(); for (const u of uses) { const e = by.get(u.pkg) || by.set(u.pkg, { n: 0, tags: new Set() }).get(u.pkg); e.n++; e.tags.add(u.tag); }
  const pk = [...by.entries()].sort((a, b) => b[1].n - a[1].n);
  const yours = M.workspace ? `<div class="rb"><h4>Where it's used</h4><div class="sub" style="color:var(--ink2)">${esc(M.workspace)}</div></div>` : pk.length ? `<div class="rb"><h4>Your packages that use it</h4>${pk.slice(0, 9).map(([p, e]) => `<div class="pr ${st.pkg === p ? "on" : ""}" data-pkg="${esc(p)}"><b>${esc(p)}</b><span class="tags">${[...e.tags].sort((a, b) => TAG_ORDER.indexOf(a) - TAG_ORDER.indexOf(b)).map((t) => `<span title="${esc(t)}">${I[t]}</span>`).join("")}</span></div>`).join("")}${pk.length > 9 ? `<div class="sub">and ${pk.length - 9} more</div>` : ""}</div>` : `<div class="rb"><h4>Your packages</h4><div class="sub">None of them use it.</div></div>`;
  const src = `<div class="rb"><h4>Source</h4><span class="src" data-open="${esc(M.source.file)}:${M.source.line}">${I.open}${esc(M.source.file)}:${M.source.line}</span><div class="sub">${esc(M.pkg)} ${esc(M.version)}${M.lang === "rust" ? " · your pin" : ""}</div></div>`;
  const sib = (M.siblings || []).length ? `<div class="rb"><h4>Next to it</h4>${M.siblings.map((s) => `<div class="sb" ${PAGES[s.name] ? `data-go="${PAGES[s.name]}"` : ""}><span>${KIND[s.kind === "fn" ? "function" : s.kind] ? KIND[s.kind === "fn" ? "function" : s.kind].replace(/width="18" height="18"/, 'width="14" height="14"') : ""}</span><b>${esc(s.name)}</b><span>${esc(s.differs)}</span><span class="o">${s.fails ? I.ofail : ""}${s.none ? I.onone : ""}${s.later ? I.olater : ""}</span></div>`).join("")}</div>` : "";
  const caps = (M.implements || []).map(capHtml).filter(Boolean).join("");
  const can = caps ? `<div class="rb"><h4>It can</h4><div class="caps">${caps}</div></div>` : "";
  const H = M.history && M.history.releases || [];
  const hist = H.length > 1 ? `<div class="rb"><h4>Across releases</h4><div class="hist"><span class="dots">${H.map((h) => `<i class="${h.v === M.version ? "pin" : h.sig !== H[H.length - 1].sig ? "diff" : ""}" title="${esc(h.v)}"></i>`).join("")}</span>${M.history.same ? `the same in ${H.map((h) => esc(h.v)).join(" and ")}` : "it changed between releases"}</div></div>` : "";
  const facts = M.facts ? `<div class="rb"><h4>How we know</h4><div class="note">${esc(M.facts)}</div><div class="sub" style="margin-top:6px">A dotted underline marks a type read from its docs or code, not declared.</div></div>` : "";
  return `<div class="stick">${src}${yours}${sib}${can}${hist}${facts}</div>`;
}

// ---------------------------------------------------------------- hover cards
function cardFor(M, key) {
  const [k, a] = key.split(":");
  if (k === "gen") {
    const g = (M.sig.generics || []).find((x) => x.name === a); if (!g) return "";
    const fills = g.fills || [];
    const max = Math.max(1, ...fills.map((f) => f.n));
    return `<div class="ct"><span class="s6-g">${esc(g.name)}</span>${esc(roleWord(g))}</div><p class="cs">${esc(g.says || "")}</p>
      ${g.bounds.length ? `<div class="cl">It must be</div>${g.bounds.map((b) => `<div class="row">${I.implements}<b>${esc(b.name)}</b>${esc(b.word)}</div>`).join("")}` : ""}
      ${fills.length ? `<div class="cl">Your workspace chooses</div>${fills.slice(0, 7).map((f) => `<div class="row" data-fill="${esc(f.type)}" style="cursor:pointer"><b>${esc(f.type)}</b><span class="pk">${esc(f.pkgs.slice(0, 3).join(", "))}${f.pkgs.length > 3 ? "…" : ""}</span><span style="margin-left:auto;display:inline-flex;align-items:center;gap:6px"><i class="bar" style="width:${Math.round(40 * f.n / max)}px"></i>${f.n}</span></div>`).join("")}${fills.length > 7 ? `<div class="fine">and ${fills.length - 7} more types</div>` : ""}` : ""}
      ${g.elsewhere && g.elsewhere.length ? `<div class="cl">Elsewhere in the registry</div><div class="fine" style="margin-top:0">${g.elsewhere.slice(0, 6).map((e) => `${esc(e.type)} <span style="color:var(--ink4)">${e.crates}</span>`).join(" · ")}</div>` : ""}
      ${fills.length ? `<div class="fine">Click a type to see those places.</div>` : ""}`;
  }
  if (k === "err") {
    const f = M.sig && M.sig.out.fails; if (!f) return "";
  if (!(f.kinds || []).length && (!f.when || f.when.length < 90)) return "";
    return `<div class="ct">${J.fail}<span style="color:var(--fail)">${esc(f.type.word)}</span></div><p class="cs">${md(f.when || "")}</p>${(f.kinds || []).length ? `<div class="cl">It tells you which</div>${f.kinds.map((x) => `<div class="row">${I.ofail}<b>${esc(x.name)}</b>${esc(x.doc.replace(/^The error was caused by /, ""))}</div>`).join("")}` : ""}`;
  }
  return "";
}

// ---------------------------------------------------------------- the page
export async function symbol6({ reader, jump, side, Q }) {
  const id = Q.get("id") || "rs-from_str";
  const M = await get(id);
  reader.innerHTML = "";
  // The page lives in its own shadow root, as a native view would: the board's global class names can't reach it.
  const host = document.createElement("div"); host.style.cssText = "position:absolute;inset:0"; reader.appendChild(host);
  const shadow = host.attachShadow({ mode: "open" });
  shadow.innerHTML = `<link rel="stylesheet" href="symbol6.css">`;
  await new Promise((r) => { const l = shadow.querySelector("link"); l.onload = r; l.onerror = r; });
  const root = document.createElement("div"); root.className = "s6"; shadow.appendChild(root);
  const st = { pkg: Q.get("pkg") || null, tag: Q.get("tag") || null, fill: Q.get("fill") || null, imports: false, tests: false, open: new Set() };
  jump.innerHTML = `<span class="c">Library</span><span class="rel">›</span><span class="c">${esc(M.pkg)}</span>${M.path.slice(1).map((p) => `<span class="rel">›</span><span class="md">${esc(p)}</span>`).join("")}<span class="rel">›</span><b>${esc(M.name)}</b>`;
  side.innerHTML = `<div class="scope"><span class="up">‹ ${esc(M.pkg)}</span></div><div class="hd">symbol pages <i>7</i></div><div class="rows">${Object.entries(PAGES).map(([n, pid]) => `<div class="r ${pid === id ? "hot" : ""}" data-go="${pid}">${esc(n)}</div>`).join("")}</div>`;
  side.onclick = (e) => { const g = e.target.closest("[data-go]"); if (g) go(g.dataset.go); };
  const draw = () => {
    const kind = M.kind === "function" && M.path.length > 1 && M.lang === "rust" && M.name !== "from_str" ? "method" : M.kind;
    root.innerHTML = `<div class="s6-in"><div class="s6-main">
      <div class="s6-kind">${KIND[kind] || KIND.function}${esc(M.kind)}<span class="path">${M.path.map((p, i) => i === M.path.length - 1 ? `<b>${esc(p)}</b>` : esc(p)).join(" › ")}</span><span class="lang">${esc({ rust: "Rust", python: "Python", javascript: "JavaScript", typescript: "TypeScript", go: "Go" }[M.lang])}</span></div>
      <h1>${esc(M.name)}</h1><p class="s6-lede">${md(M.summary)}</p>
      ${M.sig ? callHtml(M) : ""}
      <section class="s6-sec s6-docs">${docsHtml(M)}</section>
      ${failsHtml(M)}${shapeHtml(M)}${M.required ? traitHtml(M) : ""}${verbsHtml(M)}${usesHtml(M, st)}
    </div><aside class="s6-rail">${railHtml(M, st)}</aside></div><div class="s6-card"></div><div class="s6-toast"></div>`;
  };
  const go = (pid) => { const q = new URLSearchParams(location.search); q.set("id", pid); ["pkg", "tag", "fill"].forEach((k) => q.delete(k)); history.pushState(null, "", "?" + q); symbol6({ reader, jump, side, Q: q }); };
  draw();
  let hideT = null;
  root.addEventListener("mouseover", (e) => {
    const t = e.target.closest("[data-card]"); const card = root.querySelector(".s6-card");
    if (e.target.closest(".s6-card")) { clearTimeout(hideT); return; }
    if (!t) return;
    clearTimeout(hideT);
    const html = cardFor(M, t.dataset.card); if (!html) return;
    card.innerHTML = html;
    const o = root.getBoundingClientRect(), r = t.getBoundingClientRect();
    let x = r.left - o.left + root.scrollLeft, y = r.bottom - o.top + root.scrollTop + 8;
    x = Math.min(x, root.clientWidth - 380);
    card.style.left = x + "px"; card.style.top = y + "px"; card.classList.add("on");
    root.querySelectorAll(".s6-g").forEach((g) => g.classList.toggle("hot", t.dataset.card === `gen:${g.textContent}`));
  });
  root.addEventListener("mouseout", (e) => {
    const t = e.target.closest("[data-card], .s6-card"); if (!t) return;
    if (t.contains(e.relatedTarget) || (e.relatedTarget && e.relatedTarget.closest && e.relatedTarget.closest(".s6-card"))) return;
    hideT = setTimeout(() => { root.querySelector(".s6-card").classList.remove("on"); root.querySelectorAll(".s6-g.hot").forEach((g) => g.classList.remove("hot")); }, 160);
  });
  root.addEventListener("click", (e) => {
    const t = e.target;
    const g = t.closest("[data-go]"); if (g) { go(g.dataset.go); return; }
    if (t.closest("[data-menu]")) { st.menu = !st.menu; draw(); return; }
    const p = t.closest("[data-pkg]"); if (p) { st.pkg = p.dataset.pkg && st.pkg !== p.dataset.pkg ? p.dataset.pkg : null; st.menu = false; draw(); return; }
    if (st.menu && !t.closest(".s6-menu")) { st.menu = false; draw(); }
    const tg = t.closest("[data-tag]"); if (tg) { const v = tg.dataset.tag; if (v === "imports") st.imports = !st.imports; st.tag = st.tag === v ? null : v; draw(); return; }
    const f = t.closest("[data-fill]"); if (f) { st.fill = f.dataset.fill || null; st.pkg = null; draw(); const u = root.querySelector("#s6-uses"); if (u && st.fill) root.scrollTo({ top: u.offsetTop - 20, behavior: "smooth" }); return; }
    if (t.closest("[data-tests]")) { st.tests = !st.tests; draw(); return; }
    const m = t.closest("[data-more]"); if (m) { st.open.add(m.dataset.more); draw(); return; }
    const fo = t.closest(".fold"); if (fo) { fo.closest(".s6-vg").classList.remove("shut"); return; }
    const c = t.closest(".s6-c"); if (c && c.querySelector(".more")) { c.classList.toggle("open"); return; }
    const sb = t.closest("[data-sub]"); if (sb) { const n = sb.dataset.sub; OPEN_SUB.has(n) ? OPEN_SUB.delete(n) : OPEN_SUB.add(n); draw(); return; }
    const o = t.closest("[data-open]"); if (o) { const toast = root.querySelector(".s6-toast"); toast.innerHTML = `Opens <b>${esc(o.dataset.open)}</b> in your editor`; toast.style.top = (root.scrollTop + root.clientHeight - 60) + "px"; toast.classList.add("on"); setTimeout(() => toast.classList.remove("on"), 1600); }
  });
  if (Q.get("opts")) { OPEN_SUB.add(Q.get("opts")); draw(); }
  if (Q.get("card")) { const t = root.querySelector(`[data-card="${CSS.escape(Q.get("card"))}"]`); if (t) t.dispatchEvent(new MouseEvent("mouseover", { bubbles: true })); }
  if (Q.get("sc")) root.scrollTop = +Q.get("sc");
  if (Q.get("to") === "uses") { const u = root.querySelector("#s6-uses"); if (u) root.scrollTop = u.offsetTop - 20; }
}

function traitHtml(M) {
  const rq = M.required.map((r, i) => `<div class="s6-c ${i === 0 ? "first" : ""} ${i === M.required.length - 1 ? "last" : ""}"><span class="j"><i class="ln t"></i><i class="ln b"></i><span class="gl">${J.field}</span></span><span class="nm">${esc(r.name)}</span><span class="ty"><span class="s6-t"><span class="w">a serializer</span></span></span><span class="doc">${esc(r.doc)}</span><span class="doc" style="display:inline-flex;gap:4px;align-items:center">${outcomeGlyphs(r.sig)}</span></div>`).join("");
  const im = M.implementors;
  return `<section class="s6-sec"><div class="s6-h">What you write<i>${M.required.length}</i><span class="aside">Implement ${M.required.length === 1 ? "this" : "these"}, or derive it</span></div><div class="s6-shape holds">${rq}</div>
    ${im ? `<p class="s6-foot" style="margin-top:12px">${im.total.toLocaleString()} types implement it across ${im.crates} crates on this machine, ${im.derived}% of them by derive.</p>` : ""}</section>`;
}
