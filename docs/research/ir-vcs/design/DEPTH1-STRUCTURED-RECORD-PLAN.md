# DEPTH-1 STRUCTURED RECORD PLAN — F1 field pairing as a native pijul diff algorithm

**Status:** Normative implementation plan (Rev 2, 2026-07-18). **Supersedes Rev 1
in full.** Rev 1 layered foreign machinery over the engine: a `StructuredRecord`
callback trait threaded through `Builder`/`Recorded`, a bespoke `FrameDelta` op
vocabulary, a hand-driven lowering file calling `pub(super)` internals, a runtime
fallback protocol, and a store-level flag. Rev 2 deletes all of it. libpijul
already has a first-class seam for exactly this: `diff::diff` is a pure pairing
function (lines → `D`), `Myers`/`Patience`/`ImaraHistogram` are three
interchangeable pairing algorithms behind it, and everything downstream — context
construction, atoms, hunks, hashing, apply, unrecord — is driven entirely by the
`D` it returns. Depth-1 is therefore **a fourth pairing algorithm**, dispatched by
content sniff inside `diff::diff`, emitting the same `D` through the same door.

**Inputs:** `DEPTH1-RESEARCH-DOSSIER.md` (**[D]**) for the record-path ground
truth, plus direct re-verification of the diff-module internals done for this
revision (**[V]**) against
`~/.local/share/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/libpijul-1.0.0-beta.11/`
and `…/diffs-0.5.1/`. Every `[V]` citation below was read verbatim on 2026-07-18.

**Scope:** Depth-1 of the ladder (SEMANTIC-IR-VCS-PLAN §11) — field-grained
pairing for canonical NdIrF1 blobs on the record path. No new atom kinds, no new
hunk variants, no second apply engine, no edits to `apply/`, `unrecord/`,
`pristine/`, `output/`, `alive/`, `record.rs`, or `diff/mod.rs` (K-Fork-Depth).

**Precondition (met):** Depth-0 is DONE and green — `f1.rs` canonical
serializer/parser, `subst.rs`, `continuity.rs`, and `nudox-ir-diff` all exist and
pass CI.

**License posture (normative, unchanged from Rev 1):** GPL is NOT a constraint;
all license precautions are handled externally. Optimize for correctness and
clarity. Vendor libpijul source directly, refactor freely. libpijul is
`GPL-2.0-or-later` and `diffs` is `MIT/Apache-2.0` ([V] both Cargo.tomls); the
dependency-direction rules in §3.4 survive as engineering layering, not license
hygiene.

Conventions: **MUST/SHOULD/MAY** per RFC 2119. `LIB` = vendored libpijul root
(post-§2: `workspace/vendor/libpijul/`). File:line citations are beta.11
coordinates and remain valid verbatim after vendoring because §2 vendors that
exact version.

---

## §0. The design in one breath

pijul's record path for one modified file is a four-stage pipeline, all inside
`Recorded::diff` (`LIB/src/diff/mod.rs:146-222`, [V]):

```
pristine graph ──output_graph──▶ vertex buffer d (contents_a + pos_a)
working bytes b ─────────────────────────────┐
                                             ▼
   (1) SPLIT     make_old_lines(d) / make_new_lines(b)      → Vec<Line>
   (2) PAIR      diff::diff(&lines_a, &lines_b, algo, stop) → D  ◀── Depth-1 lives HERE
   (3) LOWER     for r in 0..dd.len() { delete(..r..); replace(..r..) }
   (4) EMIT      make_change(actions, contents) → change file, hash, apply
```

Stage (2) is a pure function from two line arrays to a list of span
replacements. Myers, Patience, and ImaraHistogram already sit behind it as
interchangeable strategies. Depth-1 adds one more: when line 0 is the F1 magic
line, pair lines **field-aware** — scalar keys positionally, set keys by sorted
merge, seq keys by section-scoped Myers — and return the same `D`. Stages (1),
(3), (4) run byte-for-byte stock code, so contexts, atoms, hunks, hashes, apply,
unrecord, and sync are inherited, not reimplemented.

Because *any* well-formed `D` yields a correct record (the pairing chooses hunk
*grain*, never file *content* — stage (3) reconstructs `b` exactly from any
valid span list), the F1 pairer is a **total function**: it never validates,
never fails, and never falls back. Malformed input degrades in hunk quality
only. There is no fallback path, no flag, no hook object, and no configuration.

Deliverables:

1. **`workspace/nudox-f1`** — new leaf crate: the frozen F1 key registry (moved
   from `nudox-ir-vcs/f1.rs`) + `line_diff(a, b) -> Vec<LineReplace>`, the pure
   pairing function. Depends only on `diffs`. (§3, §4)
2. **Vendored libpijul** at `workspace/vendor/libpijul` with a fork diff of
   exactly two files: `src/diff/diff.rs` (+~30 lines) and `Cargo.toml`. (§2, §5)
3. **Acceptance suite** in `nudox-f1/tests.rs` and a new
   `nudox-ir-vcs/record_shape_tests.rs` module. (§9)

`nudox-ir-vcs` record call sites (`session.rs`, `repo.rs`) are **not touched**.
`record.rs` in the fork is **not touched**. `FrameDelta` is **not built**.

---

## §1. Ground truth

Everything here is verified fact, not design. If any item fails to re-verify
against the vendored tree, STOP and re-run the dossier before proceeding.

### §1.1 The record call chain — untouched end to end

```
Builder::record                       LIB/src/record.rs:280      [D §A2]
  └─ Recorded::record_existing_file   LIB/src/record.rs:1091     [D §A3]
       └─ Recorded::record_nondeleted LIB/src/record.rs:1154     [V]
            ├─ working_copy.decode_file(&item.full_path, &mut b)   record.rs:1229-1231
            └─ self.diff(…, &b, &encoding, diff_sep)               record.rs:1234
```

- Our two call sites — `session.rs:580` and `repo.rs:546` — are the only
  `Builder::record` callers in the workspace (re-verified by grep, 2026-07-18).
  Both pass `Algorithm::default()` (= `Myers`, [V] diff.rs:11-15),
  `stop_early=false`, `&DEFAULT_SEPARATOR` (regex `"\n"`, [V] mod.rs:16-18),
  `n_workers=1`. **Neither site changes in Rev 2.**
- `record_nondeleted`'s tail ([V] record.rs:1223-1260): `retrieve` → `decode_file`
  → `self.diff(…)` → `oldest_change` bookkeeping keyed on `self.actions.len()`
  growth. Because Depth-1 dispatches *inside* stage (2), all of this — including
  the bookkeeping Rev 1 had to replicate — runs unchanged.
