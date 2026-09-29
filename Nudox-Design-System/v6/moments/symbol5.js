// The symbol page, redrawn dense (v5 of the page, after the owner's "clever, but blank space, doesn't scale").
// Where it comes from / where it goes stays the idea; the fan of one-token rows becomes a LEDGER: two
// columns of wrapped token flows, weighted by how much your code uses each, meeting at a spine. How it's
// used becomes a REACH MATRIX (who × which member) with its lines, instead of one deck per crate.
// Functions get the lab's plate, family and ways (lab.js).
import { sigil, tok, wordsHtml } from "./symbol.js";
import { plate, ways, shapeData, evidence } from "./lab.js";

const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
const short = (n) => String(n).replace(/^backend-/, "");

export async function symbol5({ SYM, reader, jump, side, Q }) {
  reader.innerHTML = "";
  const root = document.createElement("div"); root.className = "l5-root"; reader.appendChild(root);
  const id = Q.get("id");
  if (id) return fnPage(root, id, { jump, side });
  const pname = Q.get("p") || "toml", sname = Q.get("s") || "Value";
  const M = await SYM.modelOf(pname, sname, Q.get("mod"));
  if (!M) { root.textContent = "no such symbol"; return; }
  jump.innerHTML = `<span class="c">Library</span><span class="rel">›</span><span class="c">${esc(M.P.name)}</span><span class="rel">›</span><span class="md">${esc(M.mod)}</span><span class="rel">›</span><b>${esc(M.sname)}</b>`;
  sideFor(side, M);
  root.innerHTML = `${hero(M)}${M.item.f === "type" ? ledger(M) : ""}${reach(M)}`;
  wire(root, M);
  if (Q.get("sc")) root.scrollTop = +Q.get("sc");
}

// ---------------------------------------------------------------- the hero: compact, one band
function hero(M) {
  const kind = (M.kind || "").toUpperCase();
  const yours = M.yours.length;
  return `<header class="l5-hero">${sigil(M.F, 52)}<div class="t"><div class="k"><b>${esc(kind)}</b> · ${esc(M.P.name)}::${esc(M.mod)}</div><h1>${esc(M.sname)}</h1><p>${esc(M.item.d || "")}</p></div>
    <div class="facts">${M.members ? `<span class="f"><b>${M.members.rows.length}</b> ${esc(M.members.kind === "one of" ? "cases" : "fields")}</span>` : ""}<span class="f"><b>${countMakers(M)}</b> ways to get one</span><span class="f"><b>${countTakers(M)}</b> things to do with it</span>${M.total ? `<span class="f y"><b>${M.total}</b> uses in ${yours} of your crates</span>` : ""}</div></header>`;
}
const countMakers = (M) => (M.makers || []).reduce((s, m) => s + (m.meths ? m.meths.length : 1), 0);
const countTakers = (M) => { const d = M.does || { reads: [], changes: [], consumes: [] }; return d.reads.length + d.changes.length + d.consumes.length + (M.takers || []).reduce((s, t) => s + (t.meths ? t.meths.length : 1), 0) + (M.held || []).length; };

