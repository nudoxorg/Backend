// facts.mjs — what a docs page needs from the source itself (rustsrc.mjs), per item:
// the declaration with bounds, docs split into their sections, methods grouped by the
// condition of their impl block, trait impls with their arrival (written / derived /
// through another trait / from its parts), gates and whether your project opens them,
// deprecation, "acts like" (Deref), required vs provided, dyn-compatibility, examples.
import fs from "node:fs";
import path from "node:path";
import { readCrate, lex, splitTop, cfgOf, deprecationOf, stabilityOf, norm } from "./rustsrc.mjs";

const RUST_SRC = "/nix/store/j2il416s2xyl5swimf1w3sr9s9qi4mzv-rust-src-stable-2026-07-16/lib/rustlib/src/rust/library";

// capabilities in plain words (the same table as app.js CAPW / semantics::caps)
export const CAPW = { Copy: "copies freely", Clone: "clones", Debug: "debug-prints", Display: "prints", PartialEq: "compares", Eq: "compares", PartialOrd: "orders", Ord: "sorts", Hash: "hashes", Default: "has a default", Serialize: "serializes", Deserialize: "deserializes", Error: "is an error", Iterator: "iterates", From: "converts", ToString: "to text", FromStr: "parses", Send: "crosses threads", Sync: "shares across threads", Deref: "reads through", DerefMut: "writes through", Drop: "cleans up", AsRef: "borrows as", AsMut: "borrows as", Borrow: "borrows as", BorrowMut: "borrows as", Index: "indexes", IndexMut: "indexes", Extend: "extends", FromIterator: "collects", IntoIterator: "iterates", Write: "writes bytes", IntoDeserializer: "IntoDeserializer", Deserializer: "Deserializer" };
const last = (p) => p.replace(/<.*$/s, "").split("::").pop().trim();