- `has_binary_files` is set ONLY in `add_file` (record.rs:1010) and never at
  diff time ([V] grep: record.rs:98,143,176,1010). Nothing in Rev 2 can touch it.
- `add_file` records a brand-new file as ONE whole-file `NewVertex`
  ([D] §D2); the first re-record of such a file exercises sub-vertex splitting
  in stage (3) (test T-6).

### §1.2 Stage (1) — SPLIT: what a `Line` is

[V] mod.rs:20-65, 84-144:

```rust
// LIB/src/diff/mod.rs:20  (module-private; fields visible to submodules)
#[derive(Clone, Copy)]
struct Line<'a> {
    l: &'a [u8],              // the line's bytes, INCLUDING its trailing separator
    cyclic: bool,             // inside a cyclic-conflict region (conflict output only)
    before_end_marker: bool,  // old-side line just before a conflict-end marker
    last: bool,               // last line of its buffer
    ptr: *const u8,
}
```

- `make_old_lines(&d, sep)` splits `d.contents_a` (the pristine bytes);
  `make_new_lines(&b, sep)` splits the working-copy bytes. Both return
  `Vec<Line>`; each `Line.l` carries its terminating `\n` (final line may lack
  one). Since F1's separator is `\n` == `DEFAULT_SEPARATOR`, **one F1
  `key\tvalue\n` field line == one `Line`** ([D] §D6).
- `Line::eq` ([V] mod.rs:53-64) is byte equality plus two conflict-only wrinkles
  (`before_end_marker` eol-forgiveness, `cyclic` must match). F1 files are
  recorded only from conflict-free output ([D] §D8), so on the IR path
  `Line::eq` degenerates to byte equality. The F1 pairer compares raw `l` bytes;
  the divergence is confined to marker-bearing content, where the pairer is
  still total and stage (3) still lowers correctly (§4.6, law L-4).
- Binary detection: `decode_file` returns `Some(Encoding)` for anything
  `chardetng`+`get_valid_encoding` accepts ([D] §A10). Canonical F1 is valid
  UTF-8 by construction (`ascii.rs` escapes all C0 bytes), so F1 files always
  take the text split. If a *corrupt* F1 file ever binary-detects, stage (1)
  produces 8192-byte chunk "lines" ([V] mod.rs:169-178) whose first chunk is not
  byte-equal to the 9-byte magic line, the §5 sniff fails, and stock chunk
  diffing proceeds — correct, no special case needed.

### §1.3 Stage (2) — PAIR: the seam itself

[V] diff.rs:3-15, 17-61, 62-94:

```rust
// LIB/src/diff/diff.rs:3
pub enum Algorithm { Myers, Patience, ImaraHistogram }   // Default = Myers

// LIB/src/diff/diff.rs:62-94  (all fields pub)
pub struct D {
    pub r: Vec<Replacement>,
    pub stop_early: bool,
}
pub struct Replacement {
    pub old: usize,      // first old line of the span
    pub old_len: usize,  // deleted old lines (0 = pure insertion)
    pub new: usize,      // first new line of the span
    pub new_len: usize,  // inserted new lines (0 = pure deletion)
}

// LIB/src/diff/diff.rs:17
pub(super) fn diff(lines_a: &[Line], lines_b: &[Line],
                   algorithm: Algorithm, stop_early: bool) -> D
```

- `diff::diff` is **infallible** and returns `D` by value. It has exactly one
  caller: `mod.rs:189` ([V] grep). This function is the entire pairing seam.
- For `Myers`/`Patience` it wraps `D` in `diffs::Replace::new(…)` and calls
  `diffs::{myers,patience}::diff(&mut dd, lines_a, 0, len_a, lines_b, 0, len_b)`
  ([V] diff.rs:28-53). `impl diffs::Diff for D` ([V] diff.rs:96-152) pushes one
  `Replacement` per `delete`/`insert`/`replace` callback; `equal`/`finish` are
  default no-ops. `stop_early` works by returning `Err(())` from the sink after
  the first push, aborting the driver; the partial `D` (≤1 entry) is still
  returned via `.unwrap_or(())`.
- `D::is_deleted` ([V] diff.rs:204-219) binary-searches `r` by `old` — entries
  MUST be sorted ascending by `old` (all producers guarantee this).

### §1.4 Stage (3) — LOWER: entry shape *is* hunk shape

[V] mod.rs:189-218; replace.rs (read in full); delete.rs (read in full):

```rust
// LIB/src/diff/mod.rs:189-218 (verbatim, abridged whitespace)
let dd = diff::diff(&lines_a, &lines_b, algorithm, stop_early);
let mut conflict_contexts = replace::ConflictContexts::new();
for r in 0..dd.len() {
    if dd[r].old_len > 0 {
        self.delete(&*txn, txn.graph(&*channel), &d, &dd, &mut conflict_contexts,
                    &lines_a, &lines_b, inode_, r, encoding)?;
    }
    if dd[r].new_len > 0 {
        self.replace(&d, &mut conflict_contexts, &lines_a, &lines_b,
                     inode_, &dd, r, encoding);
    }
}
```

The lowering machinery maps entry shape to wire shape mechanically:

| `Replacement` shape | Emitted hunk | Mechanism [V] |
|---|---|---|
| `old_len > 0, new_len == 0` | `Hunk::Edit { change: Atom::EdgeMap(del) }` | `delete` → `delete_lines` pushes the Edit with `local.line = d[r].new + 1` (delete.rs:63-77) |
| `old_len == 0, new_len > 0` | `Hunk::Edit { change: Atom::NewVertex(ins) }` | `replace` skips the pairing pop when `old_len == 0` (replace.rs:87) |
| `old_len > 0, new_len > 0` | `Hunk::Replacement { change: EdgeMap(del), replacement: NewVertex(ins) }` | `replace` pops the just-pushed delete Edit and fuses when `local.line == from_new + 1` — i.e. when it came from the same entry (replace.rs:87-110) |

Facts the §4 emission grammar is built on ([V], load-bearing):

1. **Context resolution is `D`-aware.** `get_down_context` consults
   `dd.is_deleted(…)` to skip old lines deleted by *other* entries and returns
   an empty down-context when the next old line is replaced by another entry
   (replace.rs:252-331). Adjacent entries are therefore *anticipated* by the
   stock code.
2. **Up-contexts are not `D`-aware.** `get_up_context` (replace.rs:125-205)
   anchors an insertion at `old` to the vertex holding the last byte of old
   line `old-1` — even if a *different* entry deletes that line. Two entries
   with `new_len > 0` whose anchors fall in the same gap between surviving old
   lines would produce two `NewVertex` atoms with no order edge between them —
   an order-ambiguous graph that stock Myers can never produce, because…
