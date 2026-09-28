// extract2.mjs — page2's data: five symbol pages, everything docs.rs carries plus what only we know.
//
//   node page2/extract2.mjs            (needs the v4 server on :47811 for the harvest step)
//
// Sources, all read-only:
//   graph/world.js      the workspace + dependencies (relations, callers, yours)
//   graph/registry/…    crate sources: declarations with bounds, docs, impls, cfgs, deprecations
//   graph/releases.json release history + API diffs (toml, smallvec)
//   rust-src (nix)      core/alloc slice methods, for "acts like a slice"
//   ../Cargo.toml       which features your project turns on
//   Graph.html          the graph prototype's own page sections (anatomy, can, Getting one, …),
//                       harvested with headless Chrome so page2 shows exactly what exists today.
// Writes page2/page2.json.
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import * as W from "./world.mjs";
import { readCrate, splitTop, cfgOf, deprecationOf, stabilityOf, norm } from "./rustsrc.mjs";
import { sourceFacts } from "./source.mjs";

const HERE = path.dirname(new URL(import.meta.url).pathname);
const V4 = W.V4; const REG = path.join(V4, "graph/registry"); const REPO = path.resolve(V4, "../..");
const RELEASES = JSON.parse(fs.readFileSync(path.join(V4, "graph/releases.json"), "utf8"));
const CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

export const ITEMS = [
  { slug: "value", key: "toml::Value", pkg: "toml", mod: "value", name: "Value", crate: "toml-0.8.23", release: "toml", aliases: ["toml::Value"], rpath: "toml::value::Value" },
  { slug: "from_str", key: "serde_json::from_str", pkg: "serde_json", mod: "de", name: "from_str", crate: "serde_json-1.0.151", aliases: ["serde_json::from_str"] },
  { slug: "serialize", key: "serde::Serialize", pkg: "serde_core", mod: "ser", name: "Serialize", crate: "serde_core-1.0.229", aliases: ["serde::Serialize", "serde::ser::Serialize"] },
  { slug: "smallvec", key: "smallvec::SmallVec", pkg: "smallvec", mod: "", name: "SmallVec", crate: "smallvec-1.16.0", release: "smallvec", rpath: "smallvec::SmallVec" },
  { slug: "error", key: "serde_json::Error", pkg: "serde_json", mod: "error", name: "Error", crate: "serde_json-1.0.151", aliases: ["serde_json::Error"] },
];

// ---------------------------------------------------------------- small utils
const readLines = (() => { const c = new Map(); return (f) => { if (c.has(f)) return c.get(f); let t = null; try { t = fs.readFileSync(f, "utf8").split("\n"); } catch { t = null; } c.set(f, t); return t; }; })();
const fileOf = (j) => { const t = W.N[W.topOf[j]]; const f = W.N[j].f || t.f; if (!f) return null; return W.PK[W.N[j].p].external ? path.join(REG, f) : path.join(REPO, f); };
const band = (j, i) => (W.yours(j) ? "yours" : W.N[j].p === W.N[i].p ? "here" : "elsewhere");
const peeks = new Map();
function peek(j) {
  if (j < 0 || peeks.has(j)) return;
  const n = W.N[j];
  peeks.set(j, { k: n.k, n: W.nameOf(j), q: W.qual(j), s: n.s || "", d: n.d || "", y: W.yours(j) ? 1 : 0, pk: W.pkgName(n.p), f: n.f ? `${n.f.split("/").pop()}:${n.l || ""}` : "" });
}
const S = (n, one, many) => `${n} ${n === 1 ? one : many || one + "s"}`;
const listWords = (xs, max = 3, more = "more") => { if (xs.length <= max) return xs.length <= 1 ? xs.join("") : xs.slice(0, -1).join(", ") + " and " + xs[xs.length - 1]; return xs.slice(0, max).join(", ") + ` and ${xs.length - max} ${more}`; };

