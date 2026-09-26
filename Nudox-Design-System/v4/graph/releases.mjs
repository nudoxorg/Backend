// releases.mjs — release history + API diffs for registry crates, crossed with
// this workspace's own use sites. Writes releases.json and releases.js
// (window.RELEASES = {...}) next to this file.
//
//   node releases.mjs
//
// Crates covered: toml, smallvec (the two the workspace pins outside
// serde_core/serde_json). Add more by extending CRATES below — each entry
// just needs a crate name; everything else (pinned version, release list,
// local sources, use sites) is derived.
//
// ============================================================== SCHEMA
// RELEASES[crateName] = {
//   pinned: "0.8.23",              // the version this workspace actually builds with
//                                  // (resolved from root Cargo.toml's requirement
//                                  // against the versions locked in Cargo.lock —
//                                  // NOT necessarily the bare Cargo.toml string:
//                                  // smallvec's `"1.15.1"` requirement resolves to
//                                  // the locked 1.16.0).
//
//   versions: [{ v, at, yanked, local }, ...]
//     Every release known to the sparse-index cache
//     (~/.cargo/registry/index/.../.cache/<shard>/<name>), in semver order.
//       v      — the exact "vers" string from the index (build metadata like
//                "+spec-1.1.0" is part of the version and kept verbatim).
//       at     — publish time ("pubtime" from the index), ISO 8601.
//       yanked — bool.
//       local  — true iff `<name>-<v>/Cargo.toml` exists under the registry's
//                `src/` checkout (i.e. we actually have the source to parse).
//
//   api: { [version]: { [stablePath]: entry, ... }, ... }    — LOCAL versions only.
//     stablePath is a "::"-joined path such as "toml::value::Value::as_table",
//     rooted at the crate name. A type reachable through a `pub use` re-export
//     gets an entry at BOTH its true declared path and every public alias path
//     (e.g. both "toml::value::Value" and the root re-export "toml::Value");
//     whichever of those is the item's own true module location is the
//     "canonical" one members are nested under (so methods are always at
//     canonical::method, never duplicated per alias-path prefix... except the
//     alias entries themselves DO get their own member entries too, since a
//     caller writing `toml::Table::try_from` needs that to resolve directly).
//     Every entry that is not itself the item's true declared location carries
//     `via`: the true declared path (this can itself be a private module path,
//     e.g. toml::Table and toml::value::Table both have
//     `via: "toml::table::Table"`, because `table` is a private module only
//     reachable through re-exports — there is no single "more public" home to
//     prefer between the two aliases, so the shortest wins as canonical and
//     both still carry `via` back to the truth).
//     Entry shape: { path, k (kind), sig (normalized), params?, ret?, derives?,
//     owner? (member's owning type name), fields?/variants? (for struct/enum,
//     each { name, ty } / { name, shape, ty }), fullyPublicFields? (struct has
//     zero private fields, i.e. literal-constructible/exhaustively-destructurable
//     from outside the crate), deprecated?, nonExhaustive?, via? }.
//     `sig`/`ret`/params[]/fields[].ty/variants[].ty are NORMALIZED: trailing
//     commas before a closing bracket are dropped, whitespace is collapsed, and
//     (within a member) a literal `Self` is rewritten to the owner type's name
//     — so a version that only reformats, or swaps `Self` for the spelled-out
//     type name, does NOT show up as a change. See "Self" proof in the
//     verification output.
//     Scope limits: items whose only public surface comes from a
//     `pub use other_crate::Thing;` (e.g. toml's `Date`/`Datetime`, re-exported
//     from `toml_datetime`) are NOT in `api`, since we don't parse other_crate.
//     Cfg-gated items are treated as always present (we don't evaluate
//     `#[cfg(feature = ...)]`), matching a default-features docs.rs-style view.
//
//   diff: { "<a>→<b>": {...}, ... }
//     One entry for pinned→(every OTHER local version), and one for each
//     consecutive pair of local versions (by semver order). Each value:
//       { from, to,
//         added:   [{ path, k, sig }],
//         removed: [{ path, k, sig }],
//         changed: [{ path, k, before, after, kind }],
//           — before/after are normalized sig strings for a function/type-level
//             change, or a field/variant's normalized `ty` for a same-named
//             field/variant whose payload type changed (path is then
//             "owner::member"), or a synthetic "derive(..)" string when only
//             the derive list changed (sig itself identical).
//         deprecated: [{ path }]  — newly deprecated in `to` (wasn't in `from`).
//         fields:   [{ path, added: [{name,kind}], removed: [{name,kind}] }]
//         variants: [{ path, added: [{name,kind}], removed: [{name,kind}] }]
//         renamed:  [{ from, to, k }]
//           — a removed item and an added item that share kind, owner, AND
//             normalized signature are reported ONLY here (pulled out of
//             added/removed, not double-counted).
//         semverSlip: bool
//           — true iff there's a breaking-kind change anywhere above AND the
//             version bump stayed inside the same Cargo caret-compatibility
//             class (same major once major>0; same major.minor for a 0.y.z
//             series; 0.0.z has no compatibility class at all, so it can never
//             slip). Breaking across a real major bump (or a 0.x "minor-acts-
//             like-major" bump) is expected, not a slip.
//       }
//     `kind` on every change is "breaking" or "additive":
//       breaking — item removed; a common item's signature or derive-removal
//                  changed; a field/variant's payload type changed; a new
//                  variant added to an enum that ISN'T #[non_exhaustive]; a
//                  field added to a struct that has zero private fields (so
//                  external code can construct/destructure it by literal) and
//                  ISN'T #[non_exhaustive]; any field/variant removed; a rename
//                  (the old name is a breaking removal for existing callers).
//       additive — item added; a derive gained (not lost); a new field on a
//                  struct that already has a private field or is
//                  #[non_exhaustive]; a new variant on a #[non_exhaustive] enum.
//
//   uses: [{ path, file, line, text }]
//     Every workspace source reference to one of this crate's public items:
//     a full path expression (`toml::Value::as_table`), a turbofish type
//     argument (`from_str::<toml::Value>` — both `toml::from_str` AND the
//     nested `toml::Value` are recorded, as two separate use sites), and bare
//     names after a `use toml::Whatever;` import (then `Whatever::method(...)`
//     in that file resolves through the import). `path` is resolved to the
//     item's stable path in the PINNED version's `api` (falling back to the
//     longest owner-prefix match plus the remaining segments, e.g.
//     `toml::Value::String` for an enum-variant literal, so it can still be
//     matched against that owner's `variants` list even though variants don't
//     get their own top-level api entry). `text` is the source line, trimmed.
//     OUT OF SCOPE, by design: method calls on a VALUE of the crate's type
//     (`some_value.as_table()`) — only path/UFCS-style expressions and
//     turbofish/import-resolved bare names are tracked, never `.method()`
//     dispatch on an arbitrary receiver. Files under a directory literally
//     named tests/benches/examples/fixtures are excluded (matching
//     extract.mjs's own workspace walk), and so is anything inside
//     `#[cfg(test)]` or a `mod tests { ... }` block.
//
//   impact: { "<a>→<b>": [{ ...use site, change }] }
//     Same keys as `diff`. Every use site whose resolved path is removed or
//     changed in that diff (including a field/variant-level change reached via
//     the owner-prefix fallback above), with that specific change object
//     attached as `change`. Always present, even when empty — an empty array
//     is a real (good) result, not a missing one.
// ======================================================================