3. **…the `diffs::Replace` adapter defines the shape grammar.** [V]
   DIFFS/src/replace.rs:1-112: it buffers one pending delete-run and one pending
   insert-run, extends them while calls stay contiguous, and flushes (fusing
   del+ins into `replace`) only when an `equal` arrives or at `finish`.
   Consequently **every `D` the stock path feeds the lowering satisfies: entries
   ascending, non-overlapping, and separated by at least one paired-equal line
   on both sides.** Rev 2 names this **Replace-normal form** and makes it the
   pairer's output contract (§4.5) — the fork feeds the lowering only shapes it
   has always digested.
4. **Conflict machinery is inert on the IR path.** `ConflictContexts` maps
   populate only via marker branches (replace.rs:182-204, 279-301;
   delete.rs:227-289); F1 files are recorded only from conflict-free output
   ([D] §D8), markers are absent, and every context call takes the fast path
   ([D] §D5). No asserts needed — this is stock behavior, not a new obligation.
5. **Sub-vertex splits are stock.** `delete_lines` walks
   `first_vertex_containing` + `Diff::vertex(i, pos, end_pos)`
   (delete.rs:98-168), which slices vertices at arbitrary byte offsets
   ([D] §D1). A single-vertex file (fresh `add_file`) is handled by the same
   arithmetic Myers relies on. Test T-6 pins it.

### §1.5 Stage (4) — EMIT: unchanged wire

`make_change` consumes `Recorded.actions` (globalized) + `Recorded.contents`
verbatim ([D] §A9). The F1 path produces those two fields through the identical
stage-(3) code, so change files, hashes, apply, unrecord, and sync see an
indistinguishable wire. `output_repository_no_pending` still returns
`BTreeSet<Conflict>` and our conflict gate before F1 parsing is unaffected
([D] §D8).

### §1.6 The F1 registry — full table (corrects [D] §B4, which omitted `fieldty`)

Verified against `workspace/nudox-ir-vcs/f1.rs:40-74` and `serialize_f1`
(f1.rs:687+): keys are emitted in frozen registry order; set-class lines are
sorted ascending by raw line bytes; the magic line is `NdIrF1\t1\n`; values may
contain further TABs (`span\t<start>\t<end>`), so **only the FIRST TAB delimits
key from value**.

| # | key | class | | # | key | class | | # | key | class |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `name` | scalar | | 13 | `retgt` | scalar | | 24 | `super` | set |
| 2 | `vis` | scalar | | 14 | `fnsig` | scalar | | 25 | `tflags` | scalar |
| 3 | `kind` | scalar | | 15 | `gparam` | seq | | 26 | `iof` | scalar |
| 4 | `span` | scalar | | 16 | `where` | set | | 27 | `ifor` | scalar |
| 5 | `src` | scalar | | 17 | `in` | seq | | 28 | `iflags` | scalar |
| 6 | `parent` | scalar | | 18 | `out` | seq | | 29 | `cty` | scalar |
| 7 | `cfg` | scalar | | 19 | `fieldty` | scalar | | 30 | `cval` | scalar |
| 8 | `attr` | set | | 20 | `recform` | scalar | | 31 | `auto` | set |
| 9 | `deprecated` | scalar | | 21 | `recfield` | seq | | 32 | `type` | scalar |
| 10 | `alias` | set | | 22 | `vform` | scalar | | 33 | `link` | set |
| 11 | `doc` | seq | | 23 | `vdiscr` | scalar | | 34 | `lfact` | set |
| 12 | `dlink` | set | | | | | | | | |

21 scalar + 8 set + 5 seq = 34 field keys; key 0 is the magic line. This table
becomes `nudox_f1::registry::REGISTRY` (§3.2) — the single source of truth for
both the serializer and the pairer.

---

## §2. Vendoring: mechanism, layout, fork hygiene

### §2.1 Mechanism: direct source copy (not `cargo vendor`)

Unchanged from Rev 1. The fork MUST be a direct source copy of
`libpijul-1.0.0-beta.11` committed into the repository; `cargo vendor` is
rejected (checksum-managed mirrors fight hand edits).

```sh
SRC=~/.local/share/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/libpijul-1.0.0-beta.11
mkdir -p workspace/vendor
cp -R "$SRC" workspace/vendor/libpijul
rm -f workspace/vendor/libpijul/.cargo-checksum.json
# Commit VERBATIM as its own commit ("vendor: libpijul 1.0.0-beta.11 pristine")
# before any fork edit. The pristine commit is the diff base for §2.4.
```

### §2.2 Crate layout and naming

- Path: `workspace/vendor/libpijul/`. NOT added to `[workspace] members` — it
  builds as a path dependency with its own edition (2021) and lints.
- **Crate name stays `libpijul`** — zero churn in `use libpijul::…` across the
  workspace, clean upstream diffs.
- Version MUST be bumped to `1.0.0-beta.11+nudox.1` (build metadata is ignored
  in semver comparison, so the existing `=1.0.0-beta.11` requirement still
  matches) and incremented (`+nudox.2`, …) on each fork-diff revision.

### §2.3 Dependency swap

Workspace root `Cargo.toml` gains:

```toml
[patch.crates-io]
libpijul = { path = "workspace/vendor/libpijul" }
```

That is the entire swap. The lockfile MUST contain exactly one `libpijul`
(path-sourced) and exactly one `diffs` (0.5.1, shared by the fork and
`nudox-f1`). CI asserts both (§9 T-11).

### §2.4 The fork diff: two files, enumerated

The complete fork diff versus the pristine import MUST be exactly:

| # | File | Change |
|---|---|---|
| F-1 | `LIB/src/diff/diff.rs` | The F1 dispatch + bridge, ~30 lines (§5, verbatim). |
| F-2 | `LIB/Cargo.toml` | Version `+nudox.1`; add `nudox-f1 = { path = "../../nudox-f1" }`. |