// ---------------------------------------------------------------- docs: sections, examples, links
export function splitDocs(md) {
  // rustdoc: first paragraph = summary; `# Heading` sections; fenced code; link reference defs
  const lines = (md || "").split("\n");
  const defs = {}; const body = [];
  let fence = false;
  for (const l of lines) {
    if (/^\s*(```|~~~)/.test(l)) fence = !fence;
    const d = !fence && /^\s*\[([^\]]+)\]:\s*(\S+)/.exec(l);
    if (d) { defs[d[1]] = d[2]; continue; }
    body.push(l);
  }
  // summary = first paragraph
  let k = 0; while (k < body.length && !body[k].trim()) k++;
  const sumLines = []; while (k < body.length && body[k].trim()) sumLines.push(body[k++]);
  const sections = []; let cur = { kind: "body", title: "", lines: [] }; fence = false;
  for (; k < body.length; k++) {
    const l = body[k];
    if (/^\s*(```|~~~)/.test(l)) fence = !fence;
    const h = !fence && /^(#{1,3})\s+(.*)$/.exec(l);
    if (h) {
      sections.push(cur);
      const t = h[2].trim(); const lk = t.toLowerCase();
      const kind = /^errors?$/.test(lk) ? "errors" : /^panics?$/.test(lk) ? "panics" : /^safety$/.test(lk) ? "safety" : /^examples?$/.test(lk) ? "examples" : "other";
      cur = { kind, title: t, lines: [] }; continue;
    }
    cur.lines.push(l);
  }
  sections.push(cur);
  const out = sections.map((s) => ({ kind: s.kind, title: s.title, md: s.lines.join("\n").trim() })).filter((s) => s.md);
  return { summary: sumLines.join(" ").trim(), sections: out, defs };
}
export function codeBlocks(md) {
  const out = []; const re = /^\s*```([^\n]*)\n([\s\S]*?)^\s*```/gm; let m;
  while ((m = re.exec(md || ""))) { const lang = m[1].trim(); if (/ignore|text|toml|json|sh|console|compile_fail/.test(lang) && !/rust/.test(lang)) continue; out.push({ lang, code: m[2].replace(/\n$/, "") }); }
  return out;
}

// ---------------------------------------------------------------- bounds in words
const PLAIN = { str: "text", String: "text", usize: "a size", u8: "a byte", bool: "yes or no" };
const CAPP = { Copy: "copy freely", Clone: "clone", Debug: "debug-print", Display: "print", PartialEq: "compare", Eq: "compare", PartialOrd: "order", Ord: "sort", Hash: "hash", Serialize: "serialize", Deserialize: "deserialize", Send: "cross threads", Sync: "share across threads", Default: "have a default" };
function boundWords(b, plural) {
  const t = b.trim(); const l = last(t);
  if (plural && CAPP[l] && !/</.test(t)) return CAPP[l];
  if (plural && /^Deserialize<'/.test(t)) return "deserialize";
  if (/^'/.test(t)) return `outlives ${t}`;
  if (t === "?Sized") return "may be unsized";
  const m = /^(?:de::|ser::|serde::(?:de::|ser::)?)?(Deserialize)<'(\w+)>$/.exec(t); if (m) return `can be built by a deserializer, borrowing for '${m[2]}`;
  const it = /^Array<Item\s*=\s*(\w+)>$/.exec(t); if (it) return `is an array of ${it[1] === "u8" ? "bytes" : PLAIN[it[1]] ? PLAIN[it[1]] + "s" : it[1]}`;
  const fn = /^(?:Fn|FnMut|FnOnce)\((.*)\)\s*(?:->\s*(.*))?$/.exec(t); if (fn) return `is a function of ${fn[1] || "nothing"}${fn[2] ? " → " + fn[2] : ""}`;
  const into = /^Into<(.+)>$/.exec(t); if (into) return `becomes ${PLAIN[into[1]] || into[1]}`;
  const peq = /^PartialEq<(.+)>$/.exec(t); if (peq) return `${plural ? "compare" : "compares"} with ${peq[1].replace(/^B::Item$/, "B’s items")}`;
  if (CAPW[l] && !/^[A-Z]/.test(CAPW[l])) return CAPW[l];
  return `is any ${t}`;
}
export function subjectWords(x, ctx) {
  if (/^<A as Array>::Item$|^A::Item$/.test(x)) return "its items";
  if (x === "A" && ctx.array) return "its buffer A";
  if (/^B::Item$/.test(x)) return "B’s items";
  return x;
}
export const colonAt = (p) => { for (let k = 0; k < p.length; k++) if (p[k] === ":" && p[k + 1] !== ":" && p[k - 1] !== ":") return k; return -1; };
export function whereWords(gen, wh, ctx = {}) { // → [{subject, words, exact}]
  const out = [];
  const push = (subj, bs) => { const list = splitPlus(bs).filter((b) => b && b !== "?Sized" || false); if (!list.length) return; out.push({ subject: subj, exact: `${subj}: ${list.join(" + ")}`, words: `${subjectWords(subj, ctx)} ${list.map((b) => boundWords(b, / items$/.test(subjectWords(subj, ctx)))).join(" and ")}` }); };
  for (const p of splitTop(gen || "")) { const q = p.replace(/^#\[[^\]]*\]\s*/, "").trim(); if (/^'/.test(q)) continue; const c = colonAt(q); const cst = /^const\s+(\w+)\s*:\s*(\w+)/.exec(q); if (cst) { out.push({ subject: cst[1], exact: q, words: `${cst[1]} is a fixed ${cst[2]}` }); continue; } if (c > 0) push(q.slice(0, c).trim(), q.slice(c + 1)); }
  for (const p of splitTop((wh || "").replace(/,\s*$/, ""))) { const c = colonAt(p); if (c > 0) push(p.slice(0, c).trim(), p.slice(c + 1)); }
  return out;
}
export function splitPlus(s) { const out = []; let d = 0, st = 0; for (let k = 0; k < s.length; k++) { const c = s[k]; if (c === "<" || c === "(") d++; else if ((c === ">" && s[k - 1] !== "-") || c === ")") d--; else if (c === "+" && d === 0) { out.push(s.slice(st, k).trim()); st = k + 1; } } out.push(s.slice(st).trim()); return out.filter(Boolean); }

// ---------------------------------------------------------------- gates: cfg predicates → features, open for you or not
export function gateOf(preds, crateName, feats) {
  const all = preds.filter((p) => !p.startsWith("doc:"));
  if (!all.length) return null;
  const pred = all.join(" and ");
  const need = [...pred.matchAll(/(not\()?\s*feature\s*=\s*"([^"]+)"/g)].map((m) => ({ f: m[2], not: !!m[1] }));
  if (!need.length) return /\btest\b/.test(pred) ? { pred, testOnly: true } : { pred, other: true };
  const on = new Set((feats.on[crateName] || []));
  const open = need.every((x) => (x.not ? !on.has(x.f) : on.has(x.f)));
  const why = need.filter((x) => !x.not && on.has(x.f)).map((x) => (feats.why[crateName] || {})[x.f]).filter(Boolean);
  return { pred, need, open, why, crate: crateName };
}

// ---------------------------------------------------------------- slice methods, for "acts like a slice"
let sliceCache = null;
export function sliceMethods() {
  if (sliceCache) return sliceCache;
  const out = [];
  for (const [f, crate] of [["core/src/slice/mod.rs", "core"], ["alloc/src/slice.rs", "alloc"], ["core/src/slice/ascii.rs", "core"]]) {
    let src; try { src = fs.readFileSync(path.join(RUST_SRC, f), "utf8"); } catch { continue; }
    const { items } = parseRustFile(src, crate + "/" + f);
    for (const im of items) {
      if (im.k !== "impl" || im.trait || !/^\[.*\]$/.test(im.self)) continue;
      const cond = im.self === "[T]" ? (im.gen && im.gen !== "T" ? im.gen : "") : im.self;
      for (const m of im.members) {
        if (m.k !== "fn" || m.vis !== "pub") continue;
        const st = stabilityOf(m.attrs); const unst = m.attrs.some((a) => a.name === "unstable");
        if (unst && !st) continue;
        if (m.attrs.some((a) => a.name === "doc" && /hidden/.test(a.text))) continue;
        out.push({ n: m.n, sig: m.sig, since: st, on: im.self, cond, unsafe: m.quals.includes("unsafe"), summary: (m.docs || "").split("\n\n")[0].replace(/\n/g, " ").trim(), deprecated: deprecationOf(m.attrs) });
      }
    }
  }
  sliceCache = out; return out;
}
import { parseFile as parseRustFile } from "./rustsrc.mjs";
