// Badges, not code. A symbol's first line is read into small tags you can take in at a glance — what
// kind of thing it is, whether it can fail, whether it borrows, what it's generic over, whether it's
// async or unsafe — and a package's heads-up is read into icons. Nothing here prints `pub unsafe fn`.
// Every badge carries its own words for a hover that opens inside the badge itself.

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

// 12 px glyphs, drawn on a 12-unit grid, stroke = currentColor.
export const ICON = {
  unsafe: `<path d="M6 1.5 11 10.5H1Z" fill="none"/><path d="M6 5v2.6M6 9.1v.2"/>`,
  async: `<path d="M1.5 4.5h6l-2-2M10.5 7.5h-6l2 2" fill="none"/>`,
  fail: `<circle cx="6" cy="6" r="4.3" fill="none"/><path d="M6 3.6v3M6 8.3v.2"/>`,
  maybe: `<circle cx="6" cy="6" r="4.3" fill="none"/><path d="M6 1.7a4.3 4.3 0 0 1 0 8.6Z" class="solid"/>`,
  borrow: `<path d="M4.5 7.5 7.5 4.5M3.6 5.4 2.4 6.6a2 2 0 0 0 3 3l1.2-1.2M8.4 6.6l1.2-1.2a2 2 0 0 0-3-3L5.4 3.6" fill="none"/>`,
  mutates: `<path d="M2 10 2.5 7.8 8.2 2.1l1.7 1.7-5.7 5.7Z" fill="none"/>`,
  generic: `<path d="M4.5 2 1.8 6l2.7 4M7.5 2l2.7 4-2.7 4" fill="none"/>`,
  konst: `<rect x="2.5" y="5.2" width="7" height="5" fill="none"/><path d="M4 5.2V3.8a2 2 0 0 1 4 0v1.4" fill="none"/>`,
  macro: `<path d="M6 1.8v5.4M6 9.4v.8"/>`,
  iter: `<path d="M2 3.5h6.5M2 6h6.5M2 8.5h6.5M9.5 7l1.5 1.5-1.5 1.5" fill="none"/>`,
  error: `<rect x="2" y="2" width="8" height="8" fill="none"/><path d="M4.2 4.2l3.6 3.6M7.8 4.2 4.2 7.8"/>`,
  build: `<path d="M2 10l4.2-4.2M5.3 2.4l4.3 4.3-1.4 1.4-4.3-4.3Z" fill="none"/>`,
  macroPkg: `<path d="M6 1.2 7.1 4.9 10.8 6 7.1 7.1 6 10.8 4.9 7.1 1.2 6 4.9 4.9Z" fill="none"/>`,
  shield: `<path d="M6 1.3 10 2.8v3.1c0 2.4-1.7 4-4 4.8-2.3-.8-4-2.4-4-4.8V2.8Z" fill="none"/>`,
  net: `<circle cx="6" cy="6" r="4.4" fill="none"/><path d="M1.6 6h8.8M6 1.6c1.6 1.4 1.6 7.4 0 8.8M6 1.6c-1.6 1.4-1.6 7.4 0 8.8" fill="none"/>`,
  files: `<path d="M1.8 3h3.1l1 1.2h4.3v5.3H1.8Z" fill="none"/>`,
  process: `<rect x="1.5" y="2.2" width="9" height="7.6" fill="none"/><path d="M3.3 4.6 4.9 6 3.3 7.4M6 7.5h2.4" fill="none"/>`,
  env: `<circle cx="4.2" cy="6" r="2.2" fill="none"/><path d="M6.4 6h4.1M8.8 6v1.8" fill="none"/>`,
  ffi: `<path d="M4 1.5v3M8 1.5v3M2.8 4.5h6.4v2a3.2 3.2 0 0 1-6.4 0Z M6 9.7v1.3" fill="none"/>`,
  you: `<circle cx="6" cy="4.2" r="2" fill="none"/><path d="M2.4 10.3c.5-2 1.9-3 3.6-3s3.1 1 3.6 3" fill="none"/>`,
  undoc: `<path d="M3 1.8h4.2l2 2v6.4H3Z M5 6h3M5 8h2" fill="none"/>`,
  build2: `<path d="M1.5 9.5h9M3 9.5V5l3-2.5L9 5v4.5" fill="none"/>`,
  owner: `<path d="M2.5 9.5 6 2.5l3.5 7" fill="none"/>`,
  ctor: `<path d="M6 2v8M2 6h8" fill="none"/>`,
  marker: `<path d="M3 10.5V1.8M3 2.2h6l-1.4 2 1.4 2H3" fill="none"/>`,
};
export const icon = (k, cls = "") => `<svg class="ic ${cls}" viewBox="0 0 12 12" width="12" height="12">${ICON[k] || ""}</svg>`;

