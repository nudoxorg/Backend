// How it fails: which kinds of an error a callable can give, and which callables give each kind.
//
// docs.rs says `Result<Workspace, RuntimeError>` and stops. The index knows every mention of a
// variant in a body. A mention by a callable that *returns* the error builds that kind (a maker).
// A mention by the error's own methods that take `&self` (fmt, source, is_retryable), or by a
// callable returning something else (`Fault::from_client_error`), only tells kinds apart: not a
// failure. Failure spreads through calls: a callable returning E that calls g, which returns E too,
// can give whatever g gives (`?` carries it up); the page says which call it comes through.
(() => {
  "use strict";
  let G = null, P = null;
  const errOf = new Map(); let aliases = null; // package -> its `type Result<T> = …`
  const plainName = (s) => s.trim().replace(/^&\s*/, "").replace(/<.*$/, "").split("::").pop();

  // the E in a callable's Result<_, E> (or its package's `type Result<T> = Result<T, E>`), as a node
  function errorOf(j) {
    if (errOf.has(j)) return errOf.get(j);
    let e = -1; const n = G.N[j];
    const m = n.ret && /^(?:[\w:]+::)?Result\s*<(.*)>$/.exec(n.ret.trim());
    if (m) {
      const parts = P.splitTop(m[1]);
      if (parts.length >= 2) e = P.resolveName(plainName(parts[1]), j);
      else {
        if (!aliases) { aliases = new Map(); G.N.forEach((x, a) => { if (x.k === "type" && x.n === "Result" && x.u < 0 && !aliases.has(x.p)) aliases.set(x.p, a); }); }
        const alias = aliases.has(n.p) ? aliases.get(n.p) : -1;
        const am = alias >= 0 && G.N[alias].ty && /Result\s*<(.*)>$/.exec(G.N[alias].ty);
        if (am) { const ap = P.splitTop(am[1]); if (ap.length >= 2) e = P.resolveName(plainName(ap[1]), alias); }
      }
      if (e >= 0 && !["enum", "struct", "type", "union"].includes(G.N[e].k)) e = -1;
    }
    errOf.set(j, e); return e;
  }
  const kinds = (E) => (G.kids[E] || []).filter((v) => G.N[v].k === "variant");
  const callable = (j) => G.N[j].k === "function" || G.N[j].k === "method";
  // a maker of E: returns E, and is not one of E's own methods that read it
  const maker = (j, E) => callable(j) && errorOf(j) === E && !(G.N[j].u === E && G.N[j].recv);

  const direct = new Map(); // E -> Map(callable -> Set(variant))
  function directOf(E) {
    if (direct.has(E)) return direct.get(E);
    const m = new Map();
    for (const v of kinds(E)) for (const j of G.inEdges(v, G.B.uses | G.B.calls | G.B.type | G.B.has)) {
      if (!maker(j, E)) continue; (m.get(j) || m.set(j, new Set()).get(j)).add(v);
    }
    direct.set(E, m); return m;
  }

  // what j can give: [{ v, via }] (via = the call it comes through, -1 when j builds it itself)
  const memo = new Map();
  function failsOf(j) {
    const E = errorOf(j); if (E < 0) return null;
    const key = j; if (memo.has(key)) return memo.get(key);
    const out = new Map(); memo.set(key, { E, kinds: out, all: kinds(E).length }); // cycles see what is known so far
    for (const v of directOf(E).get(j) || []) out.set(v, -1);
    for (const g of G.outEdges(j, G.B.calls)) {
      if (g === j || !callable(g) || errorOf(g) !== E) continue;
      const r = failsOf(g); if (!r) continue;
      for (const v of r.kinds.keys()) if (!out.has(v)) out.set(v, g);
    }
    return memo.get(key);
  }
  // per kind of E: who builds it (directly), yours first, then importance
  function makersOf(E) {
    const per = new Map(kinds(E).map((v) => [v, []]));
    for (const [j, vs] of directOf(E)) for (const v of vs) per.get(v).push(j);
    for (const list of per.values()) list.sort((a, b) => (G.yours(b) - G.yours(a)) || G.IMP[b] - G.IMP[a]);
    return per;
  }
  // how many callables in this world can fail with E, and which of its kinds your code tells apart
  function reachOf(E) {
    let can = 0; for (let j = 0; j < G.N.length; j++) if (callable(j) && errorOf(j) === E) can++;
    const told = new Set();
    for (const v of kinds(E)) for (const j of G.inEdges(v, G.B.uses | G.B.calls | G.B.type)) if (G.yours(j) && !maker(j, E)) told.add(v);
    return { can, told };
  }

  function init(api, types) { G = api; P = types; }
  window.GRAPH_FAILS = { init, errorOf, failsOf, makersOf, reachOf, kinds };
})();
