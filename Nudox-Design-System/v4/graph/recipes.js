// How to get one, and search by shape: both read one table of every callable, variant, open
// struct and Default type, keyed by plain-word types ("text", "path", "list of Foo", "#<node>").
//
// Getting one: plain values (text, a path, a number, …) cost nothing. Every producer costs one
// step plus what its inputs cost. A least-cost derivation over that AND-OR graph (Knuth's
// generalisation of Dijkstra: a producer fires once all of its inputs are settled) gives every
// type its cheapest recipe from plain values. The page shows the best few as rails of steps, with
// the exact code one keystroke (⌥) away. For a callable the same tree answers "how do I call it":
// how to get each argument it needs.
//
// By shape: "path -> maybe text", "takes a Page gives list of Relation", "text, number → Span".
// Inputs match in any order; the result matches through maybe/fails; extra parameters cost a little.
(() => {
  "use strict";
  let G = null, P = null; // the graph api, the page's type helpers
  const GROUND = new Set(["text", "path", "number", "bool", "char", "bytes", "nothing", "any"]);
  const isGround = (k) => GROUND.has(k) || k.startsWith("any ");
  const NUM = new Set(["u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64", "NonZeroU32", "NonZeroU64", "NonZeroUsize", "Duration"]);
  const SEE = new Set(["Option", "Result", "Box", "Rc", "Arc", "Cow", "Pin", "RefCell", "Cell", "Mutex", "RwLock", "Ref", "RefMut", "MutexGuard", "RwLockReadGuard", "RwLockWriteGuard", "AsRef", "Into", "Borrow", "ManuallyDrop", "Weak"]);
  const LISTS = new Set(["Vec", "VecDeque", "SmallVec", "LinkedList", "HashSet", "BTreeSet", "IndexSet", "BinaryHeap", "IntoIterator", "Iterator", "Peekable"]);
  const MAPS = new Set(["HashMap", "BTreeMap", "IndexMap"]);

  // ------------------------------------------------------------------ a type, as a key
  function plain(last) {
    if (last === "String" || last === "str" || last === "OsStr" || last === "OsString") return "text";
    if (last === "Path" || last === "PathBuf") return "path";
    if (last === "u8") return "byte";
    if (NUM.has(last)) return "number";
    if (last === "bool") return "bool"; if (last === "char") return "char";
    return null;
  }
  function tkey(s, from, owner) {
    const r = { k: "nothing", maybe: false, fails: false };
    if (!s || !s.trim()) return r;
    const gens = P.genericsOf(from);
    const t = P.lexType(s); let i = 0, depth = 0;
    const peek = () => t[i], next = () => t[i++];
    function generics() {
      if (peek() !== "<") return []; next(); const out = []; let g = 0;
      while (i < t.length && peek() !== ">" && g++ < 64) {
        if (/^'/.test(peek())) { next(); if (peek() === ",") next(); continue; }
        let a = ty(); if (peek() === "=") { next(); a = ty(); } out.push(a);
        if (peek() === ",") next(); else if (peek() !== ">") next();
      }
      next(); return out;
    }
    function bound(name) { // a generic reads as what its bound says it stands for
      for (const b of gens.get(name) || []) for (const part of P.splitPlus(b)) {
        const p = part.trim(); const m = /^(?:AsRef|Into|Borrow)\s*<\s*([\w:]+)\s*>$/.exec(p);
        if (m) { const w = plain(m[1].split("::").pop()); if (w) return w === "byte" ? "number" : w; }
        if (/^(ToString|Display)\b/.test(p)) return "text";
        const named = /^(?:[\w]+::)*([A-Z]\w*)(?:\s*<.*>)?$/.exec(p); if (named && !["Sized", "Send", "Sync", "Clone", "Copy", "Debug", "Unpin", "Default", "PartialEq", "Eq", "Hash", "Ord", "PartialOrd"].includes(named[1])) return "any " + named[1];
        const it = /^IntoIterator\s*<\s*Item\s*=\s*(.+)>$/.exec(p); if (it) { const inner = tkey(it[1], from, owner).k; return inner === "number" && /\bu8\b/.test(it[1]) ? "bytes" : "list of " + inner; }
      }
      return "any";
    }
    function ty() {
      const x = next(); if (x === undefined) return "?";
      if (x === "&") { if (/^'/.test(peek())) next(); if (peek() === "mut") next(); return ty(); }
      if (x === "*") { next(); return ty(); }
      if (x === "(") { const parts = []; let g = 0; depth++; while (i < t.length && peek() !== ")" && g++ < 32) { parts.push(ty()); if (peek() === ",") next(); else if (peek() !== ")") next(); } depth--; next(); return !parts.length ? "nothing" : parts.length === 1 ? parts[0] : "(" + parts.join(", ") + ")"; }
      if (x === "[") { depth++; const inner = ty(); depth--; if (peek() === ";") { next(); next(); } next(); return inner === "byte" ? "bytes" : "list of " + inner; }
      if (x === "!") return "never";
      if (x === "dyn" || x === "impl") { const inner = ty(); while (peek() === "+") { next(); ty(); } return inner; }
      if (x === "mut" || x === "const") return ty();
      const segs = x.split("::"); const last = segs[segs.length - 1];
      if (segs.length > 1 && (segs[0] === "Self" || gens.has(segs[0]) || /^[A-Z]$/.test(segs[0]))) { generics(); return "any"; }
      if (segs.length === 1 && (gens.has(x) || /^[A-Z]$/.test(x))) { generics(); return bound(x); }
      if (/^Fn(Mut|Once)?$/.test(last) && peek() === "(") { let g = 0; next(); while (i < t.length && peek() !== ")" && g++ < 32) next(); next(); if (peek() === "->") { next(); ty(); } return "a function"; }
      const see = SEE.has(last); if (!see) depth++; const args = generics(); if (!see) depth--;
      const a0 = args[0] || "?";
      if (last === "Option") { if (depth === 0) { r.maybe = true; return a0; } return "maybe " + a0; }
      if (last === "Result") { if (depth === 0) r.fails = true; return args.length ? a0 : "nothing"; }
      if (see) return a0;
      if (LISTS.has(last)) return a0 === "byte" ? "bytes" : last === "SmallVec" && a0.startsWith("list of ") ? a0 : "list of " + a0;
      if (MAPS.has(last)) return "map " + a0 + " to " + (args[1] || "?");
      if (last === "Self") return owner >= 0 ? "#" + owner : "any";
      const p = plain(last); if (p) return p;
      if (/^[a-z]/.test(last)) return last;
      const j = P.resolveName(last, from); return j >= 0 ? "#" + j : last;
    }
    let k = ty(); while (i < t.length) ty(); // tolerate trailing tokens
    r.k = k === "byte" ? "number" : k.replace(/\bbyte\b/g, "number");
    return r;
  }

  // every trait a type is: derived, written in this world (`impls` point at the trait's node), or written
  // against a trait outside it (`implsExt`, a path)
  const traitNames = (n) => [
    ...(n.derives || []),
    ...(n.impls || []).map((o) => (o.trait >= 0 ? G.N[o.trait].n : null)).filter(Boolean),
    ...(n.implsExt || []).map((t) => t.split("::").pop().replace(/<.*/, "")),
  ];

  // ------------------------------------------------------------------ the table of producers
  let table = null;
  const isTest = (f) => /(^|\/)(tests?|benches)\//.test(f || "") || /_tests?\.rs$|\/tests\.rs$/.test(f || "");
  function build() {
    table = [];
    const N = G.N;
    for (let i = 0; i < N.length; i++) {
      const n = N[i]; if (n.orphan) continue;
      const f = n.f || (G.topOf[i] >= 0 ? N[G.topOf[i]].f : "");
      if (isTest(f)) continue;
      if (n.k === "function" || n.k === "method") {
        const owner = n.u; const o = n.ret ? tkey(n.ret, i, owner) : { k: "nothing", maybe: false, fails: false };
        const ins = [], names = [];
        if (n.recv && owner >= 0) { ins.push("#" + owner); names.push("it"); }
        for (const p of P.params(n)) { ins.push(tkey(p.ty, i, owner).k); names.push(p.name.replace(/^_+/, "")); }
        table.push({ i, how: n.recv ? "method" : "call", ins, names, out: o.k, fails: o.fails, maybe: o.maybe });
      } else if (n.k === "variant" && n.u >= 0) {
        const ins = [], names = [];
        if (n.ty) for (const part of P.splitTop(n.ty)) { const c = n.shape === "record" ? part.indexOf(":") : -1; names.push(c > 0 ? part.slice(0, c).trim() : ""); ins.push(tkey(c > 0 ? part.slice(c + 1).trim() : part, i, n.u).k); }
        table.push({ i, how: "variant", ins, names, out: "#" + n.u, record: n.shape === "record" });
      } else if (n.k === "struct" || n.k === "enum") {
        if (n.k === "struct" && !n.nonExhaustive) {
          const fs = (G.kids[i] || []).filter((j) => N[j].k === "field");
          table.push({ i, how: "literal", ins: fs.map((j) => tkey(N[j].ty, j, i).k), names: fs.map((j) => N[j].n), out: "#" + i,
            closed: fs.some((j) => N[j].v !== "pub"), tuple: fs.length > 0 && fs.every((j) => /^\d+$/.test(N[j].n)) });
        }
        if (traitNames(n).includes("Default")) table.push({ i, how: "default", ins: [], names: [], out: "#" + i });
      }
    }
  }
  // a producer is usable from package `pk` when it is public, or lives in `pk` itself
  const usable = (e, pk) => { const n = G.N[e.i]; if (e.how === "literal" && e.closed) return n.p === pk; if (e.how === "variant") return G.N[n.u].v === "pub" || n.p === pk; return n.v === "pub" || !!n.via || n.p === pk; };
  const weight = (e) => 1 + (e.fails ? 0.45 : 0) + (e.maybe ? 0.35 : 0) + (e.how === "literal" ? 0.12 * e.ins.length : 0) - (e.how === "default" ? 0.3 : 0) + (G.PK[G.N[e.i].p].external ? 0.05 : 0);

  // ------------------------------------------------------------------ least-cost derivations, per package seen from
  // Knuth over the table: `free` keys cost nothing (plain values, and in a chain search what you
  // have); a producer fires once all of its inputs are settled.
  function knuth(pk, free) {
    if (!table) build();
    const cost = new Map(), best = new Map(), done = new Set(), waiting = new Map();
    const left = new Int32Array(table.length);
    const heap = mkheap();
    const relax = (k, c, q) => { if (!done.has(k) && c < (cost.has(k) ? cost.get(k) : Infinity)) { cost.set(k, c); best.set(k, q); heap.push(c, k); } };
    for (const g of GROUND) relax(g, 0, -1);
    for (const g of free || []) relax(g, 0, -1);
    for (const e of table) for (const k of e.ins) if (k.startsWith("any ") && !cost.has(k)) relax(k, 0, -1);
    table.forEach((e, q) => {
      left[q] = -1;
      if (isGround(e.out) || e.out === "never" || e.out === "?" || e.ins.includes(e.out) || !usable(e, pk)) return;
      const ks = [...new Set(e.ins)]; left[q] = ks.length;
      if (!ks.length) relax(e.out, weight(e), q);
      else for (const k of ks) (waiting.get(k) || waiting.set(k, []).get(k)).push(q);
    });
    while (heap.size()) {
      const [, k] = heap.pop(); if (done.has(k)) continue; done.add(k);
      for (const q of waiting.get(k) || []) {
        if (left[q] <= 0 || --left[q] > 0) continue;
        const e = table[q]; let s = weight(e); for (const x of e.ins) s += cost.get(x); relax(e.out, s, q);
      }
    }
    return { cost, best, pk };
  }
  function mkheap() {
    const h = [];
    return {
      size: () => h.length,
      push(c, k) { h.push([c, k]); let a = h.length - 1; while (a > 0) { const b = (a - 1) >> 1; if (h[b][0] <= h[a][0]) break; [h[a], h[b]] = [h[b], h[a]]; a = b; } },
      pop() { const top = h[0], last = h.pop(); if (h.length) { h[0] = last; let a = 0; for (;;) { const l = 2 * a + 1, r = l + 1; let m = a; if (l < h.length && h[l][0] < h[m][0]) m = l; if (r < h.length && h[r][0] < h[m][0]) m = r; if (m === a) break; [h[a], h[m]] = [h[m], h[a]]; a = m; } } return top; },
    };
  }
  const runs = new Map();
  function run(pk) {
    if (!runs.has(pk)) runs.set(pk, knuth(pk, null));
    return runs.get(pk);
  }
  function tree(R, q, seen) {
    const e = table[q];
    return { q, kids: e.ins.map((k, a) => {
      const name = e.names[a];
      if (isGround(k)) return { ground: k, name };
      const b = R.best.get(k);
      if (b === undefined || b < 0 || seen.has(k)) return { ground: k, name, opaque: true };
      const s = new Set(seen); s.add(k); return { key: k, name, node: tree(R, b, s) };
    }) };
  }
  const costOf = (R, e) => { let s = weight(e); for (const k of e.ins) { const c = R.cost.get(k); if (c === undefined) return Infinity; s += c; } return s; };
  // the best few routes to a type, one per distinct first maker
  function routes(i, max = 3) {
    const n = G.N[i]; const R = run(n.p); const key = "#" + i;
    const cands = [];
    let picks = 0;
    // the type's own makers first, then its package's, then helpers elsewhere; fewer arguments first
    const adj = (e) => (G.N[e.i].p !== n.p ? 0.3 : 0) + 0.04 * e.ins.length - (G.N[e.i].u === i ? 0.15 : 0) + (G.yours(e.i) && !G.yours(i) ? 0.2 : 0);
    table.forEach((e, q) => { if (e.out !== key || e.ins.includes(key) || !usable(e, n.p)) return; if (e.how === "variant" && G.N[e.i].u === i) { picks++; return; } const c = costOf(R, e); if (c < Infinity) cands.push([c + adj(e), q]); });
    cands.sort((a, b) => a[0] - b[0] || G.IMP[table[b[1]].i] - G.IMP[table[a[1]].i]);
    const out = [], groups = new Map(); const size = (t) => 1 + t.kids.reduce((a, k) => a + (k.node ? size(k.node) : 0), 0);
    for (const [c, q] of cands) {
      const e = table[q]; const tag = e.how + ":" + G.N[e.i].u + ":" + G.N[e.i].n;
      if (groups.has(tag)) { const r = groups.get(tag); if (r.alts.length < 8 && e.ins.length === 1) r.alts.push(e.ins[0]); continue; }
      if (out.length >= max) continue;
      const node = tree(R, q, new Set([key]));
      if (out.length && (size(node) > 6 || c > out[0].cost + 3)) continue; // a long way round when a short one exists
      const r = { cost: c, node, alts: [] }; groups.set(tag, r); out.push(r);
    }
    return { routes: out, makers: cands.length, picks };
  }
  // how to call a callable: its own entry, with each argument derived
  function callRoute(i) {
    if (!table) build();
    const q = table.findIndex((e) => e.i === i && (e.how === "call" || e.how === "method")); if (q < 0) return null;
    const R = run(G.N[i].p); return { cost: costOf(R, table[q]), node: tree(R, q, new Set()) };
  }

  // ------------------------------------------------------------------ words and code
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const A = { text: "text", path: "a path", number: "a number", bool: "yes or no", char: "a character", bytes: "bytes", any: "anything", nothing: "" };
  function words(k, link = true) {
    if (k.startsWith("#")) { const j = +k.slice(1); return link ? `<a class="gtl${G.yours(j) ? " y" : ""}" data-i="${j}">${esc(G.N[j].n)}</a>` : esc(G.N[j].n); }
    if (k.startsWith("list of ")) return `<i class="gtw">list of</i> ${words(k.slice(8), link)}`;
    if (k.startsWith("maybe ")) return `<i class="gtw">maybe</i> ${words(k.slice(6), link)}`;
    if (k.startsWith("map ")) { const m = /^map (.*) to (.*)$/.exec(k); return m ? `<i class="gtw">map</i> ${words(m[1], link)} <i class="gtp">→</i> ${words(m[2], link)}` : esc(k); }
    if (k.startsWith("any ")) { const t = k.slice(4); const j = G.N.findIndex((n) => n.n === t && n.k === "trait"); return `<i class="gtw">any</i> ${j >= 0 && link ? `<a class="gtl" data-i="${j}">${esc(t)}</a>` : esc(t)}`; }
    return `<i class="gtw">${esc(A[k] !== undefined ? A[k] || "nothing" : k)}</i>`;
  }
  const snake = (s) => s.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
  const VAR = { text: "text", path: "path", number: "n", bool: "yes", char: "c", bytes: "bytes", any: "value", nothing: "()" };
  function leafVar(c) {
    if (c.have && c.ground.startsWith("#")) return snake(G.N[+c.ground.slice(1)].n); // your value is named for its type
    if (c.name && c.name !== "it" && c.name !== "self" && !/^\d+$/.test(c.name)) return c.name;
    return c.key || c.ground.startsWith("#") ? snake(G.N[+(c.key || c.ground).slice(1)].n) : VAR[c.ground] || snake(c.ground.replace(/\W+/g, "_"));
  }
  const crate = (j) => G.PK[G.N[j].p].name.replace(/-/g, "_");
  function code(node) {
    const lets = [], bound = new Map(), used = new Set(), calls = new Set();
    // two different types with one name in the same code (toml's Value and serde_json's) are spelled with their crate
    const named = new Map(); const note = (j) => { if (j >= 0) (named.get(G.N[j].n) || named.set(G.N[j].n, new Set()).get(G.N[j].n)).add(j); };
    (function walk(t) { const e = table[t.q]; if (e.how === "call") calls.add(G.N[e.i].n); note(G.N[e.i].u); if (e.out.startsWith("#")) note(+e.out.slice(1)); t.kids.forEach((k) => { if (k.node) walk(k.node); else if (k.ground && k.ground.startsWith("#")) note(+k.ground.slice(1)); }); })(node);
    qualify = new Set(); for (const set of named.values()) if (set.size > 1) for (const j of set) qualify.add(j);
    // a local named like a function it calls would shadow it: `let page = …; page(page)` does not compile
    const fresh = (v) => { let w = calls.has(v) ? "the_" + v : v, k = 2; const base = w; while (used.has(w)) w = base + k++; used.add(w); return w; };
    const nameFor = (key) => { const m = /^(?:list of )?#(\d+)$/.exec(key || ""); const base = m ? snake(G.N[+m[1]].n) : "value"; return key && key.startsWith("list of ") ? base + "s" : base; };
    const arg = (c) => {
      if (!c.node) { const v = leafVar(c); used.add(v); return v; }
      if (bound.has(c.key)) return bound.get(c.key);
      const s = one(c.node);
      if (!c.node.kids.some((x) => x.node) && s.length <= 40) return s;
      const v = fresh(nameFor(c.key)); lets.push(`let ${v} = ${s};`); bound.set(c.key, v); return v;
    };
    const one = (t, root = false) => spell(t, t.kids.map(arg), root);
    const last = one(node, true);
    qualify = null;
    return lets.concat([last]).join("\n");
  }
  let qualify = null;
  function spell(node, args, root = true) {
    const e = table[node.q], n = G.N[e.i];
    const owner = n.u >= 0 ? (qualify && qualify.has(n.u) ? crate(n.u) + "::" : "") + G.N[n.u].n : "";
    const fields = () => e.names.map((f, k) => (f === args[k] ? f : `${f}: ${args[k]}`)).join(", ");
    let s;
    if (e.how === "method") s = `${/^\w[\w:]* \{/.test(args[0]) ? `(${args[0]})` : args[0]}.${n.n}(${args.slice(1).join(", ")})`;
    else if (e.how === "call") s = `${owner ? owner + "::" : ""}${n.n}(${args.join(", ")})`;
    else if (e.how === "variant") s = `${owner}::${n.n}` + (e.ins.length ? (e.record ? ` { ${fields()} }` : `(${args.join(", ")})`) : "");
    else if (e.how === "literal") s = e.tuple ? `${n.n}(${args.join(", ")})` : e.ins.length ? `${n.n} { ${fields()} }` : n.n;
    else s = `${n.n}::default()`;
    return s + (e.fails || (!root && e.maybe) ? "?" : "");
  }
  // a route read as a rail: the spine follows the costliest input; the other inputs ride along as "+ …"
  function rail(route, targetKey, whose = "your") {
    const R = run(G.N[table[route.node.q].i].p);
    const steps = []; let cur = route.node, start = null;
    while (cur) {
      const kids = cur.kids; let spine = -1, sc = -1;
      if (cur.spine !== undefined) spine = cur.spine; // a chain: the spine is what you have
      else kids.forEach((c, a) => { if (c.node) { const v = R.cost.get(c.key) || 0; if (v > sc) { sc = v; spine = a; } } });
      const sides = kids.filter((_, a) => a !== spine).filter((c) => !(c.ground === "nothing"));
      steps.unshift({ q: cur.q, sides, spineKey: spine >= 0 ? kids[spine].key : null });
      if (spine >= 0 && !kids[spine].node) { start = kids[spine]; cur = null; } else cur = spine >= 0 ? kids[spine].node : null;
    }
    const trivial = (e) => !e.ins.length && (e.how === "variant" || e.how === "default" || e.how === "literal");
    let lead = start ? `<span class="rsrc">from ${whose} ${words(start.ground)}${start.as ? `, <i>a</i> ${esc(start.as.slice(4))}` : ""}</span>` : "";
    if (!lead && steps.length > 1 && trivial(table[steps[0].q])) { lead = `<span class="rsrc">from <i>a</i> ${words(table[steps[0].q].out)}</span>`; steps.shift(); }
    const e0 = table[steps[0].q];
    const first = !lead && e0.how === "method" && steps[0].sides.length && steps[0].sides[0].name === "it" ? steps[0].sides.shift() : null;
    // a first step fed only by plain values reads "from text what", not "from · shape + text what"
    let from = "";
    if (!lead && !first && steps[0].sides.length && steps[0].sides.every((c) => !c.key)) { from = steps[0].sides.slice(0, 2).map((c) => `${words(c.ground)}${c.name && c.name !== "it" && !/^\d+$/.test(c.name) ? ` <i class="rnm">${esc(c.name)}</i>` : ""}`).join(", "); steps[0].sides = steps[0].sides.slice(2); }
    let html = lead || (first ? `<span class="rsrc">from ${words(first.ground || first.key)}</span>` : from ? `<span class="rsrc">from ${from}</span>` : "");
    steps.forEach((s, a) => {
      const e = table[s.q], n = G.N[e.i];
      const verb = e.how === "variant" ? `${esc(G.N[n.u].n)}::${esc(n.n)}` : e.how === "literal" ? `build` : e.how === "default" ? `default` : esc(n.n);
      const sides = s.sides.map((c) => `<span class="rside">+ ${c.key && c.node && trivial(table[c.node.q]) ? "a " : ""}${c.opaque || c.key ? words(c.key || c.ground) : words(c.ground)}${c.name && c.name !== "it" && !/^\d+$/.test(c.name) ? ` <i class="rnm">${esc(c.name)}</i>` : ""}</span>`).join("");
      html += `<span class="rstep">${a === 0 && !html ? "" : '<i class="rl"></i>'}<a class="gtl rv" data-i="${e.i}">${verb}</a>${e.fails ? `<i class="rq" title="may fail">?</i>` : e.maybe ? `<i class="rq m" title="may give nothing">?</i>` : ""}${sides}<i class="rl"></i></span>`;
      const k = a < steps.length - 1 ? e.out : null;
      if (k) html += `<span class="rst">${words(k)}</span>`;
    });
    html += `<span class="rend"${targetKey ? "" : ' data-call="1"'}></span>`;
    const alts = (route.alts || []).filter((k) => k !== (table[route.node.q].ins[0]));
    if (alts.length) html += `<span class="ralt">also from ${[...new Set(alts)].slice(0, 5).map((k) => words(k)).join(", ")}${alts.length > 5 ? ` <i>and ${alts.length - 5} more</i>` : ""}</span>`;
    return `<div class="route"><div class="rrail">${html}</div><code class="rcode">${esc(code(route.node))}</code></div>`;
  }

  // ------------------------------------------------------------------ the page sections
  function gettingOne(i) {
    const n = G.N[i];
    if (!["struct", "enum", "union", "type"].includes(n.k)) return "";
    const { routes: rs, makers, picks } = routes(i);
    const pickNote = picks ? `or pick one of its ${picks} variant${picks === 1 ? "" : "s"} above` : "";
    if (!rs.length && picks) return `<section class="csec gget"><h2>Getting one</h2><p class="rnone">Pick one of its ${picks} variant${picks === 1 ? "" : "s"} above; nothing else in this world makes one.</p></section>`;
    if (!rs.length) {
      return `<section class="csec gget"><h2>Getting one</h2><p class="rnone">Nothing public makes one from plain values. You receive it${makers ? "" : " from its own crate"}; the prism shows from where.</p></section>`;
    }
    return `<section class="csec gget"><h2>Getting one</h2>${rs.map((r) => rail(r, "#" + i)).join("")}`
      + `<div class="rfoot">${[`${makers} way${makers === 1 ? "" : "s"} in this world make${makers === 1 ? "s" : ""} one`, pickNote, "⌥ for code"].filter(Boolean).join(" · ")}</div></section>`;
  }
  function callingIt(i) {
    const n = G.N[i]; if (n.k !== "function" && n.k !== "method") return "";
    const r = callRoute(i); if (!r) return "";
    const deep = r.node.kids.filter((k) => k.node).length; const need = r.node.kids.filter((k) => k.opaque && !isGround(k.ground));
    if (!deep && !need.length) return ""; // plain arguments only: the pipe already says it
    return `<section class="csec gget"><h2>Calling it</h2>${rail(r, null)}`
      + (need.length ? `<div class="rfoot">you need ${need.map((c) => words(c.ground)).join(", ")} — nothing public makes one from plain values</div>` : `<div class="rfoot">every argument, from plain values · ⌥ for code</div>`) + `</section>`;
  }

  // ------------------------------------------------------------------ search by shape
  const isShape = (q) => /->|→|=>/.test(q) || /^\s*(takes|gives)\s/i.test(q);
  function wordKeys(w) {
    let s = w.trim().toLowerCase().replace(/^(an?|the|some)\s+/, "");
    let maybe = false; const m = /^(maybe|option(?:al)?(?: of)?)\s+(.*)$/.exec(s); if (m) { maybe = true; s = m[2]; } if (s.endsWith("?")) { maybe = true; s = s.slice(0, -1); }
    const l = /^(?:list of|vec of|many|all|several)\s+(.*)$/.exec(s) || /^\[(.*)\]$/.exec(s) || /^vec<(.*)>$/.exec(s);
    if (l) { const inner = wordKeys(l[1]); return { keys: new Set([...inner.keys].map((k) => (k === "number" && /u8|byte/.test(l[1]) ? "bytes" : "list of " + k))), maybe }; }
    const P = { text: ["text", "string", "str", "name", "&str"], path: ["path", "file", "pathbuf", "&path", "dir", "directory"], number: ["number", "int", "integer", "count", "size", "usize", "u32", "u64", "i32", "i64", "f32", "f64", "float", "index"], bool: ["bool", "boolean", "flag", "yes or no"], bytes: ["bytes", "[u8]", "&[u8]", "vec<u8>", "buffer"], nothing: ["nothing", "()", "unit", ""], char: ["char", "character"], any: ["anything", "any", "whatever"] };
    for (const [k, ws] of Object.entries(P)) if (ws.includes(s)) return { keys: new Set([k]), maybe };
    const keys = new Set(); const base = s.split("::").pop();
    G.N.forEach((n, j) => { if (n.u < 0 && n.n.toLowerCase() === base && ["struct", "enum", "trait", "type", "union"].includes(n.k)) keys.add("#" + j); });
    return { keys, maybe };
  }
  function parseShape(q) {
    let s = q.replace(/→|=>/g, "->").trim();
    s = s.replace(/^takes\s+(.*?)\s+gives\s+/i, "$1 -> ").replace(/^takes\s+/i, "").replace(/^gives\s+/i, "-> ");
    const at = s.indexOf("->"); const left = at < 0 ? s : s.slice(0, at), right = at < 0 ? null : s.slice(at + 2);
    const ins = left.split(/,|\band\b/).map((w) => w.trim()).filter(Boolean).map(wordKeys);
    const out = right !== null && right.trim() ? wordKeys(right) : null;
    return { ins, out };
  }
  const traitsOf = (keys) => { const t = new Set(); for (const k of keys) if (k.startsWith("#")) { const n = G.N[+k.slice(1)]; for (const x of traitNames(n)) t.add("any " + x); } return t; };
  function shape(q, max = 7) {
    if (!table) build();
    const { ins, out } = parseShape(q);
    const via = ins.map((x) => traitsOf(x.keys));
    if (out && !out.keys.size) return [];
    if (ins.some((x) => !x.keys.size)) return [];
    const res = [];
    for (const e of table) {
      if (e.how !== "call" && e.how !== "method") continue;
      const n = G.N[e.i]; let score = 0;
      if (out) {
        if (out.keys.has(e.out)) score += 3; else if ([...out.keys].some((k) => e.out === "list of " + k)) score += 1; else continue;
        if (out.maybe) score += e.maybe ? 0.5 : -0.4;
      } else if (!ins.length) continue;
      const used = new Array(e.ins.length).fill(false); let ok = true;
      for (const [xi, x] of ins.entries()) {
        let a = e.ins.findIndex((k, z) => !used[z] && (x.keys.has(k) || (x.keys.has("any") && k.startsWith("any "))));
        let w = 2; if (a < 0) { a = e.ins.findIndex((k, z) => !used[z] && via[xi].has(k)); w = 1; } // yours, through a trait it implements
        if (a < 0) { ok = false; break; } used[a] = true; score += w;
      }
      if (!ok) continue;
      score -= 0.6 * used.filter((u) => !u).length;
      score += G.IMP[e.i] * 1.5 + (G.yours(e.i) ? 0.2 : 0) + (n.v === "pub" ? 0.3 : 0) - (isTest(n.f) ? 2 : 0);
      res.push([score, e.i, e]);
    }
    res.sort((a, b) => b[0] - a[0]);
    // one row per shape: From<String>, From<&str> and From<Cow<str>> all read "(text) → Value"
    const seen = new Set(), outL = [];
    for (const [, i, e] of res) { const tag = G.N[i].u + ":" + G.N[i].n + ":" + e.ins.join(",") + ">" + e.out + (e.fails ? "!" : "") + (e.maybe ? "?" : ""); if (seen.has(tag)) continue; seen.add(tag); outL.push(i); if (outL.length >= max) break; }
    return outL;
  }
  // ------------------------------------------------------------------ in steps: from what you have to what you need
  // A shape no single call answers is often a few calls away. A second least-cost pass counts only
  // derivations that consume what you have: a producer fed by one of those pays its weight, that
  // input's cost, and the plain-value cost of its other inputs (a first run in which your values
  // are free too). Traits your values implement count as had ("any Serialize" takes your Value).
  // The spine of each route is your value becoming the answer; the rest rides along as "+ …".
  let byIn = null; const r0s = new Map();
  function chains(q, max = 3) {
    if (!table) build();
    const { ins, out } = parseShape(q);
    if (!out || !out.keys.size || !ins.length || ins.some((x) => !x.keys.size)) return [];
    if (ins.every((x) => [...x.keys].every(isGround))) return []; // from text alone, every road leads somewhere
    return chainsFor(ins, out, max, false);
  }
  // from what you have (ins: [{ keys }]) to what you need (out: { keys, maybe }); `oneCall` keeps
  // single-call answers (a conversion wants the best road, however short)
  function chainsFor(ins, out, max, oneCall) {
    const have = new Set(), traits = new Map(); // traits: "any Serialize" -> the value of yours that is one
    for (const x of ins) for (const k of x.keys) {
      have.add(k);
      if (k.startsWith("#")) { const n = G.N[+k.slice(1)]; for (const t of traitNames(n)) { const tk = "any " + t; if (!traits.has(tk)) traits.set(tk, k); } }
    }
    if (!byIn) { byIn = new Map(); table.forEach((e, q) => { for (const k of new Set(e.ins)) (byIn.get(k) || byIn.set(k, []).get(k)).push(q); }); }
    const hk = [...have].sort().join("|"); if (!r0s.has(hk)) { if (r0s.size > 8) r0s.clear(); r0s.set(hk, knuth(-1, have)); }
    const R0 = r0s.get(hk);
    // a chain stays near home: the packages of what you have and what you need; a step elsewhere costs more
    const home = new Set(); for (const k of [...have, ...out.keys]) if (k.startsWith("#")) home.add(G.N[+k.slice(1)].p);
    const away = (e) => (home.size && !home.has(G.N[e.i].p) ? 0.6 : 0);
    // wrapping a value in a variant only to ask the enum what it is (`Value::String(t).as_table()`) says nothing
    const hollow = (k, e) => {
      const b = bt.get(k); if (!b || e.how !== "method" || e.ins[0] !== k) return false;
      const made = table[b.q];
      if (made.how === "variant") return true; // `Value::String(t).as_table()`
      return made.names.includes(G.N[e.i].n); // a round trip: `HeadExpectation::new(root, sequence).sequence()`
    };
    const ct = new Map(), bt = new Map(), done = new Set(), heap = mkheap();
    const others = (e, j) => { let s = 0; for (let a = 0; a < e.ins.length; a++) if (a !== j && e.ins[a] !== "nothing") { const c = R0.cost.get(e.ins[a]); if (c === undefined) return Infinity; s += c + 0.3; } return s; };
    // views of the same thing (as_ref, borrow, clone, into) and comparisons are not steps
    const IDENT = /^(as_ref|as_mut|borrow|borrow_mut|deref|deref_mut|clone|to_owned|into|index|index_mut|eq|ne|cmp|partial_cmp|lt|le|gt|ge|hash)$/;
    for (const k of have) { ct.set(k, 0); heap.push(0, k); }
    for (const k of traits.keys()) if (!have.has(k)) { ct.set(k, 0.1); heap.push(0.1, k); }
    while (heap.size()) {
      const [c, k] = heap.pop(); if (done.has(k)) continue; done.add(k);
      if (out.keys.has(k) && !have.has(k)) continue; // an answer: the routes end here
      for (const q of byIn.get(k) || []) {
        const e = table[q];
        if (e.out === "never" || e.out === "?" || e.out === "nothing" || have.has(e.out) || e.ins.includes(e.out) || !usable(e, -1)) continue;
        if (isGround(e.out) && !out.keys.has(e.out)) continue; // plain values are free anyway: a detour
        if (hollow(k, e) || IDENT.test(G.N[e.i].n)) continue;
        const s = c + weight(e) + away(e) + others(e, e.ins.indexOf(k));
        if (s < Infinity && !done.has(e.out) && s < (ct.has(e.out) ? ct.get(e.out) : Infinity)) { ct.set(e.out, s); bt.set(e.out, { q, j: e.ins.indexOf(k) }); heap.push(s, e.out); }
      }
    }
    const cands = [];
    table.forEach((e, q) => {
      if (!out.keys.has(e.out) || have.has(e.out) || e.ins.includes(e.out) || !usable(e, -1)) return;
      let b = Infinity, bj = -1;
      if (IDENT.test(G.N[e.i].n)) return;
      e.ins.forEach((k, j) => { if (!done.has(k) || hollow(k, e)) return; const s = ct.get(k) + weight(e) + away(e) + others(e, j) + (out.maybe && !e.maybe ? 0.2 : 0); if (s < b) { b = s; bj = j; } });
      if (bj >= 0) cands.push([b, q, bj]);
    });
    cands.sort((a, b) => a[0] - b[0] || G.IMP[table[b[1]].i] - G.IMP[table[a[1]].i]);
    // a trait you have stands for the value of yours that implements it
    const leaf = (k, name) => (traits.has(k) && !have.has(k) ? { ground: traits.get(k), name, have: true, as: k } : { ground: k, name, have: have.has(k) });
    const plainKid = (k, name) => {
      if (isGround(k) || have.has(k)) return leaf(k, name);
      const b = R0.best.get(k); if (b === undefined || b < 0) return { ground: k, name, opaque: true };
      return { key: k, name, node: tree(R0, b, new Set([k])) };
    };
    const grow = (q, j, depth) => {
      const e = table[q];
      return { q, spine: j, kids: e.ins.map((k, a) => {
        const name = e.names[a]; if (a !== j) return plainKid(k, name);
        const b = bt.get(k); if (have.has(k) || traits.has(k) || !b || depth > 8) return leaf(k, name);
        return { key: k, name, node: grow(b.q, b.j, depth + 1) };
      }) };
    };
    const outL = [], groups = new Set();
    for (const [c, q, j] of cands) {
      const e = table[q];
      if (!oneCall && (have.has(e.ins[j]) || traits.has(e.ins[j]))) continue; // one call: the shape results already say it
      const node = grow(q, j, 0); const path = pathOf(node);
      const tag = spineNames(node).join("›"); if (groups.has(tag)) continue; groups.add(tag);
      if (path.steps > 3 || c > 3.6 || (outL.length && c > outL[0].cost + 1.5)) continue;
      outL.push({ cost: c, node, path: path.nodes, steps: path.steps, from: path.from });
      if (outL.length >= max) break;
    }
    return outL;
  }
  // how to turn a value of type i into a type j: the best road, one call or a few
  function convert(i, j) {
    if (!table) build();
    const r = chainsFor([{ keys: new Set(["#" + i]) }], { keys: new Set(["#" + j]), maybe: false }, 1, true);
    return r[0] || null;
  }
  // a chain's spine as graph nodes: your value's type, every step, the answer's type
  function pathOf(node) {
    const nodes = []; let t = node, steps = 0, from = null;
    while (t) {
      nodes.unshift(table[t.q].i); steps++;
      const k = t.kids[t.spine];
      if (k && k.node) t = k.node; else { from = k ? k.ground : null; if (from && from.startsWith("#")) nodes.unshift(+from.slice(1)); t = null; }
    }
    const o = table[node.q].out; if (o.startsWith("#")) nodes.push(+o.slice(1));
    return { nodes, steps, from };
  }
  function spineNames(node) {
    const names = []; let t = node;
    while (t) { const e = table[t.q], n = G.N[e.i]; names.unshift(e.how === "variant" ? G.N[n.u].n + "::" + n.n : n.n); const k = t.kids[t.spine]; t = k && k.node ? k.node : null; }
    return names;
  }
  // a chain in one line, for the find box: from Value › as_table › get › as_str → text
  function brief(ch) {
    const names = spineNames(ch.node);
    return `<i class="gtw">from</i> ${words(ch.from || "any", false)} ${names.map((x) => `<i class="gtp">›</i> ${esc(x)}`).join(" ")} <i class="gtp">→</i> ${words(table[ch.node.q].out, false)}`;
  }
  const chainRail = (ch, whose) => rail(ch, table[ch.node.q].out, whose);

  function label(i) { // a callable's shape in words, for search results
    if (!table) build();
    const e = table.find((x) => x.i === i && (x.how === "call" || x.how === "method")); if (!e) return "";
    const a = e.ins.map((k) => words(k, false)).join('<i class="gtp">, </i>');
    return `${a ? `<i class="gtp">(</i>${a}<i class="gtp">)</i> ` : ""}<i class="gtp">→</i> ${e.maybe ? '<i class="gtw">maybe</i> ' : ""}${words(e.out, false)}${e.fails ? ' <i class="gtw">or fails</i>' : ""}`;
  }

  function init(g, p) { G = g; P = p; }
  window.GRAPH_RECIPES = { init, tkey, routes, callRoute, gettingOne, callingIt, isShape, shape, chains, convert, brief, chainRail, label, code, words, traitNames, get table() { if (!table) build(); return table; }, run };
})();
