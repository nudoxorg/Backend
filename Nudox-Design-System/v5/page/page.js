// page.js: renders one v5 page model (data/pages.js) into a window, then draws its ink:
// the spine, the specimen's strokes, the section rules and the relation stubs, measured
// from the laid-out text exactly as a GPUI canvas would read bounds after layout.
//
// URL: Page.html?p=<slug>&w=1440&h=900&theme=glacier&dir=b&tall=1&hover=<key>&x=1
(() => {
  "use strict";
  const Q = new URLSearchParams(location.search);
  const P = window.PAGES[Q.get("p") || "rs-value"];
  const W = +(Q.get("w") || 1440), H = +(Q.get("h") || 900);
  const TALL = Q.get("tall") === "1", DIR = Q.get("dir") || "a", XRAY = Q.get("x") === "1", HOVER = Q.get("hover") || "";
  const esc = (s) => String(s ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  const md = (s) => "<span>" + esc(s).replace(/`([^`]+)`/g, "<code>$1</code>").replace(/\*\*([^*]+)\*\*/g, "$1").replace(/\*([^*]+)\*/g, "<i>$1</i>") + "</span>";
  const brk = (s) => esc(s).replace(/([a-z0-9])([A-Z])/g, "$1<wbr>$2").replace(/_/g, "_<wbr>");
  const K = window.KINDS || {};
  const FAMV = { ty: "--k-ty", ca: "--k-ca", co: "--k-co", va: "--k-va", ns: "--k-ns" };
  const KIND_GLYPH = { enum: "enum", struct: "struct", fn: "function", method: "method", trait: "trait", interface: "interface", type: "type", "abstract class": "class", package: "package", macro: "macro", const: "constant", module: "module" };
  const glyph = (kind, fam, cls = "") => `<svg class="${cls}" viewBox="0 0 24 24" style="color:var(${FAMV[fam] || "--ink3"})" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="square" stroke-linejoin="miter">${(K[KIND_GLYPH[kind] || kind] || K.unknown || "").replace(/class="f"/g, 'class="f" fill="currentColor" stroke="none" opacity=".45"')}</svg>`;
  function gemSVG(kind, fam, px) {
    const F = ["24,2 35,13 24,10", "24,10 35,13 38,24", "35,13 46,24 38,24", "46,24 35,35 38,24", "38,24 35,35 24,38", "35,35 24,46 24,38", "24,46 13,35 24,38", "24,38 13,35 10,24", "13,35 2,24 10,24", "2,24 13,13 10,24", "10,24 13,13 24,10", "13,13 24,2 24,10"];
    const LIT = [0.4, 0.3, 0.34, 0.14, 0.08, 0.11, 0.22, 0.2, 0.3, 0.56, 0.5, 0.66];
    const g = (K[KIND_GLYPH[kind] || kind] || "").replace(/class="f"/g, 'class="f" fill="currentColor" stroke="none" opacity=".5"');
    return `<svg width="${px}" height="${px}" viewBox="0 0 48 48" style="color:var(${FAMV[fam]})">${F.map((p, k) => `<polygon points="${p}" fill="currentColor" opacity="${LIT[k]}"></polygon>`).join("")}<path d="M24 2 46 24 24 46 2 24z" fill="none" stroke="currentColor" stroke-width="1"></path><path d="M24 10 38 24 24 38 10 24z" fill="var(--table)" stroke="currentColor" stroke-width=".7" stroke-opacity=".55"></path><g transform="translate(17.4 17.4) scale(.55)" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="square" stroke-linejoin="miter">${g}</g></svg>`;
  }
  // the type glyph: the only icon a type has. plain value = stone; named = plate (or diamond for a contract);
  // list = stacked; maybe = dotted; a literal = a warm stone
  function tglyph(tok, mod) {
    const dash = mod === "maybe" ? ' stroke-dasharray="1.2 1.6"' : "";
    const stack = mod === "list";
    const c = tok.t === "prim" ? "var(--ink3)" : tok.t === "lit" ? "var(--lit)" : `var(${FAMV[tok.t] || "--k-ty"})`;
    let shape;
    if (tok.t === "prim" || tok.t === "lit") shape = `<circle cx="5" cy="5" r="3.2" fill="${mod === "maybe" ? "none" : c}" stroke="${c}"${dash}/>`;
    else if (tok.t === "co") shape = `<path d="M5 .8 9.2 5 5 9.2 .8 5z" fill="none" stroke="${c}" stroke-width="1.2"${dash}/>`;
    else if (tok.t === "ca") shape = `<path d="m3 1 4 4-4 4" fill="none" stroke="${c}" stroke-width="1.3"/>`;
    else shape = `<path d="M2.6 1.2H9.2V7.4L7.4 9.2H.8V3z" fill="${mod === "maybe" ? "none" : c}" fill-opacity=".35" stroke="${c}" stroke-width="1.1"${dash}/>`;
    if (stack) shape = `<g transform="translate(2 -2)" opacity=".5">${shape}</g>${shape}`;
    return `<svg class="g" viewBox="0 0 10 10">${shape}</svg>`;
  }
  function tyHTML(toks, opts = {}) {
    if (!toks || !toks.length) return "";
    let mod = null; const head = toks.find((t) => t.t !== "w" && t.t !== "p");
    const w0 = toks[0].t === "w" ? toks[0].s : "";
    if (/^maybe/.test(w0)) mod = "maybe"; else if (/^list of/.test(w0)) mod = "list";
    const parts = toks.map((t) => {
      if (t.t === "w") return `<span class="w">${esc(t.s)}</span>`;
      if (t.t === "p") return `<span class="p">${esc(t.s)}</span>`;
      if (t.t === "prim") return `<span class="prim" title="${esc(t.x || "")}">${esc(t.s)}</span>`;
      if (t.t === "var") return `<span class="var">${esc(t.s)}</span>`;
      if (t.t === "lit") return `<span class="lit">${esc(t.s)}</span>`;
      return `<span class="nm" data-link="${esc(t.s)}">${esc(t.s)}</span>`;
    }).join("");
    return `<span class="ty">${head && !opts.noGlyph && head.t !== "var" ? tglyph(head, mod) : ""}${parts}</span>`;
  }
  const exact = (s) => (XRAY && s ? `<span class="mono" style="color:var(--ink3);font-size:12px;margin-left:10px">${esc(s)}</span>` : "");

  // ---------------------------------------------------------------- geometry
  const narrow = W < 900;
  const shelfW = narrow ? 42 : 264;
  const readerW = W - shelfW;
  const SP = narrow ? 36 : 44; // spine sits this far left of the column
  const colW = Math.min(640, readerW - 2 * 20 - SP);
  const colX = narrow ? 20 + SP : Math.round((readerW - colW) / 2);
  const spineX = colX - SP;
  const marginL = spineX - 24; // room for sources and in-stubs
  const wideMargins = marginL >= 150;

  // ---------------------------------------------------------------- the window chrome (context)
  const theme = Q.get("theme") === "glacier" ? "glacier" : "";
  document.body.className = theme;
  const win = document.getElementById("win");
  win.className = `win ${theme} ${TALL ? "tall" : ""}`;
  win.style.width = W + "px"; if (!TALL) win.style.height = H + "px";
  const jumpPath = P.concept === "package" ? `<b>${esc(P.name)}</b> <span class="sep">·</span> ${esc(P.pkg.version)}` : `${esc(P.pkg.name)} <span class="sep">›</span> ${esc((P.path || "").split(/::|·|\//).filter(Boolean).slice(-1)[0] || "")} <span class="sep">›</span> <b>${esc(P.owner ? P.owner + "." + P.name : P.name)}</b>`;
  const shelfRows = (P.shelf || defaultShelf()).map((r) => `<div class="row ${r.in ? "in" : ""} ${r.cur ? "cur" : ""}">${glyph(r.kind, r.fam)}<span>${esc(r.name)}</span></div>`).join("");
  function defaultShelf() {
    const rows = [];
    if (P.concept === "package") { for (const m of P.mods || []) rows.push({ name: m.mod, kind: "module", fam: "ns" }); return rows; }
    rows.push({ name: (P.path || "").split(/::|·|\//).filter(Boolean).slice(-1)[0] || P.pkg.name, kind: "module", fam: "ns" });
    rows.push({ name: P.owner || P.name, kind: P.owner ? "struct" : P.kind, fam: P.owner ? "ty" : P.fam, in: true, cur: !P.owner });
    if (P.owner) rows.push({ name: P.name, kind: "method", fam: "ca", in: true, cur: true });
    return rows;
  }
  const ecoWord = { "crates.io": "crates.io", npm: "npm", go: "Go modules" }[P.eco] || P.eco;
  win.innerHTML = `<div class="tb"><div class="lights"><i></i><i></i><i></i></div><div class="jump">${jumpPath}</div></div>
    <div class="body">${narrow ? `<div class="spine-rail"><i></i><i></i><i></i></div>` : `<div class="shelf"><div class="up">‹ Library</div><div class="book">${gemSVG("package", "ns", 28)}<div><div class="bn">${esc(P.pkg.name)}</div><div class="bv">${esc(P.pkg.version)}</div></div></div>${shelfRows}</div>`}
    <div class="reader" id="reader"><div class="page" id="page" style="width:${readerW}px"></div></div></div>
    <div class="status">nudox://${esc(P.eco)}/${esc(P.pkg.name)}@${esc(P.pkg.version)}${P.concept === "package" ? "" : "/" + esc(P.name)}</div>`;
  const page = document.getElementById("page");
  const C = (inner, extra = "") => `<div class="col" style="margin-left:${colX}px;width:${colW}px;${extra}">${inner}</div>`;

  // ---------------------------------------------------------------- hero
  const markGlyph = {
    path: '<path d="M1.5 3.5h4l1.5 1.5h7.5v8h-13z"/>', source: '<path d="M3 1.5h6l4 4v9H3z"/><path d="M9 1.5v4h4"/>',
    since: '<path d="M1 12h14M3 12V7M6 12V4M9 12V8M12 12V6"/>', "alias-deprecated": '<path d="M1 8h14"/><path d="M4 4.5h8M4 11.5h8" opacity=".5"/>',
    eco: '<circle cx="8" cy="8" r="6.5"/><path d="M1.5 8h13M8 1.5c2.5 2 2.5 11 0 13M8 1.5c-2.5 2-2.5 11 0 13"/>',
    lic: '<circle cx="5.5" cy="8" r="4.5"/><circle cx="10.5" cy="8" r="4.5"/>',
  };
  const ECO = { "crates.io": "crates.io", npm: "npm", go: "pkg.go.dev" };
  function marksHTML() {
    const ms = [];
    ms.push(`<span class="mk" data-h="pkg">${glyph("package", "ns")}<span class="mono">${esc(P.pkg.name)} ${esc(P.pkg.version)}</span></span>`);
    for (const m of P.marks) {
      if (m.k === "path") ms.push(`<span class="mk" data-h="path"><svg viewBox="0 0 16 16">${markGlyph.path}</svg><span class="mono">${esc(m.text)}</span></span>`);
      else if (m.k === "since") ms.push(`<span class="mk" data-h="since"><svg viewBox="0 0 16 16">${markGlyph.since}</svg><span class="mono">${esc(m.text)}</span></span>`);
      else if (m.k === "alias-deprecated") ms.push(`<span class="mk dep" data-h="alias"><svg viewBox="0 0 16 16">${markGlyph["alias-deprecated"]}</svg><span class="mono">${esc(m.text)}</span></span>`);
    }
    return ms.join("");
  }
  const gemPx = narrow ? 40 : 56;
  function hero() {
    const nm = P.owner ? `<span class="owner">${brk(P.owner)}.</span>${brk(P.name)}` : brk(P.name);
    return C(`<header class="hero" id="hero"><div class="gem" id="gem" style="left:${-SP - gemPx / 2}px;top:-6px">${gemSVG(P.kind, P.fam, gemPx)}</div>
      <h1 class="name">${nm}</h1>${P.lede ? `<p class="lede">${md(P.lede)}</p>` : P.shapeLede ? `<p class="shape-lede" title="no doc comment: the page states its shape">${md(P.shapeLede)}</p>` : ""}<div class="marks">${marksHTML()}</div></header>`);
  }

  // ---------------------------------------------------------------- specimens (direction A: drawn)
  function specChoice(s) {
    const shared = s.shared && s.shared.length ? `<div class="shared" data-a="shared"><span class="lbl">every case</span>${s.shared.map((f) => `<span class="fld">${esc(f.name)}${f.optional ? '<span class="opt">?</span>' : ""}</span>`).join("")}</div>` : "";
    const rows = s.cases.map((c, k) => {
      const pay = c.payload && c.payload.length
        ? (c.payload[0].name !== undefined ? c.payload.map((f) => `<span class="fld">${esc(f.name)}${f.optional ? '<span class="opt">?</span>' : ""}</span>`).join('<span class="sep"></span>') : tyHTML(c.payload[0].type) + exact(c.payload[0].exact))
        : "";
      const first = s.disc ? `<span class="cl" data-a="case-${k}">${esc(c.lit)}</span>` : `<span class="cn" data-a="case-${k}">${esc(c.name)}</span>`;
      const second = s.values ? `<span class="val">${esc(c.lit)}</span>` : `<span class="pl">${pay}</span>`;
      const third = c.reads && c.reads.length ? `<span class="cr">${c.reads.map((m) => `<span class="${m.recv === "changes" ? "chg" : ""}">${esc(m.name)}</span>`).join("")}</span>` : `<span class="cd ${c.quiet ? "quiet" : ""}">${esc(c.doc || "")}</span>`;
      return `<div class="case">${first}${second}${third}</div>`;
    }).join("");
    const cnt = `one of <b>${s.total}</b>${s.disc ? `, told apart by <span class="mono" style="color:var(--ink2)">${esc(s.disc)}</span>` : ""}${s.readonlyAll ? " · all readonly" : ""}${s.values ? ` · each an <span class="mono">${esc(s.underlying)}</span>` : ""}`;
    const open = s.open ? `<div class="note" data-a="open" style="margin-top:4px">${esc(s.open.why)}</div>` : "";
    return `<div class="count">${cnt}</div><div class="fork" id="fork">${shared}${rows}</div>${open}`;
  }
  function specRecord(s) {
    const rung = (f, k, grp) => `<div class="rung" data-opt="${f.optional ? 1 : 0}" data-grp="${grp || ""}"><span class="fn ${f.optional ? "opt" : ""}" data-a="rung-${grp || "own"}-${k}">${esc(f.name)}${f.optional ? '<span class="q">?</span>' : ""}</span><span>${tyHTML(f.type)}${exact(f.exact)}</span><span class="fd">${f.requiredFor && f.requiredFor.length ? "" : esc(f.doc || "")}</span></div>`;
    let html = s.fields.map((f, k) => rung(f, k)).join("");
    for (const e of s.extends || []) html += `<div class="grp" data-a="grp-${esc(e.name)}">from<span class="mono">${esc(e.name)}</span></div>` + e.fields.map((f, k) => rung(f, k, e.name)).join("");
    if (s.private) html += `<div class="priv" data-a="priv">and ${s.private} private</div>`;
    const n = s.fields.length + (s.extends || []).reduce((a, e) => a + e.fields.length, 0);
    const opt = s.fields.filter((f) => f.optional).length;
    const gens = (s.generics || []).map((g) => ` · <span class="ty"><span class="var">${esc(g.name)}</span></span> ${esc(g.words)}`).join("");
    const cnt = `holds <b>${n}</b>${opt ? `, ${opt} optional` : ""}${s.readonlyAll ? " · all readonly" : ""}${s.memberOf ? ` · the <span class="mono" style="color:var(--lit)">${esc(s.memberOf.case)}</span> case of <span class="mono" style="color:var(--ink2)">${esc(s.memberOf.name)}</span>` : ""}${gens}`;
    // a record whose field docs name which forms need them: that is a presence grid, drawn
    let grid = "";
    if (s.forms && s.fields.some((f) => f.requiredFor && f.requiredFor.length)) grid = `<div id="forms" class="forms"></div>`;
    return `<div class="count">${cnt}</div><div class="bracket" id="bracket">${html}</div>${grid}`;
  }
  function specCallable(s) {
    const ins = s.inputs.map((p, k) => `<div class="port ${p.optional ? "opt" : ""}" data-a="in-${k}"><span class="pn">${esc(p.name)}${p.optional ? '<span style="color:var(--ink3)">?</span>' : ""}</span>${tyHTML(p.type)}</div>${p.options ? `<div class="port" style="gap:12px;margin-top:-6px">${p.options.map((o) => `<span class="ph">${esc(o.name)}</span>`).join("")}</div>` : ""}`).join("");
    const gens = (s.generics || []).filter((g) => !g.lifetime);
    const out = s.output
      ? `<div class="gives" data-a="out">${tyHTML(s.output.type)}</div>${gens.length ? `<div class="words">${esc(gens[0].words)}</div>` : s.output.words ? `<div class="words">${esc(s.output.words)}</div>` : ""}`
      : `<div class="words" data-a="out" style="padding-top:2px">${esc(s.nothing || "gives nothing back")}</div>`;
    const recv = s.recv ? `<div class="recv" id="recv">on <span class="mono">${esc(s.recv.name)}</span> ${tyHTML(s.recv.type)} <span style="margin-left:6px">${esc(s.recv.mode)}</span></div>` : "";
    const drops = s.fails.map((f, k) => `<div class="drop" data-a="drop-${k}"><span class="how">${esc(f.how === "Result" ? "or fails with" : f.how === "error" ? "or returns" : f.how)}</span>${tyHTML(f.type)}<span class="when">${esc(f.when || (f.kinds ? `${f.kinds} kinds, below` : ""))}</span></div>`).join("");
    const pol = s.policy ? `<div class="drop" id="policy-lbl"><span class="how" style="color:var(--ink3)">then</span><span class="when">the FlagSet's <span class="mono" style="color:var(--ink1)">ErrorHandling</span> decides</span></div><div class="policy" id="policy">${s.policy.map((p, k) => `<span class="pc" data-a="pol-${k}">${esc(p.case)}</span><span class="pa">${polWords(p)}</span>`).join("")}</div>` : "";
    const alt = s.alt ? `<div class="drop alt" data-a="alt"><span class="how">instead</span><span class="mono" style="color:var(--ink1);font-size:13px">${esc(s.alt.name)}</span><span class="when">${esc(s.alt.words)}</span></div>` : "";
    return `<div class="pipe" id="pipe" style="margin-top:${s.recv ? 36 : 8}px"><div class="ins">${ins}</div><div class="body" id="pbody">${esc(P.name)}${recv}</div><div class="outs">${out}</div></div><div class="drops" id="drops">${drops}${pol}${alt}</div>`;
  }
  function polWords(p) {
    const a = p.action;
    if (a === "return") return "you get it back";
    if (/exit/.test(a)) return `<b>exits 2</b> after printing it${p.note ? '; <span class="mono" style="font-size:12px">exits 0</span> for help' : ""}`;
    if (a === "panic") return "<b>panics</b> with it";
    return esc(a);
  }
  function specContract(s) {
    const words = (m) => (typeof m.words === "string" && m.words ? `<span class="ty"><span class="w">${esc(m.words)}</span></span>` : Array.isArray(m.words) ? tyHTML(m.words) : "") + (m.ret && m.ret.length && m.ret[0].s !== "nothing" ? tyHTML([{ t: "p", s: "→" }, ...m.ret], { noGlyph: true }) : "");
    const req = s.required.map((m, k) => `<div class="mem" data-n="open"><span class="mn" data-a="req-${k}">${esc(m.name)}</span><span class="mw">${words(m)}</span><span class="md">${md(m.say || "")}</span></div>`).join("");
    const prov = s.provided.map((m, k) => `<div class="mem got" data-n="tab"><span class="mn" data-a="prov-${k}">${esc(m.name)}</span><span class="mw"></span><span class="md">${md(m.say || "")}</span></div>`).join("");
    const flds = (s.fields || []).map((f, k) => `<div class="mem got" data-n="field"><span class="mn" data-a="fld-${k}">${esc(f.name)}${f.optional ? '<span style="color:var(--ink3)">?</span>' : ""}</span><span class="mw">${tyHTML(f.type)}</span><span class="md">${md(f.doc || "")}</span></div>`).join("");
    const body = `<div class="grp"><span>you write</span></div>${req}${s.provided.length ? '<div class="grp"><span>you get</span></div>' : ""}${prov}${flds ? '<div class="grp"><span>it holds</span></div>' + flds : ""}`;
    const cnt = `asks for <b>${s.required.length}</b>${s.provided.length ? `, gives <b>${s.provided.length}</b>` : ""}${s.implicit ? " · no type says so: having the methods is enough" : ""}`;
    const g = satGroups(s.sat || {}).length;
    const minH = wideMargins && g ? Math.max(0, (g - 1) * 22 + 120) : 0;
    return `<div class="count">${cnt}</div><div class="outline" id="outline">${body}</div>${minH ? `<div style="height:0" data-fan-min="${minH}"></div>` : ""}`;
  }
  // ---------------------------------------------------------------- specimens (direction B: typeset)
  function tset() {
    const s = P.spec; const k = `var(${FAMV[P.fam]})`;
    if (s.kind === "choice") {
      const lhs = s.shared && s.shared.length ? `${esc(P.name)}` : esc(P.name);
      let pre = s.shared && s.shared.length ? `<div class="fac" style="margin:0 0 6px"><span class="lhs">${lhs}</span> <span class="op" style="--k:${k}">=</span> { ${s.shared.map((f) => esc(f.name) + (f.optional ? "?" : "")).join(", ")} } <span class="op" style="--k:${k}">×</span></div>` : "";
      const rows = s.cases.map((c, i) => `<span class="lhs">${i === 0 && !pre ? esc(P.name) : ""}</span><span class="op" style="--k:${k}">${i === 0 ? (pre ? "(" : "=") : "+"}</span><span class="alt">${s.disc ? `<span class="lit">${esc(c.lit)}</span>` : esc(c.name)}${s.values ? ` <span class="lit">${esc(c.lit)}</span>` : ""}</span><span class="say">${c.payload && c.payload.length ? (c.payload[0].name !== undefined ? "{ " + c.payload.map((f) => esc(f.name) + (f.optional ? "?" : "")).join(", ") + " }" : tyHTML(c.payload[0].type, { noGlyph: true })) : esc(c.doc || "")}</span>`).join("");
      return `<div class="tset">${pre}<div class="eq">${rows}${pre ? `<span></span><span class="op" style="--k:${k}">)</span><span></span><span></span>` : ""}</div></div>`;
    }
    if (s.kind === "record") {
      const fs = [...s.fields, ...(s.extends || []).flatMap((e) => e.fields)];
      return `<div class="tset"><div class="eq">${fs.map((f, i) => `<span class="lhs">${i === 0 ? esc(P.name) : ""}</span><span class="op" style="--k:${k}">${i === 0 ? "=" : "×"}</span><span class="alt">${esc(f.name)}${f.optional ? '<span style="color:var(--ink3)">?</span>' : ""}</span><span class="say">${tyHTML(f.type, { noGlyph: true })}</span>`).join("")}</div></div>`;
    }
    if (s.kind === "callable") {
      const ins = s.inputs.map((p) => `${esc(p.name)}${p.optional ? "?" : ""}: ${tyHTML(p.type, { noGlyph: true })}`).join(", ");
      const out = s.output ? tyHTML(s.output.type, { noGlyph: true }) : "nothing";
      const fails = s.fails.map((f) => `<span class="op" style="--k:var(--coral)">+</span> ${tyHTML(f.type, { noGlyph: true })}`).join(" ");
      return `<div class="tset"><div class="fac" style="line-height:32px"><span class="lhs">${esc(P.name)}</span> <span class="op" style="--k:${k}">:</span> ${s.recv ? tyHTML(s.recv.type, { noGlyph: true }) + " , " : ""}(${ins}) <span class="op" style="--k:${k}">→</span> ${out} ${fails}</div>${(s.generics || []).filter((g) => !g.lifetime).map((g) => `<div class="say">where <span class="ty"><span class="var">${esc(g.name)}</span></span> is ${esc(g.words)}</div>`).join("")}</div>`;
    }
    if (s.kind === "contract") {
      return `<div class="tset"><div class="eq">${s.required.map((m, i) => `<span class="lhs">${i === 0 ? esc(P.name) : ""}</span><span class="op" style="--k:${k}">${i === 0 ? "⊨" : "∧"}</span><span class="alt">${esc(m.name)}</span><span class="say">you write</span>`).join("")}${s.provided.map((m) => `<span class="lhs"></span><span class="op" style="--k:var(--ink3)">+</span><span class="alt" style="color:var(--ink2)">${esc(m.name)}</span><span class="say">you get</span>`).join("")}</div></div>`;
    }
    return "";
  }
  function specimen() {
    const s = P.spec; if (!s) return "";
    const inner = DIR === "b" ? tset() : s.kind === "choice" ? specChoice(s) : s.kind === "record" ? specRecord(s) : s.kind === "callable" ? specCallable(s) : specContract(s);
    const fanMin = s.kind === "contract" && DIR !== "b" && wideMargins ? (satGroups(s.sat || {}).length - 1) * 11 + 100 : 0;
    const xs = XRAY && s.exact ? `<div class="xsrc"><span class="lbl">the exact source, while ⌥ is held</span>${esc(s.exact)}</div>` : "";
    return C(`<section class="spec" id="spec" data-kind="${s.kind}" style="${fanMin ? `min-height:${fanMin}px` : ""}">${inner}${xs}</section>`);
  }

  // ---------------------------------------------------------------- sections
  const head = (sec, count) => `<div class="sec-h" data-a="h-${sec.kind}"><span>${esc(sec.title)}</span></div>`;
  function sectionHTML(sec) {
    const body = ({ getting: getting, calling: calling, does: does, fails: fails, uses: uses, changed: changed, words: words, doing: doing })[sec.kind];
    const inner = body ? body(sec) : "";
    if (!inner) return "";
    return C(`<section class="sec" data-kind="${sec.kind}" data-dir="${sec.dir || ""}" id="sec-${sec.kind}">${head(sec)}${inner}</section>`);
  }
  function railHTML(r, k) {
    const steps = r.steps.map((s) => `<span class="step" data-fam="${s.fam}" data-fails="${s.fails ? 1 : 0}" data-maybe="${s.maybe ? 1 : 0}">${s.on ? `<span style="color:var(--ink3);font-weight:400">${esc(s.on)}.</span>` : ""}${esc(s.name)}${s.fails ? '<span class="q">?</span>' : ""}</span>`).join("");
    const lands = r.lands ? `<span class="lands">${r.lands.case ? `<span class="case">${esc(r.lands.case)}</span>` : ""}<span class="why">${esc(r.lands.why || "")}</span></span>` : "";
    const src = `<span class="src" data-a="src-${k}">${tyHTML(r.from)}</span>`;
    return `<div class="rail ${r.lead ? "lead" : ""} ${r.trap ? "trap" : ""}" data-a="rail-${k}" style="${wideMargins ? "" : "padding-left:0"}">${wideMargins ? src : ""}${!wideMargins ? `<span class="src-in" style="margin-right:48px">${tyHTML(r.from)}</span>` : ""}${steps}${lands}</div>${r.lead && r.code ? `<div class="code" data-a="code-${k}">${esc(r.code)}</div>` : ""}`;
  }
  function getting(sec) {
    if (sec.pick && !sec.routes.length) return `<div class="note">${md(sec.note)}</div>`;
    let h = sec.routes.map(railHTML).join("");
    if (sec.wraps && sec.wraps.length) h += `<div class="wraps" data-a="wraps"><span>wraps, never fails:</span>${sec.wraps.map((w) => `<span class="wr">${tyHTML(w.from, { noGlyph: true })}${w.also && w.also.length ? `<span style="color:var(--ink3)">+${w.also.length}</span>` : ""}<span class="p" style="color:var(--ink3)">→</span><span class="mono">${esc(w.case)}</span></span>`).join("")}</div>`;
    if (sec.more) { const shown = new Set(sec.routes.flatMap((r) => r.steps.map((x) => (x.on ? x.on + "." : "") + x.name))); const rest = (sec.more.names || []).filter((n) => !shown.has(n)); h += `<div class="more">and ${sec.more.n} more that give one back${rest.length ? ": " : ""}<span class="mono">${esc(rest.slice(0, 4).join(", "))}</span></div>`; }
    if (sec.note) h += `<div class="note">${md(sec.note)}</div>`;
    return h;
  }
  function calling(sec) {
    if (sec.routes) return sec.routes.map(railHTML).join("");
    if (sec.pick) {
      const p = sec.pick;
      return `<div class="pick"><span class="ty"><span class="var">${esc(p.param)}</span></span><span class="why" style="color:var(--ink2)">is your choice: any of <b style="color:var(--ink1);font-weight:500">${p.n}</b> types here, <span style="color:var(--mint)">${p.yours} yours</span></span></div>
        <div class="verbs" style="margin-top:4px"><div class="vr"><span class="vb">yours</span><span class="vn"></span><span class="vs">${p.names.map((n) => `<span class="who y">${glyph("struct", "ty")}${esc(n)}</span>`).join("")}<span class="rest">and ${p.yours - p.names.length} more</span></span></div></div>
        <div class="wraps" style="margin-top:14px">${p.how.map((h) => `<span class="wr"><span>${esc(h.way)}:</span><span class="mono">${esc(h.code)}</span>${h.note ? `<span>${esc(h.note)}</span>` : ""}</span>`).join("")}</div>`;
    }
    if (sec.chain) return `<div class="note" style="margin-top:0">Every schema has it: <code>z.string().parse(x)</code>, <code>z.object({…}).parse(x)</code>.</div>`;
    return "";
  }
  const diamondSVG = (arrival) => {
    const d = "M7 1 13 7 7 13 1 7z";
    const c = "var(--k-co)";
    if (arrival === "derived") return `<svg viewBox="0 0 14 14"><path d="${d}" fill="none" stroke="${c}" stroke-width="1.2"/></svg>`;
    if (arrival === "blanket") return `<svg viewBox="0 0 14 14"><path d="${d}" fill="none" stroke="${c}" stroke-width="1.2" stroke-dasharray="2.5 2"/></svg>`;
    if (arrival === "auto") return `<svg viewBox="0 0 14 14"><path d="${d}" fill="none" stroke="${c}" stroke-width="1.2" stroke-dasharray="1 2"/></svg>`;
    return `<svg viewBox="0 0 14 14"><path d="${d}" fill="${c}" fill-opacity=".55" stroke="${c}" stroke-width="1.2"/></svg>`;
  };
  function does(sec) {
    if (sec.decides) return `<div class="note" style="margin-top:0">Decides ${esc(sec.decides.what)}: see <span class="mono" style="color:var(--ink1)">FlagSet.Parse</span>. Taken by <span class="mono" style="color:var(--ink1)">${sec.takers.map(esc).join(", ")}</span>.</div>`;
    if (sec.tellApart) return `<div class="note" style="margin-top:0">Nothing: it is data. Tell the ${sec.tellApart.n} cases apart with <code>switch (issue.${esc(sec.tellApart.by)})</code>; TypeScript narrows each branch to its shape.</div>`;
    let h = "";
    if (sec.onCases) h += `<div class="note" style="margin:0 0 16px">${sec.onCases} more read one case each: they sit on its tine above.</div>`;
    if (sec.groups && sec.groups.length) {
      h += `<div class="caps" style="grid-template-columns:repeat(${narrow ? 2 : Math.min(3, sec.groups.length)},1fr)">${sec.groups.map((g) => `<div class="cap-g"><div class="gh">${glyph(P.kind, P.fam, "")}<span>${esc(g.word)}</span></div>${g.members.map((m) => `<div class="m"><span class="mn">${esc(m.name)}</span>${m.result ? tyHTML(m.result) : ""}</div>`).join("")}</div>`).join("")}</div>`;
    }
    const ts = (sec.traits || []).filter((t) => !t.usual && !(t.name === "IndexMut" && (sec.traits || []).some((x) => x.name === "Index")));
    if (ts.length) h += `<div class="tiles" style="${sec.groups && sec.groups.length ? "" : "margin-top:0;"}grid-template-columns:repeat(${narrow ? 2 : 3},1fr)">${ts.map((t) => `<div class="tile">${diamondSVG(t.arrival)}<span class="tv">${esc(t.verb)}${t.n > 1 ? `<span style="color:var(--ink3)"> ×${t.n}</span>` : ""}</span>${XRAY ? `<span class="tn">${esc(t.name)}</span>` : ""}${t.panics ? '<span class="pan">can panic</span>' : ""}</div>`).join("")}${sec.usual ? `<div class="tile usual">${diamondSVG("derived")}<span class="tv">and the usual ${sec.usual.n}</span></div>` : ""}</div>`;
    return h;
  }
  function fails(sec) {
    if (sec.roots) return `<div class="ftree" id="ftree">${sec.roots.map((r, k) => `<div class="fr"><span class="op" data-a="fop-${k}">${esc(r.op)}</span><span class="how">${esc(r.how)}${r.err ? tyHTML(r.err.words) : ""}</span><span class="what">${esc(r.what || "")}${r.instead ? `<span class="instead">instead <span class="mono">${esc(r.instead.name)}</span> ${tyHTML(r.instead.words)}</span>` : ""}</span></div>`).join("")}</div>`;
    if (sec.kinds && sec.err) {
      // a callable failing with one error type that has kinds
      return `<div class="kinds" id="kinds">${sec.kinds.map((k, i) => {
        const shown = k.causes.slice(0, narrow ? 3 : 5);
        return `<div class="${k.never ? "never" : ""}" style="display:contents"><span class="kn" data-a="kind-${i}">${esc(k.name)}</span><span class="kc"><span class="kd">${esc(k.doc)}${k.never ? ` — never from <code>from_str</code>` : ""}</span>${shown.map((c) => `<span class="c">${esc(c)}</span>`).join("")}${k.causes.length > shown.length ? `<span class="c more">and ${k.causes.length - shown.length} more</span>` : ""}</span></div>`;
      }).join("")}</div><div class="tells">tell them apart <span class="mono">${esc(sec.tells.how)}</span><span>where <span class="mono">${esc(sec.tells.also.join(" · "))}</span></span></div>`;
    }
    if (sec.throws) return `<div class="kinds" id="kinds">${sec.throws.map((t, i) => `<div style="display:contents"><span class="kn" data-a="kind-${i}">${esc(t.cls)}</span><span class="kc"><span class="kd">${t.issues ? 'its <span class="mono">issues</span>: a list of <span class="mono">$ZodIssue</span>, one of 11 kinds' : "a check or transform returned a Promise"}</span>${t.message ? `<span class="c">“${esc(t.message)}”</span>` : ""}<span class="c" style="color:var(--ink3)">${esc(t.line)}</span></span></div>`).join("")}</div>`;
    if (sec.kinds) return `<div class="kinds" id="kinds">${sec.kinds.map((k, i) => `<div style="display:contents"><span class="kn" data-a="kind-${i}">${k.sentinel ? '<span style="color:var(--ink3);font-weight:400">var&nbsp;</span>' : '<span style="color:var(--ink3);font-weight:400">*</span>'}${esc(k.name)}</span><span class="kc">${k.messages.map((m) => `<span class="c">${esc(m)}</span>`).join("")}${k.wraps && sec.through ? `<span class="c" style="font-family:var(--ui);color:var(--ink2)">wraps what your <span class="mono">${esc(sec.through.via)}</span> returned</span>` : ""}</span></div>`).join("")}</div><div class="tells">tell them apart <span class="mono">${esc(sec.tells.how)}</span><span class="mono">${esc(sec.tells.sentinel)}</span></div>`;
    return "";
  }
  function siteHTML(s) {
    const raw = ((s.code || [])[0] || "").replace(/\u0001|\u0002/g, "");
    const at = raw.search(new RegExp(`\\b${P.name.replace(/\$/g, "\\$")}\\b`));
    const cut = at > 26 ? "…" + raw.slice(at - 22) : raw;
    const code = esc(cut).replace(new RegExp(`\\b(${esc(P.name).replace(/\$/g, "\\$")})\\b`), "<b>$1</b>");
    return `<div class="site ${s.yours ? "y" : ""}"><div class="sc">${esc(s.caller)}</div><div class="sw">${esc(s.pkg)} · ${esc(s.where)}</div>${code ? `<div class="sx">${code}</div>` : ""}</div>`;
  }
  function uses(sec) {
    let h = "";
    if (sec.none && !(sec.sites || []).length && !(sec.verbs || []).length) return `<div class="none">${md(sec.none)}</div>`;
    if (sec.usedIn) h += `<div class="verbs" style="margin-top:0"><div class="vr"><span class="vb">yours</span><span class="vn"><span class="y">0</span></span><span class="vs"><span class="rest">none of your code names it</span></span></div>${sec.usedIn.map((u) => `<div class="vr"><span class="vb">${esc(u.file.split("/").pop().replace(/\.d\.ts$/, ""))}</span><span class="vn">${u.n}</span><span class="vs"><span class="rest">mentions in yaml, matched by name</span></span></div>`).join("")}</div>`;
    if (sec.mentions) h += `<div class="verbs" style="margin-top:0"><div class="vr"><span class="vb">yours</span><span class="vn"><span class="y">${sec.mentions.yours.length}</span></span><span class="vs"><span class="rest">none of your code names it</span></span></div><div class="vr"><span class="vb">toml_edit</span><span class="vn">${sec.mentions.toml_edit}</span><span class="vs"><span class="rest">mentions, matched by name</span></span></div><div class="vr"><span class="vb">toml</span><span class="vn">${sec.mentions.toml}</span><span class="vs"><span class="rest">mentions, matched by name</span></span></div></div>`;
    const sites = (sec.sites || []).slice(0, narrow ? 2 : 3);
    if (sites.length) h += `<div class="sites" style="grid-template-columns:repeat(${narrow ? 1 : Math.min(3, Math.max(2, sites.length))},1fr)">${sites.map(siteHTML).join("")}</div>`;
    if (sec.verbs && sec.verbs.length) h += `<div class="verbs" style="${sites.length ? "" : "margin-top:0"}">${sec.verbs.map((v) => {
      const ents = v.bands.flatMap((b) => b.pkgs.flatMap((p) => (p.ids || []).map((e) => ({ ...e, band: b.band })))).slice(0, narrow ? 2 : 3);
      const shown = ents.length; const rest = v.n - shown;
      return `<div class="vr"><span class="vb">${esc(v.verb)}</span><span class="vn">${v.n}${v.yours ? ` · <span class="y">${v.yours} yours</span>` : ""}</span><span class="vs">${ents.map((e) => `<span class="who ${e.yours ? "y" : ""}">${glyph(e.kind === "fn" ? "function" : e.kind, { struct: "ty", enum: "ty", trait: "co", function: "ca", method: "ca", fn: "ca", field: "va", type: "ty" }[e.kind] || "ty")}${esc(e.name)}</span>`).join("")}${rest > 0 ? `<span class="rest">and ${rest} more${v.bands.length > 1 ? ` in ${new Set(v.bands.flatMap((b) => b.pkgs.map((p) => p.pkg))).size} packages` : ""}</span>` : ""}</span></div>`;
    }).join("")}</div>`;
    if (sec.none) h += `<div class="none" style="margin-top:12px">${md(sec.none)}</div>`;
    return h;
  }
  function doing(sec) {
    const d = P.spec.doing || [];
    let h = d.map((x, k) => `<div class="rail" data-a="rail-${k}">${wideMargins ? `<span class="src" data-a="src-${k}"><span class="ty"><span class="w">your type</span></span></span>` : ""}<span class="step" data-fam="co">${esc(x.code)}</span><span class="lands"><span class="why">${esc(x.how === "derive" ? "derived: most do it this way" : x.how === "write" ? "written by hand" : x.note || "")}</span></span></div>`).join("");
    const sites = (sec.sites || []).slice(0, 3);
    if (sites.length) h += `<div class="sites" style="margin-top:16px;grid-template-columns:repeat(${narrow ? 1 : 3},1fr)">${sites.map((s) => `<div class="site y"><div class="sc">${esc(s.caller)}</div><div class="sw">${esc(s.pkg)} · ${esc(s.where)}</div><div class="sx">${(() => { const raw = s.code[0] || ""; const at = raw.indexOf(P.name); const cut = at > 18 ? "…" + raw.slice(at - 14) : raw; return esc(cut).replace(new RegExp("(" + P.name + ")"), "<b>$1</b>"); })()}</div></div>`).join("")}</div>`;
    return h;
  }
  function changed(sec) {
    if (!sec.since) return sec.unknown ? `<div class="none">${esc(sec.unknown)}</div>` : "";
    const s = sec.since;
    const plain = (sig) => (sig || "").replace(/'\w+\s*/g, "").replace(/\s+/g, "");
    const rows = s.changed.map((c) => {
      const what = c.sample.length ? c.sample.map((x) => `<span class="mono">${esc(x.path.split("::").pop())}</span> ${plain(x.before) === plain(x.after) ? "reads the same in plain words; only a lifetime moved" : "changed its signature"}`).join("; ") : `${c.added ? plural(c.added, "member") + " added" : ""}${c.changed ? plural(c.changed, "change") : ""}`;
      return `<span class="cv" style="${c.v === s.pinned ? "color:var(--mint)" : ""}">${esc(c.v.replace(/\+.*$/, ""))}${c.v === s.pinned ? ' <span style="font:400 12px var(--ui);color:var(--ink3)">you pin</span>' : ""}</span><span class="ct">${c.breaking && !c.sample.every((x) => plain(x.before) === plain(x.after)) ? "breaking: " : ""}${what}</span>`;
    }).join("");
    return `<div class="comb" id="comb"></div><div class="comb-rows"><span class="cv">≤ ${esc(s.oldestRead)}</span><span class="ct">here since at least this release, the oldest of the ${s.read.length} read</span>${rows}</div>`;
  }
  function plural(n, one) { return `${n} ${n === 1 ? one : one + "s"}`; }
  function inline(t) {
    return esc(t).replace(/`([^`]+)`/g, "<code>$1</code>").replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>").replace(/\[([^\]]+)\]\([^)]*\)/g, "$1").replace(/\[([^\]]+)\]\[[^\]]*\]/g, "$1").replace(/\[(<code>[^\]]+<\/code>)\]/g, "$1").replace(/\[([^\]]+)\]/g, "$1").replace(/\*([^*\s][^*]*)\*/g, "<i>$1</i>");
  }
  function markdown(src) {
    const L = src.replace(/\r/g, "").split("\n"); const out = []; let i = 0;
    while (i < L.length) {
      const l = L[i];
      if (/^\s*$/.test(l)) { i++; continue; }
      if (/^\[[^\]]+\]:/.test(l.trim())) { i++; continue; }
      const fence = l.match(/^\s*(>\s*)?```/);
      if (fence) { const q = !!fence[1]; const body = []; i++; while (i < L.length && !/^\s*(>\s*)?```/.test(L[i])) { body.push(L[i].replace(/^\s*>\s?/, q ? "" : "$&").replace(/^# /, "")); i++; } i++; out.push(`<pre>${esc(body.filter((x) => !/^\s*# /.test(x)).join("\n"))}</pre>`); continue; }
      if (/^\s*\|/.test(l)) { const rows = []; while (i < L.length && /^\s*\|/.test(L[i])) { rows.push(L[i]); i++; } const cells = rows.filter((r) => !/^\s*\|\s*-/.test(r)).map((r) => r.trim().replace(/^\||\|$/g, "").split("|").map((c) => c.trim())); out.push(`<table>${cells.map((r, k) => `<tr>${r.map((c) => (k ? `<td>${inline(c)}</td>` : `<th>${inline(c)}</th>`)).join("")}</tr>`).join("")}</table>`); continue; }
      if (/^\s*>/.test(l)) { const q = []; while (i < L.length && /^\s*>/.test(L[i]) && !/```/.test(L[i])) { q.push(L[i].replace(/^\s*>\s?/, "")); i++; } out.push(`<blockquote>${markdown(q.join("\n"))}</blockquote>`); continue; }
      if (/^\s*\d+\.\s/.test(l)) { const it = []; while (i < L.length && (/^\s*\d+\.\s/.test(L[i]) || (/^\s{2,}\S/.test(L[i]) && it.length))) { if (/^\s*\d+\.\s/.test(L[i])) it.push(L[i].replace(/^\s*\d+\.\s/, "")); else it[it.length - 1] += " " + L[i].trim(); i++; } out.push(`<ol>${it.map((x) => `<li>${inline(x)}</li>`).join("")}</ol>`); continue; }
      if (/^#+\s/.test(l)) { out.push(`<h4>${inline(l.replace(/^#+\s/, ""))}</h4>`); i++; continue; }
      const para = []; while (i < L.length && !/^\s*$/.test(L[i]) && !/^\s*(\||>|```|\d+\.\s)/.test(L[i])) { para.push(L[i].trim()); i++; }
      if (para.length) out.push(`<p>${inline(para.join(" "))}</p>`); else i++;
    }
    return out.join("");
  }
  function words(sec) {
    if (!sec.blocks || !sec.blocks.length) return "";
    const bl = sec.blocks.filter((b) => b.s && b.s.replace(/\s+/g, " ").trim() !== (P.lede || "").trim());
    if (!bl.length) return "";
    return `<div class="words">${bl.map((b) => (b.t === "md" ? markdown(b.s) : b.t === "code" ? `<pre>${esc(b.s)}</pre>` : b.t === "h" ? `<h4>${esc(b.s)}</h4>` : b.t === "quote" ? `<blockquote>${md(b.s)}</blockquote>` : `<p>${md(b.s)}</p>`)).join("")}</div>`;
  }

  // ---------------------------------------------------------------- the package page
  function packagePage() {
    const deps = P.deps.map((d) => `<span class="mk" data-h="dep-${esc(d.name)}"><svg viewBox="0 0 16 16"><path d="M8 1.5 14.5 8 8 14.5 1.5 8z" fill="${d.optional && !d.on ? "none" : "currentColor"}" fill-opacity=".5"/></svg><span class="mono" style="${d.optional && !d.on ? "" : "color:var(--ink1)"}">${esc(d.name)}</span></span>`).join("");
    const heroP = C(`<header class="hero" id="hero"><div class="gem" id="gem" style="left:${-SP - gemPx / 2}px;top:-6px">${gemSVG("package", "ns", gemPx)}</div><h1 class="name">${esc(P.name)}</h1><p class="lede">${md(P.lede)}</p>
      <div class="marks"><span class="mk"><svg viewBox="0 0 16 16">${markGlyph.eco}</svg><span>${esc(ECO[P.eco])}</span></span><span class="mk"><svg viewBox="0 0 16 16">${markGlyph.lic}</svg><span>${esc(P.pkg.license)}</span></span><span class="mk" style="gap:6px"><span>on</span></span>${deps}</div>
      <div class="comb" id="pcomb" style="margin-top:20px;height:44px"></div></header>`);
    const tour = C(`<section class="sec" data-kind="tour" id="sec-tour"><div class="sec-h" data-a="h-tour"><span>Start here</span></div><div class="tour" id="tour">${P.tour.map((t, k) => `<div class="stop" data-a="stop-${k}"><div class="sr">${esc(t.role)}</div><div class="sn">${esc(t.name)}</div><div class="ss">${esc(t.say)}</div></div>`).join("")}</div></section>`);
    const mods = P.mods.filter((m) => m.items.length).map((m) => ({ ...m, items: dedupe(m.items).filter((it) => !(m.mod === "macros" && it.reexport)) }));
    // a type case: one tray, cut into compartments sized by public items, packed into rows
    const sorted = mods.slice().sort((x, y) => y.items.length - x.items.length);
    const nrows = narrow ? sorted.length : Math.max(1, Math.round(Math.sqrt(sorted.length) / 1.1));
    const total = sorted.reduce((a2, m) => a2 + m.items.length, 0); const per = total / nrows;
    const rows = []; let cur = [], sum = 0;
    for (const m of sorted) { if (sum >= per * 0.9 && rows.length < nrows - 1) { rows.push(cur); cur = []; sum = 0; } cur.push(m); sum += m.items.length; }
    if (cur.length) rows.push(cur);
    const cell = (m) => `<div class="cmp" data-a="mod-${esc(m.mod)}" style="flex:${m.items.length} 1 0"><div class="mh"><span class="mnm">${esc(m.mod)}</span><span class="mc">${m.items.length}${m.yours ? ` · <span class="y">yours</span>` : ""}</span></div><div class="stones">${m.items.map((it) => `<span class="it ${it.yours ? "y" : ""}">${glyph(it.kind, it.fam)}${esc(it.name)}${it.yours ? `<span class="yc">${it.yours}</span>` : ""}</span>`).join("")}</div></div>`;
    const tray = C(`<section class="sec" data-kind="tray" id="sec-tray"><div class="sec-h" data-a="h-tray"><span>Its territory</span></div><div class="tcase" id="tray">${rows.map((r) => `<div class="trow">${r.map(cell).join("")}</div>`).join("")}</div></section>`);
    const yours = mods.flatMap((m) => m.items.filter((i) => i.yours).map((i) => ({ ...i, mod: m.mod })));
    const used = C(`<section class="sec" data-kind="uses" data-dir="out" id="sec-uses"><div class="sec-h" data-a="h-uses"><span>Who uses it</span></div><div class="verbs" style="margin-top:0"><div class="vr"><span class="vb">yours</span><span class="vn"><span class="y">desktop</span></span><span class="vs">${yours.map((i) => `<span class="who y">${glyph(i.kind, i.fam)}${esc(i.name)}</span>`).join("")}<span class="rest">7 of your items reach it, all in desktop</span></span></div><div class="vr"><span class="vb">elsewhere</span><span class="vn">3</span><span class="vs"><span class="who">local-service</span><span class="who">engine</span><span class="who">advisory</span></span></div></div></section>`);
    page.innerHTML = heroP + tour + tray + used;
  }
  function dedupe(xs) { const seen = new Set(); return xs.filter((x) => (seen.has(x.name) ? false : seen.add(x.name))); }

  // ---------------------------------------------------------------- assemble
  if (P.concept === "package") packagePage();
  else page.innerHTML = hero() + specimen() + P.sections.map(sectionHTML).join("");

  // ================================================================= INK: measured strokes
  const css = (v) => getComputedStyle(win).getPropertyValue(v).trim();
  function fit() {
    for (const vs of page.querySelectorAll(".verbs .vs")) {
      const room = vs.getBoundingClientRect().right;
      const who = [...vs.querySelectorAll(".who")]; const rest = vs.querySelector(".rest");
      let dropped = 0;
      while (who.length > 1 && (who[who.length - 1].getBoundingClientRect().right > room - (rest ? 110 : 0))) { who.pop().remove(); dropped++; }
      if (dropped && rest) rest.textContent = rest.textContent.replace(/and (\d+)/, (m, n) => `and ${+n + dropped}`);
      else if (dropped) vs.insertAdjacentHTML("beforeend", `<span class="rest">and ${dropped} more</span>`);
    }
  }
  function ink() {
    fit();
    const pr = page.getBoundingClientRect();
    const box = (el) => { const r = el.getBoundingClientRect(); return { x: r.left - pr.left, y: r.top - pr.top, w: r.width, h: r.height, r: r.right - pr.left, b: r.bottom - pr.top, cx: r.left - pr.left + r.width / 2, cy: r.top - pr.top + r.height / 2 }; };
    const $ = (sel) => page.querySelector(sel), $$ = (sel) => [...page.querySelectorAll(sel)];
    const kc = css(FAMV[P.fam]), hair = css("--line2"), hair3 = css("--line3"), coral = css("--coral"), mint = css("--mint"), ink3 = css("--ink3"), ink4 = css("--ink4"), peri = css("--k-ca"), co = css("--k-co"), ty = css("--k-ty");
    const out = [];
    const L = (x1, y1, x2, y2, c, w = 1, dash = "") => out.push(`<path d="M${x1} ${y1}L${x2} ${y2}" stroke="${c}" stroke-width="${w}" fill="none" ${dash ? `stroke-dasharray="${dash}"` : ""} shape-rendering="${x1 === x2 || y1 === y2 ? "crispEdges" : "geometricPrecision"}"/>`);
    const Pa = (d, c, w = 1, dash = "", fill = "none", op = 1) => out.push(`<path d="${d}" stroke="${c}" stroke-width="${w}" fill="${fill}" ${dash ? `stroke-dasharray="${dash}"` : ""} opacity="${op}"/>`);
    const alpha = (c, a) => { const m = c.match(/^#(..)(..)(..)$/); return m ? `rgba(${parseInt(m[1], 16)},${parseInt(m[2], 16)},${parseInt(m[3], 16)},${a})` : c; };
    const sx = spineX + 0.5;
    const gem = $("#gem") && box($("#gem"));
    const gemBottom = gem ? gem.b + 2 : 0;
    // ---- section heads: a mark on the spine, the rule from it, and the stub into the margin
    const heads = $$(".sec-h");
    const lastY = heads.length ? box(heads[heads.length - 1]).cy : gemBottom + 40;
    // the spine: the node's body, drawn through the whole page
    L(sx, gemBottom, sx, lastY, alpha(kc, 0.28), 1);
    const secMark = (kind, x, y) => {
      const c = kind === "fails" ? coral : kc;
      const shapes = {
        getting: `M${x - 5} ${y}h10M${x + 1} ${y - 4}l4 4-4 4`, calling: `M${x - 5} ${y}h10M${x + 1} ${y - 4}l4 4-4 4`, doing: `M${x - 5} ${y}h10M${x + 1} ${y - 4}l4 4-4 4`,
        does: `M${x - 5} ${y - 5}h4v4h-4zM${x + 1} ${y - 5}h4v4h-4zM${x - 5} ${y + 1}h4v4h-4zM${x + 1} ${y + 1}h4v4h-4z`,
        fails: `M${x} ${y - 6}v5M${x} ${y - 1}l-5 6M${x} ${y - 1}l5 6`,
        uses: `M${x - 5} ${y}h4M${x - 1} ${y}l6-5M${x - 1} ${y}h6M${x - 1} ${y}l6 5`,
        changed: `M${x - 6} ${y + 5}h12M${x - 5} ${y + 5}v-4M${x - 1} ${y + 5}v-9M${x + 3} ${y + 5}v-6`,
        words: `M${x - 5} ${y - 5}h10M${x - 5} ${y - 1}h10M${x - 5} ${y + 3}h6`,
        tour: `M${x - 6} ${y}h12M${x - 3} ${y - 3}v6M${x + 3} ${y - 3}v6`, tray: `M${x - 6} ${y - 5}h12v10h-12zM${x - 2} ${y - 5}v10M${x - 6} ${y}h4`,
      };
      out.push(`<rect x="${x - 8}" y="${y - 8}" width="16" height="16" fill="${css("--g1")}"/>`);
      Pa(shapes[kind] || `M${x - 4} ${y}h8`, c, 1.4);
    };
    for (const h of heads) {
      const sec = h.closest(".sec"); const kind = sec.dataset.kind; const dir = sec.dataset.dir;
      const b = box(h); const t = box(h.firstElementChild); const y = Math.round(b.cy) + 0.5;
      secMark(kind, sx, y);
      L(t.r + 14, y, colX + colW, y, hair, 1);
      const model = (P.sections || []).find((s) => s.kind === kind) || {};
      const count = model.count || {};
      const tier = count.tier === "name" || model.tier === "name";
      const lab = kind === "doing" && wideMargins ? "" : count.label || (kind === "tray" ? `${P.mods.reduce((a, m) => a + m.items.length, 0)} public items` : kind === "uses" && P.concept === "package" ? "7 yours" : "");
      if (dir === "in" && wideMargins) {
        L(16, y, sx - 10, y, alpha(kc, 0.55), 1.5, tier ? "5 4" : "");
        if (lab) out.push(`<foreignObject x="16" y="${y - 22}" width="${sx - 32}" height="18"><div class="stub" style="position:static">${esc(lab)}</div></foreignObject>`);
      } else if (dir === "in") {
        if (lab) out.push(`<foreignObject x="${colX + colW - 200}" y="${y - 8}" width="200" height="18"><div class="stub" style="position:static;text-align:right;background:${css("--g1")};padding-left:8px;float:right">${esc(lab)}</div></foreignObject>`);
      }
      if (dir === "out") {
        const x0 = colX + colW, x1 = readerW - 16;
        if (x1 - x0 > 60) {
          L(x0 + 8, y, x1, y, alpha(kind === "fails" ? coral : kc, 0.55), 1.5, tier ? "5 4" : "");
          if (lab) { const yl = count.yours ? lab.replace(/(\d+ yours)/, '<span class="y">$1</span>') : esc(lab); out.push(`<foreignObject x="${x0 + 12}" y="${y - 22}" width="${x1 - x0 - 12}" height="18"><div class="stub" style="position:static;text-align:right">${yl}</div></foreignObject>`); }
        } else if (lab) out.push(`<foreignObject x="${colX + colW - 220}" y="${y - 8}" width="220" height="18"><div class="stub" style="position:static;text-align:right"><span style="background:${css("--g1")};padding-left:8px">${esc(lab)}</span></div></foreignObject>`);
      }
    }
    // ---- specimen ink
    const spec = $("#spec"); const kind = spec && spec.dataset.kind;
    if (DIR !== "b" && kind === "choice") {
      const rows = $$("#fork [data-a^=case-]");
      const shared = $("#fork [data-a=shared]");
      let top = gemBottom;
      if (shared) { const s = box(shared); const y = Math.round(s.cy) + 0.5; L(sx, top, sx, y, kc, 1.5); L(sx, y, colX - 10, y, kc, 1); Pa(`M${colX - 14} ${y - 3}h6v6h-6z`, kc, 1, "", kc); top = y; }
      const ys = rows.map((r) => Math.round(box(r).cy) + 0.5);
      const last = ys[ys.length - 1];
      L(sx, top, sx, last - 9, kc, 1.5);
      for (const y of ys) Pa(`M${sx} ${y - 9}L${sx + 9} ${y}H${colX - 10}`, kc, 1.2);
      if (P.spec.open) { const o = box($("[data-a=open]")); L(sx, last - 9, sx, o.cy, kc, 1.2, "3 3"); L(sx, o.cy, colX - 10, o.cy, kc, 1, "3 3"); }
      if (P.spec.values) for (const el of $$("#fork .val")) { const b = box(el); out.push(`<circle cx="${b.x - 10}" cy="${b.cy}" r="3.5" fill="${css("--k-va")}"/>`); }
    }
    if (DIR !== "b" && kind === "record") {
      const rungs = $$("#bracket [data-a^=rung-]");
      const ys = rungs.map((r) => ({ y: Math.round(box(r).cy) + 0.5, opt: r.closest(".rung") ? r.closest(".rung").dataset.opt === "1" : false, grp: r.closest(".rung").dataset.grp }));
      const top = ys[0].y - 14, bot = ys[ys.length - 1].y + 14;
      L(sx, gemBottom, sx, top, alpha(kc, 0.45), 1);
      Pa(`M${sx + 7} ${top}H${sx}V${bot}H${sx + 7}`, kc, 1.5);
      for (const r of ys) {
        const x0 = r.grp ? sx + 14 : sx;
        L(x0, r.y, colX - 12, r.y, kc, 1, r.opt ? "1.5 3" : "");
        Pa(`M${colX - 14} ${r.y - 3}h6v6h-6z`, kc, 1, "", r.opt ? css("--g1") : kc);
      }
      const inh = ys.filter((r) => r.grp); if (inh.length) { const g = box($("#bracket .grp")); const t2 = g.cy - 2, b2 = inh[inh.length - 1].y + 10; Pa(`M${sx + 20} ${t2}H${sx + 14}V${b2}H${sx + 20}`, kc, 1.2); }
      const forms = $("#forms");
      if (forms && P.spec.forms) {
        const fcol = box($("#bracket .fd"));
        const x0 = fcol.x; const cw = narrow ? 64 : 92; const names = P.spec.forms;
        out.push(names.map((n, i) => `<foreignObject x="${x0 + i * cw}" y="${ys[0].y - 50}" width="${cw}" height="36"><div style="font:400 12px/16px var(--ui);color:${ink3};text-align:center">${esc(n.replace("Date-Time", "date-time").toLowerCase())}</div></foreignObject>`).join(""));
        P.spec.fields.forEach((f, j) => names.forEach((n, i) => { const cx = x0 + i * cw + cw / 2, cy = ys[j].y; const on = f.requiredFor.includes(n); out.push(on ? `<path d="M${cx - 4} ${cy - 4}h8v8h-8z" fill="${kc}"/>` : `<path d="M${cx - 1} ${cy}h2" stroke="${ink4}"/>`); }));
        names.forEach((n, i) => L(x0 + i * cw + cw / 2 + 0.5, ys[0].y - 12, x0 + i * cw + cw / 2 + 0.5, ys[ys.length - 1].y + 12, hair, 1));
      }
    }
    if (DIR !== "b" && kind === "callable") {
      // layout pass: the drops hang under the pipe body
      const body0 = box($("#pbody")); const dropsEl = $("#drops");
      dropsEl.style.marginLeft = `${narrow ? 24 : Math.max(0, body0.x - colX + 30)}px`; if (narrow) dropsEl.style.maxWidth = `${colW - 24}px`;
      const body = box($("#pbody")); const bx0 = body.x, bx1 = body.r, by0 = body.y, by1 = body.b, cut = 8;
      Pa(`M${bx0 + cut} ${by0}H${bx1}V${by1 - cut}L${bx1 - cut} ${by1}H${bx0}V${by0 + cut}Z`, kc, 1.5);
      const ins = $$("#pipe [data-a^=in-]");
      const n = ins.length;
      ins.forEach((el, i) => {
        const b = box(el); const yIn = Math.round(body.cy + (i - (n - 1) / 2) * 10) + 0.5; const y = Math.round(b.cy) + 0.5;
        const opt = el.classList.contains("opt");
        if (wideMargins) L(16, y, b.x - 12, y, alpha(kc, 0.35), 1, opt ? "1.5 3" : "");
        const mx = bx0 - 18;
        Pa(`M${b.r + 10} ${y}H${mx - Math.abs(y - yIn)}L${mx} ${yIn}H${bx0}`, kc, 1.2, opt ? "1.5 3" : "");
      });
      const o = box($("#pipe [data-a=out]"));
      const oy = Math.round(body.cy) + 0.5;
      L(bx1, oy, o.x - 12, oy, kc, 1.5); Pa(`M${o.x - 16} ${oy - 4}l4 4-4 4`, kc, 1.5);
      if (wideMargins && P.spec.output) { const words = $("#pipe .outs"); const wr = box(words); if (readerW - 16 - (wr.r + 16) > 40) L(wr.r + 16, oy, readerW - 16, oy, alpha(kc, 0.35), 1); }
      const recv = $("#recv"); if (recv) { const r = box(recv); recv.style.left = `${(body.w - r.w) / 2}px`; recv.style.top = `-36px`; const rx = Math.round(body.cx) + 0.5; L(rx, by0 - 14, rx, by0, ty, 1.5); }
      const drops = $$("#drops [data-a^=drop-]");
      const dx = Math.round(narrow ? colX + 12 : bx0 + 18) + 0.5;
      let lastDrop = by1;
      drops.forEach((d) => { const hb = box(d.querySelector(".how")); const y = Math.round(hb.cy) + 0.5; Pa(`M${dx} ${lastDrop}V${y}H${hb.x - 8}`, coral, 1.5); lastDrop = y; });
      const pol = $("#policy");
      if (pol) {
        const ks = $$("#policy [data-a^=pol-]"); const ysP = ks.map((k) => Math.round(box(k).cy) + 0.5);
        const px = Math.round(box(ks[0]).x - 26) + 0.5;
        const lab = $("#policy-lbl"); const ly = lab ? box(lab).cy : ysP[0] - 20;
        Pa(`M${dx} ${lastDrop}V${ly}H${px}V${ysP[ysP.length - 1] - 9}`, coral, 1.5);
        ysP.forEach((y, i) => { const kb = box(ks[i]); Pa(`M${px} ${y - 9}L${px + 9} ${y}H${kb.x - 8}`, ty, 1.2); });
      }
      const alt = $("#drops [data-a=alt]"); if (alt) { const a = box(alt.querySelector(".how")); Pa(`M${dx} ${lastDrop}V${Math.round(a.cy) + 0.5}H${a.x - 8}`, ink3, 1, "2 3"); }
    }
    if (DIR !== "b" && kind === "contract") {
      const mems = $$("#outline .mem");
      const rows = mems.map((m) => ({ y: Math.round(box(m.querySelector(".mn")).cy) + 0.5, n: m.dataset.n }));
      const grpEls = $$("#outline .grp");
      const top = Math.round(box(grpEls[0]).y) - 4.5, bot = rows[rows.length - 1].y + 20;
      const mw = Math.max(...$$("#outline .mn").map((e) => box(e).r), ...$$("#outline .grp span").map((e) => box(e).r));
      const xl = sx, xr = Math.round(mw + 24) + 0.5, cut = 10;
      // the socket: a notch cut into its left edge for each thing you write, a tab on its right for each you get
      let d = `M${xl + cut} ${top}H${xr}`;
      for (const r of rows) if (r.n === "tab") d += `V${r.y - 5}H${xr + 8}V${r.y + 5}H${xr}`;
      d += `V${bot - cut}L${xr - cut} ${bot}H${xl}`;
      for (const r of rows.slice().reverse()) if (r.n === "open") d += `V${r.y + 7}H${xl + 10}V${r.y - 7}H${xl}`;
      d += `V${top + cut}Z`;
      Pa(d, co, 1.5, "", alpha(co, 0.05));
      for (const r of rows) if (r.n === "tab") Pa(`M${xr} ${r.y - 5}H${xr + 8}V${r.y + 5}H${xr}Z`, co, 1, "", alpha(co, 0.6));
      L(sx, gemBottom, sx, top, alpha(co, 0.45), 1);
      // the plug: who does it, converging from the margin; its pins fill the notches
      const sat = P.spec.sat; const pins = rows.filter((r) => r.n === "open").map((r) => r.y);
      const cy = Math.round((pins[0] + pins[pins.length - 1]) / 2) + 0.5;
      const groups = satGroups(sat);
      const jx = xl - 34, bus = xl - 14;
      if (pins.length > 1) L(bus, pins[0], bus, pins[pins.length - 1], co, 1.5);
      pins.forEach((y) => { L(pins.length > 1 ? bus : jx, y, xl + 9, y, co, 1.5); });
      if (pins.length > 1) L(jx, cy, bus, cy, co, 1.5);
      if (wideMargins && groups.length) {
        const step = 22; const y0 = cy - ((groups.length - 1) * step) / 2; const gx = jx - 58;
        const headY = y0 - 60;
        out.push(`<foreignObject x="0" y="${y0 - 56}" width="${jx + 20}" height="44"><div class="fan" style="position:static"><div class="head" style="position:static;white-space:nowrap;text-align:left;padding-left:${Math.max(0, gx - 150)}px"><span class="n">${sat.n}</span><span class="t">${sat.computed ? "types satisfy it" : "types do it"}${sat.yours ? ` · <span class="y">${sat.yours} yours</span>` : ""}</span><br><span class="t">${sat.computed ? "computed by the type checker" : ""}${sat.exported === 0 ? " · <b>none exported</b>" : ""}</span></div></div></foreignObject>`);
        groups.forEach((g, i) => {
          const y = Math.round(y0 + i * step) + 0.5;
          out.push(`<foreignObject x="0" y="${y - 9}" width="${gx - 8}" height="18"><div class="fan" style="position:static"><div class="fl ${g.yours ? "y" : ""}" style="position:static;overflow:hidden;text-overflow:ellipsis">${esc(g.label)}</div></div></foreignObject>`);
          const c = g.yours ? mint : alpha(co, 0.75); const w = Math.max(1, Math.min(3, 0.8 + Math.log10(g.n + 1)));
          g.y = y;
          if (g.via) { const pi = groups.findIndex((x) => x.name === g.via); const py = Math.round(y0 + pi * step) + 0.5; Pa(`M${gx} ${y}H${gx + 12}C${gx + 26} ${y} ${gx + 18} ${py} ${gx + 34} ${py}`, c, w); }
          else Pa(`M${gx} ${y}H${gx + 24}C${gx + 56} ${y} ${jx - 30} ${cy} ${jx} ${cy}`, c, w, g.dashed ? "3 3" : "");
        });
        out.push(`<circle cx="${jx}" cy="${cy}" r="3" fill="${co}"/>`);
      } else {
        const cnt = $("#spec .count"); cnt.insertAdjacentHTML("beforeend", ` · done by <b>${sat.n}</b>${sat.yours ? ` <span style="color:var(--mint)">${sat.yours} yours</span>` : ""}${sat.computed ? " (computed)" : ""}`);
        out.push(`<circle cx="${jx}" cy="${cy}" r="3" fill="${co}"/>`); L(16, cy, jx, cy, alpha(co, 0.6), 1.5);
      }
    }
    // ---- rails: source → step → the type's terminal
    const rails = $$(".rail");
    if (rails.length) {
      const bySec = new Map(); for (const r of rails) { const s = r.closest(".sec"); (bySec.get(s) || bySec.set(s, []).get(s)).push(r); }
      for (const [sec, rs] of bySec) {
        const kindS = sec.dataset.kind;
        const steps = rs.map((r) => r.querySelector(".step")).filter(Boolean);
        const tx = Math.round(Math.max(...steps.map((s) => box(s).r)) + 28) + 0.5;
        const ys = [];
        rs.forEach((r) => {
          const st = r.querySelector(".step"); const src = r.querySelector(".src") || r.querySelector(".src-in");
          const sb = st ? box(st) : null; const y = Math.round(box(r).cy) + 0.5; ys.push(y);
          const sr = src ? box(src) : null;
          const fromX = sr ? sr.r + 10 : colX;
          if (sb) {
            const fam = st.dataset.fam; const c = css(FAMV[fam] || "--k-ca");
            L(fromX, y, sb.x - 2, y, alpha(kc, 0.6), 1.2);
            Pa(`M${sb.x + 6} ${sb.y}H${sb.r}V${sb.b - 6}L${sb.r - 6} ${sb.b}H${sb.x}V${sb.y + 6}Z`, alpha(c, 0.8), 1);
            const maybe = st.dataset.maybe === "1";
            L(sb.r + 2, y, tx - 8, y, alpha(kc, 0.6), 1.2, maybe ? "1.5 3" : "");
            if (st.dataset.fails === "1") Pa(`M${sb.cx} ${sb.b}v6`, coral, 1.2);
            // the lands label moves to sit after the terminal
            const la = r.querySelector(".lands"); if (la) la.style.marginLeft = `${tx + 14 - sb.r}px`;
            const code = r.nextElementSibling && r.nextElementSibling.classList.contains("code") ? r.nextElementSibling : null;
            if (code) code.style.marginLeft = `${tx + 14 - colX}px`;
          }
        });
        if (kindS !== "doing" && steps.length) {
          const t0 = ys[0] - 10, t1 = ys[ys.length - 1] + 10;
          L(tx, t0, tx, t1, kc, 1.5);
          ys.forEach((y) => Pa(`M${tx - 8} ${y - 4}l4 4-4 4`, alpha(kc, 0.8), 1.2));
        } else if (steps.length) ys.forEach((y) => { Pa(`M${tx - 12} ${y}h6`, co, 1.2); Pa(`M${tx - 2} ${y - 6}l6 6-6 6-6-6z`, co, 1.2, "", alpha(co, 0.3)); });
      }
    }
    // ---- the failure tree: coral from the spine
    const fr = $$("#ftree .op");
    if (fr.length) { const ys = fr.map((f) => Math.round(box(f).cy) + 0.5); const hb = $$("#ftree .how").map(box); L(sx, ys[0] - 20, sx, ys[ys.length - 1] - 9, coral, 1.5); ys.forEach((y, i) => { Pa(`M${sx} ${y - 9}L${sx + 9} ${y}H${colX - 10}`, coral, 1.2); const ob = box(fr[i]); L(ob.r + 8, y, hb[i].x - 6, y, alpha(coral, 0.5), 1); }); }
    const kn = $$("#kinds .kn");
    if (kn.length) { const ys = kn.map((k) => Math.round(box(k).y + 16) + 0.5); L(sx, ys[0] - 24, sx, ys[ys.length - 1] - 9, coral, 1.5); ys.forEach((y, i) => { const never = kn[i].closest(".never"); Pa(`M${sx} ${y - 9}L${sx + 9} ${y}H${colX - 10}`, never ? ink3 : coral, 1.2, never ? "2 3" : ""); }); }
    // ---- the comb: releases along time, left = older; the pin in mint
    for (const combEl of $$("#comb, #pcomb")) {
      const cb = box(combEl); const n = 120; const w = cb.w; const y1 = cb.b - 14;
      const s = (P.sections || []).find((x) => x.kind === "changed");
      const since = s && s.since; const pinned = since ? since.pinned : "0.8.23";
      const vs = P.releases && P.releases.sample ? null : null;
      const order = window.TOML_RELEASES || [];
      for (let i = 0; i < n; i++) {
        const x = Math.round(cb.x + (i / (n - 1)) * w) + 0.5; const v = order[i] || "";
        const strip = (x) => x.replace(/\+.*$/, ""); const pin = v === pinned; const read = since && since.read.map(strip).includes(v); const ch = since && since.changed.some((c) => strip(c.v) === v);
        const hh = pin ? 22 : ch ? 16 : read ? 12 : /\.0$/.test(v) ? 9 : 5;
        L(x, y1 - hh, x, y1, pin ? mint : ch ? coral : read ? css("--ink2") : ink4, pin ? 2 : 1);
      }
      out.push(`<foreignObject x="${cb.x}" y="${y1 + 2}" width="${w}" height="16"><div style="display:flex;justify-content:space-between;font:400 12px/16px var(--mono);color:${ink3}"><span>${esc(order[0] || "")} · 2014</span><span>${esc(order[n - 1] || "")}</span></div></foreignObject>`);
      const pi = order.indexOf(pinned); if (pi >= 0) out.push(`<foreignObject x="${cb.x + (pi / (n - 1)) * w - 40}" y="${y1 - 40}" width="80" height="16"><div style="text-align:center;font:500 12px/16px var(--mono);color:${mint}">${esc(pinned)}</div></foreignObject>`);
    }
    // ---- the tour: one line through the stops
    const stops = $$("#tour .stop");
    if (stops.length) {
      const y = Math.round(box(stops[0]).y + 10) + 0.5; L(box(stops[0]).x + 8, y, box(stops[stops.length - 1]).x + 8, y, alpha(kc, 0.6), 1.5);
      stops.forEach((st, i) => { const b = box(st); const t = P.tour[i]; out.push(`<rect x="${b.x - 2}" y="${y - 10}" width="20" height="20" fill="${css("--g1")}"/><g transform="translate(${b.x} ${y - 8}) scale(.6667)" fill="none" stroke="${css(FAMV[t.fam])}" stroke-width="1.8" stroke-linecap="square">${(K[KIND_GLYPH[t.kind] || t.kind] || "").replace(/class="f"/g, 'class="f" fill="currentColor" stroke="none" opacity=".45"').replace(/currentColor/g, css(FAMV[t.fam]))}</g>`); });
    }
    // ---- the tray: each module a chamfered territory
    const tc = $("#tray");
    if (tc) {
      const b = box(tc); const c = 12; const ns = css("--k-ns");
      Pa(`M${b.x + c} ${b.y + 0.5}H${b.r - 0.5}V${b.b - c}L${b.r - c} ${b.b - 0.5}H${b.x + 0.5}V${b.y + c}Z`, alpha(ns, 0.55), 1.5);
      const trs = $$("#tray .trow");
      trs.slice(1).forEach((r) => { const rb = box(r); L(b.x + 1, Math.round(rb.y) + 0.5, b.r - 1, Math.round(rb.y) + 0.5, alpha(ns, 0.3), 1); });
      for (const r of trs) { const cs = [...r.children]; cs.slice(0, -1).forEach((cEl) => { const cb = box(cEl); L(Math.round(cb.r) + 0.5, cb.y + 1, Math.round(cb.r) + 0.5, cb.b - 1, alpha(ns, 0.3), 1); }); }
      for (const cEl of $$("#tray .cmp")) if (cEl.querySelector(".it.y")) { const cb = box(cEl); Pa(`M${cb.x + 1} ${cb.y + 14}V${cb.y + 1}H${cb.x + 14}`, mint, 2); }
    }
    const svg = `<svg class="ink" xmlns="http://www.w3.org/2000/svg">${out.join("")}</svg>`;
    page.insertAdjacentHTML("afterbegin", svg);
    page.dataset.h = Math.ceil(win.getBoundingClientRect().height);
    document.body.dataset.h = Math.ceil(win.scrollHeight);
  }
  function satGroups(sat) {
    if (sat.families) { const top = sat.families.slice(0, 5); const rest = sat.families.slice(5); return [...top.map((f) => ({ label: `${f.family} ${f.n}`, n: f.n })), ...(rest.length ? [{ label: `${rest.length} more families`, n: rest.reduce((a, f) => a + f.n, 0) }] : [])]; }
    if (sat.list) { const direct = sat.list.filter((s) => !s.through.length); const via = sat.list.filter((s) => s.through.length); return [...direct.map((s) => ({ label: s.name, n: 1, name: s.name })), ...via.map((s) => ({ label: s.name, n: 1, via: s.through[0] }))]; }
    if (sat.bands) {
      const g = [];
      for (const b of sat.bands) for (const p of b.pkgs) g.push({ label: `${p.pkg} ${p.n}`, n: p.n, yours: b.band === "yours" });
      g.sort((a, b) => (b.yours - a.yours) || b.n - a.n);
      const top = g.slice(0, 5), rest = g.slice(5);
      return [...top, ...(rest.length ? [{ label: `${rest.length} more packages`, n: rest.reduce((a, x) => a + x.n, 0) }] : [])];
    }
    return [];
  }
  function hover() {
    if (!HOVER) return;
    const [kind, name] = HOVER.split(":");
    const pr = page.getBoundingClientRect();
    const box = (el) => { const r = el.getBoundingClientRect(); return { x: r.left - pr.left, y: r.top - pr.top, w: r.width, h: r.height, r: r.right - pr.left, b: r.bottom - pr.top, cy: r.top - pr.top + r.height / 2 }; };
    const kc = getComputedStyle(win).getPropertyValue(FAMV[P.fam]).trim();
    const marks = [];
    let target = null;
    if (kind === "case") target = [...page.querySelectorAll("#fork .cn, #fork .cl")].find((e) => e.textContent.replace(/"/g, "") === name);
    if (!target) return;
    const row = target; const tb = box(row);
    // the target: ink rises one step, the hit shape draws its bevel (1 px, the kind's hue, low alpha)
    const rowEls = [...row.parentElement.children].filter((e) => !e.classList.contains("quiet")); const inner = (e) => { const c = [...e.querySelectorAll("*")].filter((x) => !x.children.length && x.textContent.trim()); return c.length ? Math.max(...c.map((x) => box(x).r)) : box(e).x; }; const rb = { x: tb.x - 12, y: tb.y - 4, r: Math.max(...rowEls.map(inner)) + 14, b: tb.b + 4 };
    marks.push(`<path d="M${rb.x + 6} ${rb.y}H${rb.r}V${rb.b - 6}L${rb.r - 6} ${rb.b}H${rb.x}V${rb.y + 6}Z" fill="${kc}" fill-opacity=".06" stroke="${kc}" stroke-opacity=".45" stroke-width="1"/>`);
    rowEls.forEach((e) => e.classList.add("hov-on"));
    // its tine thickens to the focus stroke
    const sx = spineX + 0.5, y = Math.round(tb.cy) + 0.5;
    marks.push(`<path d="M${sx} ${y - 9}L${sx + 9} ${y}H${colX - 10}" stroke="${kc}" stroke-width="1.5" fill="none"/>`);
    // related: every other occurrence of the same name lights, and the stroke that reaches it
    const occ = [...page.querySelectorAll(".nm, .case, .mono, .mn, .cn")].filter((e) => e !== target && e.textContent.trim() === name && !e.closest(".fork"));
    occ.forEach((e) => { e.style.color = "var(--ink0)"; const b = box(e); marks.push(`<path d="M${b.x} ${b.b + 1.5}H${b.r}" stroke="${kc}" stroke-width="1.5"/>`); const rail = e.closest(".rail"); if (rail) { const rr = box(rail); const st = rail.querySelector(".step"); const sb = box(st); marks.push(`<path d="M${sb.r + 2} ${Math.round(rr.cy) + 0.5}H${b.x - 16}" stroke="${kc}" stroke-width="1.5"/>`); } });
    // at 350 ms the peek unfurls from the target, and leaves the way it came
    const pk = (P.peeks || {})[name];
    if (pk) {
      const left = Math.min(rb.r + 20, readerW - 344), top = tb.y - 14;
      page.insertAdjacentHTML("beforeend", `<div class="peek" style="left:${left}px;top:${top}px"><div class="ph">${glyph(pk.kind, pk.fam)}<span>${esc(pk.name)}</span></div><div class="pw">${esc(pk.where)}</div><div style="margin-top:8px">${md(pk.say)}</div>${pk.fact ? `<div class="pw" style="margin-top:8px;color:var(--ink2)">${md(pk.fact)}</div>` : ""}</div>`);
      const pb = box(page.querySelector(".peek"));
      marks.push(`<path d="M${pb.x + 8} ${pb.y}H${pb.r}V${pb.b - 8}L${pb.r - 8} ${pb.b}H${pb.x}V${pb.y + 8}Z" fill="none" stroke="${kc}" stroke-opacity=".5"/>`);
    }
    page.insertAdjacentHTML("beforeend", `<svg class="ink" style="position:absolute;inset:0;width:100%;height:100%;overflow:visible;pointer-events:none;z-index:6">${marks.join("")}</svg>`);
  }
  document.fonts.ready.then(() => requestAnimationFrame(() => { ink(); hover(); document.body.dataset.ready = "1"; }));
})();