Nothing else. In particular the fork MUST NOT touch `record.rs`,
`diff/mod.rs`, `diff/delete.rs`, `diff/replace.rs`, `diff/vertex_buffer.rs`,
`change.rs`, `apply/`, `unrecord/`, `pristine/`, `output/`, `alive/`, or
`lib.rs`. (Rev 1's F-1…F-5 enumeration is void.)

A regenerable patch MUST be maintained at `workspace/vendor/libpijul-fork.patch`:

```sh
git diff <pristine-import-commit> -- workspace/vendor/libpijul > workspace/vendor/libpijul-fork.patch
```

CI applies it to a scratch copy of the pristine commit on every PR touching
`workspace/vendor/` (bitrot tripwire). Ad-hoc upstream cherry-picks into the
fork are forbidden; any PR growing the fork diff beyond F-1/F-2 requires an ADR.

### §2.5 Pin-bump re-verify checklist (normative)

On ANY future move off beta.11, in order:

1. Re-import pristine new version as its own commit; `git apply
   libpijul-fork.patch`; resolve.
2. Re-verify every §1 citation, specifically: `diff::diff` signature and its
   single caller at mod.rs:189; `D`/`Replacement` field sets (all `pub`);
   `impl diffs::Diff for D` semantics incl. `stop_early`; the
   `diffs::Replace` adapter grammar; `Line.l` field and `Line::eq`;
   the lowering loop shape and the `local.line == from_new + 1` pairing rule;
   `is_deleted`'s binary search by `old`; `DEFAULT_SEPARATOR == "\n"`;
   `has_binary_files` set only in `add_file`;
   `output_repository_no_pending` returning `BTreeSet<Conflict>`.
3. Re-run the full §9 suite including byte goldens (T-8/T-9). A pin bump that
   changes change-hash bytes is a **breaking store event** — treat as a schema
   epoch decision, never wave through.
4. Bump `+nudox.N`, regenerate the patch, update this document's citations.

---

## §3. `workspace/nudox-f1` — the F1 format crate

### §3.1 Character and placement

A new leaf crate holding the two things both planes must agree on: **the frozen
key registry** and **the pairing function**. It is pure data + pure functions
over byte slices: no libpijul types, no graph awareness, no I/O.

Why a new crate and not `nudox-ir-diff`: the fork must link the pairer, and
`nudox-ir-diff` drags `nudox-ir` + `nudox-change` + serde/postcard/blake3 into
any dependent — the wrong closure and the wrong altitude for the engine. The
pairer is *format* knowledge, not IR-table knowledge. `nudox-f1` depends on
`diffs` alone.

```
workspace/nudox-f1/
  Cargo.toml
  lib.rs          — crate docs, `pub mod registry; pub mod pair;`, re-exports,
                    `#[cfg(test)] mod tests;`
  registry.rs     — MAGIC_LINE, Class, KeySpec, KEY_* consts, REGISTRY,
                    key_of, class_of
  pair.rs         — LineReplace, line_diff, internals (§4)
  tests.rs
```

```toml
# workspace/nudox-f1/Cargo.toml
[package]
name = "nudox-f1"
edition.workspace = true
version.workspace = true
license.workspace = true

[lints]
workspace = true

[lib]
path = "lib.rs"

[dependencies]
# Pinned to the exact version libpijul resolves (its req is "0.5"), so both
# consumers run the same Myers. MIT/Apache-2.0.
diffs = "=0.5.1"
```

Add `"workspace/nudox-f1"` to `[workspace] members` in the root `Cargo.toml`.

### §3.2 The registry moves here (single source of truth)

Move the `// Key registry constants` block (`nudox-ir-vcs/f1.rs:40-74`) into
`nudox-f1/registry.rs`, extended with the class table:

```rust
// workspace/nudox-f1/registry.rs

/// The complete version-1 magic line, including its trailing newline.
/// This is the sniff constant: the fork dispatches to field pairing iff a
/// file's first line is byte-equal to this. A future format bump (`NdIrF1\t2`)
/// deliberately fails the sniff and records via stock Myers until this crate
/// learns the new registry.
pub const MAGIC_LINE: &[u8] = b"NdIrF1\t1\n";

pub const KEY_MAGIC: &str = "NdIrF1"; // key 0 — magic header
pub const KEY_NAME: &str = "name";    // 1
/* … all 34 constants exactly as in f1.rs:41-74 … */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Scalar, // 0 or 1 line; a value change is a rewrite
    Set,    // 0..n lines, byte-sorted; members are independent, never rewrites
    Seq,    // 0..n lines, declaration order; position-respecting LCS
}

pub struct KeySpec {
    pub name: &'static str,
    pub class: Class,
}

/// Keys 1–34 in frozen registry (= canonical emission) order, per §1.6.
/// Key 0 (magic) is intentionally absent: `class_of` treats it, like every
/// unknown key, as `Seq`.
pub const REGISTRY: [KeySpec; 34] = [
    KeySpec { name: KEY_NAME, class: Class::Scalar },
    KeySpec { name: KEY_VIS, class: Class::Scalar },
    /* … the §1.6 table, in order … */
];

/// The key of a raw F1 line: the bytes before the FIRST tab (values may
/// contain further tabs). A line with no tab is its own key.
pub fn key_of(line: &[u8]) -> &[u8] {
    match line.iter().position(|&b| b == b'\t') {
        Some(i) => &line[..i],
        None => line,
    }
}

/// Class of a key. Unknown keys — the magic line, malformed lines, future
/// registry additions — diff as `Seq`: position-respecting LCS is the safest
/// total behavior for content this table does not describe.
pub fn class_of(key: &[u8]) -> Class {
    for spec in REGISTRY.iter() {
        if spec.name.as_bytes() == key {
            return spec.class;
        }
    }
    Class::Seq
}
```

`nudox-ir-vcs` changes (mechanical, part of P-1):

- `Cargo.toml`: add `nudox-f1 = { path = "../nudox-f1" }`.
- `f1.rs`: delete the local constants block; add
  `pub use nudox_f1::registry::{KEY_MAGIC, KEY_NAME, /* … all 35 … */};` so the
  crate's public surface is unchanged. Replace the serializer's magic literal
  (`out.push_str("NdIrF1\t1\n")`) with a `str::from_utf8(MAGIC_LINE)`-backed
  const or keep it byte-identical via the re-export — either way the bytes MUST
  come from `nudox_f1`.
- Nothing else in `nudox-ir-vcs` changes. `session.rs` and `repo.rs` are not
  touched in any phase of this plan.

### §3.3 The pairing interface (frozen)

```rust
// workspace/nudox-f1/pair.rs

/// One span replacement in line coordinates: `old_len` lines of the old blob
/// starting at `old` are replaced by `new_len` lines of the new blob starting
/// at `new`. `old_len == 0` is a pure insertion anchored before old line
/// `old`; `new_len == 0` is a pure deletion. Field-for-field identical to
/// libpijul's `diff::Replacement` — by design, not coincidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineReplace {
    pub old: usize,
    pub old_len: usize,
    pub new: usize,
    pub new_len: usize,
}

/// Field-aware pairing of two line arrays. Total, pure, deterministic:
/// defined for EVERY input (F1 or not, canonical or corrupt), never errors,
/// and depends only on the argument bytes. Output is in Replace-normal form
/// (§4.5). Lines carry their trailing separator, exactly as pijul splits them.
pub fn line_diff(a: &[&[u8]], b: &[&[u8]]) -> Vec<LineReplace>;
```

### §3.4 Dependency directions (frozen)

```
nudox-ir-vcs ──▶ nudox-f1 ──▶ diffs        (registry + pairing, engine-neutral)
fork (libpijul) ──▶ nudox-f1               (the ONLY nudox crate the fork sees)
```

`nudox-f1` MUST NOT depend on `libpijul`, `nudox-ir-diff`, `nudox-ir`, or
`nudox-change` — it stays a leaf so the fork's closure stays minimal and the
pairer remains testable and reusable (semantic plane, future engines) without
pijul. The interface between `nudox-f1` and the fork is **plain data**
(`Vec<LineReplace>`), not the `diffs::Diff` trait, so no cross-crate trait
unification is required; `diffs` is an internal implementation detail on both
sides, pinned to one version by §3.1. CI enforces all of this (§9 T-11).

---

## §4. The pairing algorithm (normative, complete)

`line_diff` runs four stages. Stages A–C produce a stream of *micro-ops*
(`LineReplace` values at run or line granularity, ascending); stage D
normalizes the stream into Replace-normal form. Total code: ~200 lines.

### §4.1 Stage A — runs

Group each side into **runs**: maximal spans of consecutive lines sharing the
same `key_of` bytes.

```rust
struct Run<'x> {
    key: &'x [u8],
    start: usize, // first line index
    len: usize,   // number of lines
}

fn runs<'x>(lines: &[&'x [u8]]) -> Vec<Run<'x>> {
    let mut out: Vec<Run<'x>> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let key = crate::registry::key_of(line);
        match out.last_mut() {
            Some(r) if r.key == key => r.len += 1,
            _ => out.push(Run { key, start: i, len: 1 }),
        }
    }
    out
}
```

In a canonical blob every run is one key's whole section (magic line = its own
run, since `NdIrF1` appears once). In a corrupt blob runs are whatever they
are — the algorithm does not care.

