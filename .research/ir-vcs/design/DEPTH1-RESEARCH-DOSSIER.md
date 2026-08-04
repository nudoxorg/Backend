# DEPTH-1 RESEARCH DOSSIER: libpijul §11 Vendored Structured Record

**Topic:** Feed field-grained F1 hunks into libpijul's change/apply engine without
new atom kinds or a second apply engine. Keep Myers on the TEXT path; bypass it only
for our canonical F1 format. Hook site lives inside a fork of libpijul; all
`pub(crate)` machinery is therefore accessible.

**Source:** libpijul-1.0.0-beta.11 at
`~/.local/share/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/libpijul-1.0.0-beta.11/`
(hereafter `LIB`). Our code at `/Users/philocalyst/Projects/Backend/workspace/`.

---

## KEY FACTS (top-of-mind for the planner)

- `output_repository_no_pending` returns `BTreeSet<Conflict>` (confirmed:
  `LIB/src/output/output.rs:108`). Our `repo.rs` checks `!conflicts.is_empty()` and
  propagates `VcsError::ConflictedState`.
- `decode_file` is a trait-default on `WorkingCopyRead`
  (`LIB/src/working_copy/mod.rs:22-36`). The encoding decision lives inside that
  default; the hook point is **right after `decode_file` returns**, before `diff()` is
  called (`LIB/src/record.rs:1229-1234`).
- `encoding.is_none()` → binary path (`ROLLING_SIZE = 8192` chunks);
  `encoding.is_some()` → TEXT/line-split path. Forcing `encoding = Some(UTF_8)` lands
  us on the text path: `LIB/src/diff/mod.rs:169-178`.
- The `diff()` method on `Recorded` takes `&Option<Encoding>` and passes it all the
  way into `delete()` / `replace()`, which stamp it on `Hunk::Edit` and
  `Hunk::Replacement`. No encoding munging after atoms are created.
- `Hunk::Replacement { change: Atom::EdgeMap(…), replacement: Atom::NewVertex(…) }` is
  the natural fit for scalar-field replace (delete old vertex, insert new vertex).
  `Hunk::Edit { change: Atom::EdgeMap(…) }` alone is pure deletion (set-remove /
  seq-remove). `Hunk::Edit { change: Atom::NewVertex(…) }` is pure insertion
  (set-insert / seq-append).
- The entire `delete.rs` / `replace.rs` context-construction machinery is
  `pub(super)` — only reachable from inside the `diff` module. This **mandates** the
  hook be inside the fork, not in a wrapper.
- `FrameDelta` (nudox-ir-diff) has no current F1-line API; it diff's
  `PristineIntroTable` objects at the payload level (`diff.rs:59-146`). The
  line-class diffing for FrameDelta must be hand-rolled.
- `diff-struct` generates per-field diff structs via a derive macro; not a match for
  our use: F1 values are `String`/`Vec<u8>` token-encoded blobs, not typed scalar
  fields. Verdict: hand-rolled F1 line-class diffing is cleaner (see §D).

---

## A. libpijul seam — exact file:line citations

### A1. `Recorded` struct (`LIB/src/record.rs:87-109`)

```rust
// LIB/src/record.rs:87
pub struct Recorded {
    /// The "byte contents" of the change.
    pub contents: Arc<Mutex<Vec<u8>>>,
    /// The current records, to be later converted into change operations.
    pub actions: Vec<Hunk<Option<ChangeId>, LocalByte>>,
    /// The updates that need to be made to the ~tree~ and ~revtree~
    /// tables when this change is applied to the local repository.
    pub updatables: HashMap<usize, InodeUpdate>,
    /// The size of the largest file that was recorded in this change.
    pub largest_file: u64,
    /// Whether we have recorded binary files.
    pub has_binary_files: bool,
    /// Timestamp of the oldest changed file.
    pub oldest_change: std::time::SystemTime,
    /// Redundant edges found during the comparison.
    pub redundant: Vec<crate::alive::Redundant>,
    // ... (private fields omitted)
}
```

Key fields for the structured-record hook:
- `contents: Arc<Mutex<Vec<u8>>>` — flat byte store; every `NewVertex.start`/`end`
  are `ChangePosition` offsets into this buffer. The structured hook pushes F1 field
  bytes here.