import fs from "node:fs";
import path from "node:path";
import { extractWorld, readCrate, walk, lex, tokText, skipAngles, splitTop } from "./extract.mjs";

const HERE = path.dirname(new URL(import.meta.url).pathname);
const REPO = path.resolve(HERE, "../../..");
const REG = fs.readdirSync(path.join(process.env.HOME, ".cargo/registry/src"))
  .map((d) => path.join(process.env.HOME, ".cargo/registry/src", d))[0];
const INDEX_CACHE = path.join(process.env.HOME, ".cargo/registry/index/index.crates.io-1949cf8c6b5b557f/.cache");

const CRATES = ["toml", "smallvec"];

// ---------------------------------------------------------------- semver
function parseSemver(v) {
  const m = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?$/.exec(v);
  if (!m) return null;
  return { major: +m[1], minor: +m[2], patch: +m[3], pre: m[4] ? m[4].split(".") : null, build: m[5] || null };
}
function cmpSemver(a, b) {
  const pa = parseSemver(a), pb = parseSemver(b);
  if (!pa || !pb) return a < b ? -1 : a > b ? 1 : 0;
  for (const k of ["major", "minor", "patch"]) if (pa[k] !== pb[k]) return pa[k] - pb[k];
  if (!pa.pre && !pb.pre) return 0;
  if (!pa.pre) return 1;
  if (!pb.pre) return -1;
  const la = pa.pre, lb = pb.pre;
  for (let i = 0; i < Math.max(la.length, lb.length); i++) {
    const x = la[i], y = lb[i];
    if (x === undefined) return -1;
    if (y === undefined) return 1;
    const nx = /^\d+$/.test(x), ny = /^\d+$/.test(y);
    if (nx && ny) { const d = +x - +y; if (d) return d; }
    else if (nx) return -1; else if (ny) return 1;
    else if (x !== y) return x < y ? -1 : 1;
  }
  return 0;
}
function caretClass(v) {
  const p = parseSemver(v); if (!p) return v;
  if (p.major > 0) return `${p.major}`;
  if (p.minor > 0) return `0.${p.minor}`;
  return `0.0.${p.patch}`; // 0.0.z: no compatibility class — any bump can break, by design
}