### §4.2 Stage B — run alignment (Myers over key tokens)

Align the two run sequences with `diffs::myers`, comparing runs **by key bytes
only**. On canonical inputs (both sides emitted in frozen registry order) this
degenerates to the identity walk over shared sections; on non-canonical inputs
it is still a well-defined LCS.

```rust
enum RunOp {
    Equal { ra: usize, rb: usize, len: usize },
    Delete { ra: usize, len: usize, rb: usize },
    Insert { ra: usize, rb: usize, len: usize },
    Replace { ra: usize, ra_len: usize, rb: usize, rb_len: usize },
}

struct RunAlign { ops: Vec<RunOp> }

impl diffs::Diff for RunAlign {
    type Error = std::convert::Infallible;
    fn equal(&mut self, ra: usize, rb: usize, len: usize) -> Result<(), Self::Error> {
        self.ops.push(RunOp::Equal { ra, rb, len }); Ok(())
    }
    fn delete(&mut self, ra: usize, len: usize, rb: usize) -> Result<(), Self::Error> {
        self.ops.push(RunOp::Delete { ra, len, rb }); Ok(())
    }
    fn insert(&mut self, ra: usize, rb: usize, len: usize) -> Result<(), Self::Error> {
        self.ops.push(RunOp::Insert { ra, rb, len }); Ok(())
    }
    fn replace(&mut self, ra: usize, ra_len: usize, rb: usize, rb_len: usize)
        -> Result<(), Self::Error>
    {
        self.ops.push(RunOp::Replace { ra, ra_len, rb, rb_len }); Ok(())
    }
}
```

Driven by (`keys_a`/`keys_b` are `Vec<&[u8]>` of per-run keys; the callbacks
report absolute run indices because we pass full ranges — the same calling
convention pijul itself uses at diff.rs:28-53 [V]):

```rust
let mut align = RunAlign { ops: Vec::new() };
diffs::myers::diff(&mut align, &keys_a[..], 0, keys_a.len(),
                   &keys_b[..], 0, keys_b.len())
    .unwrap(); // Infallible
```

Expansion of run-level ops into micro-ops (all anchors follow the `diffs`
convention: a deletion carries the current new-side cursor, an insertion the
current old-side cursor):

- `Equal { ra, rb, len }` → for each of the `len` paired runs, per-class
  section pairing (§4.3). (`runs_a[ra+k].key == runs_b[rb+k].key` by
  construction.)
- `Delete { ra, len, rb }` → one micro per old run `r` in `ra..ra+len`:
  `{ old: runs_a[r].start, old_len: runs_a[r].len, new: line_start_b(rb), new_len: 0 }`
  where `line_start_b(rb)` = `runs_b.get(rb).map(|r| r.start).unwrap_or(b.len())`.
- `Insert { ra, rb, len }` → one micro per new run `r` in `rb..rb+len`:
  `{ old: line_start_a(ra), old_len: 0, new: runs_b[r].start, new_len: runs_b[r].len }`.
- `Replace { ra, ra_len, rb, rb_len }` (keys differ — a section was swapped for
  a different section) → delete-micros for the old runs anchored at
  `line_start_b(rb)`, then insert-micros for the new runs anchored at the line
  *after* the deleted old region. Stage D fuses the whole cluster into one
  entry: an honest "this region was rewritten" hunk.

### §4.3 Stage C — per-class section pairing

For a paired section (`sa` = old lines `sa.start..sa.start+sa.len`, `sb`
likewise), dispatch on `class_of(sa.key)`:

**Scalar** — positional pairing. Canonically both sides have 1 line.

```rust
let n = sa.len.min(sb.len);
for k in 0..n {
    if a[sa.start + k] != b[sb.start + k] {
        out.push(LineReplace { old: sa.start + k, old_len: 1,
                               new: sb.start + k, new_len: 1 });
    }
}
for k in n..sa.len { // old tail (non-canonical), anchored at section end
    out.push(LineReplace { old: sa.start + k, old_len: 1,
                           new: sb.start + sb.len, new_len: 0 });
}
for k in n..sb.len { // new tail
    out.push(LineReplace { old: sa.start + sa.len, old_len: 0,
                           new: sb.start + k, new_len: 1 });
}
```

A changed scalar value is thus a `{1,1}` micro → a `Hunk::Replacement` (a
*rewrite*, §1.4) — the correct semantic for a scalar.

**Set** — sorted merge-join on full line bytes. Canonical sets are emitted
byte-sorted (§1.6), so equal members meet and unequal members are independent
insert/delete micros — **two differing members are never paired as a rewrite**.
On unsorted (corrupt) input the walk still terminates deterministically, merely
pairing less.

