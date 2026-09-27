// rustsrc.mjs — a small, honest Rust item reader for page2's data script.
//
// graph/extract.mjs keeps names and edges but strips what a docs page needs: bounds,
// where-clauses, full doc comments, cfg predicates, deprecation notes, impl headers,
// associated items. This reader keeps those, verbatim, with line spans. It is a token
// parser, not a compiler: macro invocations are expanded only when a macro_rules! arm
// matches without repetition; everything else is reported as "made by a macro".
import fs from "node:fs";
import path from "node:path";

// ---------------------------------------------------------------- lexer (keeps strings, offsets, lines)
const OPS = ["..=", "...", "::", "->", "=>", "..", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "%=", "^=", "|=", "&="];
export function lex(src) {
  const T = []; let i = 0, line = 1; const n = src.length;
  const push = (t, v, s, e, l) => T.push({ t, v, s, e, line: l });
  while (i < n) {
    const c = src[i];
    if (c === "\n") { line++; i++; continue; }
    if (c === " " || c === "\t" || c === "\r") { i++; continue; }
    if (c === "/" && src[i + 1] === "/") {
      let j = src.indexOf("\n", i); if (j < 0) j = n;
      const body = src.slice(i, j);
      if (body.startsWith("///") && !body.startsWith("////")) push("doc", body.slice(3).replace(/^ /, ""), i, j, line);
      else if (body.startsWith("//!")) push("idoc", body.slice(3).replace(/^ /, ""), i, j, line);
      i = j; continue;
    }
    if (c === "/" && src[i + 1] === "*") {
      const l0 = line; let d = 1, j = i + 2;
      while (j < n && d) { if (src[j] === "\n") line++; if (src[j] === "/" && src[j + 1] === "*") { d++; j += 2; } else if (src[j] === "*" && src[j + 1] === "/") { d--; j += 2; } else j++; }
      const body = src.slice(i, j);
      if (body.startsWith("/**") && !body.startsWith("/***") && body !== "/**/") push("doc", body.slice(3, -2).split("\n").map((s) => s.replace(/^\s*\* ?/, "")).join("\n").trim(), i, j, l0);
      i = j; continue;
    }
    // raw strings r"..", r#".."#, br".."
    const rm = /^(br|r)(#*)"/.exec(src.slice(i, i + 8));
    if (rm) {
      const close = '"' + rm[2]; const st = i + rm[0].length; const e = src.indexOf(close, st);
      const l0 = line; for (let k = i; k < e; k++) if (src[k] === "\n") line++;
      push("str", src.slice(st, e), i, e + close.length, l0); i = e + close.length; continue;
    }
    if (c === '"' || (c === "b" && src[i + 1] === '"') || (c === "c" && src[i + 1] === '"')) {
      const l0 = line; let j = c === '"' ? i + 1 : i + 2; let v = "";
      while (j < n && src[j] !== '"') { if (src[j] === "\\") { v += src[j] + src[j + 1]; j += 2; continue; } if (src[j] === "\n") line++; v += src[j]; j++; }
      push("str", v, i, j + 1, l0); i = j + 1; continue;
    }
    if (c === "'" || (c === "b" && src[i + 1] === "'")) {
      const s = c === "b" ? i + 1 : i;
      if (src[s + 1] === "\\") { let j = s + 2; while (src[j] !== "'") j++; push("chr", src.slice(s, j + 1), i, j + 1, line); i = j + 1; continue; }
      const cp = src.codePointAt(s + 1); const w = cp > 0xffff ? 2 : 1;
      if (src[s + 1 + w] === "'") { push("chr", src.slice(s, s + 2 + w), i, s + 2 + w, line); i = s + 2 + w; continue; }
      let j = s + 1; while (j < n && /[A-Za-z0-9_]/.test(src[j])) j++;
      push("lt", src.slice(s, j), i, j, line); i = j; continue;
    }
    if (/[A-Za-z_]/.test(c)) {
      let j = i + 1; while (j < n && /[A-Za-z0-9_]/.test(src[j])) j++;
      let v = src.slice(i, j);
      if (v === "r" && src[j] === "#" && /[A-Za-z_]/.test(src[j + 1] || "")) { let k = j + 1; while (/[A-Za-z0-9_]/.test(src[k])) k++; v = src.slice(j + 1, k); j = k; }
      push("id", v, i, j, line); i = j; continue;
    }
    if (/[0-9]/.test(c)) { let j = i + 1; while (j < n && (/[A-Za-z0-9_]/.test(src[j]) || (src[j] === "." && /[0-9]/.test(src[j + 1] || "")))) j++; push("num", src.slice(i, j), i, j, line); i = j; continue; }
    const op = OPS.find((o) => src.startsWith(o, i));
    if (op) { push("p", op, i, i + op.length, line); i += op.length; continue; }
    push("p", c, i, i + 1, line); i++;
  }
  return T;
}

// ---------------------------------------------------------------- helpers over tokens
const OPEN = { "(": ")", "[": "]", "{": "}" };
function group(T, i) { // T[i] is ( [ {; returns index after the match
  const o = T[i].v, c = OPEN[o]; let d = 0;
  for (let k = i; k < T.length; k++) { if (T[k].t !== "p") continue; if (T[k].v === o) d++; else if (T[k].v === c) { d--; if (!d) return k + 1; } }
  return T.length;
}
function angles(T, i) { // T[i] is '<'; returns index after the matching '>'
  let d = 0;
  for (let k = i; k < T.length; k++) {
    const x = T[k]; if (x.t !== "p") continue;
    if (x.v === "<") d++; else if (x.v === ">") { d--; if (!d) return k + 1; }
    else if (x.v === "(" || x.v === "[") { k = group(T, k) - 1; }
    else if (x.v === "{" || x.v === ";") return k;
  }
  return T.length;
}
// scan forward at depth 0 (angles, parens, brackets) until a stop token
function until(T, i, stops) {
  let a = 0;
  for (let k = i; k < T.length; k++) {
    const x = T[k];
    if (x.t === "p") {
      if (a === 0 && stops.includes(x.v)) return k;
      if (x.v === "(" || x.v === "[") { k = group(T, k) - 1; continue; }
      if (x.v === "{") { if (stops.includes("{")) return k; k = group(T, k) - 1; continue; }
      if (x.v === "<") a++; else if (x.v === ">") a = Math.max(0, a - 1);
    } else if (x.t === "id" && a === 0 && stops.includes(x.v)) return k;
  }
  return T.length;
}
export const norm = (s) => s.replace(/\s+/g, " ").replace(/\s*,\s*([)\]>}])/g, "$1").replace(/\(\s+/g, "(").replace(/\s+\)/g, ")").replace(/<\s+/g, "<").replace(/\s+>/g, ">").trim();
function text(src, T, a, b) { if (b <= a) return ""; return norm(src.slice(T[a].s, T[b - 1].e)); }
export function splitTop(s) { const out = []; let d = 0, st = 0; for (let k = 0; k < s.length; k++) { const c = s[k]; if (c === "<" || c === "(" || c === "[" || c === "{") d++; else if ((c === ">" && s[k - 1] !== "-" && s[k - 1] !== "=") || c === ")" || c === "]" || c === "}") d--; else if (c === "," && d === 0) { out.push(s.slice(st, k).trim()); st = k + 1; } } out.push(s.slice(st).trim()); return out.filter(Boolean); }