- `actions: Vec<Hunk<…>>` — we push `Hunk::Edit` / `Hunk::Replacement` directly.
- `updatables: HashMap<usize, InodeUpdate>` — populated by `add_file` for brand-new
  files; NOT touched for content diffs.
- `has_binary_files` — must remain `false` when we force UTF-8.

### A2. `Builder::record` signature (`LIB/src/record.rs:280-300`)

```rust
// LIB/src/record.rs:280
pub fn record<
    T,
    W: WorkingCopyRead + Clone + Send + Sync + 'static,
    C: ChangeStore + Clone + Send + 'static,
>(
    &mut self,
    txn: ArcTxn<T>,
    diff_algorithm: diff::Algorithm,
    stop_early: bool,
    diff_separator: &regex::bytes::Regex,       // ← DEFAULT_SEPARATOR = "\n"
    channel: ChannelRef<T>,
    working_copy: &W,
    changes: &C,
    prefix: &str,
    _n_workers: usize,
) -> Result<(), RecordError<C::Error, W::Error, T>>
```

We call this from `record_and_apply_with_meta` (`session.rs:578-591`) and
`record_generation` (`repo.rs:544-557`) with `Algorithm::default()` and
`&libpijul::DEFAULT_SEPARATOR` (which is `b"\n"`).

### A3. `record_nondeleted` call chain (`LIB/src/record.rs:1154-1264`)

The call sequence for existing files:

```
Builder::record                       (record.rs:280)
  └─ Recorded::record_existing_file   (record.rs:1091)
       └─ Recorded::record_nondeleted (record.rs:1154)
            ├─ working_copy.decode_file(&item.full_path, &mut b)  ← line 1229
            └─ self.diff(…, &b, &encoding, diff_sep)              ← line 1234
```

**Injection point** — lines 1223-1247:

```rust
// LIB/src/record.rs:1223-1247
if new_meta.is_file()
    && (self.force_rediff
        || modified_since_last_commit(…)?)
{
    let mut ret = {
        let txn = txn.read();
        let channel = channel.read();
        retrieve(&*txn, txn.graph(&*channel), vertex, false)?
    };
    let mut b = Vec::new();
    let encoding = working_copy                       // ← line 1229
        .decode_file(&item.full_path, &mut b)
        .map_err(RecordError::WorkingCopy)?;          // ← line 1231
    debug!("diffing…");
    let len = self.actions.len();
    self.diff(                                        // ← line 1234
        changes,
        txn,
        channel,
        diff_algorithm,
        stop_early,
        item.full_path.clone(),
        item.inode,
        vertex.to_option(),
        &mut ret,
        &b,
        &encoding,
        diff_sep,
    )?;
```

**Fork hook:** After line 1231, if `item.full_path` matches `symbols/{hex}.nir`
(our naming convention), override `encoding = Some(Encoding(encoding_rs::UTF_8))`,
call our `structured_diff(self, &ret, &b, &item, …)` instead of `self.diff(…)`, and
`continue` (skip the stock `self.diff` call). The `StructuredDiff` function pushes
directly into `self.actions` and `self.contents`.

### A4. `Recorded::diff` method — binary vs text branch (`LIB/src/diff/mod.rs:147-221`)

```rust
// LIB/src/diff/mod.rs:147
pub(crate) fn diff<T: ChannelTxnT, P: ChangeStore>(
    &mut self,
    changes: &P,
    txn: &ArcTxn<T>,
    channel: &ChannelRef<T>,
    algorithm: Algorithm,
    stop_early: bool,
    path: String,
    inode_: Inode,
    inode: Position<Option<ChangeId>>,
    a: &mut Graph,          // ← the "old" side from pristine
    b: &[u8],               // ← the "new" side (working copy bytes)
    encoding: &Option<Encoding>,
    separator: &regex::bytes::Regex,
) -> Result<(), DiffError<P::Error, T>>
```

The binary/text branch:

```rust
// LIB/src/diff/mod.rs:169
let (lines_a, lines_b) = if encoding.is_none() {
    const ROLLING_SIZE: usize = 8192;
    debug!("contents_a: {:?}", d.contents_a.len());
    let (ah, old) = bin::make_old_chunks(ROLLING_SIZE, &d.contents_a);
    let (bb, new) = bin::make_new_chunks(ROLLING_SIZE, &ah, &b);
    (old, new)
} else {
    (make_old_lines(&d, separator), make_new_lines(&b, separator))
};
```