```rust
let (mut i, mut j) = (0, 0);
while i < sa.len && j < sb.len {
    let (la, lb) = (a[sa.start + i], b[sb.start + j]);
    match la.cmp(lb) {
        Ordering::Equal => { i += 1; j += 1; }
        Ordering::Less => { // member only in old
            out.push(LineReplace { old: sa.start + i, old_len: 1,
                                   new: sb.start + j, new_len: 0 });
            i += 1;
        }
        Ordering::Greater => { // member only in new, at its sort position
            out.push(LineReplace { old: sa.start + i, old_len: 0,
                                   new: sb.start + j, new_len: 1 });
            j += 1;
        }
    }
}
while i < sa.len { out.push(/* delete, new anchor = sb.start + sb.len */); i += 1; }
while j < sb.len { out.push(/* insert, old anchor = sa.start + sa.len */); j += 1; }
```

**Seq** (and every unknown key, including the magic line) — section-scoped
Myers on full line bytes, reusing the same `diffs` driver with the section's
absolute ranges; callbacks arrive in absolute line coordinates:

```rust
struct MicroSink<'o> { out: &'o mut Vec<LineReplace> }

impl<'o> diffs::Diff for MicroSink<'o> {
    type Error = std::convert::Infallible;
    fn delete(&mut self, old: usize, len: usize, new: usize) -> Result<(), Self::Error> {
        self.out.push(LineReplace { old, old_len: len, new, new_len: 0 }); Ok(())
    }
    fn insert(&mut self, old: usize, new: usize, new_len: usize) -> Result<(), Self::Error> {
        self.out.push(LineReplace { old, old_len: 0, new, new_len }); Ok(())
    }
    fn replace(&mut self, old: usize, old_len: usize, new: usize, new_len: usize)
        -> Result<(), Self::Error>
    {
        self.out.push(LineReplace { old, old_len, new, new_len }); Ok(())
    }
}

diffs::myers::diff(&mut MicroSink { out }, a, sa.start, sa.start + sa.len,
                   b, sb.start, sb.start + sb.len)
    .unwrap();
```

The magic line needs no special case: it is a 1-line unknown-key section on
both sides — equal lines emit nothing; a version bump emits one `{1,1}` micro
(a `Replacement` hunk on line 0), which is exactly right.

### §4.4 Why micro-ops must be merged: the one-vertex-per-gap law

Stage (3) resolves an insertion's up-context to the old line above its anchor
even when another entry deletes that line (§1.4 fact 2). Two entries with
`new_len > 0` anchored in the same gap between surviving old lines would emit
two `NewVertex` atoms with no order edge between them — an order-ambiguous
graph. Stock Myers structurally never produces that shape, because the
`diffs::Replace` adapter only flushes on `equal` (§1.4 fact 3). The engine's
native grain is therefore: **one entry — hence at most one `NewVertex` — per
gap between surviving lines.** The pairer MUST honor it. This is the single
place where field grain yields to graph grain: when two *adjacent* lines of
different fields both change, they share one hunk, exactly as good Myers output
would — and stage (3) is only ever fed shapes it has digested since upstream
day one.

### §4.5 Stage D — normalization to Replace-normal form

**Replace-normal form (frozen output contract):** entries sorted strictly
ascending in both `old` and `new`; non-overlapping; `old_len + new_len ≥ 1`;
and between any two consecutive entries at least one line survives on each
side (`next.old > prev.old + prev.old_len` and `next.new > prev.new +
prev.new_len`).

Stages A–C emit micros ascending in both coordinates with contiguous-cursor
anchors, so normalization is one fold:

```rust
fn normalize(micros: Vec<LineReplace>) -> Vec<LineReplace> {
    let mut out: Vec<LineReplace> = Vec::new();
    for m in micros {
        if m.old_len == 0 && m.new_len == 0 {
            continue;
        }
        if let Some(t) = out.last_mut() {
            debug_assert!(m.old >= t.old + t.old_len);
            debug_assert!(m.new >= t.new + t.new_len);
            if m.old == t.old + t.old_len && m.new == t.new + t.new_len {
                // No surviving line between the spans on either side:
                // graph-mandated merge (§4.4).
                t.old_len += m.old_len;
                t.new_len += m.new_len;
                continue;
            }
        }
        out.push(m);
    }
    out
}
```

The `debug_assert`s document a structural invariant of stages A–C (anchors are
monotone cursors regardless of input content); they are internal-consistency
checks, not input validation.

### §4.6 Laws (all property-tested in `nudox-f1/tests.rs`, §9)

- **L-1 Totality.** `line_diff` is defined for every pair of line arrays and
  never panics (release builds). No parsing, no validation, no error type.
- **L-2 Round-trip.** Applying the output to `a` reconstructs `b` exactly:
  replay entries ascending, copying unpaired old spans and substituting
  replacement spans. (This is what makes "any pairing is a correct record"
  true; the test applier is ~15 lines.)
- **L-3 Determinism.** Pure function of the argument bytes; canonical micro
  order + deterministic Myers ⇒ byte-stable output across runs, platforms, and
  opt levels.
- **L-4 Normal form.** Output satisfies §4.5's contract (checkable in O(n)).
- **L-5 Field locality.** Every entry either (a) pairs lines within a single
  key section on both sides, or (b) is a §4.5 merge of adjacent class-(a)
  micros, or (c) covers a §4.2 `Replace` cluster (sections swapped wholesale).
  In particular: a set member is never *paired* against a line of another key,
  and seq LCS never escapes its section. This is the Depth-1 guarantee Myers
  cannot make.

### §4.7 Worked example

Old blob (canonical order: `name`(1), `vis`(2), `attr`(8), `doc`(11)):

```
0  NdIrF1\t1
1  name\tparse
2  vis\tpub
3  attr\tinline
4  attr\tmust_use
5  doc\tParses a string.
```

New blob — `doc` edited; `attr\tcold` added (sorts before `inline`):

```
0  NdIrF1\t1
1  name\tparse
2  vis\tpub
3  attr\tcold
4  attr\tinline
5  attr\tmust_use
6  doc\tParses a string slice.
```

Stage A/B: runs `[NdIrF1, name, vis, attr, doc]` on both sides → all `Equal`.
Stage C micros:

```
attr (set):  { old: 3, old_len: 0, new: 3, new_len: 1 }   // insert "cold" at sort position
doc  (seq):  { old: 5, old_len: 1, new: 6, new_len: 0 }   // Myers delete…
             { old: 6, old_len: 0, new: 6, new_len: 1 }   // …+ adjacent insert
```

Stage D: the two doc micros are adjacent on both sides → merged; the attr micro
is separated from them by surviving lines 3–4 → kept apart. Final `D`:

```
E1 { old: 3, old_len: 0, new: 3, new_len: 1 }
E2 { old: 5, old_len: 1, new: 6, new_len: 1 }
```

Stage (3) lowers E1 → `Hunk::Edit { NewVertex("attr\tcold\n") }` anchored
between `vis` and `attr\tinline`; E2 → `Hunk::Replacement {
EdgeMap(doc line), NewVertex("doc\tParses a string slice.\n") }`. Two hunks,
each confined to one field. (Myers often gets small cases like this right too —
per the parent plan §6.6, F1's key prefixes keep it mostly honest. Depth-1's
win is the L-5 *guarantee*, pinned adversarially by T-1/T-12.)

