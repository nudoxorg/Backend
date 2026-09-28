// The hand's connector: the held pieces, arranged by what feeds what, and the verbs between them.
//
// Reads the graph prototype's world and chain finder (graph/recipes.js) through the same api the
// symbol page uses; never edits them. A piece is a noun: a type (its own key), a callable (what
// it takes, what it gives), or context (a package or module, which joins nothing). A join is the
// verb from one piece to the next:
//   direct  the first gives exactly what the next takes (or a trait value the next accepts),
//   as      the first gives "any T" you choose, and the next is a T (from_str → Value),
//   steps   the chain finder's road between two types, at most three steps (Value → as_table → Table).
// No road, no join: the piece stands apart. The arrangement tries every order of at most five
// pieces and keeps the one with the most joins; where the flow allows more than one order, the one
// closest to the order you held them in (fewest swapped pairs); then the cheapest.
(() => {
  "use strict";
  let G = null, R = null, P = null, T = null;
  const byNode = new Map();
  const TYPES = new Set(["struct", "enum", "union", "type", "class", "interface"]);
  // blanket supertraits the prototype's extractor cannot see: DeserializeOwned is Deserialize for every lifetime
  const SUPER = { DeserializeOwned: "Deserialize" };
  const MAX_STEPS = 3;

  function init(g, r, p) {
    G = g; R = r; P = p; T = R.table; byNode.clear();
    T.forEach((e, q) => { (byNode.get(e.i) || byNode.set(e.i, []).get(e.i)).push(q); });
  }

  // every trait a type is, including what a type alias's target is (`type Table = Map<String, Value>`)
  const traitMemo = new Map();
  function traitsOf(j) {
    if (traitMemo.has(j)) return traitMemo.get(j);
    const out = new Set(R.traitNames(G.N[j])); const n = G.N[j];
    if (n.k === "type") {
      const m = /=\s*(.+)$/.exec(n.s || "");
      if (m) { const k = R.tkey(m[1], j, -1).k; if (k.startsWith("#")) for (const t of R.traitNames(G.N[+k.slice(1)])) out.add(t); }
    }
    traitMemo.set(j, out); return out;
  }
  // a type alias stands for its target: `Table` is a `Map<String, Value>`
  const aliasMemo = new Map();
  function target(k) {
    if (!k || !k.startsWith("#")) return k;
    if (aliasMemo.has(k)) return aliasMemo.get(k);
    const j = +k.slice(1), n = G.N[j]; let out = k;
    if (n.k === "type") { const m = /=\s*(.+)$/.exec(n.s || ""); if (m) { const t = R.tkey(m[1], j, -1).k; if (t.startsWith("#")) out = t; } }
    aliasMemo.set(k, out); return out;
  }
  const isA = (j, t) => { const ts = traitsOf(j); return ts.has(t) || (SUPER[t] && ts.has(SUPER[t])); };
  const traitOf = (k) => (k && k.startsWith("any ") ? k.slice(4) : null);

  function piece(i) {
    const n = G.N[i];
    if (TYPES.has(n.k)) return { i, role: "type", ins: ["#" + i], out: "#" + i };
    if (n.k === "trait") return { i, role: "trait", ins: [], out: null }; // a contract, not a value: it stands apart
    const q = (byNode.get(i) || []).find((q) => ["call", "method", "variant"].includes(T[q].how));
    if (q !== undefined) { const e = T[q]; return { i, role: "call", q, ins: e.ins.filter((k) => k !== "nothing"), out: e.out, fails: e.fails, maybe: e.maybe }; }
    if (n.k === "field" && n.u >= 0) return { i, role: "call", ins: ["#" + n.u], out: R.tkey(n.ty, i, n.u).k };
    return { i, role: "context", ins: [], out: null };
  }

  const cmemo = new Map();
  function convert(a, b) {
    const key = a + ">" + b;
    if (!cmemo.has(key)) { const c = R.convert(a, b); cmemo.set(key, c && c.steps <= MAX_STEPS ? c : null); }
    return cmemo.get(key);
  }
  // what the first gives (k) into what the next takes (t)
  const GROUND = new Set(["text", "path", "number", "bool", "char", "bytes", "nothing", "any"]);
  function link(k, t) {
    if (GROUND.has(k) || GROUND.has(t)) return null; // plain values connect everything, so they connect nothing
    if (k === t || target(k) === target(t)) return { cost: 0.1, how: "direct" };
    const tt = traitOf(t);
    if (tt && k.startsWith("#") && isA(+k.slice(1), tt)) return { cost: 0.2, how: "direct" };
    const kt = traitOf(k);
    if (kt && t.startsWith("#") && isA(+t.slice(1), kt)) return { cost: 0.25, how: "as" };
    if (k.startsWith("#") && t.startsWith("#")) { const ch = convert(+k.slice(1), +t.slice(1)); if (ch) return { cost: 1 + ch.cost, how: "steps", chain: ch }; }
    return null;
  }
  const jmemo = new Map();
  function join(a, b) {
    const key = a.i + ">" + b.i;
    if (jmemo.has(key)) return jmemo.get(key);
    let best = null;
    if (a.i !== b.i && a.out && a.out !== "nothing" && b.ins.length) {
      for (const t of new Set(b.ins)) { const l = link(a.out, t); if (l && (!best || l.cost < best.cost)) best = { ...l, into: t }; }
    }
    jmemo.set(key, best); return best;
  }

  function perms(xs) {
    if (xs.length <= 1) return [xs.slice()];
    const out = [];
    xs.forEach((x, k) => { for (const p of perms(xs.filter((_, z) => z !== k))) out.push([x, ...p]); });
    return out;
  }
  // held: node ids in the order they were held. Returns runs (connected, longest first) and apart pieces.
  function arrange(held) {
    const ps = held.map(piece);
    const live = ps.filter((p) => p.role === "type" || p.role === "call");
    const rest = ps.filter((p) => !(p.role === "type" || p.role === "call"));
    const at = new Map(held.map((i, k) => [i, k]));
    let best = null;
    for (const order of perms(live)) {
      let joins = 0, cost = 0, swaps = 0;
      for (let k = 0; k + 1 < order.length; k++) { const j = join(order[k], order[k + 1]); if (j) { joins++; cost += j.cost; } }
      for (let a = 0; a < order.length; a++) for (let b = a + 1; b < order.length; b++) if (at.get(order[a].i) > at.get(order[b].i)) swaps++;
      const s = [joins, -swaps, -cost];
      const better = !best || s[0] > best.s[0] || (s[0] === best.s[0] && (s[1] > best.s[1] || (s[1] === best.s[1] && s[2] > best.s[2] + 1e-9)));
      if (better) best = { s, order };
    }
    const runs = []; let cur = [];
    for (const p of best ? best.order : []) {
      if (!cur.length) { cur.push({ p, j: null }); continue; }
      const j = join(cur[cur.length - 1].p, p);
      if (j) cur.push({ p, j }); else { runs.push(cur); cur = [{ p, j: null }]; }
    }
    if (cur.length) runs.push(cur);
    const joined = runs.filter((r) => r.length > 1).sort((a, b) => b.length - a.length || at.get(a[0].p.i) - at.get(b[0].p.i));
    const apart = runs.filter((r) => r.length === 1).map((r) => r[0].p).concat(rest).sort((a, b) => at.get(a.i) - at.get(b.i));
    return { runs: joined, apart };
  }

  // ------------------------------------------------------------------ words
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
  const crate = (j) => G.PK[G.N[j].p].name.replace(/^backend-/, "").replace(/-/g, "_"); // how the board names it
  const ident = (j) => G.PK[G.N[j].p].name.replace(/-/g, "_"); // how code spells it
  // the words a key reads as: "text", "Table", "JSON Value" when two held types share a name
  function keyWords(k, twins) {
    if (!k) return "";
    if (k.startsWith("#")) { const j = +k.slice(1); return twins && twins.has(G.N[j].n) ? `${crate(j)} ${G.N[j].n}` : G.N[j].n; }
    if (k.startsWith("any ")) return k.slice(4);
    return k;
  }
  // the verb names of a steps join: Value → as_table → Table reads "as_table"
  function verbs(j) {
    if (!j || j.how !== "steps") return [];
    const names = []; let t = j.chain.node;
    while (t) { const e = T[t.q], n = G.N[e.i]; names.unshift(e.how === "variant" ? `${G.N[n.u].n}::${n.n}` : n.n); const k = t.kids[t.spine]; t = k && k.node ? k.node : null; }
    return names;
  }
  // what a run starts from and ends with, as keys
  function ends(run) {
    const first = run[0].p, last = run[run.length - 1].p;
    const from = first.role === "call" ? (first.ins.find((k) => !k.startsWith("#")) || first.ins[0] || null) : first.out;
    return { from, to: last.out };
  }
  // the one sentence: "text to Table, in two steps"; a round trip names what it passes through:
  // "text to Table and back, in three steps"
  const NUM = ["no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten"];
  function sentence(run, twins) {
    const e = ends(run), n = steps(run);
    const tail = `, <i>in ${NUM[n] || n} step${n === 1 ? "" : "s"}</i>`;
    const w = (k) => `<b>${esc(keyWords(k, twins))}</b>`;
    if (e.from === e.to && !e.from.startsWith("#")) {
      // the same plain value at both ends: one package reading and writing it is a round trip
      const first = run[0].p, last = run[run.length - 1].p;
      const via = run.map((x) => x.p).filter((p) => p.role === "type").pop();
      if (via && G.N[first.i].p === G.N[last.i].p) return `${w(e.from)} <i>to</i> ${w(via.out)} <i>and back</i>${tail}`;
      return `${esc(crate(first.i))} ${w(e.from)} <i>to</i> ${esc(crate(last.i))} ${w(e.to)}${tail}`;
    }
    return `${w(e.from)} <i>to</i> ${w(e.to)}${tail}`;
  }
  function steps(run) {
    let n = 0;
    run.forEach((x, k) => { if (x.p.role === "call") n++; if (x.j && x.j.how === "steps") n += x.j.chain.steps; });
    return n;
  }

  // ------------------------------------------------------------------ code: the run as the calls you would write
  const snake = (s) => s.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();
  const GROUNDVAR = { text: "text", path: "path", number: "n", bytes: "bytes", bool: "yes", char: "c" };
  function code(run, twins) {
    const lines = []; let v = null;
    const qualT = (j) => (twins && twins.has(G.N[j].n) ? `${ident(j)}::${G.N[j].n}` : G.N[j].n);
    const nameFor = (k) => (k && k.startsWith("#") ? snake(G.N[+k.slice(1)].n) : k && GROUNDVAR[k] ? GROUNDVAR[k] : "value");
    const last = run.length - 1;
    run.forEach((x, k) => {
      const p = x.p, n = G.N[p.i];
      if (x.j && x.j.how === "steps") {
        let src = R.code(x.j.chain.node);
        // two held types share a name (toml's Value, serde_json's): spell the chain's own with its crate
        if (twins) for (const j of x.j.chain.path) { const nm = G.N[j].n; if (twins.has(nm) && TYPES.has(G.N[j].k)) src = src.replace(new RegExp(`(^|[^:\\w])${nm}::`, "g"), `$1${ident(j)}::${nm}::`); }
        const expr = src.split("\n");
        const root = T[x.j.chain.node.q];
        // a step that may give nothing asks with ? unless the recipe ends there
        const ask = root.maybe && !root.fails && (k < last || p.role === "call") && !/\?$/.test(expr[expr.length - 1]) ? "?" : "";
        expr.slice(0, -1).forEach((l) => lines.push(l));
        const name = nameFor(x.j.into);
        lines.push(`let ${name} = ${expr[expr.length - 1]}${ask};`); v = name;
      }
      if (p.role === "call") {
        const params = P.params(n); const e = T[p.q];
        const into = x.j ? x.j.into : null;
        // the value arrives as the receiver when the join goes into it: `table.get(key)`
        const off = n.recv ? 1 : 0;
        // the value arrives where its join went: the receiver (`table.get(key)`), else the argument whose
        // type it filled (`relation_label(kind, relation_direction)`), else the first argument
        const recvGets = !!(n.recv && v && x.j && into === e.ins[0]);
        let at = -1;
        if (!recvGets && v && x.j) { at = params.findIndex((_, a) => e.ins[off + a] === into); if (at < 0) at = 0; }
        const args = params.map((pa, a) => {
          const key = e.ins[off + a];
          if (a === at) return (/^&/.test(pa.ty) ? "&" : "") + v;
          return GROUNDVAR[key] || pa.name.replace(/^_+/, "") || "value";
        });
        const call = n.recv && v ? `${v}.${n.n}(${args.join(", ")})` : `${n.u >= 0 ? G.N[n.u].n + "::" : ident(p.i) + "::"}${n.n}(${args.join(", ")})`;
        const next = run[k + 1];
        let name = nameFor(p.out), ann = "";
        if (next && next.j && next.j.how === "as") { name = nameFor(next.p.out); ann = `: ${qualT(next.p.i)}`; }
        const ask = p.fails || (p.maybe && k < last) ? "?" : "";
        lines.push(`let ${name}${ann} = ${call}${ask};`); v = name;
      } else if (k === 0) { v = nameFor(p.out); }
    });
    return lines.join("\n");
  }

  window.HAND_CORE = { init, piece, join, arrange, verbs, ends, steps, sentence, keyWords, code, traitsOf, esc, crate, ident };
})();
