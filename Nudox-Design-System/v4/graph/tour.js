// Start here: a package's reading path, computed. Not an index of everything (docs.rs lists items
// alphabetically); the five or six things a newcomer should read, in the order they meet them:
//
//   the door        the call you make first (a free function most used from outside, else a maker)
//   what you hold   the type everything turns on (importance + use from other packages)
//   inside it       the types its parts are made of (at most two)
//   what it promises  the trait others implement or take
//   when it fails   the error the door or the heart gives
//
// Each stop carries its role in words and its lede. The graph flies the road stop to stop (T).
(() => {
  "use strict";
  let G = null;
  const isTest = (f) => /(^|\/)(tests?|benches|examples)\//.test(f || "") || /_tests?\.rs$|\/tests\.rs$/.test(f || "");
  const TYPE = new Set(["struct", "enum", "union", "type"]);
  const cache = new Map();

  // distinct top-level items outside the package that refer to i (and how many of them are yours)
  function useOf(i, pk) {
    const I = G.isItem(i) ? G.IIN : G.IN; const seen = new Set(); let yours = 0;
    for (let e = I.off[i]; e < I.off[i + 1]; e++) {
      const t = G.topOf[I.dst[e]]; if (t < 0 || G.N[t].p === pk || seen.has(t)) continue;
      seen.add(t); if (G.yours(t)) yours++;
    }
    return { n: seen.size, yours };
  }
  // the package's own types a type is made of: its fields' and variants' types
  function partsOf(i) {
    const R = window.GRAPH_RECIPES; const out = new Map();
    for (const j of G.kids[i] || []) {
      const n = G.N[j]; if ((n.k !== "field" && n.k !== "variant") || !n.ty) continue;
      const k = R.tkey(n.ty, j, i).k;
      for (const m of k.matchAll(/#(\d+)/g)) { const t = +m[1]; if (t !== i) out.set(t, (out.get(t) || 0) + 1); }
    }
    return out;
  }
  const retMentions = (j, t) => { const n = G.N[j]; return !!n.ret && new RegExp(`\\b${G.N[t].n}\\b`).test(n.ret); };

  function of(pk) {
    if (cache.has(pk)) return cache.get(pk);
    const N = G.N; const items = [];
    for (let i = 0; i < N.length; i++) {
      const n = N[i]; if (n.p !== pk || n.u >= 0 || n.orphan || isTest(n.f)) continue;
      if (n.v !== "pub" && !n.via) continue;
      items.push(i);
    }
    const use = new Map(items.map((i) => [i, useOf(i, pk)]));
    const u = (i) => use.get(i) || use.set(i, useOf(i, pk)).get(i);
    const score = (i) => G.IMP[i] + 0.22 * Math.log1p(u(i).n) + (u(i).yours ? 0.15 : 0);
    const best = (list) => list.reduce((a, b) => (a < 0 || score(b) > score(a) ? b : a), -1);
    const types = items.filter((i) => TYPE.has(N[i].k));
    const errors = types.filter((i) => /Error$/.test(N[i].n));
    let heart = best(types.filter((i) => !/Error$/.test(N[i].n)));
    // the door: a free function used from outside; failing that, the heart's own maker
    // prefer a door that hands you the heart
    const doors = items.filter((i) => N[i].k === "function" && u(i).n > 0);
    let door = doors.reduce((a, b) => (a < 0 || score(b) + (heart >= 0 && retMentions(b, heart) ? 0.3 : 0) > score(a) + (heart >= 0 && retMentions(a, heart) ? 0.3 : 0) ? b : a), -1);
    if (door < 0 && heart >= 0) {
      const makers = (G.kids[heart] || []).filter((j) => (N[j].k === "method" || N[j].k === "function") && !N[j].recv && N[j].v === "pub" && retMentions(j, heart));
      door = makers.sort((a, b) => useOf(b, pk).n - useOf(a, pk).n || G.IMP[b] - G.IMP[a])[0] ?? -1;
    }
    const inside = heart >= 0 ? [...partsOf(heart)].filter(([t]) => N[t].p === pk && N[t].u < 0 && (N[t].v === "pub" || N[t].via) && TYPE.has(N[t].k) && !/Error$/.test(N[t].n)).sort((a, b) => score(b[0]) - score(a[0])).slice(0, 2).map(([t]) => t) : [];
    const doers = (t) => (t >= 0 ? G.inEdges(t, G.B.impl | G.B.derives).length : 0);
    const traits = items.filter((i) => N[i].k === "trait").sort((a, b) => doers(b) + u(b).n - doers(a) - u(a).n);
    const contract = traits.length ? traits[0] : -1;
    const fails = errors.find((e) => (door >= 0 && retMentions(door, e)) || (heart >= 0 && retMentions(heart, e))) ?? best(errors);
    const stops = [];
    const add = (i, role, why) => { if (i >= 0 && !stops.some((s) => s.i === i)) stops.push({ i, role, why }); };
    // a package whose idea is a trait (serde) starts with its traits, and its types only if they carry weight
    const weight = (t) => u(t).n + doers(t);
    if (contract >= 0 && (heart < 0 || weight(contract) >= 2 * u(heart).n + 5)) {
      add(contract, "the idea", `${doers(contract)} types do it`);
      const second = traits[1]; if (second !== undefined && weight(second) * 3 >= weight(contract)) add(second, "and the other half", `${doers(second)} types do it`);
      if (heart >= 0 && u(heart).n * 4 < weight(contract)) heart = -1;
    }
    add(door, "start here", N[door] && N[door].k === "function" ? "the call you make first" : "how you get one");
    add(heart, "what you hold", "everything turns on it");
    if (heart >= 0) for (const t of inside) add(t, "inside it", `part of ${N[heart].n}`);
    add(contract, "what it promises", G.inEdges(contract, G.B.impl | G.B.derives).length ? `${G.inEdges(contract, G.B.impl | G.B.derives).length} types do it` : "others implement it");
    add(fails, "when it fails", "the error you handle");
    const out = { pk, stops: stops.slice(0, 6), items: items.length, use };
    cache.set(pk, out);
    return out;
  }

  const plain = (d) => (d || "").replace(/\[([^\]]+)\]\([^)]*\)/g, "$1").replace(/\[`?([^\]`]+)`?\]/g, "$1").replace(/\*\*|__|`/g, "");
  function init(api) { G = api; }
  window.GRAPH_TOUR = { init, of, plain };
})();
