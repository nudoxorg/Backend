// The symbol page — a structured, language-neutral anatomy over the same world data.
//
// The page answers, in order: what is it (hero), what shape is it (anatomy: a
// fork for "one of", a bundle for "all of", a pipe for "takes → gives", a
// contract for "you write / you get"), what can it do (capabilities in plain
// words), where do values come from and go (the prism, the same one the graph
// gathers), and what does it do (members grouped by what they do to it).
// Code is one keystroke away (⌘. or the view switch); ⌥ spells the exact source.
(() => {
  "use strict";
  let G = null; // the graph api
  let cur = -1, view = "page";
  const $ = (id) => document.getElementById(id);
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

  // ------------------------------------------------------------------ types, spelled for people
  let nameIndex = null;
  function buildIndex() {
    nameIndex = new Map();
    G.N.forEach((n, i) => { if (n.u < 0 && ["struct", "enum", "trait", "type", "union"].includes(n.k)) (nameIndex.get(n.n) || nameIndex.set(n.n, []).get(n.n)).push(i); });
  }
  function resolveName(name, from) {
    const c = nameIndex.get(name); if (!c) return -1;
    const p = G.N[from].p;
    return c.slice().sort((a, b) => ((G.N[b].p === p) - (G.N[a].p === p)) || G.IMP[b] - G.IMP[a])[0];
  }
  function lexType(s) {
    const t = []; const re = /\s*('[A-Za-z_]\w*|[A-Za-z_]\w*(?:::[A-Za-z_]\w*)*|->|::|[<>()[\],;&!*=+?]|\d+|\S)/g; let m;
    while ((m = re.exec(s))) t.push(m[1]);
    return t;
  }
  const W = (w) => `<i class="gtw">${w}</i>`;
  function genericsOf(j) {
    const out = new Map(); if (j < 0) return out;
    const n = G.N[j]; const src = [n.gen || "", n.u >= 0 ? G.N[n.u].gen || "" : ""].join(",");
    for (const part of splitTop(src)) { const m = /^(?:const\s+)?([A-Za-z_]\w*)\s*(?::\s*(.*))?$/.exec(part.trim()); if (m && !/^'/.test(part.trim())) out.set(m[1], m[2] ? [m[2]] : []); }
    for (const part of splitTop(n.wh || "")) { const c = part.indexOf(":"); if (c < 0) continue; const k = part.slice(0, c).trim(); if (/^[A-Za-z_]\w*$/.test(k)) (out.get(k) || out.set(k, []).get(k)).push(part.slice(c + 1).trim()); }
    return out;
  }
  function typeHTML(s, from, owner) {
    if (!s) return "";
    const gens = genericsOf(from);
    const t = lexType(s); let i = 0;
    const peek = () => t[i], next = () => t[i++];
    function list(close) { const out = []; while (i < t.length && peek() !== close) { out.push(ty()); if (peek() === ",") next(); else if (peek() !== close) { while (i < t.length && peek() !== close && peek() !== ",") next(); if (peek() === ",") next(); } } next(); return out; }
    function generics() { if (peek() !== "<") return []; next(); const out = []; while (i < t.length && peek() !== ">") { if (/^'/.test(peek())) { next(); if (peek() === ",") next(); continue; } const g = ty(); if (peek() === "=") { next(); out.push(ty()); } else out.push(g); if (peek() === ",") next(); } next(); return out; }
    function ty() {
      const x = next(); if (x === undefined) return "";
      if (x === "&") { if (/^'/.test(peek())) next(); if (peek() === "mut") { next(); return W("mutable") + " " + ty(); } return ty(); }
      if (x === "*") { next(); return W("pointer to") + " " + ty(); }
      if (x === "(") { const parts = list(")"); return parts.length ? parts.join(' <i class="gtp">×</i> ') : W("nothing"); }
      if (x === "[") { const inner = ty(); let n = ""; if (peek() === ";") { next(); n = next(); } next(); return (n ? W(`${n} ×`) : W("list of")) + " " + inner; }
      if (x === "!") return W("never returns");
      if (x === "dyn" || x === "impl") { const inner = ty(); while (peek() === "+") { next(); ty(); } return W("any") + " " + inner; }
      if (x === "mut" || x === "const") return ty();
      const segs = x.split("::"); const last = segs[segs.length - 1];
      if (segs[0] === "Self" && segs.length > 1) { generics(); return W("its") + ` <span class="gv">${esc(last)}</span>`; }
      if (segs.length === 1 && (gens.has(x) || /^[A-Z]$/.test(x))) { generics(); return `<span class="gv">${esc(x)}</span>`; }
      if (/^Fn(Mut|Once)?$/.test(last) && peek() === "(") { next(); const a = list(")"); let r = ""; if (peek() === "->") { next(); r = ty(); } return W("a function of") + " " + (a.join(", ") || W("nothing")) + (r ? ` <i class="gtp">→</i> ${r}` : ""); }
      const args = generics();
      switch (last) {
        case "Option": return W("maybe") + " " + args[0];
        case "Vec": case "VecDeque": case "SmallVec": case "LinkedList": return W("list of") + " " + args[0];
        case "HashSet": case "BTreeSet": case "IndexSet": return W("set of") + " " + args[0];
        case "HashMap": case "BTreeMap": case "IndexMap": return W("map") + " " + args[0] + ` <i class="gtp">→</i> ` + (args[1] || "");
        case "Result": return (args[0] || W("nothing")) + " " + W("or fails with") + " " + (args[1] || W("an error"));
        case "Box": case "Cow": case "Pin": return args[0] || last;
        case "Rc": case "Arc": return W("shared") + " " + args[0];
        case "RefCell": case "Cell": return W("changeable") + " " + args[0];
        case "Mutex": case "RwLock": return W("locked") + " " + args[0];
        case "String": case "str": case "OsStr": case "OsString": return W("text");
        case "PathBuf": case "Path": return W("path");
        case "Self": return owner >= 0 ? link(owner) : "Self";
        case "self": return W("it");
        default: {
          if (/^[a-z]/.test(last)) return `<span class="gtk">${esc(last)}</span>`; // primitives
          const j = resolveName(last, from);
          const base = j >= 0 ? link(j) : `<span class="gtk">${esc(last)}</span>`;
          return args.length ? base + `<i class="gtp">‹</i>${args.join('<i class="gtp">, </i>')}<i class="gtp">›</i>` : base;
        }
      }
    }
    const out = []; while (i < t.length) { const before = i; out.push(ty()); if (i === before) i++; }
    return `<span class="gty" title="${esc(s)}">${out.join(" ")}</span>`;
  }
  const link = (j) => `<a class="gtl${G.yours(j) ? " y" : ""}" data-i="${j}">${esc(G.N[j].n)}</a>`;
  const linkName = (j, text) => `<a class="gtl${G.yours(j) ? " y" : ""}" data-i="${j}">${esc(text || G.nameOf(j))}</a>`;
  function params(n) {
    return (n.params || []).filter((p) => !/^(&\s*('\w+\s+)?)?(mut\s+)?self\b/.test(p) && !/^self\s*:/.test(p)).map((p) => {
      const c = p.indexOf(":"); if (c < 0) return { name: "", ty: p };
      return { name: p.slice(0, c).replace(/^mut\s+/, "").trim(), ty: p.slice(c + 1).trim() };
    });
  }
  const RECV = { reads: "reads it", changes: "changes it", consumes: "uses it up", "": "makes or stands alone" };

  // ------------------------------------------------------------------ anatomy
  function anatomy(i) {
    const n = G.N[i]; const kids = (G.kids[i] || []);
    if (n.k === "enum") {
      const vs = kids.filter((j) => G.N[j].k === "variant");
      const rows = vs.map((j) => { const v = G.N[j];
        const payload = v.ty ? (v.shape === "record" ? v.ty.split(/,\s*(?![^<]*>)/).map((f) => { const c = f.indexOf(":"); return c > 0 ? `<span class="fn">${esc(f.slice(0, c).trim())}</span> ${typeHTML(f.slice(c + 1).trim(), j, i)}` : typeHTML(f, j, i); }).join('<i class="gtp">, </i>') : typeHTML("(" + v.ty + ")", j, i)) : "";
        return `<div class="ar"><span class="vn">${esc(v.n)}</span><span class="vt">${payload}</span><span class="say">${esc(v.d || "")}</span></div>`; }).join("");
      return `<section class="anat fork"><div class="ah">${n.nonExhaustive ? "one of, and more may come" : "one of"}</div><div class="arows">${rows}</div></section>`;
    }
    if (n.k === "struct" || n.k === "union") {
      const fs = kids.filter((j) => G.N[j].k === "field");
      if (!fs.length) return `<section class="anat bundle"><div class="ah">holds nothing — a marker</div></section>`;
      const tuple = fs.every((j) => /^\d+$/.test(G.N[j].n));
      const rows = fs.map((j) => { const f = G.N[j];
        return `<div class="ar${f.v ? "" : " priv"}"><span class="vn">${tuple ? "" : esc(f.n)}</span><span class="vt">${typeHTML(f.ty, j, i)}</span><span class="say">${esc(f.d || "")}</span></div>`; }).join("");
      const priv = fs.filter((j) => !G.N[j].v).length;
      return `<section class="anat bundle"><div class="ah">holds${priv === fs.length ? ", all private" : priv ? `, ${priv} private` : ""}</div><div class="arows">${rows}</div></section>`;
    }
    if (n.k === "trait") {
      const ms = kids.filter((j) => G.N[j].k === "method" && !G.N[j].via && !G.N[j].n.startsWith("__"));
      const req = ms.filter((j) => G.N[j].required), prov = ms.filter((j) => !G.N[j].required);
      const row = (j) => `<div class="ar"><span class="vn">${esc(G.N[j].n)}</span><span class="vt">${sigLine(j, i)}</span><span class="say">${esc(G.N[j].d || "")}</span></div>`;
      const rows = (list) => fold(list).map((g) => (g.length === 1 ? row(g[0]) : foldRow(g, i))).join("");
      const impls = G.inEdges(i, G.B.impl | G.B.derives).length;
      return `<section class="anat contract">${req.length ? `<div class="ah">you write</div><div class="arows req">${rows(req)}</div>` : ""}`
        + (prov.length ? `<div class="ah">you get</div><div class="arows">${rows(prov)}</div>` : "")
        + (impls ? `<div class="afoot">${impls} type${impls === 1 ? "" : "s"} in this world do it</div>` : "") + `</section>`;
    }
    if (n.k === "function" || n.k === "method") return pipe(i);
    if (n.k === "type") return `<section class="anat bundle"><div class="ah">another name for</div><div class="arows"><div class="ar"><span class="vn"></span><span class="vt">${typeHTML((n.s || "").replace(/^.*?=\s*/, "").replace(/;$/, ""), i, -1)}</span></div></div></section>`;
    if (n.k === "constant") return `<section class="anat bundle"><div class="ah">a fixed value</div><div class="arows"><div class="ar"><span class="vn"></span><span class="vt">${typeHTML((n.s || "").replace(/^[^:]*:\s*/, "").replace(/=.*$/, ""), i, -1)}</span></div></div></section>`;
    return "";
  }
  // look-alikes: ≥ 4 members sharing a name prefix and a result read as one row ("visit_* — 29, one per input")
  function fold(list) {
    const key = (j) => { const n = G.N[j]; const m = /^([a-z]+_)/.exec(n.n); return m ? m[1] + "|" + (n.ret || "") + "|" + (n.recv || "") : "#" + j; };
    const by = new Map(); for (const j of list) (by.get(key(j)) || by.set(key(j), []).get(key(j))).push(j);
    const out = []; const done = new Set();
    for (const j of list) { if (done.has(j)) continue; const g = by.get(key(j)); if (g.length >= 4) { out.push(g); g.forEach((x) => done.add(x)); } else { out.push([j]); done.add(j); } }
    return out;
  }
  function foldRow(g, owner) {
    const n0 = G.N[g[0]]; const pre = /^([a-z]+_)/.exec(n0.n)[1];
    const ins = [...new Set(g.map((j) => (params(G.N[j])[0] || {}).ty).filter(Boolean))];
    const inTxt = ins.length ? `<i class="gtp">(</i>${W("one of")} ${ins.slice(0, 4).map((t) => typeHTML(t, g[0], owner)).join('<i class="gtp">, </i>')}${ins.length > 4 ? W(` and ${ins.length - 4} more`) : ""}<i class="gtp">)</i>` : "";
    const r = n0.ret ? ` <i class="gtp">→</i> ${typeHTML(n0.ret, g[0], owner)}` : "";
    return `<div class="ar fold" data-fold="${g.join(",")}"><span class="vn">${esc(pre)}<i class="gtp">…</i></span><span class="vt"><span class="gsl">${inTxt}${r}</span></span><span class="say">${g.length} of them: ${g.slice(0, 5).map((j) => esc(G.N[j].n.slice(pre.length))).join(", ")}${g.length > 5 ? ", …" : ""}</span></div>`;
  }
  function pipe(i) {
    const n = G.N[i]; const ps = params(n); const owner = n.u;
    const ins = [];
    if (n.recv) ins.push(`<div class="pi"><span class="pn">${esc(RECV[n.recv])}</span></div>`);
    for (const p of ps) ins.push(`<div class="pi"><span class="pn">${esc(p.name)}</span>${typeHTML(p.ty, i, owner)}</div>`);
    let out = n.ret || ""; let fails = "";
    const m = /^Result\s*<(.*)>$/.exec(out.trim());
    if (m) { const parts = splitTop(m[1]); out = parts[0] || "()"; fails = parts[1] || "error"; }
    const flags = [...(n.quals || []).filter((q) => q !== "extern"), n.mustUse ? "must use" : ""].filter(Boolean);
    return `<section class="anat pipe"><div class="pins">${ins.join("") || `<div class="pi"><span class="pn">takes nothing</span></div>`}</div>`
      + `<div class="parrow"><i></i></div>`
      + `<div class="pouts"><div class="po">${out ? typeHTML(out, i, owner) : W("nothing")}</div>${fails ? `<div class="pf">${W("or fails with")} ${typeHTML(fails, i, owner)}</div>` : ""}</div>`
      + whereRows(i)
      + (flags.length ? `<div class="pflags">${flags.map((f) => `<span>${esc(f === "async" ? "waits (async)" : f === "unsafe" ? "you uphold its rules (unsafe)" : f === "const" ? "runs at compile time too" : f)}</span>`).join("")}</div>` : "")
      + `</section>`;
  }
  function whereRows(i) {
    const gens = genericsOf(i); if (!gens.size) return "";
    const rows = [...gens].map(([name, bounds]) => {
      const bs = bounds.flatMap((b) => splitPlus(b)).filter((b) => !/^'/.test(b.trim()) && b.trim() !== "?Sized");
      const txt = bs.length ? W("is any") + " " + bs.map((b) => typeHTML(b, i, G.N[i].u)).join(W(" and ")) : W("is any type");
      return `<div class="pwr"><span class="gv">${esc(name)}</span>${txt}</div>`;
    });
    return `<div class="pwhere">${rows.join("")}</div>`;
  }
  function splitPlus(s) { const out = []; let d = 0, st = 0; for (let k = 0; k < s.length; k++) { const c = s[k]; if (c === "<" || c === "(") d++; else if (c === ">" || c === ")") d--; else if (c === "+" && d === 0) { out.push(s.slice(st, k).trim()); st = k + 1; } } out.push(s.slice(st).trim()); return out.filter(Boolean); }
  function splitTop(s) { const out = []; let d = 0, st = 0; for (let k = 0; k < s.length; k++) { const c = s[k]; if (c === "<" || c === "(" || c === "[") d++; else if (c === ">" || c === ")" || c === "]") d--; else if (c === "," && d === 0) { out.push(s.slice(st, k).trim()); st = k + 1; } } out.push(s.slice(st).trim()); return out.filter(Boolean); }
  function sigLine(j, owner) {
    const n = G.N[j]; const ps = params(n);
    const a = ps.map((p) => typeHTML(p.ty, j, owner)).join('<i class="gtp">, </i>');
    const r = n.ret ? ` <i class="gtp">→</i> ${typeHTML(n.ret, j, owner)}` : "";
    return `<span class="gsl">${a ? `<i class="gtp">(</i>${a}<i class="gtp">)</i>` : ""}${r}</span>`;
  }

  // ------------------------------------------------------------------ the prism (static, the page's form)
  function prism(i, width) {
    const shown = new Set(["made of", "takes", "gives"]);
    const groups = G.relationsOf(i).filter((g) => g.side !== 0 && !shown.has(g.word));
    const cols = { "-1": groups.filter((g) => g.side < 0), "1": groups.filter((g) => g.side > 0) };
    if (!groups.length) return "";
    const narrow = width < 620;
    const ROW = 24, HEAD = 24, GAP = 8, MAX = 5;
    const fmt = (g) => ({ word: g.word, list: g.entries.slice(0, MAX), more: Math.max(0, g.entries.length - MAX) });
    const L = cols["-1"].map(fmt), R = cols["1"].map(fmt);
    const h = (gs) => gs.reduce((a, g) => a + HEAD + ROW * (g.list.length + (g.more ? 1 : 0)) + GAP, 0) - GAP;
    const counts = new Map(); for (const g of groups) for (const en of g.entries) { const t = en.text || G.nameOf(en.j); counts.set(t, (counts.get(t) || 0) + 1); }
    const entry = (en, side) => {
      const j = en.j; const text = en.text || G.nameOf(j);
      const note = en.note || (j >= 0 && G.N[j].p !== G.N[i].p ? G.PK[G.N[j].p].name.replace(/^backend-/, "") : j >= 0 && counts.get(text) > 1 ? (G.MD[G.N[G.topOf[j]].m].path || "") : "");
      const mark = `<i class="pm ${j >= 0 ? (G.KFAM[G.N[j].k] || "") : "co"}"></i>`;
      const nm = j >= 0 ? linkName(j, text) : `<span>${esc(text)}</span>`;
      const nt = note ? `<span class="pw">${esc(note)}</span>` : "";
      return side < 0 ? `<div class="pe l">${nt}${nm}${mark}</div>` : `<div class="pe r">${mark}${nm}${nt}</div>`;
    };
    const col = (gs, side) => gs.map((g) => `<div class="pg"><div class="ph">${esc(g.word)}</div>${g.list.map((en) => entry(en, side)).join("")}${g.more ? `<div class="pe more ${side < 0 ? "l" : "r"}">and ${g.more} more</div>` : ""}</div>`).join("");
    if (narrow) {
      return `<section class="prism narrow"><div class="pc">${col(L, 1)}</div><div class="pgem"><i></i><span>${esc(G.N[i].n)}</span></div><div class="pc">${col(R, 1)}</div></section>`;
    }
    const HL = h(L), HR = h(R); const H = Math.max(HL, HR, 40);
    const cx = width / 2, gx = 150; // columns end/start 150 px either side of the gem
    const ys = (gs, H0) => { const out = []; let y = (H - H0) / 2; for (const g of gs) { y += HEAD; for (const _ of g.list) { out.push(y + ROW / 2); y += ROW; } if (g.more) y += ROW; y += GAP; } return out; };
    const yl = ys(L, HL), yr = ys(R, HR);
    const c = (x0, y0, x1, y1) => { const k = (x1 - x0) * 0.5; return `<path d="M${x0} ${y0} C${x0 + k} ${y0} ${x1 - k} ${y1} ${x1} ${y1}"/>`; };
    const paths = yl.map((y) => c(cx - gx + 6, y, cx - 14, H / 2)).join("") + yr.map((y) => c(cx + 14, H / 2, cx + gx - 6, y)).join("");
    return `<section class="prism" style="height:${H}px">`
      + `<svg class="pcurves" width="${width}" height="${H}" viewBox="0 0 ${width} ${H}">${paths}</svg>`
      + `<div class="pc left" style="right:${cx + gx}px;top:${(H - HL) / 2}px">${col(L, -1)}</div>`
      + `<button class="pgem" style="left:${cx}px;top:${H / 2}px" data-graph="1" title="See it in the graph (G)"><i></i></button>`
      + `<div class="pc right" style="left:${cx + gx}px;top:${(H - HR) / 2}px">${col(R, 1)}</div></section>`;
  }

  // ------------------------------------------------------------------ members
  function members(i) {
    const n = G.N[i]; const kids = (G.kids[i] || []).filter((j) => G.N[j].k === "method" && !G.N[j].n.startsWith("__"));
    if (!kids.length || n.k === "trait") return "";
    const own = kids.filter((j) => !G.N[j].via), viaT = kids.filter((j) => G.N[j].via);
    const order = ["reads", "changes", "consumes", ""];
    const copy = (n.derives || []).includes("Copy");
    const recvOf = (j) => { const r = G.N[j].recv || ""; return r === "consumes" && copy ? "reads" : r; };
    const groups = order.map((r) => [r, own.filter((j) => recvOf(j) === r)]).filter(([, l]) => l.length);
    const rowF = (l) => fold(l).map((g) => (g.length === 1 ? row(g[0]) : `<div class="crow2 mrow fold"><span class="k ca sm"><svg viewBox="0 0 24 24">${(window.KINDS || {}).method || ""}</svg></span><span class="nm">${foldRow(g, i).replace(/^<div[^>]*>|<\/div>$/g, "")}</span></div>`)).join("");
    const row = (j) => { const m = G.N[j];
      return `<div class="crow2 mrow" data-i="${j}"><span class="k ${G.KFAM[m.k] || "ca"} sm"><svg viewBox="0 0 24 24">${(window.KINDS || {})[m.k] || ""}</svg></span>`
        + `<span class="nm"><a class="gtl" data-i="${j}">${esc(m.n)}</a>${sigLine(j, i)}</span><span class="say">${esc(m.d || "")}</span></div>`; };
    const word = { reads: "reads it", changes: "changes it", consumes: "uses it up", "": "makes one, or stands alone" };
    let html = `<section class="csec"><h2>Does</h2>` + groups.map(([r, l]) => `<div class="cgrp">${word[r]}</div>` + rowF(l)).join("");
    if (viaT.length) {
      const by = new Map(); for (const j of viaT) (by.get(G.N[j].via) || by.set(G.N[j].via, []).get(G.N[j].via)).push(j);
      html += `<div class="cgrp">through its traits</div>` + [...by].map(([t, l]) => `<div class="crow2 mrow via"><span class="k co sm"><svg viewBox="0 0 24 24">${(window.KINDS || {}).trait || ""}</svg></span><span class="nm"><span class="gtk">${esc(t)}</span> <i class="gtp">·</i> ${l.map((j) => `<a class="gtl" data-i="${j}">${esc(G.N[j].n)}</a>`).join('<i class="gtp">, </i>')}</span><span class="say"></span></div>`).join("");
    }
    return html + `</section>`;
  }

  // ------------------------------------------------------------------ the page
  function hero(i) {
    const n = G.N[i];
    const where = `${n.k === "method" ? "method" : n.k} in <span class="mono">${esc(G.qual(i))}</span>`;
    const refs = new Set(G.inEdges(i, -1).concat(...(G.kids[i] || []).map((m) => G.inEdges(m, -1))).map((j) => G.topOf[j])); refs.delete(i);
    const yu = [...refs].filter((j) => G.yours(j)).length;
    const facts = [where, `used in ${refs.size} place${refs.size === 1 ? "" : "s"}`];
    if (yu) facts.push(`<b>${yu}</b> in your code`);
    const gem = gemSVG(n.k, 64);
    return `<div class="chero"><span class="hgem" id="hgem">${gem}</span><div class="col" style="min-width:0"><span class="nm">${breakable(n.n)}</span>`
      + (n.d ? `<span class="ld">${esc(n.d)}</span>` : "") + `</div></div>`
      + `<div class="cfacts">${facts.map((f) => `<span>${f}</span>`).join('<i class="sep">·</i>')}</div>`;
  }
  // identifiers wrap at word boundaries (humps, _ and ::), never mid-word, never ellipsized
  const breakable = (s) => esc(s).replace(/([a-z0-9])([A-Z])/g, "$1<wbr>$2").replace(/_/g, "_<wbr>").replace(/::/g, "::<wbr>");
  function gemSVG(kind, px) {
    const fam = (G.KFAM[kind] || "ty");
    const F = ["24,2 35,13 24,10", "24,10 35,13 38,24", "35,13 46,24 38,24", "46,24 35,35 38,24", "38,24 35,35 24,38", "35,35 24,46 24,38", "24,46 13,35 24,38", "24,38 13,35 10,24", "13,35 2,24 10,24", "2,24 13,13 10,24", "10,24 13,13 24,10", "13,13 24,2 24,10"];
    const LIT = [0.4, 0.3, 0.34, 0.14, 0.08, 0.11, 0.22, 0.2, 0.3, 0.56, 0.5, 0.66];
    const glyph = ((window.KINDS || {})[kind] || "").replace(/class="f"/g, 'class="f" fill="currentColor" stroke="none" opacity=".5"');
    return `<svg class="gem ${fam}" width="${px}" height="${px}" viewBox="0 0 48 48">${F.map((p, k) => `<polygon class="fc" style="--t:${LIT[k]};--i:${k}" points="${p}"></polygon>`).join("")}`
      + `<path d="M24 2 46 24 24 46 2 24z" fill="none" stroke="currentColor" stroke-width="1"></path><path class="tb" d="M24 10 38 24 24 38 10 24z" stroke="currentColor" stroke-width=".7" stroke-opacity=".55"></path>`
      + `<g transform="translate(17.4 17.4) scale(.55)" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="square" stroke-linejoin="miter">${glyph}</g></svg>`;
  }
  function shelf(i) {
    const n = G.N[i]; const top = G.topOf[i]; const m = G.N[top].m;
    const pk = G.PK[n.p];
    const items = []; G.N.forEach((x, j) => { if (x.u < 0 && x.m === m && !x.orphan) items.push(j); });
    items.sort((a, b) => (G.N[a].l || 0) - (G.N[b].l || 0));
    const mp = G.MD[m].path;
    const rows = items.slice(0, 40).map((j) => `<div class="crow${j === top ? " cur" : ""}" data-i="${j}" style="padding-left:28px"><span class="k ${G.KFAM[G.N[j].k] || ""} sm"><svg viewBox="0 0 24 24">${(window.KINDS || {})[G.N[j].k] || ""}</svg></span><span class="n">${esc(G.N[j].n)}</span></div>`).join("");
    return `<div class="cup">‹ ${esc(pk.yours ? "your code" : "dependencies")}</div>`
      + `<div class="cbook">${gemSVG("package", 28)}<div class="col" style="gap:1px;min-width:0"><span class="bn">${esc(pk.name.replace(/^backend-/, ""))}</span><span class="bv">${esc(pk.version)}</span></div></div>`
      + `<div class="cfilter"><span>Filter</span></div><div class="crows"><div class="crow open"><span class="k ns sm"><svg viewBox="0 0 24 24">${(window.KINDS || {}).module || ""}</svg></span><span class="n">${esc(mp || "(root)")}</span></div>${rows}</div>`;
  }
  function render(i) {
    cur = i; const n = G.N[i];
    const reader = $("preader"); const rw = reader.clientWidth || (G.view.clientWidth - 264); const w = Math.min(900, rw - 64);
    if (view === "code") { renderCode(i); return; }
    const capsHTML = G.capsLine(i);
    reader.innerHTML = `<div class="cfol pfol">${hero(i)}${anatomy(i)}`
      + (capsHTML ? `<section class="pcaps"><div class="ah">can</div>${capsHTML}</section>` : "")
      + `<div class="pslot" id="pslot"></div>` + members(i) + `</div>`;
    const slot = $("pslot"); slot.outerHTML = prism(i, Math.max(300, slot.clientWidth || w)) || "";
    $("pshelf").innerHTML = shelf(i);
    setTitle(i);
  }
  function setTitle(i) {
    const n = G.N[i];
    const here = document.querySelector(".titlebar .here");
    if (here) { const nm = here.querySelector(".nm"), pa = here.querySelector(".path"); if (nm) nm.textContent = n.n; if (pa) pa.textContent = G.qual(i).replace(/::/g, " › "); }
    document.querySelectorAll(".titlebar .views button").forEach((b) => b.classList.toggle("on", b.dataset.view === (G.pageOpen ? view : "graph")));
    const addr = $("gaddr"); if (addr) addr.textContent = `nudox://${G.qual(i).replace(/::/g, "/")}/${n.n}${view === "code" ? "#L" + (n.l || 1) : ""}`;
  }

  // ------------------------------------------------------------------ code view: the real source, one keystroke away
  const srcCache = new Map();
  async function renderCode(i) {
    const n = G.N[G.topOf[i]]; const reader = $("preader");
    const pk = G.PK[n.p];
    reader.innerHTML = `<div class="cfol pfol code"><div class="scr">${esc(G.qual(i).replace(/::/g, " › "))} › <b>${esc(G.N[i].n)}</b></div><div class="src" id="psrc"><div class="sl"><span class="tx">loading…</span></div></div></div>`;
    $("pshelf").innerHTML = shelf(i); setTitle(i);
    if (!n.f) { $("psrc").innerHTML = `<div class="sl"><span class="tx">No source in this world for ${esc(n.n)}.</span></div>`; return; }
    const url = (pk.external ? "graph/registry/" : "graph/repo/") + n.f;
    let text = srcCache.get(url);
    if (text === undefined) { try { const r = await fetch(url); text = r.ok ? await r.text() : null; } catch { text = null; } srcCache.set(url, text); }
    if (cur !== i || view !== "code") return;
    if (!text) { $("psrc").innerHTML = `<div class="sl"><span class="tx">Source not served at ${esc(url)}.</span></div>`; return; }
    const lines = text.split("\n"); const a = Math.max(1, (G.N[i].l || n.l) - 3), b = Math.min(lines.length, Math.max(n.e || n.l, (G.N[i].e || 0)) + 3);
    const hl = (s) => /^\s*\/\//.test(s) ? `<span class="dc">${esc(s)}</span>` : esc(s).replace(/(\/\/.*)$/, '<span class="dc">$1</span>').replace(/\b(pub|fn|enum|struct|impl|trait|for|let|mut|match|self|Self|use|mod|const|static|return|if|else|where|type|as|crate|super|async|await|move|ref|in|loop|while|unsafe|dyn)\b/g, '<span class="kw">$1</span>').replace(/\b([A-Z][A-Za-z0-9_]*)\b/g, '<span class="ty">$1</span>').replace(/(#\[[^\]]*\])/g, '<span class="at">$1</span>');
    $("psrc").innerHTML = lines.slice(a - 1, b).map((ln, k) => { const no = a + k; const on = no >= (G.N[i].l || n.l) && no <= (G.N[i].e || n.e || n.l); return `<div class="sl${on ? " cur" : ""}"><span class="ln">${no}</span><i class="tk"></i><span class="tx">${hl(ln) || " "}</span></div>`; }).join("");
  }

  // ------------------------------------------------------------------ open, close, morph
  const page = () => $("gpage");
  function open(i, api, opts = {}) {
    G = api; if (!nameIndex) buildIndex();
    const el = page(); const wasOpen = G.pageOpen; G.pageOpen = true;
    view = opts.view || (view === "code" ? "code" : "page");
    el.classList.add("on"); render(i);
    history.replaceState(null, "", `?page=${encodeURIComponent(G.qual(i) + "::" + G.N[i].n)}${view === "code" ? "&view=code" : ""}`);
    if (wasOpen || opts.instant || new URLSearchParams(location.search).has("still")) { el.classList.add("on"); el.style.opacity = 1; setTitle(i); return; }
    // graph → page: the focus gem blooms into the hero gem while the page rises
    const vr = G.view.getBoundingClientRect();
    const fx = G.sx(G.X[i]) + vr.left, fy = G.sy(G.Y[i]) + vr.top;
    el.classList.add("on"); el.style.opacity = 0; el.style.transform = "translateY(10px)";
    const hg = $("hgem"); const hr = hg.getBoundingClientRect(); hg.style.visibility = "hidden";
    const fly = document.createElement("div"); fly.className = "gfly"; fly.innerHTML = gemSVG(G.N[i].k, 64);
    Object.assign(fly.style, { left: hr.left + "px", top: hr.top + "px", transform: `translate(${fx - hr.left - 32}px,${fy - hr.top - 32}px) scale(.28)` });
    document.body.appendChild(fly);
    requestAnimationFrame(() => {
      fly.style.transition = "transform 460ms cubic-bezier(.2,.75,.2,1)"; fly.style.transform = "translate(0,0) scale(1)";
      el.style.transition = "opacity 300ms ease 120ms, transform 380ms cubic-bezier(.2,.75,.2,1) 80ms"; el.style.opacity = 1; el.style.transform = "none";
    });
    setTimeout(() => { hg.style.visibility = ""; fly.remove(); el.style.transition = ""; }, 520);
    setTitle(i);
  }
  function close(api) {
    G = api; const el = page(); const i = cur;
    G.pageOpen = false; setTitle(i);
    // page → graph: the page lifts away, the camera swoops to the symbol from wherever the map was left, and the prism gathers
    const hg = $("hgem"); const hr = hg ? hg.getBoundingClientRect() : null;
    el.style.transition = "opacity 220ms ease, transform 260ms ease"; el.style.opacity = 0; el.style.transform = "translateY(-8px)";
    setTimeout(() => { el.classList.remove("on"); el.style.transition = ""; el.style.transform = ""; }, 240);
    history.replaceState(null, "", `?focus=${encodeURIComponent(G.qual(i) + "::" + G.N[i].n)}`);
    G.setFocus(i, true);
  }
  function setView(v) {
    if (v === "graph") { if (G && G.pageOpen) close(G); else if (G) G.wake(); return; }
    view = v; const i = cur >= 0 ? cur : G.focus;
    if (i < 0) return;
    if (!G.pageOpen) open(i, G, { view: v }); else { render(i); history.replaceState(null, "", `?page=${encodeURIComponent(G.qual(i) + "::" + G.N[i].n)}${v === "code" ? "&view=code" : ""}`); }
  }

  // ------------------------------------------------------------------ wiring
  document.addEventListener("click", (e) => {
    const a = e.target.closest("#gpage a.gtl, #gpage .crow[data-i]");
    if (a) { e.preventDefault(); const j = +a.dataset.i; const top = G.N[j].u >= 0 && !["method"].includes(G.N[j].k) ? G.N[j].u : j; render(top); $("preader").scrollTop = 0; history.replaceState(null, "", `?page=${encodeURIComponent(G.qual(top) + "::" + G.N[top].n)}`); return; }
    if (e.target.closest("#gpage .pgem")) { close(G); return; }
    const vb = e.target.closest(".titlebar .views button"); if (vb) setView(vb.dataset.view);
  });
  window.addEventListener("keydown", (e) => {
    if (!G || e.target.tagName === "INPUT") return;
    if (G.pageOpen && (e.key === "g" || e.key === "G")) { close(G); e.stopImmediatePropagation(); }
    else if (!G.pageOpen && (e.key === "g" || e.key === "G") && G.focus >= 0) { /* already in the graph */ }
    else if (e.key === "." && e.metaKey && (G.pageOpen || G.focus >= 0)) { setView(view === "code" && G.pageOpen ? "page" : "code"); e.preventDefault(); e.stopImmediatePropagation(); }
    else if (e.altKey && G.pageOpen) document.body.classList.add("xray");
  }, true);
  window.addEventListener("keyup", (e) => { if (!e.altKey) document.body.classList.remove("xray"); });
  function init(api) { G = api; if (!nameIndex) buildIndex(); }
  window.GRAPH_PAGE = { init, open, close, render, setView };
})();