// ---------------------------------------------------------------- pinned version
function workspaceDependencyReqs() {
  const txt = fs.readFileSync(path.join(REPO, "Cargo.toml"), "utf8");
  const start = txt.indexOf("[workspace.dependencies]");
  const rest = txt.slice(start + "[workspace.dependencies]".length);
  const end = rest.search(/\n\[/);
  const block = end >= 0 ? rest.slice(0, end) : rest;
  const reqs = new Map();
  for (const m of block.matchAll(/^([a-zA-Z0-9_-]+)\s*=\s*(?:"([^"]+)"|\{[^}\n]*?version\s*=\s*"([^"]+)")/gm)) {
    reqs.set(m[1], m[2] || m[3]);
  }
  return reqs;
}
function lockedVersionsOf(name) {
  const txt = fs.readFileSync(path.join(REPO, "Cargo.lock"), "utf8");
  const re = new RegExp(`\\[\\[package\\]\\]\\nname = "${name}"\\nversion = "([^"]+)"`, "g");
  return [...txt.matchAll(re)].map((m) => m[1]);
}
function satisfiesCaret(req, ver) {
  const v = parseSemver(ver); if (!v) return false;
  if (req.startsWith("=")) return req.slice(1).trim() === ver;
  const r = parseSemver(req.replace(/^[\^~]/, "")); if (!r) return false;
  if (r.major > 0) return v.major === r.major && (v.minor > r.minor || (v.minor === r.minor && v.patch >= r.patch));
  if (r.minor > 0) return v.major === 0 && v.minor === r.minor && v.patch >= r.patch;
  return v.major === 0 && v.minor === 0 && v.patch === r.patch;
}
function pinnedVersionFor(name) {
  const req = workspaceDependencyReqs().get(name);
  const locked = lockedVersionsOf(name);
  if (req) {
    const matches = locked.filter((v) => satisfiesCaret(req, v));
    if (matches.length) return matches.sort(cmpSemver).pop();
  }
  if (locked.length) return locked.sort(cmpSemver).pop();
  throw new Error(`no locked version found for ${name}`);
}

// ---------------------------------------------------------------- index cache
function shardOf(name) {
  if (name.length === 1) return path.join("1", name);
  if (name.length === 2) return path.join("2", name);
  if (name.length === 3) return path.join("3", name[0], name);
  return path.join(name.slice(0, 2), name.slice(2, 4), name);
}
function readIndexCache(name) {
  const buf = fs.readFileSync(path.join(INDEX_CACHE, shardOf(name)));
  const records = [];
  let start = 0;
  for (let i = 0; i <= buf.length; i++) {
    if (i === buf.length || buf[i] === 0) {
      const s = buf.slice(start, i).toString("utf8");
      start = i + 1;
      if (s && s[0] === "{") { try { const j = JSON.parse(s); if (j && j.vers) records.push(j); } catch { /* not a version record */ } }
    }
  }
  return records;
}
function versionsFor(name) {
  const records = readIndexCache(name);
  const localDirs = new Set(fs.readdirSync(REG));
  const list = records.map((r) => ({ v: r.vers, at: r.pubtime, yanked: !!r.yanked, local: localDirs.has(`${name}-${r.vers}`) }));
  list.sort((a, b) => cmpSemver(a.v, b.v));
  return list;
}
function localSourceDir(name, version) {
  const dir = path.join(REG, `${name}-${version}`);
  return fs.existsSync(path.join(dir, "Cargo.toml")) ? dir : null;
}

// ---------------------------------------------------------------- normalization
// A lifetime PARAMETER's name is pure bookkeeping — `struct X<'a>` and
// `struct X<'i>` are the same type to every caller, exactly like `Self` vs
// the spelled-out type name. Canonicalize every non-'static, non-'_ lifetime
// in a signature string to a position-of-first-appearance letter so a rename
// alone doesn't register as a change. Elided (`'_`) and `'static` are left
// alone since they carry real meaning (arity/absence of a named lifetime,
// and "lives forever", respectively).
function canonicalizeLifetimes(s) {
  if (!s) return s;
  const seen = new Map(); let idx = 0;
  return s.replace(/'([A-Za-z_]\w*)/g, (m, name) => {
    if (name === "static" || name === "_") return m;
    if (!seen.has(name)) seen.set(name, "'" + String.fromCharCode(97 + (idx++ % 26)));
    return seen.get(name);
  });
}
function normalizeText(text, ownerName) {
  if (!text) return text;
  let s = text;
  if (ownerName) s = s.replace(/\bSelf\b/g, ownerName);
  s = canonicalizeLifetimes(s);
  s = s.replace(/,\s*([)\]}>])/g, "$1");
  s = s.replace(/\s+/g, " ").trim();
  return s;
}

