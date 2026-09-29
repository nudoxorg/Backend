// Licences as a heads-up, the way GitHub's licence card reads: what it permits, what it asks of you, what it
// won't promise, and one verdict for YOUR project (backend is MIT OR Apache-2.0). Expressions are parsed
// (OR = you choose, AND = all apply, WITH = an exception, "/" = the old spelling of OR). Terms are in our
// own words, after choosealicense.com's categories.

export const YOURS = "MIT OR Apache-2.0";

// p: permissions, c: conditions, l: limitations. kind: how it binds you.
const T = {
  MIT: { name: "MIT", kind: "permissive", p: ["commercial", "modify", "distribute", "private"], c: ["notice"], l: ["liability", "warranty"] },
  "MIT-0": { name: "MIT No Attribution", kind: "public", p: ["commercial", "modify", "distribute", "private"], c: [], l: ["liability", "warranty"] },
  "Apache-2.0": { name: "Apache 2.0", kind: "permissive", p: ["commercial", "modify", "distribute", "private", "patent"], c: ["notice", "changes"], l: ["liability", "warranty", "trademark"] },
  "BSD-2-Clause": { name: "BSD 2-Clause", kind: "permissive", p: ["commercial", "modify", "distribute", "private"], c: ["notice"], l: ["liability", "warranty"] },
  "BSD-3-Clause": { name: "BSD 3-Clause", kind: "permissive", p: ["commercial", "modify", "distribute", "private"], c: ["notice"], l: ["liability", "warranty", "endorse"] },
  ISC: { name: "ISC", kind: "permissive", p: ["commercial", "modify", "distribute", "private"], c: ["notice"], l: ["liability", "warranty"] },
  Zlib: { name: "zlib", kind: "permissive", p: ["commercial", "modify", "distribute", "private"], c: ["notice-src", "changes"], l: ["liability", "warranty"] },
  "BSL-1.0": { name: "Boost 1.0", kind: "permissive", p: ["commercial", "modify", "distribute", "private"], c: ["notice-src"], l: ["liability", "warranty"] },
  "Unicode-3.0": { name: "Unicode 3.0", kind: "permissive", p: ["commercial", "modify", "distribute", "private"], c: ["notice"], l: ["liability", "warranty"] },
  "Unicode-DFS-2016": { name: "Unicode DFS 2016", kind: "permissive", p: ["commercial", "modify", "distribute", "private"], c: ["notice"], l: ["liability", "warranty"] },
  "0BSD": { name: "Zero-clause BSD", kind: "public", p: ["commercial", "modify", "distribute", "private"], c: [], l: ["liability", "warranty"] },
  Unlicense: { name: "The Unlicense", kind: "public", p: ["commercial", "modify", "distribute", "private"], c: [], l: ["liability", "warranty"] },
  "CC0-1.0": { name: "CC0 1.0", kind: "public", p: ["commercial", "modify", "distribute", "private"], c: [], l: ["liability", "warranty", "trademark", "patent-none"] },
  "MPL-2.0": { name: "Mozilla 2.0", kind: "weak", p: ["commercial", "modify", "distribute", "private", "patent"], c: ["notice", "disclose-file", "same-file"], l: ["liability", "warranty", "trademark"] },
  "LGPL-2.1-or-later": { name: "LGPL 2.1+", kind: "weak", p: ["commercial", "modify", "distribute", "private"], c: ["notice", "disclose", "same-lib", "changes"], l: ["liability", "warranty"] },
  "LGPL-3.0": { name: "LGPL 3.0", kind: "weak", p: ["commercial", "modify", "distribute", "private", "patent"], c: ["notice", "disclose", "same-lib", "changes"], l: ["liability", "warranty"] },
  "GPL-2.0": { name: "GPL 2.0", kind: "strong", p: ["commercial", "modify", "distribute", "private"], c: ["notice", "disclose", "same", "changes"], l: ["liability", "warranty"] },
  "GPL-3.0": { name: "GPL 3.0", kind: "strong", p: ["commercial", "modify", "distribute", "private", "patent"], c: ["notice", "disclose", "same", "changes"], l: ["liability", "warranty"] },
  "AGPL-3.0": { name: "AGPL 3.0", kind: "strong", p: ["commercial", "modify", "distribute", "private", "patent"], c: ["notice", "disclose", "same", "changes", "network"], l: ["liability", "warranty"] },
};
const WORDS = {
  commercial: "Commercial use", modify: "Modification", distribute: "Distribution", private: "Private use", patent: "Patent use",
  notice: "Keep the licence and copyright notice", "notice-src": "Keep the notice in source copies", changes: "Say what you changed",
  disclose: "Share the source when you distribute", "disclose-file": "Share changed files' source", same: "Your program takes the same licence",
  "same-file": "Changed files keep this licence", "same-lib": "Changes to the library keep this licence", network: "Network use counts as distribution",
  liability: "Liability", warranty: "Warranty", trademark: "Trademark use", endorse: "Using their name to endorse", "patent-none": "Patent rights",
};
const EXCEPTIONS = { "LLVM-exception": "LLVM exception: you needn't keep the notice in binaries built from it" };