// ---------------------------------------------------------------- the joint: where exactly each relation happens
function lineOf(j, re) { // the first body line of j matching re, trimmed, with its number
  const L = readLines(fileOf(j)); const n = W.N[j]; if (!L || !n.l) return null;
  const a = n.l - 1, b = Math.min(L.length, n.e || n.l + 40);
  let body = a; while (body < b - 1 && !/\{\s*$/.test(L[body])) body++;
  for (let k = body + 1; k < b; k++) if (re.test(L[k]) && !/^\s*(\/\/|#\[)/.test(L[k])) return { line: k + 1, text: L[k].trim() };
  return null;
}
function jointOf(word, j, i, methods) {
  const n = W.N[j]; const me = W.N[i].n;
  const lit = (s) => ({ text: s, hit: me });
  switch (word) {
    case "held by": return lit(`${W.nameOf(j)}: ${n.ty || ""}`);
    case "done by": {
      if ((n.derives || []).includes(me)) return { text: `#[derive(${n.derives.join(", ")})]`, hit: me, line: n.l };
      const im = (n.impls || []).find((x) => x.trait === i); return { text: `impl ${me} for ${n.n}`, hit: me, line: im ? im.line : n.l };
    }
    case "called on by": case "called by": {
      const names = word === "called by" ? [me] : methods;
      const re = new RegExp(`(\\.|::)(${names.map((m) => m.replace(/[^\w]/g, "")).join("|")})\\b`);
      const hit = lineOf(j, re); if (hit) { const m = re.exec(hit.text); return { text: hit.text, hit: m ? m[2] : me, line: hit.line }; }
      return lit(n.s || W.nameOf(j));
    }
    default: return lit(n.s || W.nameOf(j));
  }
}

// ---------------------------------------------------------------- relations, normalised to one verb table
// The verbs are the same for every language (VERBS.md); the world's edges map onto them here.
function boundUsers(i) { // callables whose generics or where-clause ask for this trait
  const me = W.N[i].n; const out = [];
  const re = new RegExp(`(^|[:+,<\\s])((\\w+::)*)${me}(?![\\w])`);
  for (let j = 0; j < W.NN; j++) { const n = W.N[j]; if (n.k !== "function" && n.k !== "method") continue; if (n.u >= 0 && n.u === i) continue; const s = `${n.gen || ""} , ${n.wh || ""}`; if (!/:/.test(s)) continue; const bounds = splitTop(s).flatMap((p) => { const c = p.indexOf(":"); return c < 0 ? [] : [p.slice(c + 1)]; }).join(" + "); if (re.test(" " + bounds)) out.push(j); }
  return out;
}
function relations(i, opts) {
  const n = W.N[i]; const g = W.relationsOf(i); const by = (w) => (g.find((x) => x.word === w) || { entries: [] }).entries.filter((e) => e.j >= 0).map((e) => e.j);
  const methods = (W.kids[i] || []).filter((j) => W.N[j].k === "method").map((j) => W.N[j].n);
  const typeLike = ["struct", "enum", "union", "type", "trait"].includes(n.k);
  const rows = typeLike ? [
    { verb: "comes from", dir: "in", ids: by("made by") },
    { verb: "done by", dir: "in", ids: by("implemented by") },
    { verb: "taken by", dir: "out", ids: by("taken by") },
    { verb: "held by", dir: "out", ids: by("held by") },
    { verb: "called on by", dir: "out", ids: by("calls it") },
    { verb: "asked for by", dir: "out", ids: n.k === "trait" ? boundUsers(i) : [] },
    { verb: "used by", dir: "out", ids: by("used by") },
  ] : [
    { verb: "called by", dir: "in", ids: by("called from") },
    { verb: "calls", dir: "out", ids: by("calls") },
    { verb: "used by", dir: "out", ids: by("used by") },
  ];
  const out = [];
  for (const r of rows) {
    if (!r.ids.length) continue;
    const ids = [...new Set(r.ids)].sort((a, b) => (W.yours(b) - W.yours(a)) || ((W.N[b].p === n.p) - (W.N[a].p === n.p)) || W.IMP[b] - W.IMP[a]);
    const bands = ["yours", "here", "elsewhere"].map((b) => {
      const inBand = ids.filter((j) => band(j, i) === b); if (!inBand.length) return null;
      const pk = new Map(); for (const j of inBand) { const p = W.pkgName(W.N[j].p); (pk.get(p) || pk.set(p, []).get(p)).push(j); }
      return { band: b, n: inBand.length, pkgs: [...pk].sort((a, c) => c[1].length - a[1].length).map(([p, js]) => ({ pkg: p, n: js.length, ids: js })) };
    }).filter(Boolean);
    const shown = ids.slice(0, opts.maxJoints);
    for (const j of ids) peek(j);
    const joints = {}; for (const j of shown) joints[j] = jointOf(r.verb, j, i, methods);
    const derived = r.verb === "done by" ? ids.filter((j) => (W.N[j].derives || []).includes(n.n)).length : undefined;
    out.push({ verb: r.verb, dir: r.dir, n: ids.length, yours: ids.filter((j) => W.yours(j)).length, ids, bands, joints, derived });
  }
  return out;
}

// ---------------------------------------------------------------- harvest: the graph prototype's own sections
function harvest(it) {
  const q = `${it.pkg}::${it.mod ? it.mod + "::" : ""}${it.name}`;
  const cache = path.join(HERE, ".harvest", it.slug + ".html");
  let html;
  if (fs.existsSync(cache) && !process.env.REHARVEST) html = fs.readFileSync(cache, "utf8");
  else {
    html = execFileSync(CHROME, ["--headless=new", "--disable-gpu", "--hide-scrollbars", "--window-size=1440,3000", "--virtual-time-budget=9000", "--dump-dom", `http://127.0.0.1:47811/v4/Graph.html?still=1&page=${encodeURIComponent(q)}`], { encoding: "utf8", maxBuffer: 64 << 20, stdio: ["ignore", "pipe", "ignore"] });
    fs.mkdirSync(path.dirname(cache), { recursive: true }); fs.writeFileSync(cache, html);
  }
  const at = html.indexOf('<div class="cfol pfol">'); if (at < 0) return {};
  const kidsOf = (s, from) => { // top-level child elements of the element whose content starts at `from`
    const out = []; let k = from; let depth = 0, st = -1;
    const re = /<(\/?)([a-zA-Z0-9]+)([^>]*?)(\/?)>/g; re.lastIndex = k;
    const VOID = new Set(["br", "img", "input", "hr", "meta", "link", "wbr"]);
    let m; while ((m = re.exec(s))) {
      const close = m[1] === "/", tag = m[2].toLowerCase(), self = m[4] === "/" || VOID.has(tag) && !close;
      if (self) { if (depth === 0) out.push(s.slice(m.index, re.lastIndex)); continue; }
      if (!close) { if (depth === 0) st = m.index; depth++; }
      else { depth--; if (depth === 0 && st >= 0) { out.push(s.slice(st, re.lastIndex)); st = -1; } if (depth < 0) break; }
    }
    return out;
  };
  const parts = kidsOf(html, at + '<div class="cfol pfol">'.length);
  const pick = (test) => parts.find(test) || "";
  const sec = {
    anatomy: pick((p) => /^<section class="anat /.test(p)),
    caps: pick((p) => /^<section class="pcaps/.test(p)),
    fails: pick((p) => /^<section class="csec gfails/.test(p)),
    getting: pick((p) => /^<section class="csec gget"/.test(p)),
    does: pick((p) => /^<section class="csec"><h2>Does/.test(p)),
    cousins: pick((p) => /^<section class="csec gget gcous/.test(p)),
    inuse: pick((p) => /^<section class="csec inuse/.test(p)),
  };
  for (const s of Object.values(sec)) for (const m of s.matchAll(/data-i="(\d+)"/g)) peek(+m[1]);
  return sec;
}

// ---------------------------------------------------------------- your project's features
function projectFeatures() {
  const toml = fs.readFileSync(path.join(REPO, "Cargo.toml"), "utf8");
  const want = {};
  for (const line of toml.split("\n")) {
    const m = /^(\w[\w-]*)\s*=\s*(.*)$/.exec(line.trim()); if (!m) continue;
    const name = m[1], v = m[2];
    if (!["toml", "serde", "serde_json", "smallvec", "serde_core"].includes(name)) continue;
    const feats = (/features\s*=\s*\[([^\]]*)\]/.exec(v) || [, ""])[1].split(",").map((s) => s.trim().replace(/"/g, "")).filter(Boolean);
    const noDefault = /default-features\s*=\s*false/.test(v);
    const ver = (/version\s*=\s*"([^"]+)"/.exec(v) || /^"([^"]+)"/.exec(v) || [, ""])[1];
    want[name] = { ver, feats, noDefault, line: v };
  }
  return want;
}
function crateFeatures(crateDir) {
  const t = fs.readFileSync(path.join(REG, crateDir, "Cargo.toml"), "utf8");
  const sec = (/\[features\]\n([\s\S]*?)\n\[/.exec(t + "\n[") || [, ""])[1];
  const out = {}; const re = /^(\w[\w-]*)\s*=\s*\[([\s\S]*?)\]/gm; let m;
  while ((m = re.exec(sec))) out[m[1]] = m[2].split(",").map((s) => s.trim().replace(/"/g, "")).filter(Boolean);
  const opt = new Set(); const dre = /\[dependencies\.([\w-]+)\]\n([\s\S]*?)(?=\n\[|$)/g; while ((m = dre.exec(t))) if (/optional\s*=\s*true/.test(m[2])) opt.add(m[1]);
  return { features: out, optional: [...opt] };
}
// Which features end up on, and why: your own list, the defaults, and feature unification through other crates.
function enabledFeatures() {
  const P = projectFeatures();
  const F = { toml: crateFeatures("toml-0.8.23"), serde_json: crateFeatures("serde_json-1.0.151"), serde: crateFeatures("serde-1.0.229"), serde_core: crateFeatures("serde_core-1.0.229"), smallvec: crateFeatures("smallvec-1.16.0") };
  const on = {}; const why = {};
  const turn = (c, f, by) => { if (!F[c]) return; on[c] ||= new Set(); if (on[c].has(f)) return; on[c].add(f); (why[c] ||= {})[f] = by; for (const d of F[c].features[f] || []) { const m = /^([\w-]+)\/(\w[\w-]*)$/.exec(d.replace("?", "")); if (m) turn(m[1], m[2], `${c}’s ${f}`); else if (!d.startsWith("dep:")) turn(c, d, `${c}’s ${f}`); } };
  for (const [c, p] of Object.entries(P)) { if (!p.noDefault) turn(c, "default", "the default"); for (const f of p.feats) turn(c, f, "you"); }
  // serde depends on serde_core with features ["result"], default off
  turn("serde_core", "result", "serde");
  return { project: P, crates: F, on: Object.fromEntries(Object.entries(on).map(([c, s]) => [c, [...s]])), why };
}

// ---------------------------------------------------------------- since / changed, from real release data
function sinceOf(it, paths) {
  if (!it.release) return null;
  const R = RELEASES[it.release]; const local = Object.keys(R.api);
  const order = R.versions.map((v) => v.v); const sorted = local.slice().sort((a, b) => order.indexOf(a) - order.indexOf(b));
  const at = (v) => R.versions.find((x) => x.v === v)?.at || null;
  const first = sorted.find((v) => paths.some((p) => R.api[v][p]));
  const changed = [];
  for (let k = 1; k < sorted.length; k++) {
    const d = R.diff[`${sorted[k - 1]}→${sorted[k]}`]; if (!d) continue;
    // fold every path to its declared path (follow `via` to a fixed point, as releases-ui.js declared() does),
    // and a signature change that only renames parameters is no change: Rust has no named arguments
    const decl = (path) => { for (const v of [sorted[k], sorted[k - 1]]) { const a = R.api[v]; if (!a) continue; let p = path; for (let hop = 0; hop < 8; hop++) { if (a[p] && a[p].via && a[p].via !== p) { p = a[p].via; continue; } const at = p.lastIndexOf("::"); const o = at > 0 ? a[p.slice(0, at)] : null; if (o && o.via && o.via !== p.slice(0, at)) { p = o.via + p.slice(at); continue; } break; } if (p !== path) return p; } return path; };
    const home = decl(paths[0]);
    const tail = (c) => { const p = decl(c.path); return p === home ? "·" : p.startsWith(home + "::") ? p.slice(home.length) : null; };
    const noNames = (x) => (x || "").replace(/(?<![:\w])(mut\s+)?[a-z_][a-z0-9_]*\s*:(?!:)/g, "_:");
    const uniq = (xs) => { const seen = new Set(); return xs.filter((c) => { const t = tail(c); if (t === null || seen.has(t)) return false; seen.add(t); return true; }); };
    const hits = uniq((d.changed || []).filter((c) => noNames(c.before) !== noNames(c.after)));
    const adds = uniq((d.added || []).filter((c) => tail(c) !== "·"));
    if (hits.length || adds.length) changed.push({ v: sorted[k], at: at(sorted[k]), changed: hits.length, added: adds.length, breaking: hits.filter((c) => c.kind === "breaking").length, sample: hits.slice(0, 3).map((c) => ({ path: c.path, before: c.before, after: c.after })) });
  }
  return { first, firstAt: at(first), oldest: sorted[0], oldestIsFirst: first === sorted[0], read: sorted, pinned: R.pinned, versions: R.versions.length, changed };
}
function memberSince(it, owner, name) {
  if (!it.release) return null;
  const R = RELEASES[it.release]; const order = R.versions.map((v) => v.v);
  const local = Object.keys(R.api).sort((a, b) => order.indexOf(a) - order.indexOf(b));
  const p = `${owner}::${name}`;
  const noNames = (x) => (x || "").replace(/(?<![:\w])(mut\s+)?[a-z_][a-z0-9_]*\s*:(?!:)/g, "_:");
  const has = local.filter((v) => R.api[v][p]);
  if (!has.length) return { first: null };
  const changes = [];
  for (let k = 1; k < local.length; k++) { const d = R.diff[`${local[k - 1]}→${local[k]}`]; if (!d) continue; const c = (d.changed || []).find((x) => x.path === p && noNames(x.before) !== noNames(x.after)); if (c) changes.push({ v: local[k], kind: c.kind, before: c.before, after: c.after }); const dep = (d.deprecated || []).find((x) => x.path === p); if (dep) changes.push({ v: local[k], kind: "deprecated" }); }
  const later = local.filter((v) => order.indexOf(v) > order.indexOf(R.pinned));
  const gone = later.find((v) => !R.api[v][p]);
  return { first: has[0], oldest: has[0] === local[0], changes, goneIn: gone || null };
}

// ---------------------------------------------------------------- build
function main() {
  const t0 = Date.now();
  const feats = enabledFeatures();
  const crates = new Map();
  const crate = (dir) => crates.get(dir) || crates.set(dir, readCrate(path.join(REG, dir))).get(dir);
  const pages = {};
  for (const it of ITEMS) {
    const i = W.find(it.pkg, it.mod, it.name); if (i < 0) throw new Error("not in world: " + it.key);
    const n = W.N[i]; peek(i);
    const refs = new Set(W.inEdges(i, -1).concat(...(W.kids[i] || []).map((m) => W.inEdges(m, -1))).map((j) => W.topOf[j])); refs.delete(i);
    const src = crate(it.crate);
    const page = {
      slug: it.slug, key: it.key, id: i, name: n.n, kind: n.k, fam: W.KFAM[n.k], pkg: W.pkgName(n.p), version: W.PK[n.p].version,
      path: W.qual(i) + "::" + n.n, aliases: it.aliases || [], lede: n.d || "", file: n.f, line: n.l, end: n.e,
      usedIn: refs.size, usedYours: [...refs].filter((j) => W.yours(j)).length,
      relations: relations(i, { maxJoints: 40 }),
      harvest: harvest(it),
      since: sinceOf(it, [it.rpath, ...(it.aliases || [])].filter(Boolean)),
    };
    if (n.k === "trait") { // real code: how your own types come to do it
      const d = page.relations.find((r) => r.verb === "done by"); page.treeUses = [];
      for (const j of (d ? d.ids : []).filter((j) => W.yours(j)).slice(0, 12)) {
        const L = readLines(fileOf(j)); const x = W.N[j]; if (!L || !x.l) continue;
        let a = x.l - 1; for (let k = x.l - 2; k >= Math.max(0, x.l - 8); k--) { if (/^\s*#\[/.test(L[k])) a = k; else if (!/^\s*(\/\/|$)/.test(L[k])) break; }
        const code = L.slice(a, x.l).map((t) => t.replace(/^\s{0,4}/, "")).join("\n");
        if (!new RegExp(`\\b${n.n}\\b`).test(code)) continue;
        page.treeUses.push({ id: j, pkg: W.pkgName(x.p), file: x.f.split("/").pop(), line: a + 1, code });
        peek(j); if (page.treeUses.length >= 3) break;
      }
    }
    const mod = []; for (let j = 0; j < W.NN; j++) { const x = W.N[j]; if (x.u < 0 && x.m === n.m && !x.orphan && x.n) mod.push(j); }
    mod.sort((a, b) => (W.N[a].l || 0) - (W.N[b].l || 0));
    page.shelf = { module: W.MD[n.m].path || "(root)", items: mod.slice(0, 48).map((j) => ({ id: j, n: W.N[j].n, k: W.N[j].k })) };
    pages[it.slug] = page;
    page.src = sourceFacts(it, i, src, feats, crate, { peek, memberSince });
  }
  const out = { built: new Date().toISOString(), items: ITEMS.map((x) => x.slug), pages, peeks: Object.fromEntries(peeks), features: { on: feats.on, why: feats.why, project: feats.project }, versions: Object.fromEntries(Object.entries(RELEASES).map(([c, r]) => [c, r.versions.map((v) => ({ v: v.v, at: v.at.slice(0, 10), y: v.yanked ? 1 : 0 }))])) };
  fs.writeFileSync(path.join(HERE, "page2.json"), JSON.stringify(out));
  console.log(`page2.json: ${ITEMS.length} pages, ${peeks.size} peeks, ${(fs.statSync(path.join(HERE, "page2.json")).size / 1024).toFixed(0)} KB in ${Date.now() - t0} ms`);
}

//SOURCEFACTS

main();
