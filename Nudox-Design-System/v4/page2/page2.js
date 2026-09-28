// page2.js — the symbol page, answering the reader's questions in their order:
//   what is it · how do I get one · what can I do with it · what can go wrong · who uses it, and how · what changed
// Data: page2.json (extract2.mjs). URL state for stills:
//   ?page=value|from_str|serialize|smallvec|error   ?x=1 (⌥ held)   ?t=2 (200 % text)   ?tall=1 (whole page)
//   ?rel=a|b|c (relations presentation)   ?open=<verb>[:<pkg>]   ?hover=<target>   ?flip=<section>   ?fly=<0..1>   ?scroll=<section>
(() => {
  "use strict";
  const Q = new URLSearchParams(location.search);
  const $ = (id) => document.getElementById(id);
  const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const KS = window.KINDS || {}, KF = { module: "ns", package: "ns", struct: "ty", enum: "ty", union: "ty", type: "ty", trait: "co", function: "ca", method: "ca", macro: "ca", constant: "va", field: "va", variant: "va" };
  const kmark = (k, cls = "") => `<span class="k ${KF[k] || "ns"} sm ${cls}"><svg viewBox="0 0 24 24">${KS[k] || KS.unknown || ""}</svg></span>`;
  let D = null, P = null; const REL = Q.get("rel") || "a";
  const S = (n, one, many) => `${n} ${n === 1 ? one : many || one + "s"}`;
  const list = (xs) => xs.length <= 1 ? xs.join("") : xs.slice(0, -1).join(", ") + " and " + xs[xs.length - 1];

  // ------------------------------------------------------------------ the normalised verb table (VERBS.md): one grammar, seven languages
  const VERBS = {
    "comes from": { def: "what makes one: callables that return it, constructors, conversions into it", dir: "in", lang: { Rust: "fn f() -> T · impl From<X> for T", TypeScript: "function f(): T · new T()", Python: "def f() -> T · T(...) · @classmethod", Go: "func NewT() T", Java: "T f() · new T()", "C#": "T F() · new T() · implicit operator T", "C++": "T f() · T(...) · operator T()" } },
    "done by": { def: "types that fulfil it", dir: "in", lang: { Rust: "impl Tr for X · #[derive(Tr)]", TypeScript: "class X implements I · any matching shape", Python: "class X(Base) · Protocol match", Go: "any type with its methods (implicit)", Java: "class X implements I", "C#": "class X : I", "C++": "class X : public Base" } },
    "called by": { def: "code that calls it", dir: "in", lang: { Rust: "f(…) · x.f(…)", TypeScript: "f(…)", Python: "f(…)", Go: "f(…)", Java: "f(…)", "C#": "F(…)", "C++": "f(…)" } },
    "taken by": { def: "callables with a parameter of this type", dir: "out", lang: { Rust: "fn f(x: T) · &T · Option<&T>", TypeScript: "function f(x: T)", Python: "def f(x: T)", Go: "func f(x T) · *T", Java: "void f(T x)", "C#": "void F(T x)", "C++": "void f(const T&)" } },
    "held by": { def: "fields and variants that store one", dir: "out", lang: { Rust: "struct S { x: T } · Enum::V(T)", TypeScript: "interface S { x: T }", Python: "x: T in a class or dataclass", Go: "type S struct { X T }", Java: "T x; (a field)", "C#": "T X { get; } · T x;", "C++": "T x; (a member)" } },
    "called on by": { def: "code that calls its methods", dir: "out", lang: { Rust: "x.m() · T::m(x)", TypeScript: "x.m()", Python: "x.m()", Go: "x.M()", Java: "x.m()", "C#": "x.M()", "C++": "x.m() · x->m()" } },
    "asked for by": { def: "callables that accept any type that has it", dir: "out", lang: { Rust: "fn f<T: Tr>(…) · where T: Tr", TypeScript: "<T extends I>", Python: "TypeVar(bound=P)", Go: "[T I] (a constraint)", Java: "<T extends I>", "C#": "where T : I", "C++": "template<C T> · requires C<T>" } },
    "used by": { def: "any other mention: types, casts, paths", dir: "out", lang: {} },
    "calls": { def: "what it calls inside", dir: "out", lang: { Rust: "f(…) in its body" } },
  };

  // ------------------------------------------------------------------ small renderers
  const RKW = /\b(pub|fn|enum|struct|impl|trait|for|let|mut|match|self|Self|use|mod|const|static|return|if|else|where|type|as|crate|super|async|await|move|ref|in|loop|while|unsafe|dyn|extern|true|false|Some|None|Ok|Err)\b/g;
  function hlRust(src) {
    // strings and comments first, then keywords and types, on escaped text
    const out = []; const re = /(\/\/[^\n]*)|("(?:[^"\\]|\\.)*")|(b?'(?:[^'\\]|\\.)')|([^"\/']+|\/|'|")/g; let m;
    while ((m = re.exec(src))) {
      if (m[1]) out.push(`<span class="q-co">${esc(m[1])}</span>`);
      else if (m[2] || m[3]) out.push(`<span class="q-st">${esc(m[2] || m[3])}</span>`);
      else out.push(esc(m[4]).replace(RKW, '<span class="kw">$1</span>').replace(/\b([A-Z][A-Za-z0-9_]*)\b/g, '<span class="ty">$1</span>'));
    }
    return out.join("");
  }
  function exampleHTML(code, hit, max = 14) {
    const all = code.split("\n"); const vis = all.filter((l) => !(/^\s*#(\s|$)/.test(l) && !/^\s*#[\[!]/.test(l)));
    if (vis.length > max + 3 && !(Q.get("open") || "").split(",").includes("example")) { let seen = 0, cut = all.length; for (let k = 0; k < all.length; k++) { if (!(/^\s*#(\s|$)/.test(all[k]) && !/^\s*#[\[!]/.test(all[k]))) seen++; if (seen > max) { cut = k; break; } } const odd = (n) => (all.slice(0, n).join("\n").replace(/\\./g, "").replace(/'.'/g, "").match(/"/g) || []).length % 2; while (cut < all.length && odd(cut)) cut++; if (cut >= all.length) return exampleHTML0(code, hit); return exampleHTML0(all.slice(0, cut).join("\n"), hit).replace(/<\/pre>$/, `\n<span class="q-fold" data-unfold="example">and ${vis.length - max} more lines</span></pre>`); }
    return exampleHTML0(code, hit);
  }
  function exampleHTML0(code, hit) {
    // highlight the whole block at once (strings span lines), then mark rustdoc's hidden `# ` lines
    const HID = "\u0001";
    const src = code.split("\n").map((l) => (/^\s*#(\s|$)/.test(l) && !/^\s*#[\[!]/.test(l) ? HID + l.replace(/^(\s*)#\s?/, "$1") : l)).join("\n");
    let x = hlRust(src); if (hit) x = x.replace(new RegExp(`\\b(${hit})\\b(?![^<]*>)`, "g"), "<b>$1</b>");
    const lines = x.split("\n").map((l) => (l.includes(HID) ? `<span class="hid">${l.replace(HID, "")}\n</span>` : l + "\n"));
    return `<pre class="q-code">${lines.join("").replace(/\n$/, "")}</pre>`;
  }
  // markdown, the subset rustdoc writes; intra-doc links resolve through the page's link table
  function md(src, links = {}, opts = {}) {
    const blocks = []; const L = (src || "").split("\n"); let k = 0;
    const inline = (s) => {
      let x = esc(s);
      x = x.replace(/\[`([^`\]]+)`\](?:\(([^)]+)\)|\[([^\]]*)\])?/g, (m, t) => linkTo(t, `<code>${t}</code>`));
      x = x.replace(/\[([^\]]+)\](?:\(([^)]+)\)|\[([^\]]*)\])/g, (m, t, u) => linkTo(t, inlineMarks(t), u));
      x = x.replace(/\[([^\]]+)\](?![\[(])/g, (m, t) => (links[t] ? linkTo(t, inlineMarks(t)) : m));
      x = x.replace(/`([^`]+)`/g, "<code>$1</code>");
      return inlineMarks(x);
    };
    const inlineMarks = (x) => x.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>").replace(/(^|[^*])\*([^*\s][^*]*)\*/g, "$1<em>$2</em>");
    const linkTo = (label, html, url) => {
      const r = links[label.replace(/&lt;/g, "<").replace(/&gt;/g, ">")] || links["`" + label + "`"];
      if (r && r.id !== undefined) return `<a class="live gtl" data-i="${r.id}">${html}</a>`;
      if (r && r.url) return `<a class="ext" title="${esc(r.url)}">${html}</a>`;
      if (url && /^https?:/.test(url)) return `<a class="ext" title="${esc(url)}">${html}</a>`;
      return html;
    };
    while (k < L.length) {
      const l = L[k];
      if (/^\s*```/.test(l)) { const lang = l.replace(/^\s*```/, "").trim(); const body = []; k++; while (k < L.length && !/^\s*```/.test(L[k])) body.push(L[k++]); k++; if (!opts.noCode) blocks.push(/^(text|toml|json|sh|console)$/.test(lang) ? `<pre class="q-code">${esc(body.join("\n"))}</pre>` : exampleHTML(body.join("\n"), opts.hit)); continue; }
      if (/^\s*[-*]\s+/.test(l)) { const items = []; while (k < L.length && /^\s*[-*]\s+/.test(L[k])) { let it = L[k++].replace(/^\s*[-*]\s+/, ""); while (k < L.length && /^\s{2,}\S/.test(L[k]) && !/^\s*[-*]\s+/.test(L[k])) it += " " + L[k++].trim(); items.push(`<li>${inline(it)}</li>`); } blocks.push(`<ul>${items.join("")}</ul>`); continue; }
      if (/^#{1,4}\s/.test(l)) { blocks.push(`<div class="sub">${esc(l.replace(/^#+\s*/, ""))}</div>`); k++; continue; }
      if (!l.trim()) { k++; continue; }
      const para = []; while (k < L.length && L[k].trim() && !/^\s*```/.test(L[k]) && !/^\s*[-*]\s+/.test(L[k]) && !/^#{1,4}\s/.test(L[k])) para.push(L[k++].trim());
      blocks.push(`<p>${inline(para.join(" "))}</p>`);
    }
    return blocks.join("");
  }
  const inlineCode = (s) => esc(s).replace(/`([^`]+)`/g, "<code>$1</code>").replace(/\*\*([^*]+)\*\*/g, "$1");
  const ICON = {
    src: `<svg viewBox="0 0 12 12"><path d="M4 3 1 6l3 3M8 3l3 3-3 3"/></svg>`,
    key: `<svg viewBox="0 0 12 12"><circle cx="4" cy="6" r="2.4"/><path d="M6.4 6H11M9.5 6v2M11 6v1.6"/></svg>`,
    tick: `<svg viewBox="0 0 12 12"><path d="M2 11V3M6 11V1M10 11V5"/></svg>`,
    path: `<svg viewBox="0 0 12 12"><path d="M1.5 3.5h4l1 1.5h4v5h-9z"/></svg>`,
    strike: `<svg viewBox="0 0 12 12"><path d="M1 6h10M3 3.5h6M3 8.5h6"/></svg>`,
  };

  // ------------------------------------------------------------------ the gem (page.js gemSVG, verbatim)
  function gemSVG(kind, px) {
    const fam = KF[kind] || "ty";
    const F = ["24,2 35,13 24,10", "24,10 35,13 38,24", "35,13 46,24 38,24", "46,24 35,35 38,24", "38,24 35,35 24,38", "35,35 24,46 24,38", "24,46 13,35 24,38", "24,38 13,35 10,24", "13,35 2,24 10,24", "2,24 13,13 10,24", "10,24 13,13 24,10", "13,13 24,2 24,10"];
    const LIT = [0.4, 0.3, 0.34, 0.14, 0.08, 0.11, 0.22, 0.2, 0.3, 0.56, 0.5, 0.66];
    const glyph = (KS[kind] || "").replace(/class="f"/g, 'class="f" fill="currentColor" stroke="none" opacity=".5"');
    return `<svg class="gem ${fam}" width="${px}" height="${px}" viewBox="0 0 48 48">${F.map((p, k) => `<polygon class="fc" style="--t:${LIT[k]};--i:${k}" points="${p}"></polygon>`).join("")}`
      + `<path d="M24 2 46 24 24 46 2 24z" fill="none" stroke="currentColor" stroke-width="1"></path><path class="tb" d="M24 10 38 24 24 38 10 24z" stroke="currentColor" stroke-width=".7" stroke-opacity=".55"></path>`
      + `<g transform="translate(17.4 17.4) scale(.55)" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="square" stroke-linejoin="miter">${glyph}</g></svg>`;
  }
  const breakable = (s) => esc(s).replace(/([a-z0-9])([A-Z])/g, "$1<wbr>$2").replace(/_/g, "_<wbr>");

  // ------------------------------------------------------------------ the sentence: one clause per question
  const kindWord = { enum: "enum", struct: "struct", trait: "trait", function: "function" };
  function commonWord(p) { // "TOML", "JSON": the capitalised word most variant docs share
    const cnt = new Map(); for (const v of (P.src.decl && P.src.decl.code ? (P.harvest.anatomy.match(/class="say">([^<]*)</g) || []) : [])) for (const w of v.match(/\b[A-Z]{3,}\b/g) || []) cnt.set(w, (cnt.get(w) || 0) + 1);
    const best = [...cnt].sort((a, b) => b[1] - a[1])[0]; return best && best[1] >= 3 ? best[0] + " " : "";
  }
  function sentence() {
    const s = P.src; const cl = [];
    const obj = (html, q) => `<span class="q-ob" data-q="${q}">${html}</span>`;
    const verb = (v, q) => `<span class="q-vb" data-q="${q}">${v}</span>`;
    const rel = (v) => P.relations.find((r) => r.verb === v);
    const peekName = (id) => (D.peeks[id] ? D.peeks[id].n : "?");
    // what is it
    if (P.kind === "enum") { const n = (P.harvest.anatomy.match(/class="ar"/g) || []).length; cl.push(`${verb("is", "q-what")} ${obj(`one of ${n} ${commonWord()}kinds`, "q-what")}`); }
    else if (P.kind === "struct" && s.kinds) cl.push(`${verb("is", "q-what")} ${obj(`one of ${s.kinds.kinds.length} kinds of failure`, "q-fail")}`);
    else if (P.kind === "struct" && s.actsLike) cl.push(`${verb("is", "q-what")} ${obj(`a list that ${s.actsLike.target === "[A::Item]" ? "acts like a slice" : "acts like " + s.actsLike.target}`, "q-do")}`);
    else if (P.kind === "trait") { const c = s.contract; cl.push(`${verb("asks", "q-what")} ${obj(`for ${S(c.required.length, "method")}${c.provided.length ? `, gives ${c.provided.length}` : ""}`, "q-what")}`); }
    else if (P.kind === "function") cl.push(`${verb("takes", "q-what")} ${obj("text", "q-what")} ${verb("and gives", "q-what")} ${obj("any <i>T</i> you can deserialize", "q-what")}`);
    // how do I get one
    const cf = rel("comes from");
    if (P.kind === "trait") { const d = rel("done by"); cl.push(`${verb("done by", "q-who")} ${obj(`${d.n} types`, "q-who")}${d.yours ? `, <span class="y">${d.yours} of yours</span>` : ""}`); }
    else if (cf && P.kind !== "struct" || cf && !s.kinds) { const src = [...P.harvest.getting.matchAll(/<span class="rsrc">([\s\S]*?)<\/span>/g)].map((m) => m[1].replace(/<i class="rnm">[\s\S]*?<\/i>/g, "").replace(/<[^>]+>/g, "").replace(/^\s*from\s+/, "").replace(/\s+/g, " ").trim()).filter(Boolean); const words = [...new Set(src)].slice(0, 2); cl.push(`${verb("comes from", "q-get")} ${obj(words.length ? `${list([...words, `${cf.n - words.length} more`])}` : `${cf.n} ways`, "q-get")}`); }
    // what can I do with it
    if (s.actsLike && P.kind !== "struct") cl.push(`${verb("acts like", "q-do")} ${obj("a slice", "q-do")}`);
    if (s.becomes && s.becomes.length) cl.push(`${verb("becomes", "q-do")} ${obj(list(s.becomes.map((b) => `<span class="mono">${esc(b.into)}</span>`)), "q-do")}`);
    // what can go wrong
    if (P.kind === "function") cl.push(`${verb("fails with", "q-fail")} ${obj(`<span class="mono">Error</span>`, "q-fail")}`);
    const panics = (s.inherent || []).flatMap((b) => b.members).filter((m) => m.panics).length + (s.written || []).filter((w) => w.panics).length;
    if (panics) cl.push(`${verb("panics", "q-fail")} ${obj(`in ${S(panics, "place")}`, "q-fail")}`);
    if (s.kinds && cf) cl.push(`${verb("comes from", "q-get")} ${obj(`${cf.n} calls`, "q-get")}`);
    // who uses it
    cl.push(`${verb("used in", "q-who")} ${obj(`${P.usedIn} places`, "q-who")}${P.usedYours ? `, <span class="y">${P.usedYours} in yours</span>` : ""}`);
    // what changed
    if (P.since && P.since.first) cl.push(`<span data-card="since">${verb("since", "q-changed")}</span> ${obj(`<span class="mono">${P.since.oldestIsFirst ? "≤ " : ""}${P.since.first}</span>`, "q-changed")}`);
    cl[0] = `${esc(P.name)} ${cl[0]}`;
    const html = cl.map((c, k) => `<span class="q-cl">${c}${k < cl.length - 1 ? '<span class="q-sep">·</span>' : ""}</span>`).join(" ");
    return `<div class="q-sent">${html}</div>`;
  }
  function marks() {
    const s = P.src; const out = [];
    out.push(`<span class="q-mk" data-card="path">${ICON.path}<span class="mono">${esc(P.path)}</span></span>`);
    if (P.aliases.length) out.push(`<span class="q-mk" data-card="alias"><span>also</span><span class="mono">${esc(P.aliases[0])}</span></span>`);
    if (P.since) { /* the sentence says since; its card waits on that clause */ }
    else out.push(`<span class="q-mk" data-card="since">${ICON.tick}<span>since unknown</span></span>`);
    if (s.decl && s.decl.gate) out.push(`<span class="q-mk" data-card="gate:${esc(s.decl.gate.need[0].f)}">${ICON.key}<span class="mono">${esc(s.decl.gate.need[0].f)}</span></span>`);
    if (s.decl && s.decl.deprecated) out.push(`<span class="q-mk" data-card="dep">${ICON.strike}<span>deprecated</span></span>`);
    return `<div class="q-marks">${out.join("")}</div>`;
  }

  // ------------------------------------------------------------------ the declaration, with its bounds in words
  function declHTML() {
    const d = P.src.decl; if (!d) return "";
    let code = d.code; const long = code.split("\n").length > 6;
    let h = hlRust(long ? code.split("\n").slice(0, 1).join("\n") + " … }" : code).replace(/<span class="q-co">/g, '<span class="q-cm">');
    d.bounds.forEach((b, k) => { const t = esc(b.exact).replace(/[.*+?^${}()|[\]\\]/g, "\\$&"); const re = new RegExp(t.replace(/\\ /g, "\\s*").replace(/:\\s\*/g, ":\\s*")); });
    // mark each bound in the rendered code: find its subject and bounds as plain text
    const plain = document.createElement("div"); plain.innerHTML = h;
    const walk = (node) => { for (const c of [...node.childNodes]) { if (c.nodeType === 3) { let t = c.nodeValue; for (const [k, b] of d.bounds.entries()) { const bits = b.exact.split(/:\s*/); const re = new RegExp(`${bits[0].replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}:\\s*${bits.slice(1).join(":").replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`); if (re.test(t)) { const sp = document.createElement("span"); sp.innerHTML = esc(t).replace(re, (m) => `<span class="q-bd" data-card="bd:${k}">${esc(m)}</span>`); c.replaceWith(...sp.childNodes); break; } } } else walk(c); } };
    walk(plain);
    if (!plain.querySelector(".bd")) { // bounds split across highlight spans: wrap the whole generics/where text instead
      plain.innerHTML = plain.innerHTML.replace(/(&lt;)([^&]*?:.*?)(&gt;)/, (m, a, b, c) => `${a}<span class="q-bd" data-card="bd:0">${b}</span>${c}`);
      plain.innerHTML = plain.innerHTML.replace(/(\n    )([^\n]+:[^\n]+),$/m, (m, a, b) => `${a}<span class="q-bd" data-card="bd:${d.bounds.length - 1}">${b}</span>,`);
    }
    const words = d.bounds.length ? `<div class="q-bw">${d.bounds.map((b) => `<span><span class="mono">${esc(b.exact)}</span> ${esc(b.words)}</span>`).join("")}</div>` : "";
    return `<div class="q-turn" data-flip="decl"><div class="q-face"><pre class="q-decl">${plain.innerHTML}<span class="q-flip" data-flip="decl">${ICON.src}source</span></pre>${words}</div></div>`;
  }

  // ------------------------------------------------------------------ relations: rows (A), scales (B), combs (C)
  function combSVG(r, w = 150, opts = {}) {
    const ids = []; const cls = []; for (const b of r.bands) for (const p of b.pkgs) { for (const j of p.ids) { ids.push(j); cls.push(b.band === "yours" ? "y" : b.band === "here" ? "q-h" : ""); } cls.push("gap"); ids.push(-1); }
    ids.pop(); cls.pop();
    const n = ids.length; if (!n) return "";
    const pitch = Math.min(4, w / n); let x = 0; const out = [];
    if (pitch >= 1.6) { for (let k = 0; k < n; k++) { if (cls[k] === "gap") { x += pitch * 1.5; continue; } out.push(`<line class="q-t ${cls[k]}${opts.cur === k ? " cur" : ""}" x1="${x.toFixed(1)}" x2="${x.toFixed(1)}" y1="${cls[k] === "y" ? 1 : 3}" y2="11"/>`); x += pitch; } }
    else { // a band: one bar per package, as wide as its share
      const tot = r.n; const unit = (w - 2 * r.bands.reduce((a, b) => a + b.pkgs.length, 0)) / tot;
      for (const b of r.bands) for (const p of b.pkgs) { const ww = Math.max(1.5, p.n * unit); out.push(`<rect class="q-band ${b.band === "yours" ? "y" : b.band === "here" ? "q-h" : ""}" x="${x.toFixed(1)}" y="${b.band === "yours" ? 1 : 4}" width="${ww.toFixed(1)}" height="${b.band === "yours" ? 10 : 7}"/>`); x += ww + 2; }
    }
    return `<svg class="q-comb" width="${Math.ceil(x)}" height="12" viewBox="0 0 ${Math.ceil(x)} 12">${out.join("")}</svg>`;
  }
  const nameEl = (r, j, k) => { const pk = D.peeks[j] || { n: "?", k: "unknown" }; const twin = r.ids.slice(0, 8).some((x) => x !== j && D.peeks[x] && D.peeks[x].n === pk.n); return `<span class="q-ne${pk.y ? " y" : ""}" data-rel="${esc(r.verb)}" data-j="${j}" data-card="ne:${esc(r.verb)}:${k}">${kmark(pk.k)}<span class="q-t">${esc(pk.n)}</span>${twin ? `<span class="q-dis">${esc(pk.f.replace(/:\d+$/, "").replace(/\.rs$/, ""))}</span>` : ""}</span>`; };
  const TOP = 3;
  function relRow(r, open) {
    const shown = r.ids.slice(0, TOP);
    const bandW = { yours: "yours", here: `in ${P.pkg}`, elsewhere: "elsewhere" };
    let unf = "";
    if (open) {
      const openPkg = (Q.get("open") || "").split(":")[1];
      const bandW = { yours: "yours", here: `in ${P.pkg}`, elsewhere: "elsewhere" };
      const pkgLine = (b, p) => `<span class="q-pk${b.band === "yours" ? " y" : ""}">${esc(p.pkg)}<i>${p.n}</i></span><span class="q-pn" data-n="${p.n}">${(openPkg === p.pkg ? p.ids : p.ids.slice(0, 12)).map((j) => nameEl(r, j, r.ids.indexOf(j))).join("")}${openPkg === p.pkg ? "" : `<span class="q-plus" data-unfold="${esc(r.verb)}:${esc(p.pkg)}"></span>`}</span>`;
      const bands = r.bands.map((b) => `<span class="q-bl${b.band === "yours" ? " y" : ""}">${bandW[b.band]} · ${b.n}${b.pkgs.length > 1 ? ` in ${b.pkgs.length} packages` : ""}</span>` + b.pkgs.slice(0, 7).map((p) => pkgLine(b, p)).join("") + (b.pkgs.length > 7 ? `<span class="q-pk">and ${b.pkgs.length - 7} more</span><span></span>` : ""));
      // a trait's own crate covers the standard library's types: the other half of docs.rs's "Implementors"
      const f = r.verb === "done by" && P.src.contract && P.src.contract.foreign;
      if (f && f.count) bands.splice(r.bands[0] && r.bands[0].band === "yours" ? 1 : 0, 0, `<span class="q-bl">in ${esc(P.pkg)}, for the standard library’s types · ${f.count}${f.unexpanded ? ` and ${f.unexpanded} macro-made we did not read` : ""}</span>` + f.groups.filter((g) => g.fam !== "other").map((g) => `<span class="q-pk">${esc(g.fam)}<i>${g.names.length}</i></span><span class="q-pn" data-n="${g.names.length}">${g.names.map((nm) => `<span class="q-ne q-std">${esc(nm)}</span>`).join("")}<span class="q-plus"></span></span>`).join(""));
      unf = `<div class="q-unf">${bands.join("")}<div class="q-foot"><span data-fold="${esc(r.verb)}">fold</span>${r.n > 40 ? `<span class="q-filt">filter ${r.n}</span>` : ""}<span data-door="${esc(r.verb)}">see all ${r.n} in the graph</span></div></div>`;
    }
    return `<div class="q-rl${open ? " open" : ""}" data-verb="${esc(r.verb)}" data-card-row="rl:${esc(r.verb)}"><span class="q-vb" data-card="vb:${esc(r.verb)}">${esc(r.verb)}</span>`
      + `<span class="q-ns">${open ? `<span class="q-sum">${r.n}${r.yours ? `, <span class="y">${r.yours} of yours</span>` : ""}, in ${r.bands.reduce((a, b) => a + b.pkgs.length, 0)} packages</span>` : shown.map((j, k) => nameEl(r, j, k)).join("")}${!open && r.n > TOP ? `<span class="more" data-unfold="${esc(r.verb)}">and ${r.n - TOP} more</span>` : ""}</span>`
      + `<span class="q-tz" title="${r.n} in all${r.yours ? `, ${r.yours} yours` : ""}">${combSVG(r, 72)}<span><b>${r.n}</b>${r.yours ? ` · <b class="y">${r.yours}</b>` : ""}</span></span>`
      + `<span class="q-xk">${esc(xkind(r.verb))}</span>` + unf + `</div>`;
  }
  const xkind = (v) => ({ "comes from": "returns it · From", "done by": "impl · derive", "taken by": "parameter type", "held by": "field / variant type", "called on by": "calls a method", "asked for by": "generic bound", "used by": "type mention", "called by": "call", calls: "call" })[v] || "";
  function relationsHTML(verbs, opts = {}) {
    const rs = P.relations.filter((r) => verbs.includes(r.verb)).sort((a, b) => verbs.indexOf(a.verb) - verbs.indexOf(b.verb));
    if (!rs.length) return "";
    const opened = new Set((Q.get("open") || "").split(",").map((x) => x.split(":")[0]).filter(Boolean));
    const vbw = Math.max(...rs.map((r) => r.verb.length)) * 0.62 + 0.6;
    if (REL === "b" && opts.who) return scalesHTML(rs);
    if (REL === "c" && opts.who) return `<div class="q-rel q-combs" style="--vbw:${vbw}em">${rs.map((r) => `<div class="q-rl" data-verb="${esc(r.verb)}"><span class="q-vb">${esc(r.verb)}</span>${combSVG(r, 260, { cur: Q.get("hover") === "comb:" + r.verb ? 1 : -1 })}<span class="q-cn">${esc((D.peeks[r.ids[0]] || {}).n || "")}<i>${r.n}${r.yours ? ` · ${r.yours} yours` : ""}</i></span></div>`).join("")}</div>`;
    let prevDir = null;
    return `<div class="q-rel" style="--vbw:${vbw}em">${rs.map((r) => { const gap = prevDir && prevDir !== r.dir; prevDir = r.dir; return relRow(r, opened.has(r.verb)).replace('class="q-rl', `class="q-rl${gap ? " gap" : ""}`); }).join("")}</div>`;
  }
  function scalesHTML(rs) {
    const L = rs.filter((r) => r.dir === "in"), R = rs.filter((r) => r.dir === "out");
    const grp = (r) => `<div class="q-grp"><span class="q-gv">${esc(r.verb)} · ${r.n}</span><span class="q-gn">${r.ids.slice(0, 3).map((j, k) => nameEl(r, j, k)).join("")}${r.n > 3 ? `<span class="more">+ ${r.n - 3}</span>` : ""}</span></div>`;
    return `<div class="q-scales"><div class="col q-l"><span class="q-dirw">where it comes from</span>${L.map(grp).join("")}</div><div class="q-spine"><i></i></div><div class="col r"><span class="q-dirw">where it goes</span>${R.map(grp).join("")}</div></div>`;
  }
  // the door into the graph: the prism drawn small and true — one strand per group, left in, right out
  function doorHTML() {
    const rs = P.relations; const L = rs.filter((r) => r.dir === "in"), R = rs.filter((r) => r.dir === "out");
    const H = 30, cx = 44, cy = H / 2;
    const strands = (side, grp) => grp.map((r, k) => { const y = grp.length === 1 ? cy : 3 + (k * (H - 6)) / (grp.length - 1); const len = 14 + Math.min(24, Math.log2(r.n + 1) * 4.5); const x1 = cx + side * (6 + len); return `<path class="q-st ${side < 0 ? "q-sl" : "sr"}${r.yours ? " y" : ""}" style="transform-origin:${cx}px ${cy}px" d="M${cx + side * 5} ${cy} C${cx + side * 12} ${cy} ${x1 - side * 8} ${y} ${x1} ${y}"/>`; }).join("");
    return `<button class="q-door" data-door="1" title="See it in the graph"><span class="q-lab">see it in the graph</span><kbd class="q-kc">G</kbd><svg width="${cx * 2}" height="${H}" viewBox="0 0 ${cx * 2} ${H}">${strands(-1, L)}${strands(1, R)}<path class="gm" d="M${cx} ${cy - 5} ${cx + 5} ${cy} ${cx} ${cy + 5} ${cx - 5} ${cy}z"/></svg></button>`;
  }

  // ------------------------------------------------------------------ what it does: can (one arrival axis), does, acts like, becomes
  const CAPW = { Deserializer: "is a deserializer", IntoDeserializer: "into a deserializer", Copy: "copies freely", Clone: "clones", Debug: "debug-prints", Display: "prints", PartialEq: "compares", Eq: "compares", PartialOrd: "orders", Ord: "sorts", Hash: "hashes", Default: "has a default", Serialize: "serializes", Deserialize: "deserializes", Error: "is an error", StdError: "is an error", FromStr: "parses", From: "converts", Index: "indexes", IndexMut: "indexes", Deref: "reads as a slice", DerefMut: "writes as a slice", AsRef: "borrows as a slice", AsMut: "borrows as a slice", Borrow: "borrows as a slice", BorrowMut: "borrows as a slice", Extend: "extends", FromIterator: "collects", IntoIterator: "iterates", Drop: "cleans up", Send: "crosses threads", ToString: "to text", ToOwned: "to owned", DeserializeOwned: "deserializes owned", Write: "writes bytes" };
  function canHTML() {
    const s = P.src; if (!s.written) return "";
    const caps = []; const seen = new Map();
    const add = (word, how, data) => { const key = word + "|" + how + (data.shut ? "|shut" : ""); if (seen.has(key)) { seen.get(key).n++; seen.get(key).items.push(data); return; } const c = { word, how, n: 1, items: [data], ...data }; seen.set(key, c); caps.push(c); };
    for (const d of (s.decl.derives || []).filter((d) => !({ Clone: "Copy", PartialEq: "Eq", PartialOrd: "Ord" }[d] && s.decl.derives.includes({ Clone: "Copy", PartialEq: "Eq", PartialOrd: "Ord" }[d])))) add(CAPW[d] || d, "derived", { trait: d });
    const SLICE = new Set(["Deref", "DerefMut", "AsRef", "AsMut", "Borrow", "BorrowMut"]);
    for (const w of s.written) { if (/^Drop$/.test(w.name) && w.gate && !w.gate.open) continue; if (s.actsLike && SLICE.has(w.name)) continue; add(CAPW[w.name] || w.label || w.name, "written", { trait: w.label || w.name, shut: w.gate && w.gate.need && !w.gate.open, dep: !!w.deprecated, w }); }
    const blank = s.blankets || []; const through = blank.filter((b) => /through/.test(b.why)); const usual = blank.filter((b) => !/through/.test(b.why));
    for (const b of through) add(CAPW[b.t] || b.t, "through", { trait: b.t, why: b.why });
    const USUAL = new Set(["Clone", "Copy", "Debug", "PartialEq", "Eq", "PartialOrd", "Ord", "Hash", "Default", "Display", "Drop", "Send", "Sync", "StdError"]);
    const common = caps.filter((c) => !c.shut && (USUAL.has(c.trait) || c.how === "through"));
    const open = caps.filter((c) => !c.shut && !common.includes(c)), shut = caps.filter((c) => c.shut);
    const capEl = (c) => `<span class="q-cap q-${c.how}${c.dep ? " dep" : ""}" data-card="cap:${esc(c.word)}"><i></i><span class="q-w">${esc(c.word)}</span>${c.n > 1 ? `<span class="q-x">×${c.n}</span>` : ""}${c.word !== c.trait ? `<span class="q-tr">${esc(c.trait)}</span>` : ""}</span>`;
    const autos = s.autos; const autoN = autos ? Object.keys(autos.t).length : 0;
    const no = autos ? Object.entries(autos.t).filter(([, v]) => v === "no").map(([k]) => k) : [];
    const exceptEl = no.length ? `<span class="q-cap q-parts q-except" data-card="cap:auto"><i></i><span class="q-w">${no.length === 2 && no.every((k) => /UnwindSafe/.test(k)) ? "not unwind-safe" : "not " + list(no)}</span></span>` : "";
    const nUsual = usual.length + autoN - no.length + common.reduce((a, c) => a + 1, 0);
    const usualOpen = (Q.get("open") || "").split(",").includes("usual");
    const usualList = usualOpen ? `<div class="q-usual">${[
      ["derived", common.filter((c) => c.how === "derived").map((c) => c.word)],
      ["written", common.filter((c) => c.how === "written").map((c) => c.word)],
      ["through another trait", common.filter((c) => c.how === "through").map((c) => `${c.word} (${c.why.replace(/^through /, "")})`)],
      ["from its parts", autos ? Object.entries(autos.t).filter(([, v]) => v !== "no").map(([k, v]) => (v === "yes" ? autos.words[k] : `${autos.words[k]} ${v.replace(/^written: /, "")}`)) : []],
      ["every type", usual.map((b) => b.t)],
    ].filter(([, xs]) => xs.length).map(([h, xs]) => `<span class="q-uh">${h}</span><span>${xs.map(esc).join(" · ")}</span>`).join("")}</div>` : "";
    const usualEl = nUsual ? `<span class="q-cap q-through fold" data-card="cap:usual" data-unfold="usual"><i></i><span class="q-w">${open.length ? "and " : ""}the usual ${nUsual}</span></span>` : "";
    P._common = common;
    const shutEl = shut.length ? `<span class="q-cap q-written shut fold" data-card="cap:shut">${ICON.key}<span class="q-w">${shut.length} behind features you don’t turn on</span></span>` : "";
    return `<div class="q-can">${open.map(capEl).join("")}${exceptEl}${usualEl}${shutEl}</div>${usualList}`;
  }
  function annotateDoes(html) {
    if (!html) return "";
    const t = document.createElement("div"); t.innerHTML = html;
    const sec = t.firstElementChild; const h2 = sec.querySelector("h2"); if (h2) h2.remove();
    // drop "through its traits": the can line and the trait cards say it, one fold down
    let cut = false; for (const el of [...sec.children]) { if (el.classList.contains("cgrp") && /through its traits/.test(el.textContent)) cut = true; if (cut) el.remove(); }
    const members = new Map(); const blocks = (P.src.inherent || []);
    for (const b of blocks) for (const m of b.members) members.set(m.n, { m, b });
    const condRows = new Map();
    for (const row of sec.querySelectorAll(".crow2.mrow")) {
      const a = row.querySelector("a.gtl"); const nm = a ? a.textContent : ""; const hit = members.get(nm); if (!hit) continue;
      const { m, b } = hit; const marks = [];
      const gate = m.gate || b.gate; const shut = gate && gate.need && !gate.open;
      if (shut) { row.classList.add("shut"); marks.push(`<span class="q-gate shut keep" data-card="gate:${esc(gate.need[0].f)}">${ICON.key}${esc(gate.need[0].f)}</span>`); }
      if (m.deprecated) row.classList.add("dep");
      if (m.unsafe) marks.push(`<span class="keep">you promise</span>`);
      if (m.panics) marks.push(`<span>can panic</span>`);
      if (m.since && m.since.first) marks.push(`<span>${m.since.oldest ? "≤ " : ""}${esc(m.since.first)}</span>`);
      if (m.since && m.since.changes && m.since.changes.length) marks.push(`<span>changed ${esc(m.since.changes.map((c) => c.v.replace(/\+.*/, "")).join(", "))}</span>`);
      if (m.since && m.since.goneIn) marks.push(`<span>gone in ${esc(m.since.goneIn.replace(/\+.*/, ""))}</span>`);
      marks.push(`<span>${esc((m.file || "").split("/").pop())}:${m.line}</span>`);
      row.dataset.card = "row:" + nm; row.dataset.unfold = "row:" + nm;
      if ((Q.get("open") || "").split(",").includes("row:" + nm)) { // the one-line summary folds out to the member's whole doc
        const full = document.createElement("div"); full.className = "q-mdoc";
        full.innerHTML = (m.docs || []).map((d) => (d.title ? `<div class="q-msec">${esc(d.title)}</div>` : "") + md(d.md, m.links || {}, { hit: nm })).join("") + `<div class="q-msrc">${esc((m.file || "").split("/").pop())}:${m.line}${m.since && m.since.first ? ` · since ${m.since.oldest ? "≤ " : ""}${esc(m.since.first)}` : ""} · <span class="q-flip on">${ICON.src}source</span></div>`;
        row.classList.add("unfolded"); row.after(full);
      }
      const rt = document.createElement("span"); rt.className = "rt"; rt.innerHTML = marks.join(""); const old = row.querySelector(".rt"); if (old) old.replaceWith(rt); else row.appendChild(rt);
      if (b.cond.length || b.special) { const key = b.head; (condRows.get(key) || condRows.set(key, { b, rows: [] }).get(key)).rows.push(row); }
    }
    for (const { b, rows } of condRows.values()) {
      const g = document.createElement("div"); g.className = "cgrp cond";
      const words = b.special ? (/\[T; N\]/.test(b.special) ? "when it is backed by an array [T; N]" : `for ${b.special}`) : "when " + b.cond.map((c) => c.words).join(" and ");
      g.innerHTML = `${esc(words)}${b.gate && b.gate.need ? ` <span class="q-gate ${b.gate.open ? "open" : "shut"}" data-card="gate:${esc(b.gate.need[0].f)}">${ICON.key}${esc(b.gate.need[0].f)}</span>` : ""}<span class="q-x">${esc(b.head)}</span>`;
      sec.appendChild(g); for (const r of rows) sec.appendChild(r);
    }
    for (const g of [...sec.querySelectorAll(".cgrp:not(.cond)")]) { let n = g.nextElementSibling; if (!n || n.classList.contains("cgrp")) g.remove(); }
    const opened = (Q.get("open") || "").split(",");
    let gi = 0;
    for (const g of [...sec.querySelectorAll(".cgrp")]) {
      const rows = []; let n = g.nextElementSibling; while (n && !n.classList.contains("cgrp")) { if (!n.classList.contains("q-mdoc")) rows.push(n); n = n.nextElementSibling; }
      const key = "does:" + gi++;
      if (rows.length > 7 && !opened.includes(key) && !rows.slice(6).some((r) => r.classList.contains("unfolded"))) { rows.slice(6).forEach((r) => r.remove()); const f = document.createElement("div"); f.className = "q-dmore"; f.dataset.unfold = key; f.textContent = `and ${rows.length - 6} more that ${g.textContent.replace(/^(reads|changes|uses) it.*/, (m, v) => ({ reads: "read it", changes: "change it", uses: "use it up" })[v]).replace(/^makes one.*/, "make one")}`; rows[5].after(f); }
    }
    return sec.innerHTML;
  }
  function actsLikeHTML() {
    const a = P.src.actsLike; if (!a) return "";
    const open = (Q.get("open") || "").split(",").includes("acts");
    const own = a.groups.find((g) => !g.cond) || { methods: [] };
    const top = own.methods.filter((m) => !m.shadowed).slice(0, 5).map((m) => m.n);
    return `<div class="q-acts"><div class="q-al"><b>acts like a slice</b> <span class="mono">&amp;[its items]</span> <span>through ${a.through}${a.mutable ? " and DerefMut" : ""}: ${a.count} more methods, like ${list(top.map((n) => `<span class="mono">${n}</span>`))}</span> <span class="more" data-unfold="acts">${open ? "fold" : `and ${a.count - top.length} more`}</span></div>`
      + (open ? a.groups.map((g) => `${g.words ? `<div class="q-gh">${esc(g.words)}</div>` : ""}<div class="q-grid">${g.methods.map((m) => `<span class="q-m${m.shadowed ? " q-sh" : ""}" title="${esc(m.sig)}">${esc(m.n)}</span>`).join("")}</div>`).join("") + `<div class="q-gh">struck through: SmallVec’s own ${list(a.shadowed)} come first · Rust ${a.since[0]} to ${a.since[a.since.length - 1]}</div>` : "")
      + `</div>`;
  }

  // ------------------------------------------------------------------ what can go wrong: one component for Errors / Panics / Safety
  function failHTML() {
    const s = P.src; const rows = [];
    const sec = (k) => (s.docs.sections || []).filter((x) => x.kind === k);
    for (const e of sec("errors")) rows.push({ k: "fails when", body: `<span class="q-fw">${md(e.md, s.docs.links).replace(/<\/?p>/g, "")}</span>` });
    if (P.kind === "function") { const ek = D.pages.error; rows.push({ k: "fails with", body: `<span class="q-fm"><a class="gtl" data-page="error" data-i="${ek.id}">serde_json::Error</a> <span class="q-p">·</span> one of ${ek.src.kinds.kinds.length} kinds: ${list(ek.src.kinds.kinds.map((x) => x.n))}</span>` }); }
    for (const e of sec("panics")) rows.push({ k: "panics when", body: `<span class="q-fw">${md(e.md, s.docs.links).replace(/<\/?p>/g, "")}</span>` });
    const members = (s.inherent || []).flatMap((b) => b.members);
    const mp = members.filter((m) => m.panics); if (mp.length) rows.push({ k: "panics when", body: mp.slice(0, 6).map((m) => `<span class="q-fm"><a class="gtl">${esc(m.n)}</a> <span class="q-p">—</span> <span class="q-fw">${inlineCode(firstLine(m.panics))}</span></span>`).join("") + (mp.length > 6 ? `<span class="q-fx">and ${mp.length - 6} more</span>` : "") });
    const wp = (s.written || []).filter((w) => w.panics); if (wp.length) { const w = wp[0]; rows.push({ k: "panics when", body: `<span class="q-fm">${wp.map((x) => (x.name === "Index" ? "value[key]" : "value[key] = …")).join(" and ")} <span class="q-p">—</span> <span class="q-fw">the key or index is missing (“${esc(w.panics)}”)</span></span><span class="q-fx">read from its code; its docs don’t say</span>` }); }
    for (const w of []) rows.push({ k: "panics when", body: `<span class="q-fm">${w.name === "Index" ? "value[key]" : "value[key] = …"} <span class="q-p">—</span> <span class="q-fw">the key or index is missing (“${esc(w.panics)}”)</span></span><span class="q-fx">read from its code; its docs don’t say</span>` });
    const ms = members.filter((m) => m.safety); if (ms.length) rows.push({ k: "you promise", cls: "safety", body: ms.slice(0, 6).map((m) => `<span class="q-fm"><a class="gtl">${esc(m.n)}</a> <span class="q-p">—</span> <span class="q-fw">${inlineCode(firstLine(m.safety))}</span></span>`).join("") });
    for (const e of sec("safety")) rows.push({ k: "you promise", cls: "safety", body: `<span class="q-fw">${md(e.md, s.docs.links).replace(/<\/?p>/g, "")}</span>` });
    const errs = members.filter((m) => m.errors); if (errs.length) rows.push({ k: "fails when", body: errs.map((m) => `<span class="q-fm"><a class="gtl">${esc(m.n)}</a> <span class="q-p">—</span> <span class="q-fw">${inlineCode(firstLine(m.errors))}</span></span>`).join("") });
    let kinds = "";
    if (s.kinds) {
      const open = (Q.get("open") || "").split(",");
      kinds = `<div class="q-kinds" style="--kw:${Math.max(...s.kinds.kinds.map((k) => k.n.length)) * 0.62 + 0.8}em">${s.kinds.kinds.map((k) => { const o = open.includes("kind:" + k.n); const shown = o ? k.causes : k.causes.slice(0, 4); return `<div class="q-kd" data-card="kind:${k.n}"><span class="q-kn">${esc(k.n)}</span><span class="q-ks">${inlineCode(k.doc)}</span><span class="q-tz">${S(k.causes.length, "cause")}</span><span class="q-kc">${shown.map((c) => `<span>${esc(c.msg)}</span>`).join("")}${k.causes.length > shown.length ? `<span class="more">and ${k.causes.length - shown.length} more</span>` : ""}</span></div>`; }).join("")}</div>`;
    }
    if (!rows.length && !kinds) return "";
    const fw = Math.max(...rows.map((r) => r.k.length), 6) * 0.6 + 0.6;
    return (kinds ? `<div class="sub">told apart by <span class="mono">classify()</span></div>${kinds}` : "") + (rows.length ? `<div class="q-fail" style="--fw:${fw}em">${rows.map((r) => `<div class="q-fr"><span class="q-fk ${r.cls || ""}">${r.k}</span><span class="q-fb">${r.body}</span></div>`).join("")}</div>` : "");
  }
  const firstLine = (s) => (s || "").split(/\n\s*\n/)[0].replace(/\n/g, " ").trim();

  // ------------------------------------------------------------------ what changed
  function changedHTML() {
    const s = P.since;
    if (!s) return `<div class="q-hist"><div class="q-hl"><span class="q-unk">No release history read for ${esc(P.pkg)} yet.</span> You build ${esc(P.pkg)} <span class="mono">${esc(P.version)}</span>; when its history is read, this says when ${esc(P.name)} arrived and what changed since.</div></div>`;
    const R = D.releases && D.releases[P.pkg];
    const lines = [];
    lines.push(`<div class="q-hl">In ${esc(P.pkg)} ${s.oldestIsFirst ? `since <b class="mono">${esc(s.first)}</b> or earlier <span class="q-unk">— the oldest of the ${s.read.length} releases read, of ${s.versions}</span>` : `since <b class="mono">${esc(s.first)}</b>`}. You pin <span class="mono" style="color:var(--mint)">${esc(s.pinned)}</span>.</div>`);
    const rows = s.changed.map((c) => `<div class="q-hrow"><span class="q-v">${esc(c.v.replace(/\+.*/, ""))}</span><span class="q-d">${c.changed ? `${S(c.changed, "change")}${c.breaking ? `, ${c.breaking} breaking` : ""}` : ""}${c.added ? `${c.changed ? " · " : ""}${c.added} added` : ""}${c.sample.length ? ` — ${[...new Set(c.sample.map((x) => x.path.split("::").pop()))].slice(0, 3).map((x) => `<span class="mono">${esc(x)}</span>`).join(", ")}` : ""}</span></div>`);
    if (!s.changed.length) lines.push(`<div class="q-hl">Nothing about it changed across the ${s.read.length} releases read (${s.read.map((v) => `<span class="mono">${esc(v)}</span>`).join(", ")}).</div>`);
    return `<div class="q-hist">${lines.join("")}${combHist()}${rows.join("")}</div>`;
  }
  function combHist() {
    const s = P.since; const V = D.versions && D.versions[P.pkg]; if (!V) return "";
    const W0 = 560, n = V.length; const pitch = W0 / n; const out = [];
    V.forEach((v, k) => { const x = k * pitch + 1; const read = s.read.includes(v.v); const pin = v.v === s.pinned; const first = v.v === s.first; const ch = s.changed.some((c) => c.v === v.v); const major = k > 0 && v.v.split(".")[0] !== V[k - 1].v.split(".")[0] || (v.v.startsWith("0.") && V[k - 1] && v.v.split(".")[1] !== V[k - 1].v.split(".")[1]);
      out.push(`<line class="rt${read ? " q-local" : ""}${ch ? " q-ch" : ""}${first ? " q-first" : ""}${pin ? " q-pin" : ""}" x1="${x.toFixed(1)}" x2="${x.toFixed(1)}" y1="${pin || first || ch ? 4 : major ? 8 : 12}" y2="20"/>`);
      if (pin) out.push(`<text class="q-pin" x="${x.toFixed(1)}" y="32" text-anchor="middle">${esc(v.v)} you</text>`);
      if (first) out.push(`<text class="q-first" x="${x.toFixed(1)}" y="32" text-anchor="${k < 8 ? "start" : "middle"}">${esc(v.v)}</text>`); });
    return `<svg class="q-hcomb" viewBox="0 -2 ${W0 + 4} 36" preserveAspectRatio="xMinYMid meet">${out.join("")}</svg>`;
  }

  // ------------------------------------------------------------------ examples: its docs, then your tree
  function examplesHTML() {
    const ex = (P.src.docs && P.src.docs.examples) || [];
    const doc = ex.slice(0, 1).map((e) => `<div class="q-ex"><div class="q-cap2">from its docs</div>${exampleHTML(e.code, P.name)}</div>`).join("");
    return doc;
  }

  // ------------------------------------------------------------------ the page
  function section(id, title, body, extra = "") { if (!body) return ""; return `<section id="${id}" class="p2q q-turn" data-flip="${id}"><div class="q-face">${title ? `<div class="q-qh"><h2>${title}</h2>${extra}<span class="q-flip" data-flip="${id}">${ICON.src}source</span></div>` : ""}${body}</div></section>`; }
  function render() {
    const s = P.src || {};
    const hero = `<div class="chero"><span class="hgem">${gemSVG(P.kind, 64)}</span><div class="col" style="min-width:0"><span class="nm${s.decl && s.decl.deprecated ? " dep" : ""}">${breakable(P.name)}</span>${P.lede ? `<span class="ld">${inlineCode(P.lede)}</span>` : ""}</div></div>`
      + `<div class="q-herofoot">${sentence()}${marks()}</div>`;
    // what is it: the shape, then what its author says about it
    const body = (s.docs && s.docs.sections || []).filter((x) => x.kind === "body" || x.kind === "other");
    const docs = body.length ? `<div class="q-docs">${body.map((b) => (b.title ? `<div class="sub">${esc(b.title)}</div>` : "") + md(b.md, s.docs.links, { hit: P.name })).join("")}</div>` : "";
    const calls = P.kind === "function" ? relationsHTML(["calls"]) : "";
    const contract = s.contract ? contractHTML() : "";
    const what = `<section id="q-what" class="p2q">${declHTML()}<div style="height:22px"></div>${P.kind === "trait" ? contractFoot(P.harvest.anatomy) : P.harvest.anatomy || ""}${calls ? `<div style="height:8px"></div>${calls}` : ""}${docs ? `<div style="height:14px"></div>${docs}` : ""}</section>`;
    // how do I get one
    const getTitle = P.kind === "function" ? "" : P.kind === "trait" ? "Doing it" : "Getting one";
    let get = "";
    if (P.kind === "trait") get = doingItHTML();
    else if (P.harvest.getting) get = `<div class="gget">${gettingHTML()}</div>` + relationsHTML(["comes from"]);
    // what can I do with it
    const does = annotateDoes(P.harvest.does);
    const becomes = (s.becomes || []).length ? `<div class="q-acts"><div class="q-al"><b>becomes</b> ${s.becomes.map((b) => `<span class="mono">${esc(b.into)}</span> <span>${b.gate && b.gate.need && !b.gate.open ? `<span class="q-gate ${b.gate.open ? "open" : "shut"}" data-card="gate:${esc(b.gate.need[0].f)}">${ICON.key}${esc(b.gate.need[0].f)}</span>` : ""} ${b.docs ? esc(b.docs.summary.replace(/`/g, "")) : ""}</span>`).join("")}</div></div>` : "";
    const doHTML = [canHTML(), actsLikeHTML(), becomes, does].filter(Boolean).join('<div style="height:10px"></div>');
    // who uses it, and how
    const who = relationsHTML(["done by", "called by", "taken by", "held by", "called on by", "asked for by", "used by"], { who: true });
    const uses = P.harvest.inuse ? `<div class="sub">${/class="gtl y"/.test(P.harvest.inuse) ? "in your tree" : "in use"}</div><div class="inuse">` + P.harvest.inuse.replace(/^<section class="csec inuse"><h2>In use<\/h2>/, "").replace(/<\/section>$/, "") + "</div>" : "";
    const tree = (P.treeUses || []).length ? `<div class="sub">in your tree</div><div class="inuse">${P.treeUses.map((u) => `<figure class="use"><figcaption><a class="gtl y" data-i="${u.id}">${esc(D.peeks[u.id] ? D.peeks[u.id].n : "")}</a><span class="uw">${esc(u.pkg)} · ${esc(u.file)}:${u.line}</span></figcaption><pre>${esc(u.code).replace(new RegExp(`\\b(${P.name})\\b`), "<b>$1</b>")}</pre></figure>`).join("")}</div>` : "";
    const usesAll = tree || uses;
    const whoBody = who + (examplesHTML() || usesAll ? `<div style="height:22px"></div>${examplesHTML()}${usesAll ? `<div style="height:10px"></div>${usesAll}` : ""}` : "");
    const fail = failHTML();
    $("preader").innerHTML = `<div class="cfol pfol p2">${hero}${what}`
      + section("q-get", getTitle, get)
      + section("q-do", "What it does", P.kind === "trait" || P.kind === "function" ? "" : doHTML)
      + section("q-fail", s.kinds ? "How it fails" : "When it fails", fail)
      + section("q-who", "Who uses it", whoBody, doorHTML())
      + section("q-changed", "What changed", changedHTML())
      + (P.harvest.cousins || "") + `</div>`;
    $("pshelf").innerHTML = shelfHTML(); fitRows();
    document.title = `${P.name} — Page`;
    const here = document.querySelector(".titlebar .here"); if (here) { here.querySelector(".nm").textContent = P.name; here.querySelector(".path").textContent = P.path.split("::").slice(0, -1).join(" › "); const k = here.querySelector(".k"); if (k) k.outerHTML = kmark(P.kind); }
    $("gaddr").textContent = `nudox://${P.path.replace(/::/g, "/")}`;
  }
  function gettingHTML() {
    let h = P.harvest.getting.replace(/^<section class="csec gget"><h2>Getting one<\/h2>/, "").replace(/<\/section>$/, "");
    // a route through a method behind a feature you don't turn on is not a route for you
    const t = document.createElement("div"); t.innerHTML = h;
    const gated = new Map(); for (const b of P.src.inherent || []) for (const m of b.members) { const g = m.gate || b.gate; if (g && g.need && !g.open) gated.set(m.n, g); }
    for (const a of t.querySelectorAll(".route a.gtl")) { const g = gated.get(a.textContent); if (g) { const r = a.closest(".route"); r.classList.add("shut"); r.insertAdjacentHTML("beforeend", ` <span class="q-gate shut" data-card="gate:${esc(g.need[0].f)}">${ICON.key}${esc(g.need[0].f)} — not in your build</span>`); } }
    // the foot count is now the "comes from" row below
    for (const f of t.querySelectorAll(".rfoot, .gfoot")) f.remove();
    return t.innerHTML;
  }
  function contractFoot(h) {
    const c = P.src.contract; if (!c) return h;
    const foot = `<div class="afoot">${c.provided.length ? "" : "nothing is given for free: it has no provided methods · "}${c.dyn.ok ? "can be used as <span class=\"mono\">dyn</span>" : `not usable as <span class="mono">dyn ${esc(P.name)}</span>, because ${esc(c.dyn.why)}`}</div>`;
    return h.replace(/<div class="afoot">[\s\S]*?<\/div>/, "").replace(/<\/section>$/, foot + "</section>");
  }
  function contractHTML() {
    const c = P.src.contract;
    const req = c.required.map((m) => `<div class="ar"><span class="vn">${esc(m.n)}</span><span class="q-vt"><span class="gsl">${esc(m.sig.replace(/^fn \w+/, "").replace(/(?<!:):(?!:)/g, ": "))}</span></span><span class="say">${inlineCode(m.summary)}</span></div>`).join("");
    return `<section class="anat contract"><div class="ah">you write</div><div class="arows req">${req}</div>${c.provided.length ? `<div class="ah">you get</div>` : `<div class="ah">you get nothing else for free: it has no provided methods</div>`}`
      + `<div class="afoot">${c.dyn.ok ? "can be used as <span class=\"mono\">dyn</span>" : `not usable as <span class="mono">dyn ${esc(P.name)}</span>: ${esc(c.dyn.why)}`}</div></section>`;
  }
  function doingItHTML() {
    const c = P.src.contract; const derive = (D.features.on.serde || []).includes("derive");
    const rows = [
      `<div class="q-fr"><span class="q-fk">derive it</span><span class="q-fb"><span class="q-fm">#[derive(Serialize)]</span><span class="q-fx">${derive ? "serde’s <span class=\"mono\">derive</span> feature is on: you turn it on" : "needs serde’s derive feature"} · ${(() => { const d = P.relations.find((r) => r.verb === "done by"); return `${d.derived} of its ${d.n} types in this world do it this way`; })()}</span></span></div>`,
      `<div class="q-fr"><span class="q-fk">or write</span><span class="q-fb"><span class="q-fm">fn serialize&lt;S: Serializer&gt;(&amp;self, serializer: S) -&gt; Result&lt;S::Ok, S::Error&gt;</span><span class="q-fx">the one method it asks for</span></span></div>`,
      ...c.notes.slice(1).map((n) => `<div class="q-fr"><span class="q-fk">not yours?</span><span class="q-fb"><span class="q-fw">${inlineCode(n.replace(/^for types from other crates /, "for a type from another crate, ").replace(/check/, "check"))}</span></span></div>`),
    ];
    const ex = c.required[0] && c.required[0].docs.examples[0];
    return `<div class="q-fail" style="--fw:6.5em">${rows.join("")}</div>${ex ? `<div style="height:10px"></div><div class="q-ex"><div class="q-cap2">what <b>#[derive(Serialize)]</b> writes for you</div>${exampleHTML(ex.code, "serialize")}</div>` : ""}`;
  }
  function shelfHTML() {
    const sh = P.shelf; const pk = P.pkg;
    const rows = sh.items.map((x) => `<div class="crow${x.id === P.id ? " cur" : ""}" data-i="${x.id}" style="padding-left:28px">${kmark(x.k)}<span class="n">${esc(x.n)}</span></div>`).join("");
    return `<div class="cup">‹ dependencies</div><div class="cbook">${gemSVG("package", 28)}<div class="col" style="gap:1px;min-width:0"><span class="bn">${esc(pk)}</span><span class="bv">${esc(P.version)}</span></div></div>`
      + `<div class="cfilter"><span>Filter</span></div><div class="crows"><div class="crow open">${kmark("module")}<span class="n">${esc(sh.module)}</span></div>${rows}</div>`
      + `<div class="spine2">${sh.items.slice(0, 14).map((x) => `<span class="${x.id === P.id ? "on" : ""}">${kmark(x.k)}</span>`).join("")}</div>`;
  }

  // ------------------------------------------------------------------ cards
  const card = () => $("p2card");
  function cardFor(key, el) {
    const [kind, ...rest] = key.split(":"); const arg = rest.join(":");
    const s = P.src;
    if (kind === "ne") { // the joint: where exactly this relation happens
      const [verb, k] = [rest.slice(0, -1).join(":"), +rest[rest.length - 1]];
      const r = P.relations.find((x) => x.verb === verb); const j = r.ids[k]; const pk = D.peeks[j] || {}; const jt = r.joints[j];
      const hit = jt && jt.hit ? esc(jt.text).replace(new RegExp(`\\b(${jt.hit})\\b`), "<u>$1</u>") : esc(jt ? jt.text : pk.s);
      const ln = jt && jt.line ? `<span class="q-ln">${jt.line}</span>` : "";
      const why = { "taken by": `takes ${P.name} as a parameter`, "held by": `holds a ${P.name}`, "called on by": `calls ${P.name}’s ${jt && jt.hit || "methods"}`, "done by": /derive/.test(jt && jt.text || "") ? `derives ${P.name}` : `writes ${P.name} by hand`, "comes from": `returns a ${P.name}`, "asked for by": `accepts any ${P.name}`, "called by": `calls ${P.name}`, calls: `${P.name} calls it`, "used by": `mentions ${P.name}` }[verb];
      return `<div class="q-h">${kmark(pk.k)}<span class="nm${pk.y ? " y" : ""}">${esc(pk.n)}</span></div><div class="q-wh">${esc(pk.q)} · ${esc(pk.f)}</div><pre class="q-jl">${ln}${hit}</pre>${pk.d ? `<div class="q-sy">${inlineCode(pk.d)}</div>` : ""}<div class="q-fc"><span>${esc(why)}</span>${pk.y ? `<b class="y">yours</b>` : `<span>${esc(pk.pk)}</span>`}</div>`;
    }
    if (kind === "vb") { const v = VERBS[arg] || { def: "", lang: {} }; const r = P.relations.find((x) => x.verb === arg);
      return `<div class="q-vt">${esc(arg)}</div><div class="q-sy">${esc(v.def)}.</div><div class="q-lang">${Object.entries(v.lang).map(([l, t]) => `<b>${l}</b><span class="${l === "Rust" ? "q-me" : ""}">${esc(t)}</span>`).join("")}</div>${r ? `<div class="q-fc"><span><b>${r.n}</b> here</span>${r.yours ? `<span><b class="y">${r.yours}</b> in yours</span>` : ""}<span>across ${r.bands.reduce((a, b) => a + b.pkgs.length, 0)} packages</span></div>` : ""}`; }
    if (kind === "door") { const rs = P.relations; return `<div class="q-vt">the fan, in the graph</div><div class="q-sy">${esc(P.name)} with everything around it gathered beside it: what it comes from on the left, where it goes on the right.</div><div class="q-fc">${rs.map((r) => `<span>${esc(r.verb)} <b>${r.n}</b></span>`).join("")}</div>`; }
    if (kind === "bd") { const b = s.decl.bounds[+arg]; return `<div class="q-cp">${esc(b.exact)}</div><div class="q-sy">${esc(b.words.replace(/^T /, "T is any type that "))}.</div>`; }
    if (kind === "gate") { const g = (s.gates || []).find((x) => x.feature === arg) || { feature: arg, open: false }; const unindexed = Q.get("gatestate") === "unindexed";
      return `<div class="q-h">${ICON.key}<span class="nm">${esc(arg)}</span></div><div class="q-wh">a feature of ${esc(g.crate || P.pkg)}</div>`
        + (unindexed ? `<div class="q-sy">Behind feature <code>${esc(arg)}</code>, not indexed in this build.</div><div class="q-fc"><span>This build read ${esc(P.pkg)} with the features your project turns on; what ${esc(arg)} adds was never seen, so it is not listed here, not missing.</span></div>`
          : g.open ? `<div class="q-sy">On in your build${g.why ? ` — ${g.why === "you" ? "you turn it on" : `through ${esc(g.why)}`}` : ""}.</div>` : `<div class="q-sy">Off in your build: ${(() => { const w = (g.where || []).filter((x) => !/^impl\b/.test(x)); return w.length ? `${esc(list(w.slice(0, 3)))}${w.length > 3 ? ` and ${w.length - 3} more` : ""} ${w.length === 1 ? "isn’t" : "aren’t"} there for you.` : "what it adds isn’t there for you."; })()}</div>`)
        + (g.how && !g.open ? `<div class="q-cp">${esc(g.how)}</div><div class="q-fc"><span>press to copy the line for your Cargo.toml</span></div>` : ""); }
    if (kind === "cap") {
      if (arg === "auto") { const a = s.autos; return `<div class="q-vt">from its parts</div><div class="q-sy">The compiler gives these because of what it is made of: ${esc(a.why)}.</div><div class="q-lang">${Object.entries(a.t).map(([k, v]) => `<b>${k}</b><span class="${v === "no" ? "q-no" : ""}">${v === "yes" ? esc(a.words[k]) : v === "no" ? "no" : esc(v)}</span>`).join("")}</div>`; }
      if (arg === "usual") { const u = s.blankets.filter((b) => !/through/.test(b.why)); const a = s.autos; const cm = P._common || [];
        const by = (h) => cm.filter((c) => c.how === h).map((c) => c.word);
        const parts = [by("derived").length ? `derived: ${list(by("derived"))}` : "", by("written").length ? `written: ${list(by("written"))}` : "", by("through").length ? `${by("through").length} through another trait` : "", a ? `${Object.values(a.t).filter((v) => v !== "no").length} from its parts` : "", `${u.length} every type gets`].filter(Boolean);
        return `<div class="q-vt">the usual ${u.length + cm.length + (a ? Object.values(a.t).filter((v) => v !== "no").length : 0)}</div><div class="q-sy">What most types like it can do, so it waits here.</div><div class="q-fc">${parts.map((x) => `<span>${esc(x)}</span>`).join("")}</div><div class="q-fc"><span>press to list them</span></div>`; }
      if (arg === "shut") { const sh = s.written.filter((w) => w.gate && w.gate.need && !w.gate.open); return `<div class="q-vt">behind features you don’t turn on</div><div class="q-lang">${sh.map((w) => `<b>${esc(w.label || w.name)}</b><span>${esc(w.gate.need.map((x) => x.f).join(", "))}</span>`).join("")}</div>`; }
      const ws = s.written.filter((w) => (CAPW[w.name] || w.label || w.name) === arg); const d = (s.decl.derives || []).find((x) => (CAPW[x] || x) === arg); const b = s.blankets.find((x) => (CAPW[x.t] || x.t) === arg);
      if (ws.length) { const w = ws[0]; return `<div class="q-h"><span class="nm">${esc(w.label || w.name)}</span></div><div class="q-wh">written · ${esc((w.file || "").split("/").pop())}:${w.line}${ws.length > 1 ? ` · ${ws.length} impls` : ""}</div><div class="q-cp">${esc(ws.slice(0, 4).map((x) => x.head).join("\n"))}${ws.length > 4 ? `\n… and ${ws.length - 4} more` : ""}</div>${w.cond.length ? `<div class="q-sy">when ${esc(w.cond.map((c) => c.words).join(" and "))}</div>` : ""}${w.assoc.length ? `<div class="q-fc">${w.assoc.map((a) => `<span>${esc(a)}</span>`).join("")}</div>` : ""}${w.gate && w.gate.need ? `<div class="q-fc"><span>${ICON.key} ${esc(w.gate.need[0].f)} — ${w.gate.open ? "on in your build" : "off in your build"}</span></div>` : ""}${w.deprecated ? `<div class="q-fc"><span>the trait is deprecated${w.deprecated.note ? ": " + esc(w.deprecated.note) : ""}</span></div>` : ""}`; }
      if (d) return `<div class="q-h"><span class="nm">${esc(d)}</span></div><div class="q-wh">derived</div><div class="q-cp">#[derive(${esc(s.decl.derives.join(", "))})]</div>`;
      if (b) return `<div class="q-h"><span class="nm">${esc(b.t)}</span></div><div class="q-wh">through another trait</div><div class="q-sy">${esc(b.why)}.</div>`;
    }
    if (kind === "since" || key === "since") { const si = P.since; if (!si) return `<div class="q-vt">since unknown</div><div class="q-sy">No release history read for ${esc(P.pkg)} yet.</div><div class="q-fc"><span>you build ${esc(P.version)}</span></div>`; return `<div class="q-vt">since ${si.oldestIsFirst ? "≤ " : ""}${esc(si.first)}</div><div class="q-sy">In ${esc(P.pkg)} since ${esc(si.first)}${si.oldestIsFirst ? " or earlier: that is the oldest release read" : ""}.</div><div class="q-fc"><span>${si.read.length} of ${si.versions} releases read</span><span>you pin <b class="y">${esc(si.pinned)}</b></span>${si.changed.length ? `<span>changed in ${esc(si.changed.map((c) => c.v.replace(/\+.*/, "")).join(", "))}</span>` : ""}</div>`; }
    if (kind === "path") return `<div class="q-cp">use ${esc(P.aliases[0] || P.path)};</div><div class="q-fc"><span>declared at ${esc(P.path)}</span><span>press to copy</span></div>`;
    if (kind === "row") { const m = (s.inherent || []).flatMap((b) => b.members).find((x) => x.n === arg); if (!m) return ""; return `<div class="q-h">${kmark("method")}<span class="nm">${esc(m.n)}</span></div><div class="q-cp">${esc(m.sig)}</div>${m.summary ? `<div class="q-sy">${inlineCode(m.summary)}</div>` : ""}<div class="q-fc">${m.since && m.since.first ? `<span>since ${m.since.oldest ? "≤ " : ""}${esc(m.since.first)}</span>` : ""}${m.panics ? `<span>can panic</span>` : ""}${m.gate && m.gate.need ? `<span>${esc(m.gate.need[0].f)} ${m.gate.open ? "on" : "off"}</span>` : ""}</div>`; }
    if (kind === "kind") { const k = s.kinds.kinds.find((x) => x.n === arg); return `<div class="q-h"><span class="nm">Category::${esc(k.n)}</span></div><div class="q-sy">${inlineCode(k.doc)}</div>${k.more ? `<div class="q-fc"><span>${inlineCode(k.more)}</span></div>` : ""}<div class="q-lang">${k.causes.map((c) => `<b>${esc(c.n)}</b><span>${esc(c.msg)}</span>`).join("")}</div>`; }
    if (kind === "i") { const pk = D.peeks[arg]; if (!pk) return ""; return `<div class="q-h">${kmark(pk.k)}<span class="nm${pk.y ? " y" : ""}">${esc(pk.n)}</span></div><div class="q-wh">${esc(pk.q)}</div>${pk.s ? `<pre class="q-jl">${esc(pk.s)}</pre>` : ""}${pk.d ? `<div class="q-sy">${inlineCode(pk.d)}</div>` : ""}`; }
    return "";
  }
  function showCard(key, el) {
    const html = cardFor(key, el); const c = card(); if (!html) { c.classList.remove("on"); return; }
    c.innerHTML = html; c.classList.add("on");
    const host = c.parentElement.getBoundingClientRect(); const r = el.getBoundingClientRect();
    let x = r.left - host.left, y = r.bottom - host.top + 8;
    if (x + c.offsetWidth > host.width - 12) x = host.width - c.offsetWidth - 12;
    if (y + c.offsetHeight > host.height - 8 && r.top - host.top - c.offsetHeight - 8 > 0) y = r.top - host.top - c.offsetHeight - 8;
    c.style.transform = `translate(${Math.round(Math.max(8, x))}px,${Math.round(y)}px)`;
    el.classList.add("hov"); const row = el.closest("[data-card-row]"); if (row) row.classList.add("hov");
  }

  // ------------------------------------------------------------------ flip a section to its exact source
  const srcCache = new Map();
  async function flip(id, instant) {
    const host = document.querySelector(`[data-flip="${id}"].q-turn`) || document.getElementById(id); if (!host) return;
    const face = host.querySelector(".q-face");
    if (host.dataset.flipped) { host.querySelector(".q-back").remove(); face.style.display = ""; delete host.dataset.flipped; return; }
    const s = P.src; let file = s.decl.file, a = s.decl.start, b = s.decl.end, on = [s.decl.line, s.decl.end];
    const blocks = { "q-do": (s.inherent || [])[0], "q-get": (s.inherent || [])[0] };
    if (id === "q-do" && blocks["q-do"]) ({ file, line: a, end: b } = blocks["q-do"]), on = [a, b];
    if (id === "q-fail") { const m = (s.inherent || []).flatMap((x) => x.members).find((x) => x.panics || x.safety); const w = (s.written || []).find((x) => x.panics); if (m) { file = m.file; a = m.line; b = m.end; on = [a, b]; } else if (w) { file = w.file; a = w.line; b = w.end; on = [a, b]; } }
    if (id === "q-who" || id === "q-changed") { a = s.decl.start; b = s.decl.end; }
    const url = `../graph/registry/${file.replace(/^[^/]+\//, P.src.decl.file.split("/")[0] + "/")}`;
    let text = srcCache.get(url); if (text === undefined) { try { const r = await fetch(url); text = r.ok ? await r.text() : null; } catch { text = null; } srcCache.set(url, text); }
    const L = (text || "").split("\n"); const from = Math.max(1, a - 2), to = Math.min(L.length, Math.max(b, a + 6) + 1);
    const hl = (x) => /^\s*\/\/\//.test(x) ? `<span class="q-dc">${esc(x)}</span>` : hlRust(x).replace(/class="q-co"/g, 'class="q-dc"');
    const back = document.createElement("div"); back.className = "q-back";
    back.innerHTML = `<div class="q-src2cap"><span>${esc(file)}</span><span>${from}–${to}</span><span class="q-flip on" data-flip="${id}">${ICON.src}turn back</span></div><div class="q-src2">${L.slice(from - 1, to).map((x, k) => `<div class="q-sl${from + k >= on[0] && from + k <= on[1] ? " on" : ""}"><span class="q-ln">${from + k}</span><span class="q-tx">${hl(x) || " "}</span></div>`).join("")}</div>`;
    const turn = () => { face.style.display = "none"; host.appendChild(back); host.dataset.flipped = "1"; };
    if (instant || matchMedia("(prefers-reduced-motion: reduce)").matches) { turn(); return; }
    face.animate([{ transform: "rotateX(0)" }, { transform: "rotateX(-90deg)" }], { duration: 120, easing: "ease-in" }).onfinish = () => { turn(); back.animate([{ transform: "rotateX(90deg)", transformOrigin: "50% 0" }, { transform: "rotateX(0)" }], { duration: 200, easing: "cubic-bezier(.2,.9,.25,1.18)" }); };
  }

  function fitLine(box, total, moreEl, moreText) {
    const names = [...box.querySelectorAll(".q-ne")].filter((n) => n.style.display !== "none"); if (!names.length) return;
    const top0 = names[0].getBoundingClientRect().top; let shown = names.length;
    if (Q.get("debug")) box.dataset.tops = names.map((n) => Math.round(n.getBoundingClientRect().top - top0) + "/" + Math.round(n.getBoundingClientRect().width)).join(",") + "|" + Math.round(box.getBoundingClientRect().width);
    const low = (el) => el && el.getBoundingClientRect().top > top0 + 8;
    const over = () => names.slice(0, shown).some(low) || low(moreEl());
    if (!over()) { const m = moreEl(false); if (m && total <= shown) m.remove(); else if (m && !m.textContent) m.textContent = moreText(total - shown); return; }
    while (shown > 1 && over()) { shown--; names[shown].style.display = "none"; const m = moreEl(true); m.textContent = moreText(total - shown); }
  }
  function fitRows() {
    for (const ns of document.querySelectorAll(".q-rl:not(.open) > .q-ns")) {
      const row = ns.closest(".q-rl"); const r = P.relations.find((x) => x.verb === row.dataset.verb); if (!r) continue;
      fitLine(ns, r.n, (make) => { let m = ns.querySelector(".more"); if (!m && make) { m = document.createElement("span"); m.className = "more"; m.dataset.unfold = r.verb; ns.appendChild(m); } return m; }, (k) => `and ${k} more`);
    }
    for (const pn of document.querySelectorAll(".q-unf .q-pn")) fitLine(pn, +pn.dataset.n, () => pn.querySelector(".q-plus"), (k) => (k > 0 ? `+ ${k}` : ""));
  }
  // ------------------------------------------------------------------ the door: the rows fly out and become the prism
  function fly(t0) {
    // The rows become the prism: each visible name leaves its row on an arc and takes its place in the fan
    // (what it comes from on the left, where it goes on the right), the door's gem grows into the focus
    // gem, strands draw from it, the page recedes; then the graph opens on that prism.
    const root = document.querySelector(".nx"); const rd = $("preader").getBoundingClientRect();
    const vis = (el) => el.offsetParent && el.style.display !== "none" && el.getBoundingClientRect().bottom > rd.top && el.getBoundingClientRect().top < rd.bottom;
    const per = new Map(); const names = [...document.querySelectorAll(".q-rl .q-ne")].filter((el) => el.style.display !== "none" && el.offsetParent).filter((el) => { const v = el.dataset.rel; const k = per.get(v) || 0; per.set(v, k + 1); return k < 4; }).slice(0, 22);
    const gm = document.querySelector("#q-who .q-door .gm"); if (!gm) return;
    const g0 = gm.getBoundingClientRect(); const cx = rd.left + rd.width * 0.5, cy = rd.top + rd.height * 0.5;
    const byVerb = new Map(); for (const el of names) { const v = el.dataset.rel; (byVerb.get(v) || byVerb.set(v, []).get(v)).push(el); }
    const side = (v) => (VERBS[v] && VERBS[v].dir === "in" ? -1 : 1);
    const place = new Map(); const heads = [];
    for (const sd of [-1, 1]) {
      const groups = [...byVerb].filter(([v]) => side(v) === sd); const rows = groups.reduce((a, [, l]) => a + l.length + 1, 0);
      let y = cy - (rows * 22 + (groups.length - 1) * 10) / 2;
      for (const [v, l] of groups) { heads.push({ v, sd, y }); y += 22; for (const el of l) { place.set(el, { sd, y }); y += 22; } y += 10; }
    }
    const ov = document.createElementNS("http://www.w3.org/2000/svg", "svg"); ov.setAttribute("class", "q-flysvg"); root.appendChild(ov);
    const gem = document.createElement("div"); gem.className = "q-flygem"; root.appendChild(gem);
    const fl = names.map((el) => { const r = el.getBoundingClientRect(); const f = document.createElement("div"); f.className = "q-flyer" + (el.classList.contains("y") ? " y" : ""); f.textContent = el.textContent; root.appendChild(f); el.style.visibility = "hidden"; const p = place.get(el); const w = f.offsetWidth; return { f, x0: r.left, y0: r.top, w, ...p }; });
    const hd = heads.map((h) => { const d = document.createElement("div"); d.className = "q-flyhead"; d.textContent = h.v; root.appendChild(d); return { d, ...h }; });
    const paths = fl.map(() => { const p = document.createElementNS("http://www.w3.org/2000/svg", "path"); ov.appendChild(p); return p; });
    const ease = (t) => 1 - Math.pow(1 - Math.min(1, Math.max(0, t)), 3);
    const GX = 150;
    const frame = (t) => {
      const e = ease(t);
      const gx = g0.left + g0.width / 2 + (cx - g0.left - g0.width / 2) * e, gy = g0.top + g0.height / 2 + (cy - g0.top - g0.height / 2) * e;
      gem.style.transform = `translate(${gx - 7}px,${gy - 7}px) rotate(45deg) scale(${1 + e * 0.9})`;
      fl.forEach((o, k) => {
        const ek = ease(t * 1.25 - (k % 8) * 0.035);
        const tx = o.sd < 0 ? cx - GX - o.w : cx + GX, ty = o.y - 9;
        const mx = (o.x0 + tx) / 2, my = Math.min(o.y0, ty) - 60;
        const x = (1 - ek) * (1 - ek) * o.x0 + 2 * (1 - ek) * ek * mx + ek * ek * tx, y = (1 - ek) * (1 - ek) * o.y0 + 2 * (1 - ek) * ek * my + ek * ek * ty;
        o.f.style.transform = `translate(${x}px,${y}px)`;
        const ax = o.sd < 0 ? cx - GX + 6 : cx + GX - 6; const len = Math.max(0, (ek - 0.35) / 0.65);
        paths[k].setAttribute("d", `M${cx + o.sd * 14} ${cy} C${cx + o.sd * 70} ${cy} ${ax - o.sd * 70} ${o.y} ${ax} ${o.y}`);
        paths[k].style.strokeDasharray = "400"; paths[k].style.strokeDashoffset = String(400 * (1 - len));
        paths[k].setAttribute("class", o.f.classList.contains("y") ? "y" : "");
      });
      hd.forEach((h) => { h.d.style.opacity = String(Math.max(0, (e - 0.5) * 2)); h.d.style.transform = `translate(${h.sd < 0 ? cx - GX - 200 : cx + GX}px,${h.y - 9}px)`; h.d.style.width = "200px"; h.d.style.textAlign = h.sd < 0 ? "right" : "left"; });
      document.querySelector("#preader .p2").style.opacity = String(1 - 0.82 * e);
    };
    if (t0 !== undefined) { frame(t0); return; }
    const st = performance.now(); const step = (now) => { const t = (now - st) / 480; frame(t); if (t < 1) requestAnimationFrame(step); else location.href = `../Graph.html?focus=${encodeURIComponent(P.path)}`; }; requestAnimationFrame(step);
  }

  // ------------------------------------------------------------------ wiring
  function bind() {
    let t = 0;
    document.addEventListener("mouseover", (e) => {
      const el = e.target.closest("[data-card], a.gtl[data-i]"); clearTimeout(t);
      document.querySelectorAll(".hov").forEach((x) => x.classList.remove("hov"));
      if (!el) { t = setTimeout(() => card().classList.remove("on"), 120); return; }
      const row = e.target.closest("[data-card-row]"); if (row) row.classList.add("hov");
      t = setTimeout(() => showCard(el.dataset.card || "i:" + el.dataset.i, el), 220);
    });
    document.addEventListener("click", (e) => {
      const u = e.target.closest("[data-unfold]"); if (u) { const q = new URLSearchParams(location.search); const o = new Set((q.get("open") || "").split(",").filter(Boolean)); o.add(u.dataset.unfold); q.set("open", [...o].join(",")); history.replaceState(null, "", "?" + q); Q.set("open", q.get("open")); render(); return; }
      const f = e.target.closest("[data-fold]"); if (f) { const o = (Q.get("open") || "").split(",").filter((x) => x.split(":")[0] !== f.dataset.fold); Q.set("open", o.join(",")); render(); return; }
      const fl = e.target.closest("[data-flip].q-flip"); if (fl) { flip(fl.dataset.flip); return; }
      if (e.target.closest("[data-door]")) { fly(); return; }
      const q = e.target.closest("[data-q]"); if (q) { const s = document.getElementById(q.dataset.q); if (s) s.scrollIntoView({ behavior: "smooth", block: "start" }); return; }
      const a = e.target.closest("a.gtl[data-i], .crow[data-i]"); if (a) { const id = +a.dataset.i; const slug = D.items.find((s) => D.pages[s].id === id); if (slug) { Q.set("page", slug); history.replaceState(null, "", "?" + Q); P = D.pages[slug]; render(); $("preader").scrollTop = 0; } }
    });
    window.addEventListener("keydown", (e) => { if (e.altKey) document.body.classList.add("xray"); if (e.metaKey) document.body.classList.add("cmd"); if ((e.key === "g" || e.key === "G") && !e.metaKey) fly(); });
    window.addEventListener("keyup", (e) => { if (!e.altKey) document.body.classList.remove("xray"); if (!e.metaKey) document.body.classList.remove("cmd"); });
  }
  async function boot() {
    D = await (await fetch("page2.json", { cache: "reload" })).json();
    const want = Q.get("page") || "value"; P = D.pages[want] || Object.values(D.pages).find((p) => p.name === want) || D.pages.value;
    if (Q.get("x")) document.body.classList.add("xray");
    if (Q.get("cmd")) document.body.classList.add("cmd");
    const win = $("win");
    if (Q.get("t") === "2") { document.documentElement.style.zoom = "2"; win.style.width = Math.floor(innerWidth / 2) + "px"; if (!Q.get("tall")) win.style.height = Math.floor(innerHeight / 2) + "px"; }
    if (Q.get("tall")) win.classList.add("tall");
    render(); bind();
    if (Q.get("flip")) for (const f of Q.get("flip").split(",")) await flip(f, true);
    if (Q.get("scroll")) { const el = document.getElementById(Q.get("scroll")); if (el) { const rd = $("preader"); rd.scrollTop += (el.getBoundingClientRect().top - rd.getBoundingClientRect().top) / (Q.get("t") === "2" ? 2 : 1) - (+Q.get("pad") || 28); } }
    const hv = Q.get("hover"); if (hv) { const el = document.querySelector(`[data-card="${CSS.escape(hv)}"]`) || document.querySelector(`[data-card-row="${CSS.escape(hv)}"]`); if (el) { el.scrollIntoView({ block: "center" }); if (el.dataset.card) showCard(el.dataset.card, el); else el.classList.add("hov"); } else if (hv === "door") { const d = document.querySelector(".q-door"); if (d) { if (!Q.get("scroll")) d.scrollIntoView({ block: "center" }); d.classList.add("hov"); } } }
    if (Q.get("fly")) fly(+Q.get("fly"));
    document.body.dataset.h = String(Math.ceil($("win").offsetHeight * (Q.get("t") === "2" ? 2 : 1)));
  }
  boot();
})();