// ---------------------------------------------------------------- api[version]
function buildApiForVersion(crateName, dir) {
  const { publicApi } = extractWorld([dir]);
  const raw = publicApi.get(crateName) || new Map();
  const api = {};
  for (const [key, entry] of raw) {
    const owner = entry.owner;
    const e = { ...entry };
    e.sig = normalizeText(e.sig, owner);
    if (e.ret) e.ret = normalizeText(e.ret, owner);
    if (e.params) e.params = e.params.map((p) => normalizeText(p, owner));
    if (e.fields) e.fields = e.fields.map((f) => ({ ...f, ty: normalizeText(f.ty, owner) }));
    if (e.variants) e.variants = e.variants.map((v) => ({ ...v, ty: normalizeText(v.ty, owner) }));
    api[key] = e;
  }
  return api;
}

// ---------------------------------------------------------------- diff
function diffApi(apiA, apiB, verA, verB) {
  const keysA = new Set(Object.keys(apiA)), keysB = new Set(Object.keys(apiB));
  let added = [...keysB].filter((k) => !keysA.has(k)).map((k) => ({ path: k, k: apiB[k].k, sig: apiB[k].sig }));
  let removed = [...keysA].filter((k) => !keysB.has(k)).map((k) => ({ path: k, k: apiA[k].k, sig: apiA[k].sig }));
  const commonKeys = [...keysA].filter((k) => keysB.has(k));

  const changed = [], deprecated = [], fieldsBucket = [], variantsBucket = [];

  for (const k of commonKeys) {
    const a = apiA[k], b = apiB[k];
    if (a.sig !== b.sig) {
      changed.push({ path: k, k: b.k, before: a.sig, after: b.sig, kind: "breaking" });
    } else {
      const dA = new Set(a.derives || []), dB = new Set(b.derives || []);
      const dRemoved = [...dA].filter((d) => !dB.has(d)), dAdded = [...dB].filter((d) => !dA.has(d));
      if (dRemoved.length || dAdded.length) {
        changed.push({ path: k, k: b.k, before: `derive(${[...dA].join(", ")})`, after: `derive(${[...dB].join(", ")})`, kind: dRemoved.length ? "breaking" : "additive" });
      }
    }
    if (!a.deprecated && b.deprecated) deprecated.push({ path: k });

    if (a.fields || b.fields) {
      const fa = new Map((a.fields || []).map((f) => [f.name, f])), fb = new Map((b.fields || []).map((f) => [f.name, f]));
      const fAdded = [...fb.keys()].filter((n) => !fa.has(n)), fRemoved = [...fa.keys()].filter((n) => !fb.has(n));
      for (const n of [...fa.keys()].filter((n) => fb.has(n))) {
        if (fa.get(n).ty !== fb.get(n).ty) changed.push({ path: `${k}::${n}`, k: "field", before: fa.get(n).ty, after: fb.get(n).ty, kind: "breaking" });
      }
      if (fAdded.length || fRemoved.length) {
        fieldsBucket.push({
          path: k,
          added: fAdded.map((n) => ({ name: n, kind: (b.fullyPublicFields && !b.nonExhaustive) ? "breaking" : "additive" })),
          removed: fRemoved.map((n) => ({ name: n, kind: "breaking" })),
        });
      }
    }
    if (a.variants || b.variants) {
      const va = new Map((a.variants || []).map((v) => [v.name, v])), vb = new Map((b.variants || []).map((v) => [v.name, v]));
      const vAdded = [...vb.keys()].filter((n) => !va.has(n)), vRemoved = [...va.keys()].filter((n) => !vb.has(n));
      for (const n of [...va.keys()].filter((n) => vb.has(n))) {
        const x = va.get(n), y = vb.get(n);
        if (x.ty !== y.ty || x.shape !== y.shape) changed.push({ path: `${k}::${n}`, k: "variant", before: x.ty, after: y.ty, kind: "breaking" });
      }
      if (vAdded.length || vRemoved.length) {
        variantsBucket.push({
          path: k,
          added: vAdded.map((n) => ({ name: n, kind: b.nonExhaustive ? "additive" : "breaking" })),
          removed: vRemoved.map((n) => ({ name: n, kind: "breaking" })),
        });
      }
    }
  }

  // renamed: pull matching removed/added pairs out of added/removed
  const renamed = [];
  const usedAdded = new Set();
  removed = removed.filter((r) => {
    const match = added.find((a) => !usedAdded.has(a.path) && a.k === r.k && a.sig === r.sig && (apiB[a.path].owner || null) === (apiA[r.path].owner || null));
    if (match) { renamed.push({ from: r.path, to: match.path, k: r.k }); usedAdded.add(match.path); return false; }
    return true;
  });
  added = added.filter((a) => !usedAdded.has(a.path));

  const hasBreaking = removed.length > 0 || changed.some((c) => c.kind === "breaking") || renamed.length > 0
    || fieldsBucket.some((f) => f.removed.length > 0 || f.added.some((x) => x.kind === "breaking"))
    || variantsBucket.some((v) => v.removed.length > 0 || v.added.some((x) => x.kind === "breaking"));
  const semverSlip = hasBreaking && caretClass(verA) === caretClass(verB);

  return { from: verA, to: verB, added, removed, changed, deprecated, fields: fieldsBucket, variants: variantsBucket, renamed, semverSlip };
}