---

## §5. The fork edit (F-1, verbatim)

All of it lives in `LIB/src/diff/diff.rs`. Three additions: one `use`, one
two-line dispatch at the top of `diff()`, two small functions at the bottom.

```rust
// ── near the top of LIB/src/diff/diff.rs ────────────────────────────────────
use super::Line; // (already present)

// ── inside `pub(super) fn diff`, as the FIRST statements ───────────────────
pub(super) fn diff(
    lines_a: &[Line],
    lines_b: &[Line],
    algorithm: Algorithm,
    stop_early: bool,
) -> D {
    // NUDOX FORK: canonical NdIrF1 blobs take the field-aware pairing.
    // Same D out, same lowering downstream; `algorithm` is deliberately
    // ignored for these files.
    if is_f1(lines_a) || is_f1(lines_b) {
        return f1_diff(lines_a, lines_b, stop_early);
    }
    let result = D {
        /* … stock body continues UNCHANGED … */
```

```rust
// ── at the bottom of LIB/src/diff/diff.rs ──────────────────────────────────

/// NUDOX FORK. A file is F1 iff its first line is byte-equal to the frozen
/// version-1 magic line. Exact equality makes the sniff version-safe (an
/// `NdIrF1\t2` blob records via stock Myers until nudox-f1 learns v2) and
/// chunk-safe (an 8192-byte binary chunk can never equal the 9-byte line).
fn is_f1(lines: &[Line]) -> bool {
    lines
        .first()
        .map_or(false, |l| l.l == nudox_f1::registry::MAGIC_LINE)
}

/// NUDOX FORK. Field-grained pairing for F1 blobs: bridge `nudox_f1::line_diff`
/// (plain data, engine-neutral) into pijul's own `D`. `stop_early` keeps the
/// stock observable behavior — a short-circuited D with at most one entry.
fn f1_diff(lines_a: &[Line], lines_b: &[Line], stop_early: bool) -> D {
    let a: Vec<&[u8]> = lines_a.iter().map(|l| l.l).collect();
    let b: Vec<&[u8]> = lines_b.iter().map(|l| l.l).collect();
    let mut r: Vec<Replacement> = nudox_f1::pair::line_diff(&a, &b)
        .into_iter()
        .map(|e| Replacement {
            old: e.old,
            old_len: e.old_len,
            new: e.new,
            new_len: e.new_len,
        })
        .collect();
    if stop_early {
        r.truncate(1);
    }
    D { r, stop_early }
}
```

That is the complete behavioral fork. Notes, each grounded in §1:

- Dispatch precedes the `algorithm` match, so `Myers`/`Patience`/
  `ImaraHistogram` are all bypassed identically; our callers' `Algorithm::
  default()` needs no change.
- `D`/`Replacement` construction is plain struct literals — all fields are
  `pub` ([V] §1.3) — and entries arrive already in Replace-normal form, sorted
  by `old`, satisfying `is_deleted`'s binary search.
- Encoding is untouched: F1 files reach here via the text split (§1.2), the
  detected `Some(Encoding)` is stamped on hunks by stage (3) exactly as for
  Myers, and `has_binary_files` cannot be affected (§1.1).
- A file *leaving* F1 form (or arriving corrupt) still sniffs via whichever
  side carries the magic line and pairs totally; a file that never had the
  magic line is invisible to the fork — byte-identical behavior to upstream.

`LIB/Cargo.toml` (F-2):

```toml
version = "1.0.0-beta.11+nudox.1"

[dependencies.nudox-f1]
path = "../../nudox-f1"
```

---

## §6. Commutation and conflict grain

The fork changes how `D` entries are *chosen*, never how atoms compose. Apply,
unrecord, merge, and output are untouched (§2.4), so pijul's CRDT algebra is
inherited wholesale. What Depth-1 changes is the grain:

1. **Field-line independence ⇒ commutation.** Two changes touching different
   F1 lines of the same symbol (separated by ≥1 surviving line) produce atoms
   over disjoint vertex spans — guaranteed by L-5 + §4.5, no longer
   Myers-dependent. Doc edit ∥ signature edit, param edit ∥ attr edit commute
   by construction (parent §12.1).
2. **Per-field conflict grain.** Concurrent edits to the same scalar line
   conflict on exactly that line ⇒ `ConflictOnField{intro, key}` (parent
   §12.2). Cross-field *mispairing* is structurally impossible (L-5); the only
   multi-field hunks are honest ones (adjacent edits §4.4, section swaps §4.2).
3. **Set inserts anchor at true sort positions** (§4.3), so concurrent inserts
   of different members usually have different neighbors and commute cleanly;
   same-neighbor collisions remain auto-normalizable order conflicts the
   materializer already sorts out (parent §12.2 row 2).
4. **Record-time conflict machinery stays inert** on the IR path (§1.4 fact 4)
   — and if a marker-bearing file ever *were* recorded, the pairer is total and
   the stock lowering handles markers exactly as it does under Myers.
5. **Unrecord/merge untouched.** The wire is standard `Edit`/`Replacement`
   over `NewVertex`/`EdgeMap`; `unrecord` inverts structured hunks exactly as
   Myers hunks (T-10), and cross-channel apply composes them identically (T-7).

---

## §7. Determinism and reproducibility

1. `line_diff` is a pure function with canonical emission order (L-3). Same
   blob pair ⇒ same entries, on any replica.
2. Stage (3) is a deterministic fold over entries: `contents` appends happen in
   entry order, vertex resolution reads only the pristine graph. Same entries +
   same pristine ⇒ same `actions` + same `contents` ⇒ same change bytes and
   hash via `make_change` (§1.5).
3. Therefore two replicas recording the same working-copy state over the same
   channel state produce **bit-identical change files and hashes** (T-9) —
   the Depth-0 law, intact.
4. **Cross-version note (normative).** The pairing algorithm is part of the
   recorded shape: binaries before and after P-3 produce differently-grained
   (both valid) changes for the same edit. Changes are synced as data, so mixed
   fleets stay *correct*, but any workflow relying on independent replicas
   deriving hash-identical changes requires the record plane to upgrade in
   lockstep per store. Deploy P-3 to all recorders of a store atomically; the
   same rule applies to any future `nudox-f1` algorithm revision (bump
   goldens deliberately, §2.5 step 3 spirit).

---

## §8. Sequencing

| Phase | Work | Depends on | Exit criteria |
|---|---|---|---|
| **P-1** `nudox-f1` | Crate per §3 (registry move + `line_diff` per §4) + `nudox-ir-vcs` re-export edit; pure test suite (L-1…L-5 properties, goldens, fuzz-vs-apply) | Depth-0 only — starts now | `cargo test -p nudox-f1 -p nudox-ir-vcs` green; F1 byte-goldens of the serializer unchanged (registry move is invisible) |
| **P-2** Pristine vendor | §2.1 import commit; `[patch.crates-io]`; zero fork edits | — | full existing suite green against the vendored pristine tree; T-11(a,b) lockfile asserts green |
| **P-3** Fork edit | F-1 + F-2 (§5); `record_shape_tests.rs`; patch file generated; goldens re-pinned | P-1, P-2 | full §9 suite green; T-1/T-12 reports committed to `docs/research/ir-vcs/` |

Each phase lands green and independently; P-1 and P-2 are behavior-inert.
There is no flag phase, no stub phase, and no cleanup phase — P-3 *is* the
flip, per §7.4's lockstep rule. Trigger posture is unchanged from the parent
plan §11: P-1/P-2 MAY land ahead of the Depth-1 trigger (both inert); P-3
SHOULD wait for it unless directed otherwise.

---

## §9. Acceptance suite

Pure-crate tests live in `nudox-f1/tests.rs`; engine tests in a new
`#[cfg(test)] mod record_shape_tests;` in `nudox-ir-vcs` (matching the
existing `tests.rs`/`size_tests.rs` pattern). "Myers twin" = the same blob pair
with the magic line replaced by an inert non-magic line, which routes the
record through stock Myers — the A/B lever, no flag needed.