// ---------------------------------------------------------------- the ledger
function useOf(M) {
  const u = new Map();
  for (const y of M.yours) for (const [m, n] of Object.entries(y.members || {})) u.set(m, (u.get(m) || 0) + n);
  return u;
}
function wtok(r, n, max) {
  // a token with its weight underneath: how often your code reaches it
  const w = n ? Math.max(8, Math.round(100 * Math.sqrt(n / max))) : 0;
  return `<span class="l5-w" data-m="${esc(r.n)}">${tok(r)}${n ? `<i class="bar" style="width:${w}%"></i><em>${n}</em>` : ""}</span>`;
}
function conv(m) {
  return `<span class="l5-cv" title="${esc(m.by || "")}"><b>${esc(m.trait)}</b>${m.fails ? `<i class="q">?</i>` : ""}<span class="ins">${wordsHtml(m.ins || [])}</span></span>`;
}
function group(label, body, n, cls = "", cap = 0) {
  if (!n) return "";
  return `<div class="l5-g ${cls}" data-n="${n}"><div class="gh"><span>${esc(label)}</span><i>${n}</i></div><div class="gb ${cap ? "cap" : ""}" style="${cap ? `--cap:${cap}` : ""}">${body}</div>${cap && n > cap ? `<span class="more" tabindex="0">+${n - cap} more</span>` : ""}</div>`;
}
function ledger(M) {
  const U = useOf(M); const max = Math.max(1, ...U.values());
  const mk = M.makers || [];
  const traits = mk.filter((m) => m.trait);
  const parses = traits.filter((m) => ["parse", "deserialize"].includes(m.trait));
  const converts = traits.filter((m) => !["parse", "deserialize"].includes(m.trait));
  const made = mk.filter((m) => m.via);
  const others = mk.filter((m) => m.owner);
  const d = M.does || { reads: [], changes: [], consumes: [] };
  const byUse = (a) => a.slice().sort((x, y) => (U.get(y.n) || 0) - (U.get(x.n) || 0));
  const takers = (M.takers || []);
  const cases = M.members && M.members.kind === "one of" ? M.members.rows : null;
  const L = [
    cases ? group(`is one of ${cases.length}`, cases.map((r) => `<span class="l5-case"><b>${esc(r.n)}</b>${wordsHtml(r.words)}</span>`).join(""), cases.length) : "",
    group("reads one from text", parses.map(conv).join(""), parses.length),
    group("converts from", converts.map(conv).join(""), converts.length),
    group("made by", made.map((m) => `<span class="l5-mk">${tok(m.via)}${m.fails ? `<i class="q">?</i>` : ""}<span class="ins">${wordsHtml(m.ins || [])}</span></span>`).join(""), made.length),
    group("other types give one", others.map((m) => `<span class="l5-own">${tok(m.owner)}${m.meths.map((x) => `<span class="l5-via">${esc(x.n)}</span>`).join("")}</span>`).join(""), others.reduce((s, m) => s + m.meths.length, 0)),
  ].join("");
  const R = [
    group("reads it", byUse(d.reads).map((r) => wtok(r, U.get(r.n) || 0, max)).join(""), d.reads.length, "", 12),
    group("changes it", byUse(d.changes).map((r) => wtok(r, U.get(r.n) || 0, max)).join(""), d.changes.length, "am"),
    group("consumes it", byUse(d.consumes).map((r) => wtok(r, U.get(r.n) || 0, max)).join(""), d.consumes.length, "co"),
    group("takes it", takers.map((t) => t.via ? `<span class="l5-mk">${tok(t.via)}<span class="ins">${esc(t.how)}</span></span>` : `<span class="l5-ow">${tok(t.owner)}<span class="via">${t.meths.map((x) => `<b>${esc(x.n)}</b>`).join("")}</span></span>`).join(""), takers.reduce((s, t) => s + (t.meths ? t.meths.length : 1), 0)),
    group("held in", (M.held || []).map((h) => `<span class="l5-mk">${tok(h.via)}${h.words.length ? `<span class="ins">= ${wordsHtml(h.words)}</span>` : ""}</span>`).join(""), (M.held || []).length),
  ].join("");
  const held = M.members && M.members.kind !== "one of" ? `<div class="l5-cases"><span class="ch">${esc(M.members.kind === "one of" ? `one of ${M.members.rows.length}` : M.members.kind)}</span>${M.members.rows.map((r) => `<span class="c">${r.n ? `<b>${esc(r.n)}</b>` : ""}${wordsHtml(r.words)}</span>`).join("")}</div>` : "";
  return `<section class="l5-ledger"><div class="side l"><div class="sh"><span>where it comes from</span><i>${countMakers(M)}</i></div>${L}</div>
    <div class="spine"><span class="me">${sigil(M.F, 18, "mini")}<b>${esc(M.sname)}</b></span><i class="rule"></i></div>
    <div class="side r"><div class="sh"><span>where it goes</span><i>${countTakers(M)}</i>${M.total ? `<span class="yk"><i class="bar"></i>your code's use</span>` : ""}</div>${R}</div>${held}</section>`;
}