// ---------------------------------------------------------------- uses (workspace scan)
function workspaceCrateDirs() {
  return ["crates", "frontends", "extensions", "apps"].flatMap((g) =>
    fs.readdirSync(path.join(REPO, g)).map((d) => path.join(REPO, g, d)))
    .filter((d) => fs.existsSync(path.join(d, "Cargo.toml")) && !d.endsWith("/turso"));
}
function collectUseImports(t, a, b, prefix, imports, crateName) {
  let seg = [...prefix];
  for (let i = a; i < b; i++) {
    const v = t[i].v;
    if (v === "::") continue;
    if (v === "{") { for (const [x, y] of splitTop(t, i + 1, t[i].m)) collectUseImports(t, x, y, seg, imports, crateName); return; }
    if (v === "*") return; // pub/plain glob import of our crate: rare, not tracked (documented limitation)
    if (v === "as") { const alias = t[i + 1].v; if (alias !== "_" && seg[0] === crateName) imports.set(alias, seg); return; }
    if (t[i].k === "id") seg = [...seg, v];
  }
  if (seg.length && seg[0] === crateName) {
    const last = seg[seg.length - 1] === "self" ? seg[seg.length - 2] : seg[seg.length - 1];
    if (last) imports.set(last, seg[seg.length - 1] === "self" ? seg.slice(0, -1) : seg);
  }
}
// Walk one file's tokens, calling onUse(chain, line) for every path expression
// (or turbofish type argument, or import-resolved bare name) that starts with
// this crate. Skips `#[cfg(test)]` items and `mod tests { ... }` wholesale,
// matching extract.mjs's own convention. Method calls on a value
// (`x.as_table()`) are never matched: `as_table` there isn't itself "toml" nor
// an import alias, so it never reaches the chain-start check below.
function scanFileForUses(tokens, crateName, onUse) {
  const imports = new Map();
  let i = 0; const n = tokens.length;
  let attrs = [];
  while (i < n) {
    const tk = tokens[i];
    if (tk.k === "doc") { i++; continue; }
    if (tk.v === "#" && (tokens[i + 1]?.v === "[" || (tokens[i + 1]?.v === "!" && tokens[i + 2]?.v === "["))) {
      const o = tokens[i + 1].v === "[" ? i + 1 : i + 2;
      if (o === i + 1) attrs.push(tokens.slice(o + 1, tokens[o].m));
      i = tokens[o].m + 1; continue;
    }
    const cfgTest = attrs.some((a) => a.some((x) => x.v === "cfg") && a.some((x) => x.v === "test"));
    attrs = [];
    if (tk.v === "mod" && tokens[i + 1]?.v === "tests" && tokens[i + 2]?.v === "{") { i = tokens[i + 2].m + 1; continue; }
    if (cfgTest) {
      let j = i;
      while (j < n && tokens[j].v !== ";" && tokens[j].v !== "{") { if (tokens[j].v === "(" || tokens[j].v === "[") j = tokens[j].m; j++; }
      if (tokens[j]?.v === "{") j = tokens[j].m;
      i = j + 1; continue;
    }
    if (tk.v === "use") {
      let j = i + 1; while (j < n && tokens[j].v !== ";") { if (tokens[j].v === "{") j = tokens[j].m; j++; }
      collectUseImports(tokens, i + 1, j, [], imports, crateName);
      i = j + 1; continue;
    }
    if (tk.k === "id") {
      const prev = tokens[i - 1];
      const isContinuation = prev && prev.k === "p" && (prev.v === "::" || prev.v === ".");
      if (!isContinuation) {
        let chain = null;
        if (tk.v === crateName) chain = [crateName];
        else if (imports.has(tk.v)) chain = [...imports.get(tk.v)];
        if (chain) {
          let j = i + 1;
          while (j + 1 < n && tokens[j].v === "::") {
            if (tokens[j + 1].v === "<") { j = skipAngles(tokens, j + 1); continue; }
            if (tokens[j + 1].k !== "id") break;
            chain.push(tokens[j + 1].v); j += 2;
          }
          onUse(chain, tk.line);
        }
      }
    }
    i++;
  }
}
function resolveUsePath(chain, api) {
  for (let k = chain.length; k >= 1; k--) {
    const prefix = chain.slice(0, k).join("::");
    if (api[prefix]) {
      const suffix = chain.slice(k);
      return suffix.length ? api[prefix].path + "::" + suffix.join("::") : api[prefix].path;
    }
  }
  return null;
}
function scanUses(crateName, pinnedApi) {
  const uses = [];
  for (const dir of workspaceCrateDirs()) {
    const c = readCrate(dir);
    const base = path.dirname(c.root);
    const files = walk(base).filter((f) => !f.includes("/bin/") || f === c.root);
    for (const f of files) {
      const src = fs.readFileSync(f, "utf8");
      const lines = src.split("\n");
      const tokens = lex(src);
      const rel = path.relative(REPO, f);
      scanFileForUses(tokens, crateName, (chain, line) => {
        const resolved = resolveUsePath(chain, pinnedApi);
        uses.push({ path: resolved, file: rel, line, text: (lines[line - 1] || "").trim(), raw: chain.join("::") });
      });
    }
  }
  return uses;
}

