// The real world behind the moments board: this repository's dependency graph (Cargo.lock), each
// package's public surface (a source scan), newer releases (the sparse index cache) and how much of
// each direct dependency your code names. Built by extract_world.py.

const load = async (f) => (await fetch(f, { cache: "reload" })).json();

export async function world() {
  const W = await load("data/world.json");
  const all = Object.values(W.packages);
  const byId = new Map(all.map((p) => [p.id, p]));
  const byName = new Map();
  for (const p of all) (byName.get(p.name) || byName.set(p.name, []).get(p.name)).push(p);
  for (const p of all) {
    p.depsP = p.deps.map((d) => byId.get(d)).filter(Boolean);
    p.dependentsP = p.dependents.map((d) => byId.get(d)).filter(Boolean);
    p.used = new Map(Object.entries((p.uses && p.uses.items) || {}));
    p.sites = (p.uses && p.uses.sites) || 0;
    p.dup = byName.get(p.name).length > 1;
    let k = 0; for (const m of p.modules) for (const it of m.items) it.k = k++;
  }
  const yours = all.filter((p) => p.depth === 0).sort((a, b) => b.items - a.items);
  const direct = all.filter((p) => p.depth === 1).sort((a, b) => b.sites - a.sites || b.items - a.items);
  const beneath = all.filter((p) => p.depth >= 2);
  // Beneath, grouped by what brought it: one direct dependency, or several (shared).
  const clusters = new Map();
  for (const p of beneath) {
    const key = p.via.length === 1 ? p.via[0] : "shared";
    (clusters.get(key) || clusters.set(key, []).get(key)).push(p);
  }
  const groups = [...clusters.entries()].map(([key, ps]) => ({ key, via: byId.get(key) || null, pkgs: ps.sort((a, b) => b.items - a.items) }))
    .sort((a, b) => (a.key === "shared") - (b.key === "shared") || b.pkgs.length - a.pkgs.length);
  return { W, all, byId, byName, yours, direct, beneath, groups };
}

// Everything above p (what depends on it, transitively) and below it (what it rests on).
export function above(p) { const s = new Set(); const go = (q) => { for (const d of q.dependentsP) if (!s.has(d)) { s.add(d); go(d); } }; go(p); return s; }
export function below(p) { const s = new Set(); const go = (q) => { for (const d of q.depsP) if (!s.has(d)) { s.add(d); go(d); } }; go(p); return s; }

// One shortest chain from any of your crates down to p: the answer to "why is this here".
export function why(p) {
  const prev = new Map([[p, null]]); let frontier = [p];
  while (frontier.length) {
    const next = [];
    for (const q of frontier) {
      if (q.depth === 0) { const chain = []; for (let x = q; x; x = prev.get(x)) chain.push(x); return chain; }
      for (const d of q.dependentsP) if (!prev.has(d)) { prev.set(d, q); next.push(d); }
    }
    frontier = next;
  }
  return [p];
}

export const fmt = (n) => n.toLocaleString("en-US");
export const plural = (n, one, many = one + "s") => `${fmt(n)} ${n === 1 ? one : many}`;