// attributes: #[...] → { name, text } (text = the inside, normalized)
function attrOf(src, T, i) { // T[i] is '#', T[i+1] is '['
  const e = group(T, i + 1); const inner = text(src, T, i + 2, e - 1);
  return { a: { name: (T[i + 2] || {}).v, text: inner, line: T[i].line }, next: e };
}
export function cfgOf(attrs) { // the verbatim predicates of #[cfg(...)] and doc(cfg(...))
  const out = [];
  for (const a of attrs) {
    if (a.name === "cfg") out.push(a.text.replace(/^cfg\s*\(/, "").replace(/\)$/, ""));
    const m = /doc\s*\(\s*cfg\s*\((.*)\)\s*\)\s*\)?$/.exec(a.text); if (a.name === "cfg_attr" && m) out.push("doc:" + m[1]);
  }
  return out;
}
export function deprecationOf(attrs) {
  const a = attrs.find((x) => x.name === "deprecated"); if (!a) return null;
  const since = /since\s*=\s*"([^"]*)"/.exec(a.text), note = /note\s*=\s*"([^"]*)"/.exec(a.text);
  const bare = /^deprecated\s*=\s*"([^"]*)"/.exec(a.text);
  return { since: since ? since[1] : null, note: note ? note[1] : bare ? bare[1] : null };
}
export function stabilityOf(attrs) { // std's #[stable(feature = "...", since = "1.0.0")]
  const a = attrs.find((x) => x.name === "stable" || x.name === "rustc_const_stable"); if (!a) return null;
  const since = /since\s*=\s*"([^"]*)"/.exec(a.text); return since ? since[1] : null;
}