// ---------------------------------------------------------------- impact
function impactFor(diff, uses) {
  const removedSet = new Map(diff.removed.map((r) => [r.path, { type: "removed", ...r }]));
  const changedSet = new Map(diff.changed.map((c) => [c.path, { type: "changed", ...c }]));
  for (const f of diff.fields) for (const r of f.removed) changedSet.set(`${f.path}::${r.name}`, { type: "field-removed", path: `${f.path}::${r.name}`, ...r });
  for (const v of diff.variants) for (const r of v.removed) changedSet.set(`${v.path}::${r.name}`, { type: "variant-removed", path: `${v.path}::${r.name}`, ...r });
  for (const rn of diff.renamed) removedSet.set(rn.from, { type: "renamed", ...rn, path: rn.from });

  const out = [];
  for (const u of uses) {
    if (!u.path) continue;
    const change = removedSet.get(u.path) || changedSet.get(u.path);
    if (change) out.push({ path: u.path, file: u.file, line: u.line, text: u.text, change });
  }
  return out;
}

// ================================================================== main
const RELEASES = {};
for (const crateName of CRATES) {
  const pinned = pinnedVersionFor(crateName);
  const versions = versionsFor(crateName);
  const localVersions = versions.filter((v) => v.local).map((v) => v.v).sort(cmpSemver);
  if (!localVersions.includes(pinned)) localVersions.push(pinned);
  localVersions.sort(cmpSemver);

  const api = {};
  for (const v of localVersions) {
    const dir = localSourceDir(crateName, v);
    if (!dir) continue;
    api[v] = buildApiForVersion(crateName, dir);
  }

  const diff = {};
  for (const v of localVersions) if (v !== pinned && api[v]) diff[`${pinned}→${v}`] = diffApi(api[pinned], api[v], pinned, v);
  for (let i = 0; i < localVersions.length - 1; i++) {
    const a = localVersions[i], b = localVersions[i + 1];
    if (api[a] && api[b]) diff[`${a}→${b}`] = diffApi(api[a], api[b], a, b);
  }

  const uses = scanUses(crateName, api[pinned]).map(({ raw, ...u }) => u);
  const impact = {};
  for (const key of Object.keys(diff)) impact[key] = impactFor(diff[key], uses);

  RELEASES[crateName] = { pinned, versions, api, diff, uses, impact };
}