| ID | Test | Pass criterion |
|---|---|---|
| **T-0** | **Pairer laws (P-1).** Property tests over generated canonical blobs + mutations + arbitrary byte-line garbage. | L-1 (no panic), L-2 (apply-replay reconstructs `b` byte-exactly), L-3 (repeat-call byte equality), L-4 (normal-form checker), L-5 (section-locality checker on pre-merge micros); goldens for fixed fixtures. |
| **T-1** | **A/B misalignment harness.** Symbol with a 40-line `doc` seq + `attr` set; edit doc line 20 AND add `attr\tcold` AND change `vis`; record via F1 path and via Myers twin; inspect `rec.actions`. | F1 side: exactly 3 hunks, each within one key section (1 `Replacement` doc, 1 `Edit{NewVertex}` attr, 1 `Replacement` vis). Myers side's cross-field hunks (if any) recorded in a committed report. |
| **T-2** | **Per-shape hunks.** One minimal blob pair per §1.4 table row × class: scalar change, scalar add, scalar remove, set add, set remove, seq change, seq add, seq remove. | Hunk variant + atom kinds match the §1.4 table exactly; every hunk stamped `Some(encoding)`. |
| **T-3** | **Mixed session.** One record touching an F1 file and a plain README. | F1 file's hunks are field-grained (per T-2 shapes); README records via stock Myers unchanged (compare against pre-fork golden). |
| **T-4** | **Paranoia corpus.** Depth-0.5 corpus (every escape, huge docs, non-ASCII names, 10k params) recorded via F1 path. | Record + apply + `output_repository_no_pending` reproduces every blob byte-exactly, zero conflicts; `has_binary_files == false` throughout. |
| **T-5** | **Total-on-garbage.** Working copies with corrupt F1 bodies behind a valid magic line: no-TAB lines, unknown keys, unsorted sets, duplicate scalars, missing final newline. | Record succeeds, applies, round-trips byte-exactly; deterministic across two runs. (No fallback exists to test — totality is the property.) |
| **T-6** | **Single-vertex sub-split.** `add_file` a 10-line F1 file (one vertex), then record a 2-line field edit. | Emitted `EdgeMap` edges reference sub-vertex positions; apply + output reproduces the new blob byte-exactly, zero conflicts. |
| **T-7** | **Commute pinning.** Parent §12.1 branch/merge table (doc∥sig, param∥attr, two set-adds) recorded via the F1 path. | Same conflict/no-conflict verdicts as the documented expectations; merged materialized bytes equal. |
| **T-8** | **Determinism goldens.** Fixed fixture session; change-file bytes + hash pinned. | Byte-identical across runs, platforms, debug/release. |
| **T-9** | **Replica reproducibility.** Two independent repos, same channel state, same F1 edit, same binary. | Bit-identical change hash. |
| **T-10** | **Unrecord round-trip.** Record a multi-field change, `unrecord`, re-output, re-record. | Working copy + pristine byte-identical to pre-record; re-record hash-identical to T-8 golden. |
| **T-11** | **Layering asserts (CI).** (a) lockfile has exactly one `libpijul`, path-sourced; (b) exactly one `diffs`, `0.5.1`; (c) `cargo tree -p nudox-f1` contains no `libpijul`/`nudox-ir`/`nudox-change`; (d) fork patch applies to pristine scratch; (e) fork diff touches only F-1/F-2 paths. | All mechanical checks green. |
| **T-12** | **Hunk-quality metric.** Over the replayed Depth-0 fixture corpus: per change, count hunks that *pair* lines across different keys (excluding §4.4 adjacent merges and §4.2 section swaps, which are structurally identifiable from the blob pair). | F1 path: identically 0 (this is L-5 at the wire). Myers-twin baseline recorded alongside T-1's report (parent P11 exit criterion). |

Suite gate: P-3 MUST NOT merge with any T red; T-1/T-12 artifacts MUST be
committed.

---

## §10. Non-goals (explicit, normative)

- **No new atom kinds, no new hunk variants, no fourth `Algorithm` variant in
  the public enum.** Dispatch is by content sniff; the vocabulary is
  `Atom::{NewVertex, EdgeMap}` / `Hunk::{Edit, Replacement}` (K-Fork-Depth;
  Depth-2 stays behind an ADR).
- **No second apply engine** (Depth −1 stays rejected, parent §11).
- **No hook trait, no `FrameDelta`, no runtime fallback protocol, no store
  flag** — Rev 1's machinery is superseded, not deferred. If someone proposes
  reintroducing any of it, the answer is an ADR against §0's totality argument.
- **No fork edits beyond F-1/F-2** — `record.rs`, `mod.rs`, `delete.rs`,
  `replace.rs`, `vertex_buffer.rs`, `apply/`, `unrecord/`, `pristine/`,
  `output/`, `alive/` are frozen upstream code.
- **No Myers removal.** Myers remains the pairer for every non-magic file and
  for unknown F1 versions (§5 sniff).
- **No derive-based diff dependency** ([D] §C verdict stands).
- **No F1 format change.** Depth-1 consumes the frozen Depth-0 registry; any
  registry change is a versioned format event (new magic line, new sniff, new
  table) — a separate plan.