// Split a comma list at depth 0 (ignoring commas inside <>, (), []).
function split(s) {
  const out = []; let d = 0, cur = "";
  for (const ch of s) {
    if ("<([".includes(ch)) d++; else if (">)]".includes(ch)) d--;
    if (ch === "," && d === 0) { if (cur.trim()) out.push(cur.trim()); cur = ""; } else cur += ch;
  }
  if (cur.trim()) out.push(cur.trim());
  return out;
}
function between(s, open, close) {
  const i = s.indexOf(open); if (i < 0) return null;
  let d = 0;
  for (let j = i; j < s.length; j++) { if (s[j] === open) d++; else if (s[j] === close) { d--; if (!d) return s.slice(i + 1, j); } }
  return s.slice(i + 1);
}

// Read an item into its facts. Kind words are the reader's, not the compiler's ("alias", "marker").
export function read(item) {
  const s = (item.s || "").replace(/^pub(\([^)]*\))?\s+/, "").trim();
  const n = item.n;
  const f = { kind: "item", tags: [] };
  const tag = (k, label, tip, tone = "") => f.tags.push({ k, label, tip, tone });
  let m;
  if (/^macro_rules!/.test(s) || item.f === "callable" && !/fn\b/.test(s)) { f.kind = "macro"; tag("macro", `${n}!`, "A macro: you call it with a bang and it writes code for you."); return f; }
  const q = { unsafe: /\bunsafe\b/.test(s.split(/\bfn\b|\btrait\b|\bimpl\b/)[0]), async: /\basync\b/.test(s.split(/\bfn\b/)[0]), konst: /^const\s+(unsafe\s+)?fn\b/.test(s), extern: /\bextern\s+"C"/.test(s) };
  if ((m = s.match(/\bfn\s+\w+/))) {
    f.kind = "fn";
    const after = s.slice(s.indexOf(m[0]) + m[0].length);
    const gen = after.startsWith("<") ? between(after, "<", ">") : null;
    const params = split(between(after, "(", ")") || "");
    const recv = params.find((p) => /^&?\s*(mut\s+)?self\b|^&'\w+\s+(mut\s+)?self/.test(p));
    const rest = params.filter((p) => p !== recv);
    const ret = (s.match(/->\s*(.+?)\s*(\{|where\b|$)/) || [])[1] || "";
    if (q.async) tag("async", "async", "It returns a future: you .await it.", "peri");
    if (q.unsafe) tag("unsafe", "unsafe", "Calling it is unsafe: you promise what the compiler can't check.", "amber");
    if (q.konst) tag("konst", "const", "It can run while compiling.");
    if (q.extern) tag("ffi", "C ABI", "It uses the C calling convention.");
    if (recv) tag(/mut/.test(recv) ? "mutates" : "borrow", /mut/.test(recv) ? "changes it" : /^self/.test(recv) ? "consumes it" : "reads it", /mut/.test(recv) ? "A method that changes its receiver (&mut self)." : /^self/.test(recv) ? "A method that takes its receiver by value." : "A method that only reads its receiver (&self).");
    if (rest.length) tag("ctor", `takes ${rest.length}`, rest.map((p) => p.split(":")[0].trim()).join(", "));
    else if (!recv) tag("ctor", "takes nothing", "No arguments.");
    if (rest.some((p) => /:\s*&\s*mut\b/.test(p))) tag("mutates", "writes into", "One of its arguments is borrowed mutably.");
    if (gen) { const ps = split(gen).filter((g) => !g.startsWith("'")).map((g) => g.split(/[:=]/)[0].trim()); if (ps.length) tag("generic", ps.join(" "), `Generic over ${ps.join(", ")}.`, "teal"); }
    if (/^Result\b|^io::Result|^Result</.test(ret) || /Result<|\bResult\b/.test(ret)) tag("fail", "can fail", "It returns a Result: an error is a normal outcome.", "coral");
    else if (/^Option</.test(ret)) tag("maybe", "maybe", "It returns an Option: sometimes there's nothing.");
    else if (/^impl\s+(Iterator|IntoIterator)/.test(ret) || /Iter\b/.test(ret)) tag("iter", "iterates", "It hands back an iterator.");
    else if (/^Self\b/.test(ret) || ret === n) tag("ctor", "makes one", "It builds a new value of its type.");
    else if (ret === "bool") tag("maybe", "yes / no", "It answers with a bool.");
    return f;
  }
  if ((m = s.match(/^(unsafe\s+)?trait\s+(\w+)(.*)/))) {
    f.kind = "trait";
    if (m[1]) tag("unsafe", "unsafe", "Implementing it is unsafe.", "amber");
    const sup = (m[3].match(/:\s*([^{]+)/) || [])[1];
    const gen = m[3].startsWith("<") ? between(m[3], "<", ">") : null;
    if (gen) tag("generic", split(gen).filter((g) => !g.startsWith("'")).map((g) => g.split(/[:=]/)[0].trim()).join(" "), "Generic parameters.", "teal");
    if (sup) for (const t of sup.split("+").map((x) => x.trim()).filter((x) => x && !/^'/.test(x)).slice(0, 3)) tag("owner", `needs ${t.replace(/<.*/, "")}`, `Anything that implements it must also be ${t}.`);
    return f;
  }
  if ((m = s.match(/^type\s+(\w+)(<[^=]*>)?\s*=\s*([^;]+)/))) {
    f.kind = "alias";
    tag("owner", `is ${m[3].replace(/<.*/, "").split("::").pop()}`, `Another name for ${m[3].trim()}.`);
    return f;
  }
  if ((m = s.match(/^(struct|enum|union)\s+(\w+)(<[^>{(]*>)?\s*([{;(])?/))) {
    f.kind = m[1];
    if (m[3]) {
      const gs = split(m[3].slice(1, -1));
      const life = gs.filter((g) => g.startsWith("'"));
      const ts = gs.filter((g) => !g.startsWith("'")).map((g) => g.split(/[:=]/)[0].trim());
      if (life.length) tag("borrow", "borrows", "It holds a reference: it can't outlive what it borrows.");
      if (ts.length) tag("generic", ts.join(" "), `Generic over ${ts.join(", ")}.`, "teal");
    }
    if (m[4] === ";") tag("marker", "marker", "It has no fields: its type is the point.");
    else if (m[4] === "(") { const inner = (between(s, "(", ")") || "").split(",")[0].trim().replace(/^pub\s+/, "").replace(/<.*/, ""); if (!inner || inner === "()") tag("marker", "marker", "It carries nothing: its type is the point."); else tag("owner", `wraps ${inner}`, "A tuple struct around one value."); }
    if (/Error$/.test(n)) tag("error", "error", "An error type: what goes wrong, and why.", "coral");
    else if (/(Iter|Iterator|Keys|Values|Drain|IntoIter|Stream)$/.test(n)) tag("iter", "iterator", "You loop over it.");
    else if (/Guard$/.test(n)) tag("konst", "guard", "Holds something until it's dropped.");
    else if (/Builder$/.test(n)) tag("ctor", "builder", "Builds a value step by step.");
    return f;
  }
  if ((m = s.match(/^(const|static)\s+(mut\s+)?(\w+)\s*:\s*([^=;]+)/))) {
    f.kind = m[1];
    if (m[2]) tag("unsafe", "mutable static", "A global you can change: unsafe to touch.", "amber");
    tag("owner", m[4].trim().replace(/^&'static\s+/, "&").slice(0, 18), `Its type: ${m[4].trim()}.`);
    return f;
  }
  return f;
}

// The kind word and mark every card leads with.
export const KIND = { fn: "fn", macro: "macro", trait: "trait", alias: "alias", struct: "struct", enum: "enum", union: "union", const: "const", static: "static", item: "item" };

export function tagHtml(t) {
  return `<span class="bdg ${t.tone || ""}" tabindex="0">${icon(t.k)}<b>${esc(t.label)}</b><em>${esc(t.tip || "")}</em></span>`;
}
export function tagsHtml(item, extra = []) {
  const r = read(item);
  return extra.concat(r.tags).map(tagHtml).join("");
}