fs.writeFileSync(path.join(HERE, "releases.json"), JSON.stringify(RELEASES));
fs.writeFileSync(path.join(HERE, "releases.js"), "window.RELEASES=" + JSON.stringify(RELEASES) + ";\n");

// ================================================================== verify
function summarize(crateName, a, b) {
  const d = RELEASES[crateName].diff[`${a}→${b}`];
  console.log(`\n=== ${crateName} ${a} → ${b} ===`);
  if (!d) { console.log("  (no diff computed — missing local source for one side)"); return; }
  console.log(`  added=${d.added.length} removed=${d.removed.length} changed=${d.changed.length} deprecated=${d.deprecated.length} fields=${d.fields.length} variants=${d.variants.length} renamed=${d.renamed.length} semverSlip=${d.semverSlip}`);
  const first10 = [...d.changed, ...d.added.map((x) => ({ ...x, before: undefined, after: x.sig })), ...d.removed.map((x) => ({ ...x, before: x.sig, after: undefined }))].slice(0, 10);
  console.log(`  first 10 changes:`);
  for (const c of first10) console.log(`    [${c.kind || "?"}] ${c.path}\n      before: ${c.before ?? "(absent)"}\n      after:  ${c.after ?? "(absent)"}`);
  const uses = RELEASES[crateName].uses.filter((u) => true);
  console.log(`  uses: ${RELEASES[crateName].uses.length} total in workspace`);
  for (const u of RELEASES[crateName].uses.slice(0, 15)) console.log(`    ${u.file}:${u.line} -> ${u.path}  |  ${u.text}`);
  const imp = RELEASES[crateName].impact[`${a}→${b}`] || [];
  console.log(`  impact: ${imp.length} use site(s) affected`);
  for (const i of imp) console.log(`    ${i.file}:${i.line} -> ${i.path} (${i.change.type})  |  ${i.text}`);
}

console.log(JSON.stringify({ crates: CRATES, pinned: Object.fromEntries(CRATES.map((c) => [c, RELEASES[c].pinned])) }));
const tomlLatestLocal = RELEASES.toml.versions.filter((v) => v.local).map((v) => v.v).sort(cmpSemver).slice(-1)[0];
summarize("toml", "0.8.23", tomlLatestLocal);
summarize("smallvec", "1.16.0", "1.16.1");