// ---------------------------------------------------------------- the reach matrix: who reaches which member
function reach(M) {
  const rows = [];
  for (const y of M.yours) {
    const mem = { ...(y.members || {}) };
    const sum = Object.values(mem).reduce((a, b) => a + b, 0);
    if (y.n > sum) mem["(itself)"] = y.n - sum;
    rows.push({ name: short(y.y.name), yours: true, n: y.n, mem, lines: y.lines, ml: y.ml || {} });
  }
  for (const o of M.others) {
    const mem = {}; const ml = {};
    for (const l of o.lines) { const m = (String(l[2]).match(/(?:::|\.)((?:as|is|get|try|into|to|insert|push|remove|iter|len)\w*)\b/) || [])[1] || "(itself)"; mem[m] = (mem[m] || 0) + 1; (ml[m] || (ml[m] = [])).push(l); }
    rows.push({ name: o.d.name, yours: false, n: o.lines.length, mem, lines: o.lines, ml, none: !o.lines.length });
  }
  if (!rows.length) return "";
  const cols = new Map();
  for (const r of rows) for (const [m, n] of Object.entries(r.mem)) cols.set(m, (cols.get(m) || 0) + n);
  const C = [...cols.entries()].sort((a, b) => (a[0] === "(itself)") - (b[0] === "(itself)") || b[1] - a[1]).map((x) => x[0]).slice(0, 22);
  const max = Math.max(1, ...rows.flatMap((r) => Object.values(r.mem)));
  const cell = (r, m) => { const n = r.mem[m] || 0; if (!n) return `<span class="x"></span>`; const s = Math.round(5 + 15 * Math.sqrt(n / max)); return `<span class="x on ${r.yours ? "y" : ""}" data-r="${esc(r.name)}" data-m="${esc(m)}"><i style="width:${s}px;height:${s}px"></i><em>${n}</em></span>`; };
  const grid = `grid-template-columns: 170px 44px repeat(${C.length}, minmax(26px, 1fr))`;
  return `<section class="l5-reach"><div class="rh"><b>How it's used</b><span>who reaches which part of it, from their source · ${rows.filter((r) => r.yours).length} of your crates, ${rows.filter((r) => !r.yours).length} other packages · click a square for its lines</span></div>
    <div class="mx"><div class="mr hd" style="${grid}"><span></span><span class="tot">uses</span>${C.map((m) => `<span class="ch" data-m="${esc(m)}"><b>${esc(m)}</b></span>`).join("")}</div>
    ${rows.map((r) => `<div class="mr ${r.yours ? "y" : ""}" style="${grid}"><span class="rn">${esc(r.name)}</span><span class="tot">${r.none ? "names it" : r.n}</span>${C.map((m) => cell(r, m)).join("")}</div>`).join("")}</div>
    <div class="l5-lines"></div></section>`;
}