// ---------------------------------------------------------------- the item parser
export function parseFile(src, file, modPath, macros = new Map(), depth = 0) {
  const T = lex(src); const items = []; const invocations = [];
  parseItems(src, T, 0, T.length, modPath, items, invocations, macros, file, null);
  // expand invocations of macro_rules! whose arms match without repetition
  if (depth < 2) for (const inv of invocations) {
    const m = macros.get(inv.name); if (!m) { inv.expanded = false; continue; }
    const out = expand(m, inv.args);
    if (out == null) { inv.expanded = false; continue; }
    const sub = parseFile(out, file, modPath, macros, depth + 1);
    for (const it of sub.items) { it.line = inv.line; it.end = inv.end; it.start = inv.line; it.viaMacro = inv.name; it.attrs = [...inv.attrs, ...(it.attrs || [])]; items.push(it); }
    inv.expanded = true;
  }
  return { items, invocations };
}
function parseItems(src, T, i, end, modPath, items, invocations, macros, file, owner) {
  while (i < end) {
    const docs = []; const attrs = []; const start = T[i].line;
    for (;;) {
      if (i >= end) return;
      const x = T[i];
      if (x.t === "doc") { docs.push(x.v); i++; continue; }
      if (x.t === "idoc") { i++; continue; }
      if (x.t === "p" && x.v === "#" && T[i + 1] && T[i + 1].v === "[") { const { a, next } = attrOf(src, T, i); attrs.push(a); i = next; continue; }
      if (x.t === "p" && x.v === "#" && T[i + 1] && T[i + 1].v === "!" ) { i = group(T, i + 2); continue; }
      break;
    }
    for (const a of attrs) if (a.name === "doc") { const m = /^doc\s*=\s*"(.*)"$/.exec(a.text); if (m) docs.push(m[1].replace(/\\"/g, '"').replace(/^ /, "")); }
    if (i >= end) return;
    let vis = "";
    if (T[i].t === "id" && T[i].v === "pub") { vis = "pub"; i++; if (T[i] && T[i].v === "(") { const e = group(T, i); vis = "pub" + text(src, T, i, e).replace(/\s/g, ""); i = e; } }
    const quals = [];
    while (T[i] && T[i].t === "id" && ["unsafe", "async", "const", "extern", "default", "auto"].includes(T[i].v) && T[i + 1]) {
      if (T[i].v === "const" && !(T[i + 1].t === "id" && ["fn", "unsafe", "async", "extern"].includes(T[i + 1].v))) break;
      if (T[i].v === "default" && !(T[i + 1].t === "id" && ["fn", "unsafe", "async", "const", "impl", "type"].includes(T[i + 1].v))) break;
      quals.push(T[i].v); i++; if (quals[quals.length - 1] === "extern" && T[i].t === "str") i++;
    }
    const kw = T[i]; if (!kw) return;
    const base = { docs: docs.join("\n"), attrs, vis, quals, file, mod: modPath, start, line: kw.line };
    if (kw.t === "id" && kw.v === "fn") { const r = parseFn(src, T, i, base); items.push(r.item); i = r.next; continue; }
    if (kw.t === "id" && ["struct", "enum", "union"].includes(kw.v) && T[i + 1] && T[i + 1].t === "id") { const r = parseAdt(src, T, i, base); items.push(r.item); i = r.next; continue; }
    if (kw.t === "id" && kw.v === "trait") { const r = parseTrait(src, T, i, base); items.push(r.item); i = r.next; continue; }
    if (kw.t === "id" && kw.v === "impl") { const r = parseImpl(src, T, i, base); items.push(r.item); i = r.next; continue; }
    if (kw.t === "id" && (kw.v === "type" || kw.v === "const" || kw.v === "static")) {
      const e = until(T, i, [";"]); const s = text(src, T, i, e);
      items.push({ ...base, k: kw.v === "type" ? "type" : "const", n: (T[i + 1] && T[i + 1].v === "mut" ? T[i + 2] : T[i + 1]).v, sig: s, end: T[Math.min(e, T.length - 1)].line });
      i = e + 1; continue;
    }
    if (kw.t === "id" && kw.v === "mod" && T[i + 1]) {
      const name = T[i + 1].v; i += 2;
      if (T[i] && T[i].v === "{") { const e = group(T, i); parseItems(src, T, i + 1, e - 1, modPath ? modPath + "::" + name : name, items, invocations, macros, file, owner); i = e; }
      else { items.push({ ...base, k: "mod", n: name, end: kw.line }); i++; }
      continue;
    }
    if (kw.t === "id" && (kw.v === "use" || (kw.v === "extern" && T[i + 1] && T[i + 1].v === "crate"))) { const e = until(T, i, [";"]); items.push({ ...base, k: "use", sig: text(src, T, i, e), end: T[Math.min(e, T.length - 1)].line }); i = e + 1; continue; }
    if (kw.t === "id" && kw.v === "macro_rules" && T[i + 1] && T[i + 1].v === "!") {
      const name = T[i + 2].v; const g = i + 3; const e = group(T, g);
      macros.set(name, arms(src, T, g + 1, e - 1));
      items.push({ ...base, k: "macro", n: name, end: T[e - 1].line }); i = e; if (T[i] && T[i].v === ";") i++; continue;
    }
    if (kw.t === "id" && T[i + 1] && T[i + 1].v === "!" && T[i + 2] && OPEN[T[i + 2].v]) {
      const e = group(T, i + 2); invocations.push({ name: kw.v, args: src.slice(T[i + 2].e, T[e - 1].s), attrs, docs: base.docs, line: kw.line, end: T[e - 1].line, file, mod: modPath });
      i = e; if (T[i] && T[i].v === ";") i++; continue;
    }
    i++;
  }
}
function parseFn(src, T, i, base) {
  const name = T[i + 1].v; let k = i + 2; let gen = "";
  if (T[k] && T[k].v === "<") { const e = angles(T, k); gen = text(src, T, k + 1, e - 1); k = e; }
  const pe = group(T, k); const params = splitTop(text(src, T, k + 1, pe - 1)); k = pe;
  let ret = ""; if (T[k] && T[k].v === "->") { const e = until(T, k + 1, ["where", "{", ";"]); ret = text(src, T, k + 1, e); k = e; }
  let wh = ""; if (T[k] && T[k].v === "where") { const e = until(T, k + 1, ["{", ";"]); wh = text(src, T, k + 1, e); k = e; }
  const headEnd = k; let body = null, next;
  if (T[k] && T[k].v === "{") { next = group(T, k); body = [T[k].line, T[next - 1].line]; } else next = k + 1;
  const sig = norm(src.slice(T[i].s, T[headEnd - 1].e));
  return { item: { ...base, k: "fn", n: name, gen, params, ret, wh, sig, required: !body, end: T[Math.max(0, next - 1)].line, headEnd: T[headEnd - 1].line }, next };
}
function parseAdt(src, T, i, base) {
  const kind = T[i].v; const name = T[i + 1].v; let k = i + 2; let gen = "", wh = "";
  if (T[k] && T[k].v === "<") { const e = angles(T, k); gen = text(src, T, k + 1, e - 1); k = e; }
  if (T[k] && T[k].v === "where") { const e = until(T, k + 1, ["{", ";", "("]); wh = text(src, T, k + 1, e); k = e; }
  const head = norm(src.slice(T[i].s, T[k - 1].e));
  const parts = []; let shape = "unit"; let next = k + 1;
  if (T[k] && (T[k].v === "{" || T[k].v === "(")) {
    shape = T[k].v === "{" ? "record" : "tuple"; const e = group(T, k);
    if (kind === "enum") parseVariants(src, T, k + 1, e - 1, parts); else parseFields(src, T, k + 1, e - 1, parts, shape);
    next = e;
    if (T[next] && T[next].v === "where") { const e2 = until(T, next + 1, [";"]); wh = text(src, T, next + 1, e2); next = e2; }
    if (T[next] && T[next].v === ";") next++;
  }
  return { item: { ...base, k: kind, n: name, gen, wh, head, shape, parts, end: T[Math.max(0, next - 1)].line }, next };
}
function leading(src, T, i, end) { // docs + attrs + vis before a field/variant
  const docs = [], attrs = []; let vis = "";
  for (;;) {
    if (i >= end) break; const x = T[i];
    if (x.t === "doc") { docs.push(x.v); i++; continue; }
    if (x.t === "p" && x.v === "#" && T[i + 1] && T[i + 1].v === "[") { const { a, next } = attrOf(src, T, i); attrs.push(a); i = next; continue; }
    break;
  }
  if (T[i] && T[i].t === "id" && T[i].v === "pub") { vis = "pub"; i++; if (T[i] && T[i].v === "(") { const e = group(T, i); vis = "pub" + text(src, T, i, e).replace(/\s/g, ""); i = e; } }
  return { docs: docs.join("\n"), attrs, vis, i };
}
function parseFields(src, T, i, end, out, shape) {
  let n = 0;
  while (i < end) {
    const L = leading(src, T, i, end); i = L.i; if (i >= end) break;
    const line = T[i].line; let name = String(n++);
    if (shape === "record") { name = T[i].v; i += 2; }
    const e = until(T, i, [","]); const ty = text(src, T, i, Math.min(e, end));
    out.push({ n: name, ty, docs: L.docs, attrs: L.attrs, vis: L.vis, line });
    i = Math.min(e, end) + 1;
  }
}
function parseVariants(src, T, i, end, out) {
  while (i < end) {
    const L = leading(src, T, i, end); i = L.i; if (i >= end) break;
    const name = T[i].v, line = T[i].line; i++;
    let shape = "unit", ty = "", fields = [];
    if (T[i] && (T[i].v === "(" || T[i].v === "{")) { shape = T[i].v === "(" ? "tuple" : "record"; const e = group(T, i); parseFields(src, T, i + 1, e - 1, fields, shape); ty = text(src, T, i + 1, e - 1); i = e; }
    if (T[i] && T[i].v === "=") { const e = until(T, i, [","]); i = e; }
    out.push({ n: name, shape, ty, fields, docs: L.docs, attrs: L.attrs, line });
    if (T[i] && T[i].v === ",") i++;
  }
}
function parseTrait(src, T, i, base) {
  const name = T[i + 1].v; let k = i + 2; let gen = "", sup = "", wh = "";
  if (T[k] && T[k].v === "<") { const e = angles(T, k); gen = text(src, T, k + 1, e - 1); k = e; }
  if (T[k] && T[k].v === ":") { const e = until(T, k + 1, ["where", "{"]); sup = text(src, T, k + 1, e); k = e; }
  if (T[k] && T[k].v === "where") { const e = until(T, k + 1, ["{"]); wh = text(src, T, k + 1, e); k = e; }
  const head = norm(src.slice(T[i].s, T[k - 1].e));
  const e = group(T, k); const members = []; const inv = [];
  parseItems(src, T, k + 1, e - 1, base.mod, members, inv, new Map(), base.file, name);
  return { item: { ...base, k: "trait", n: name, gen, sup, wh, head, members, end: T[e - 1].line }, next: e };
}
function parseImpl(src, T, i, base) {
  let k = i + 1; let gen = "";
  if (T[k] && T[k].v === "<") { const e = angles(T, k); gen = text(src, T, k + 1, e - 1); k = e; }
  const headStop = until(T, k, ["where", "{", ";"]);
  // the first top-level `for` not followed by `<` splits trait / self type
  let forAt = -1; { let a = 0; for (let q = k; q < headStop; q++) { const x = T[q]; if (x.t === "p") { if (x.v === "<") a++; else if (x.v === ">") a--; else if (x.v === "(" || x.v === "[") { q = group(T, q) - 1; } } else if (x.t === "id" && x.v === "for" && a === 0 && !(T[q + 1] && T[q + 1].v === "<")) { forAt = q; break; } } }
  let neg = false, trait = "", self;
  if (forAt >= 0) { let a = k; if (T[a].v === "!") { neg = true; a++; } trait = text(src, T, a, forAt); self = text(src, T, forAt + 1, headStop); }
  else self = text(src, T, k, headStop);
  let wh = ""; k = headStop;
  if (T[k] && T[k].v === "where") { const e = until(T, k + 1, ["{", ";"]); wh = text(src, T, k + 1, e); k = e; }
  const head = norm(src.slice(T[i].s, T[k - 1].e));
  const members = []; let next = k + 1;
  if (T[k] && T[k].v === "{") { const e = group(T, k); parseItems(src, T, k + 1, e - 1, base.mod, members, [], new Map(), base.file, self); next = e; }
  return { item: { ...base, k: "impl", gen, trait, neg, self, wh, head, members, end: T[Math.max(0, next - 1)].line }, next };
}

// ---------------------------------------------------------------- macro_rules!, the no-repetition subset
function arms(src, T, i, end) {
  const out = [];
  while (i < end) {
    if (!OPEN[T[i].v]) { i++; continue; }
    const pe = group(T, i); const pat = T.slice(i + 1, pe - 1);
    let k = pe; if (T[k] && T[k].v === "=>") k++;
    if (!T[k] || !OPEN[T[k].v]) break;
    const be = group(T, k); out.push({ pat, body: src.slice(T[k].e, T[be - 1].s) });
    i = be; if (T[i] && T[i].v === ";") i++;
  }
  return out;
}
function expand(arms_, args) {
  const A = lex(args);
  for (const arm of arms_) {
    if (arm.pat.some((x) => x.v === "$" && (arm.pat[arm.pat.indexOf(x) + 1] || {}).v === "(")) continue; // repetition: not supported
    const caps = new Map(); let a = 0, ok = true;
    for (let p = 0; p < arm.pat.length && ok; p++) {
      const x = arm.pat[p];
      if (x.v === "$" && arm.pat[p + 1] && arm.pat[p + 2] && arm.pat[p + 2].v === ":") {
        const name = arm.pat[p + 1].v, frag = arm.pat[p + 3].v; p += 3;
        const sep = arm.pat[p + 1]; // the literal after the capture, if any
        let e = a;
        if (frag === "ident" || frag === "lifetime" || frag === "literal" || frag === "tt") e = a + 1;
        else { e = sep ? until(A, a, [sep.v]) : A.length; }
        if (e <= a || e > A.length) { ok = false; break; }
        caps.set(name, args.slice(A[a].s, A[e - 1].e)); a = e;
      } else { if (!A[a] || A[a].v !== x.v) ok = false; else a++; }
    }
    if (!ok || a !== A.length) continue;
    return arm.body.replace(/\$([A-Za-z_]\w*)/g, (m, nm) => (caps.has(nm) ? caps.get(nm) : m));
  }
  return null;
}

// ---------------------------------------------------------------- a crate: every file, with module paths
export function readCrate(dir) {
  const files = [];
  const walk = (d) => { for (const f of fs.readdirSync(d, { withFileTypes: true })) { const p = path.join(d, f.name); if (f.isDirectory()) walk(p); else if (f.name.endsWith(".rs")) files.push(p); } };
  walk(path.join(dir, "src"));
  const macros = new Map(); const out = { items: [], invocations: [] };
  // two passes so macros defined in any file (macros.rs, lib.rs) expand everywhere
  for (const f of files) parseFile(fs.readFileSync(f, "utf8"), "", "", macros);
  for (const f of files) {
    const rel = path.relative(path.join(dir, "src"), f).replace(/\\/g, "/");
    const mod = rel.replace(/(^|\/)(lib|mod|main)\.rs$/, "").replace(/\.rs$/, "").replace(/\//g, "::");
    const r = parseFile(fs.readFileSync(f, "utf8"), path.basename(dir) + "/src/" + rel, mod, macros);
    out.items.push(...r.items); out.invocations.push(...r.invocations);
  }
  return out;
}