Then:

```rust
// LIB/src/diff/mod.rs:189-218
let dd = diff::diff(&lines_a, &lines_b, algorithm, stop_early);
let mut conflict_contexts = replace::ConflictContexts::new();
for r in 0..dd.len() {
    if dd[r].old_len > 0 {
        self.delete(…)?;
    }
    if dd[r].new_len > 0 {
        self.replace(…);
    }
}
```

For the structured hook, we bypass `diff()` entirely and call `self.delete()` and
`self.replace()` directly with pre-computed spans, using `vertex_buffer::Diff` (d) we
construct ourselves via the same `output_graph` call.

### A5. `vertex_buffer::Diff` struct (`LIB/src/diff/vertex_buffer.rs:7-17`)

```rust
// LIB/src/diff/vertex_buffer.rs:7
pub(super) struct Diff {
    pub inode: Position<Option<ChangeId>>,
    pub path: String,
    pub contents_a: Vec<u8>,   // ← old side bytes (from pristine)
    pub pos_a: Vec<Vertex>,    // ← byte-offset → graph vertex table
    pub missing_eol: HashSet<usize>,
    pub marker: HashMap<usize, ConflictMarker>,
    conflict_stack: Vec<Conflict>,
    pub conflict_ends: Vec<ConflictEnds>,
    pub cyclic_conflict_bytes: Vec<(usize, usize)>,
}
```

Constructor (`LIB/src/diff/vertex_buffer.rs:57-84`):

```rust
// LIB/src/diff/vertex_buffer.rs:57
impl Diff {
    pub fn new(
        inode: Position<Option<ChangeId>>,
        path: String,
        graph: &crate::alive::Graph,
    ) -> Self { … }
}
```

Populated by `output_graph(changes, txn, channel, &mut d, a, &mut self.redundant)?`
(called at `LIB/src/diff/mod.rs:164`). After this call, `d.contents_a` holds the
pristine bytes (what libpijul currently stores) and `d.pos_a` maps byte offsets to
graph vertices.

Key method `Diff::vertex(i, pos, end_pos)` (`LIB/src/diff/vertex_buffer.rs:87-110`):

```rust
// LIB/src/diff/vertex_buffer.rs:87
pub fn vertex(
    &self,
    i: usize,
    pos: usize,
    end_pos: usize,
) -> crate::pristine::Vertex<ChangeId>
```

This slices a `Vertex` out of `pos_a[i]` adjusted by byte offsets. The structured
diff needs this to resolve which graph vertex(es) cover a given F1 line span.

`first_vertex_containing(pos)` and `last_vertex_containing(pos)` are both `pub`
(`LIB/src/diff/vertex_buffer.rs:288-311`): these are the two methods to locate the
vertex for a given byte position, needed when computing `up_context` / `down_context`
for field-level atoms.

### A6. `delete.rs` — private context machinery (`LIB/src/diff/delete.rs`)

```rust
// LIB/src/diff/delete.rs:12
impl Recorded {
    pub(super) fn delete<T: GraphTxnT>(…) -> Result<(), TxnErr<T::GraphError>>
```

`delete` is `pub(super)`, meaning only code inside the `diff` module can call it.
It internally calls `delete_lines` (private fn at line 51) which calls
`delete_parents` (private fn at line 170). The structured diff can either:

1. Call `self.delete(…)` directly (from inside the fork's `diff` module), or
2. Hand-construct `Hunk::Edit { change: Atom::EdgeMap(EdgeMap { edges, inode }) }` and
   push it without going through `delete()`. Option (2) is feasible because
   `delete_parents` (line 170-208) is purely a graph walk to build `deletion.edges:
   Vec<NewEdge<Option<ChangeId>>>` — we can replicate that logic or inline it as a
   helper in the fork's `diff` module.

The structured hook function must live in a new file inside `src/diff/` (e.g.
`src/diff/structured.rs`) to access `pub(super)` items.

### A7. `replace.rs` — `ConflictContexts` and `replace()` (`LIB/src/diff/replace.rs`)

```rust
// LIB/src/diff/replace.rs:10
pub struct ConflictContexts {
    pub up: HashMap<usize, ChangePosition>,
    pub side_ends: HashMap<usize, Vec<ChangePosition>>,
    pub active: HashSet<usize>,
    pub reorderings: HashMap<usize, ChangePosition>,
}
```

```rust
// LIB/src/diff/replace.rs:29
impl Recorded {
    pub(super) fn replace(
        &mut self,
        diff: &Diff,
        conflict_contexts: &mut ConflictContexts,
        lines_a: &[Line],
        lines_b: &[Line],
        inode: Inode,
        dd: &D,
        r: usize,
        encoding: &Option<Encoding>,
    )
```

For **insertion** (new field that was absent): push a
`Hunk::Edit { change: Atom::NewVertex(…) }` with the new field bytes in
`self.contents`.

For **replacement** (scalar field changed value): push
`Hunk::Replacement { change: Atom::EdgeMap(…), replacement: Atom::NewVertex(…) }`.

Both `up_context` and `down_context` in `NewVertex` must reference the surrounding
vertices in `d.pos_a`. The stock `get_up_context` / `get_down_context`
(`replace.rs:125-331`) are `pub(super)`. For non-conflict files (F1 files never have
conflict markers), the fast paths in those functions reduce to simple vertex lookups
on `d.pos_a`.

### A8. `Hunk` / `Atom` enum (`LIB/src/change.rs:74-807`)

```rust
// LIB/src/change.rs:74
pub enum Atom<Change> {
    NewVertex(NewVertex<Change>),
    EdgeMap(EdgeMap<Change>),
}

// LIB/src/change.rs:80
pub struct NewVertex<Change> {
    pub up_context: Vec<Position<Change>>,
    pub down_context: Vec<Position<Change>>,
    pub flag: EdgeFlags,
    pub start: ChangePosition,   // byte offset into contents
    pub end: ChangePosition,     // exclusive
    pub inode: Position<Change>,
}

// LIB/src/change.rs:90
pub struct EdgeMap<Change> {
    pub edges: Vec<NewEdge<Change>>,
    pub inode: Position<Change>,
}
```

```rust
// LIB/src/change.rs:739-807  (BaseHunk enum — Hunk<H,L> = BaseHunk<Atom<H>,L>)
pub enum BaseHunk<Atom, Local> {
    FileAdd   { add_name, add_inode, contents: Option<Atom>, path, encoding },
    FileDel   { del, contents: Option<Atom>, path, encoding },
    FileUndel { undel, contents: Option<Atom>, path, encoding },
    FileMove  { del, add, path },
    SolveNameConflict   { name, path },
    UnsolveNameConflict { name, path },
    Edit        { change: Atom, local: Local, encoding: Option<Encoding> },
    Replacement { change: Atom, replacement: Atom, local: Local, encoding: Option<Encoding> },
    SolveOrderConflict   { change, local },
    UnsolveOrderConflict { change, local },
    ResurrectZombies     { change, local, encoding },
    AddRoot { name, inode },
    DelRoot { name, inode },
}
```

Mapping F1 line classes to Hunk variants:

| Operation            | Hunk variant                                      |
|----------------------|---------------------------------------------------|
| Scalar field changed | `Replacement { EdgeMap(del), NewVertex(ins) }`   |
| Scalar field added   | `Edit { NewVertex(ins) }`                        |
| Scalar field removed | `Edit { EdgeMap(del) }`                          |
| Set element added    | `Edit { NewVertex(ins) }`                        |
| Set element removed  | `Edit { EdgeMap(del) }`                          |
| Seq element changed  | `Replacement { EdgeMap(del), NewVertex(ins) }`   |
| Seq element added    | `Edit { NewVertex(ins) }`                        |
| Seq element removed  | `Edit { EdgeMap(del) }`                          |

This is correct because `Replacement` is what `replace.rs:88-99` builds when the
Myers diff has both a deletion and an insertion at the same position — semantically
identical to what we want for scalar overwrite.

### A9. `make_change` signature (`LIB/src/change.rs:1363-1395`)

```rust
// LIB/src/change.rs:1363
pub fn make_change<T: ChannelTxnT + DepsTxnT<DepsError = <T as GraphTxnT>::GraphError>>(
    txn: &T,
    channel: &ChannelRef<T>,
    changes: Vec<Hunk<Option<Hash>, Local>>,
    contents: Vec<u8>,
    header: ChangeHeader,
    metadata: Vec<u8>,
) -> Result<Self, MakeChangeError<T>>
```

The `changes` and `contents` come directly from `Recorded.actions` (globalized via
`.globalize()`) and `*Recorded.contents.lock()`. The structured hook pushes into the
same `Recorded` fields before globalization, so `make_change` sees exactly the same
wire as the text path.

### A10. `decode_file` and `get_valid_encoding` (`LIB/src/working_copy/mod.rs:22-36`, `LIB/src/lib.rs:744-758`)

```rust
// LIB/src/working_copy/mod.rs:22
fn decode_file(
    &self,
    file: &str,
    buffer: &mut Vec<u8>,
) -> Result<Option<Encoding>, Self::Error> {
    let init = buffer.len();
    self.read_file(&file, buffer)?;
    let mut detector = EncodingDetector::new();
    detector.feed(&buffer[init..], true);
    if let Some(e) = crate::get_valid_encoding(&detector, None, true, &buffer[init..]) {
        Ok(Some(Encoding(e)))
    } else {
        Ok(None)
    }
}
```

```rust
// LIB/src/lib.rs:744
pub(crate) fn get_valid_encoding(
    enc: &chardetng::EncodingDetector,
    tld: Option<&[u8]>,
    allow_utf8: bool,
    buffer: &[u8],
) -> Option<&'static encoding_rs::Encoding>
```

Our F1 files are pure ASCII + our own escape sequences (`\t`, `\n`, `\\`, `\xNN`).
`chardetng` will detect them as UTF-8 (`Encoding(encoding_rs::UTF_8)`) because ASCII
is a strict subset. So in practice `decode_file` already returns
`Some(Encoding(UTF_8))` for our symbol files. The fork hook that forces
`encoding = Some(UTF_8)` is technically redundant for F1 files but is needed as a
guard in case a corrupt/binary file slips through.

**Side-effect warning:** Forcing `encoding = Some(UTF_8)` also sets
`self.has_binary_files |= encoding.is_none()` (record.rs:1010 in `add_file`; in
`record_nondeleted` the flag is set at `add_file` time only, not at diff time). No
side effect from forcing encoding at the diff site.

---

## B. Our record path

### B1. `record_and_apply_with_meta` (`workspace/nudox-ir-vcs/session.rs:570-655`)

```rust
// session.rs:578-591
let mut builder = Builder::new();
builder
    .record(
        txn.clone(),
        Algorithm::default(),
        false,
        &libpijul::DEFAULT_SEPARATOR,
        channel.clone(),
        self.repo.working_copy_ref(),
        self.repo.changes_ref(),
        "",
        1,
    )
    .map_err(|e| VcsError::Pijul(anyhow::anyhow!("record: {e}")))?;

let rec = builder.finish();
```

The `Builder::record` call is single-threaded (`n_workers=1`, and the loop at
`record.rs:306` iterates `0..0`, so all work is done inline in the sequential
`record_single_thread` path at `record.rs:556`).

### B2. `record_generation` record path (`workspace/nudox-ir-vcs/repo.rs:544-612`)

```rust
// repo.rs:544-557
let mut builder = Builder::new();
builder
    .record(
        txn.clone(),
        Algorithm::default(),
        false,
        &libpijul::DEFAULT_SEPARATOR,
        channel.clone(),
        &self.working_copy,
        &self.changes,
        "",
        1,
    )
    .map_err(…)?;

let rec = builder.finish();
```

After `builder.finish()` (repo.rs:559):
- `rec.actions` — globalized by `.globalize(&*txn.read())` → `Vec<Hunk<Option<Hash>, Local>>`
- `rec.contents.lock()` — taken via `std::mem::take` → `Vec<u8>`
- `rec.updatables` — passed to `apply_local_change`

These three fields are the complete hand-off to `make_change` (repo.rs:582-595).

### B3. `StructuredRecord` hook thread-through

The hook needs these parameters from `record_nondeleted`:
- `&mut self` — the `Recorded` being built
- `a: &mut Graph` — the pristine graph (for `output_graph` to fill `Diff`)
- `&b: &[u8]` — the new working-copy bytes (post-`decode_file`)
- `item.full_path: String` — to detect `symbols/` prefix
- `item.inode: Inode`
- `vertex: Position<ChangeId>` — inode vertex in graph
- `txn`, `channel` — for `output_graph`

A minimal fork diff at `record_nondeleted` (lines 1229-1247) looks like:

```rust
// FORK PATCH in LIB/src/record.rs after line 1231
let encoding = working_copy
    .decode_file(&item.full_path, &mut b)
    .map_err(RecordError::WorkingCopy)?;
// ── STRUCTURED HOOK ──────────────────────────────────────────────────────
if crate::diff::structured::is_f1_path(&item.full_path) {
    let forced_enc = Some(crate::text_encoding::Encoding(encoding_rs::UTF_8));
    let len = self.actions.len();
    {
        let txn = txn.read();
        let channel = channel.read();
        crate::diff::structured::structured_diff(
            self, changes, &*txn, txn.graph(&*channel),
            diff_algorithm, &mut ret, &b, &forced_enc,
            item.full_path.clone(), item.inode, vertex.to_option(),
        )?;
    }
    // skip self.diff(…) for this file
    // … (oldest_change update same as stock path)
    continue; // or goto-equivalent via early return from helper
}
// ── END HOOK ─────────────────────────────────────────────────────────────
debug!("diffing…");
let len = self.actions.len();
self.diff(…)?;
```

The new `src/diff/structured.rs` file lives inside the `diff` module, giving it
access to `pub(super)` items like `delete()`, `replace()`, `ConflictContexts::new()`,
`get_up_context`, `get_down_context`, `Diff`, and the `Line` type.

### B4. F1 line structure API (`workspace/nudox-ir-vcs/f1.rs`)

The F1 format (f1.rs:1-17 header comment):
```text
NdIrF1\t1\n
<key>\t<value>\n    ← one line per logical field value
```

Line separator is `b'\n'`; field separator within a line is `b'\t'`. Keys are emitted
in frozen registry order (keys 0–34, f1.rs:40-74). The `DEFAULT_SEPARATOR` in
libpijul is `b"\n"` — so each F1 key+value pair is exactly ONE libpijul "line."

This means:
- **scalar field** → 0 or 1 lines with key `<K>\t<V>`
- **set field** → 0..n lines with key `<K>\t<V>`, sorted by raw bytes
- **seq field** → 0..n lines with key `<K>\t<V>`, in declaration order

A `FrameDelta` (field-level diff) must compare two F1 blobs line-by-line and emit
per-line edit ops. The natural data structure:

```rust
enum FieldOp {
    Unchanged { line_idx_a: usize, line_idx_b: usize },
    Deleted   { line_idx_a: usize },   // → Atom::EdgeMap
    Inserted  { line_idx_b: usize },   // → Atom::NewVertex
    Replaced  { line_idx_a: usize, line_idx_b: usize }, // → Atom::EdgeMap + NewVertex
}
```

Line indices map into `d.pos_a` (old side) and `b` (new side) byte ranges via the
`bytes_pos` / `bytes_len` helpers (mod.rs:227-250), which are module-private but can
be moved into `structured.rs` or re-derived trivially.

The F1 registry defines which keys are scalar/set/seq (f1.rs:40-74):
- **scalar** (0 or 1 occurrence): `name, vis, kind, span, src, parent, cfg, deprecated, retgt, fnsig, recform, vform, vdiscr, tflags, iof, ifor, iflags, cty, cval, type`
- **set** (0..n, sorted): `attr, alias, dlink, where, super, auto, link, lfact`
- **seq** (0..n, ordered): `doc, gparam, in, out, recfield`

For **set** fields: the correct diff strategy is set-subtract (sorted order is
canonical in F1). Two lines from the same key that differ in value are NOT
"replacements" — they are independent set members. So set changes yield only
`Delete` + `Insert` ops, never `Replace`.

For **scalar** fields: a value change is always `Replace` (delete old, insert new).

For **seq** fields: an LCS on `(key, value)` pairs finds insertions/deletions/moves.
Moves within the same seq key are represented as a delete+insert pair (libpijul has
no "move" atom; the apply engine re-assembles order via edge contexts).

---

## C. `diff-struct` crate verdict

**Crate:** `diff-struct` — derive-based trait for computing typed diffs between structs.

**Trait model:** `A.diff(&B) -> ADiff`, where `ADiff` has one field per field of `A`,
each field being the diff of that field type. `A.apply(ADiff) -> A`.

**Supported types:** integers, floats, bool, char, String, Option, Box, Rc, Arc,
HashMap, BTreeMap, HashSet, BTreeSet, Vec, tuples (up to 18), arrays.

**Verdict: NOT useful for our use case.** Reasons:

1. Our F1 "fields" are not Rust struct fields — they are `(key_str, encoded_value_str)`
   lines in a byte buffer. The types are `String` or `Vec<u8>`, so `diff-struct` would
   only tell us "the String changed" — not which subpart of the encoded value.

2. We need to compare F1 **lines** as libpijul sees them (byte ranges in
   `d.contents_a` vs `b`) to generate `Atom::EdgeMap` and `Atom::NewVertex` with
   correct `ChangePosition` offsets. `diff-struct` knows nothing about libpijul graph
   positions.

3. The set/seq ordering semantics are specific to the F1 registry. `diff-struct`'s
   Vec diff is ordered-LCS, which is correct for seq fields but would misclassify set
   fields as reordered when elements are the same.

4. Adding `diff-struct` to the fork adds a dependency not currently present.

**Conclusion:** Hand-roll an `F1LineDiff` that:
- Parses two F1 blobs into `Vec<(key, value_bytes)>` line lists.
- For each key slot in registry order, applies set-subtract (for set keys), scalar-compare (for scalar keys), or LCS (for seq keys).
- Returns `Vec<FieldOp>` with byte offsets into the old and new blobs.
- Maps each `FieldOp` to calls against `Recorded::delete()` / `Recorded::replace()`,
  or directly constructs `Hunk::Edit` / `Hunk::Replacement` atoms.

This is ~200-300 lines and has no external dependencies beyond what the fork already
has.

---

## D. Open questions and gotchas

### D1. Vertex-offset arithmetic specifics

`Diff::vertex(i, pos, end_pos)` (`vertex_buffer.rs:87-110`) adjusts `v.start` and
`v.end` based on `pos - self.pos_a[i].pos`. If the F1 line boundary does not align
with a vertex boundary, the vertex is split. This is the normal Myers behavior for
partial-vertex line edits; in our case it works correctly because F1 lines end with
`\n` (a full line in the pristine is always a full vertex or a series of vertices).

**Gotcha:** A pristine vertex may span multiple F1 lines if the file was ever stored
as a single-chunk blob (e.g. from the very first `FileAdd`). In that case
`first_vertex_containing(pos)` will find the same vertex `i` for every old line. The
`Diff::vertex(i, pos, end_pos)` call with sub-vertex offsets handles this correctly
by adjusting `.start`/`.end`. Verify this with a test: record a 10-line F1 file as a
new file (single vertex), then update 2 lines and confirm the EdgeMap references
sub-vertex positions.

### D2. `add_file` and the initial single-vertex file

`Recorded::add_file` (`record.rs:991-1089`) calls `decode_file` internally (line
1009), reads the entire file into `contents` as one chunk, and creates a single
`Hunk::FileAdd { contents: Some(Atom::NewVertex { start, end }) }` that spans the
whole file. The initial recording of a new F1 symbol file produces exactly **one
vertex** covering all bytes. Subsequent diffs that edit individual lines must split
this vertex, which `Diff::vertex` handles via sub-vertex offset arithmetic. The
structured hook is only active during `record_existing_file` (not `add_file`), so
this path is unaffected.

### D3. `Hunk::Replacement` vs `Hunk::Edit` for set-insert vs scalar-replace

As shown in §A8, `Hunk::Replacement` carries both a deletion atom and an insertion
atom. The `replace.rs` code builds `Replacement` by popping the just-pushed `Edit`
(`replace.rs:88-99`). For the structured hook, we must NOT use `Replacement` for
set-insert (which has no corresponding deletion); only for scalar-overwrite and
seq-element-change. Using `Edit { NewVertex }` for pure insert is correct.

### D4. Encoding-forcing side effects

Forcing `encoding = Some(UTF_8)` has no side effects on `has_binary_files` (that flag
is set at `add_file` time from the encoding returned by `decode_file`, not at diff
time). The `encoding` value is just stamped on the output `Hunk::Edit` /
`Hunk::Replacement` for display purposes. Setting it to `Some(UTF_8)` is correct
because F1 files ARE UTF-8 (or ASCII, a strict subset).

### D5. `ConflictContexts` initialization for F1 files

F1 files never have conflict markers (they are always output clean from libpijul's
`output_repository_no_pending`, and we check `!conflicts.is_empty()` before
proceeding). Therefore `ConflictContexts::new()` can be constructed once and reused
across all field ops for the same file — the `up`, `side_ends`, `active`,
`reorderings` maps will never be populated. The `get_up_context` and
`get_down_context` fast paths for non-conflict content (`None` marker branch, no
conflict stack entries) will always fire.

### D6. `DEFAULT_SEPARATOR` vs F1 TAB separator

`DEFAULT_SEPARATOR` is `b"\n"` (diff/mod.rs:17-18). F1 uses `\n` as line separator
(one `key\tvalue\n` per line). The TAB between key and value is NOT a separator —
it is part of the line content. The structured diff must NOT change `diff_separator`;
the stock `\n`-split is correct for F1.

### D7. `diff-struct` and F1 line escaping

F1 `ascii.rs` implements byte-oriented escape/unescape (`escape`, `unescape`). These
are independent of libpijul's encoding detection. The `Encoding` type in libpijul
wraps `encoding_rs::Encoding` and is used only for display metadata — the actual byte
comparison in the structured diff is on raw bytes (escaped), not decoded strings. No
interaction.

### D8. `output_repository_no_pending` conflict return

`LIB/src/output/output.rs:108`: returns `Result<BTreeSet<Conflict>, …>`. Our
`materialize` and `index_from_channel` both call it and check `!conflicts.is_empty()`
before parsing F1 blobs. This invariant remains valid after the structured-diff hook
because the hook does not change how libpijul resolves conflicts during output — it
only changes how we generate atoms during record.

### D9. `Inode` vs `Position<ChangeId>` in structured diff call

`record_nondeleted` has both `item.inode: Inode` (tree inode) and
`vertex: Position<ChangeId>` (content vertex). The `diff()` method takes both
(`inode_: Inode` and `inode: Position<Option<ChangeId>>`). `Diff::new` takes the
`Position<Option<ChangeId>>` (`inode.to_option()`). `Recorded::delete` takes `inode:
Inode` for the `LocalByte` struct. The structured hook must pass both correctly.

---

## Summary of injection architecture

```
FORK: src/diff/structured.rs  (new file, inside diff module)
  fn is_f1_path(path: &str) -> bool
  fn structured_diff<T, P>(
      rec: &mut Recorded,
      changes: &P,
      txn: &T,
      graph: &T::Graph,
      algorithm: Algorithm,
      a: &mut Graph,           // pristine graph
      b: &[u8],                // new file bytes
      encoding: &Option<Encoding>,
      path: String,
      inode_: Inode,
      inode: Position<Option<ChangeId>>,
  ) -> Result<(), DiffError<P::Error, T>>

  Steps:
  1. Construct Diff::new(inode, path.clone(), a)
  2. output_graph(changes, txn_ref, channel_ref, &mut d, a, &mut rec.redundant)?
     [NB: output_graph requires txn + channel; thread them through the hook args]
  3. Parse d.contents_a into Vec<(key, value_bytes, byte_range_in_a)>
  4. Parse b into Vec<(key, value_bytes, byte_range_in_b)>
  5. For each key slot in registry order, diff per line class (scalar/set/seq)
  6. Map FieldOps to calls on rec.delete() / rec.replace(), or direct Hunk push
  7. Return Ok(())

FORK PATCH: src/record.rs after line 1231, inside record_nondeleted
  → if is_f1_path, call structured_diff, skip self.diff()
```

No new `Atom` variants. No new `Hunk` variants. No second apply engine. The apply
engine sees standard `Edit`/`Replacement` hunks with `EdgeMap`/`NewVertex` atoms
exactly as if Myers had produced them.