function wire(root, M) {
  const lines = root.querySelector(".l5-lines");
  const show = (rname, m) => {
    const y = M.yours.find((x) => short(x.y.name) === rname);
    let ls = [];
    if (y) ls = m === "(itself)" ? y.lines.filter((l) => !Object.keys(y.ml || {}).some((k) => l[2].includes(k))) : (y.ml || {})[m] || [];
    else { const o = M.others.find((x) => x.d.name === rname); if (o) ls = o.lines.filter((l) => m === "(itself)" || String(l[2]).includes(m)); }
    lines.innerHTML = `<div class="lh"><b>${esc(rname)}</b> · ${esc(m)} · ${ls.length} line${ls.length === 1 ? "" : "s"}</div>` + ls.slice(0, 8).map((l) => {
      const t = String(l[2]); const i = m !== "(itself)" ? t.indexOf(m) : t.indexOf(M.sname);
      const a = i >= 0 ? t.slice(0, i) : t, b = i >= 0 ? t.slice(i, i + (m !== "(itself)" ? m.length : M.sname.length)) : "", c = i >= 0 ? t.slice(i + b.length) : "";
      return `<div class="lb-kw"><span class="src">${esc(l[0])}:${l[1]}</span><span class="pre"><span>${esc(a)}</span></span><span class="call">${esc(b)}</span><span class="post">${esc(c)}</span></div>`;
    }).join("");
    root.querySelectorAll(".l5-reach .x.pin").forEach((x) => x.classList.remove("pin"));
    const x = root.querySelector(`.l5-reach .x[data-r="${CSS.escape(rname)}"][data-m="${CSS.escape(m)}"]`); if (x) x.classList.add("pin");
    root.querySelectorAll(".l5-w").forEach((w) => w.classList.toggle("lit", w.dataset.m === m));
  };
  root.addEventListener("click", (e) => {
    const x = e.target.closest(".l5-reach .x.on"); if (x) show(x.dataset.r, x.dataset.m);
    const mo = e.target.closest(".l5-g .more"); if (mo) mo.closest(".l5-g").classList.add("open");
  });
  // hover a member in the ledger: its column lights in the matrix, and the reverse
  root.addEventListener("mouseover", (e) => {
    const w = e.target.closest(".l5-w, .l5-reach .ch, .l5-reach .x.on"); const m = w ? w.dataset.m : null;
    root.querySelectorAll(".l5-reach [data-m]").forEach((c) => c.classList.toggle("lit", !!m && c.dataset.m === m));
    root.querySelectorAll(".l5-w").forEach((c) => c.classList.toggle("lit", !!m && c.dataset.m === m));
  });
  const first = M.yours[0]; if (first) { const m = Object.entries(first.members || {}).sort((a, b) => b[1] - a[1])[0]; if (m) show(short(first.y.name), m[0]); }
}

function sideFor(side, M) {
  side.innerHTML = `<div class="scope"><span class="up">‹ ${esc(M.P.name)}</span></div><div class="hd">${esc(M.mod)} <i>${M.sibs.length}</i></div><div class="rows">${M.sibs.map((s) => `<div class="r ${s.it.n === M.sname ? "hot" : ""}">${sigil(s.F, 14, "mini")}${esc(s.it.n)}</div>`).join("")}</div>`;
}

// ---------------------------------------------------------------- a function page from the lab's data
async function fnPage(root, id, { jump, side }) {
  const D = await shapeData(id);
  jump.innerHTML = `<span class="c">Library</span><span class="rel">›</span><span class="c">${esc(D.pkg)}</span><span class="rel">›</span><span class="md">${esc(D.module)}</span><span class="rel">›</span><b>${esc(D.name)}</b>`;
  side.innerHTML = `<div class="scope"><span class="up">‹ ${esc(D.pkg)}</span></div><div class="hd">${esc(D.module)}</div><div class="rows">${(D.family ? D.family.cells : [{ n: D.name }]).map((c) => `<div class="r ${c.n === D.name ? "hot" : ""}">${esc(c.n)}</div>`).join("")}</div>`;
  root.innerHTML = `<header class="l5-hero fn"><div class="t"><div class="k"><b>${esc(D.kind.toUpperCase())}</b> · ${esc(D.pkg)} · ${esc(D.lang)}</div><h1>${esc(D.name)}</h1><p>${esc(D.doc)}</p></div>
    <div class="facts">${D.sites.length ? `<span class="f"><b>${D.sites.length}</b> call sites here</span><span class="f"><b>${new Set(D.sites.map((s) => s.p)).size}</b> ${D.lang === "rust" ? "crates" : "packages"} call it</span>` : ""}${D.sites.some((s) => s.y) ? `<span class="f y"><b>${D.sites.filter((s) => s.y).length}</b> in your code</span>` : ""}</div></header>`;
  root.appendChild(await plate(id, { head: false }));
  if (D.sites.length) root.appendChild(await ways(id, { title: false, rows: 10 }));
  root.appendChild(evidence(D));
  const sc = new URLSearchParams(location.search).get("sc"); if (sc) root.scrollTop = +sc;
}