// Parse "A OR B", "(A OR B) AND C", "A WITH X", "A/B" into { op, terms: [{ id, with, known }] , groups }.
export function parse(expr) {
  if (!expr) return { none: true, groups: [] };
  const norm = expr.replace(/\s*\/\s*/g, " OR ").replace(/[()]/g, " ").trim();
  // AND binds tighter than OR in SPDX; the expressions here are shallow, so split AND then OR.
  const ands = norm.split(/\s+AND\s+/);
  const groups = ands.map((part) => part.split(/\s+OR\s+/).map((t) => {
    const [id, ex] = t.trim().split(/\s+WITH\s+/);
    const key = id.replace(/\+$/, "-or-later").replace(/^GPL-2\.0-(only|or-later)$/, "GPL-2.0").replace(/^GPL-3\.0-(only|or-later)$/, "GPL-3.0").replace(/^LGPL-3\.0-(only|or-later)$/, "LGPL-3.0");
    return { id, key, with: ex || null, t: T[key] || null };
  }));
  return { none: false, groups };
}

const RANK = { public: 0, permissive: 1, weak: 2, strong: 3 };
// The verdict for your project: pick, from each OR group, the option that asks least of you.
export function verdict(expr) {
  const L = parse(expr);
  if (L.none) return { tone: "coral", word: "No licence", line: "It declares none. By default that reserves every right: you have no permission to copy or ship it.", pick: [] };
  const pick = L.groups.map((g) => g.slice().sort((a, b) => (a.t ? RANK[a.t.kind] : 9) - (b.t ? RANK[b.t.kind] : 9))[0]);
  const worst = Math.max(...pick.map((t) => (t.t ? RANK[t.t.kind] : 9)));
  const unknown = pick.some((t) => !t.t);
  const choice = L.groups.some((g) => g.length > 1);
  const names = pick.map((t) => t.t ? t.t.name : t.id).join(" and ");
  if (unknown) return { tone: "amber", word: "Unrecognised", line: `We don't have terms for ${pick.filter((t) => !t.t).map((t) => t.id).join(", ")}. Read it before you ship.`, pick };
  if (worst === 3) return { tone: "coral", word: "Copyleft", line: `Shipping your program with it puts your whole program under ${names}. Your project is ${YOURS}.`, pick };
  if (worst === 2) return { tone: "amber", word: "Weak copyleft", line: `Fine to use as is. If you change its files, you must share those changes under ${names}.`, pick };
  const also = L.groups.length > 1 ? " Both parts apply." : choice ? ` You may take it under ${names}, the lighter option for you.` : "";
  return { tone: "mint", word: worst === 0 ? "Public domain" : "Permissive", line: `Compatible with your ${YOURS}. ${worst === 0 ? "Nothing is asked of you." : "Keep its notice when you ship."}${also}`, pick };
}

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
// The chip that sits in a facts row: the expression, tinted by the verdict.
export function chip(expr) {
  const v = verdict(expr);
  return `<span class="lic-chip ${v.tone}" data-lic="${esc(expr || "")}"><i></i>${esc(expr || "no licence")}</span>`;
}
// The hover card.
export function card(expr) {
  const L = parse(expr), v = verdict(expr);
  const col = (title, cls, list) => `<div class="lc-col"><div class="lc-h">${title}</div>${list.map((k) => `<div class="lc-r ${cls}"><i></i>${esc(WORDS[k] || k)}</div>`).join("") || `<div class="lc-r none">nothing</div>`}</div>`;
  const union = (key) => [...new Set(v.pick.flatMap((t) => (t.t ? t.t[key] : [])))];
  const opts = L.none ? "" : L.groups.map((g) => g.map((t) => `<span class="lc-o ${v.pick.includes(t) ? "on" : ""}">${esc(t.id)}${t.with ? ` <em>with ${esc(t.with)}</em>` : ""}</span>`).join(`<span class="lc-op">or</span>`)).join(`<span class="lc-op and">and</span>`);
  const ex = v.pick.filter((t) => t.with && EXCEPTIONS[t.with]).map((t) => `<div class="lc-ex">${esc(EXCEPTIONS[t.with])}</div>`).join("");
  return `<div class="lc"><div class="lc-top"><span class="lc-v ${v.tone}">${v.word}</span>${opts ? `<span class="lc-opts">${opts}</span>` : ""}</div>
    <div class="lc-line">${esc(v.line)}</div>${ex}
    ${L.none ? "" : `<div class="lc-cols">${col("Permits", "ok", union("p"))}${col("Asks of you", "ask", union("c"))}${col("Won't promise", "no", union("l"))}</div>`}
    ${L.groups.some((g) => g.length > 1) ? `<div class="lc-foot">Terms shown for the lighter option, underlined above.</div>` : ""}</div>`;
}
