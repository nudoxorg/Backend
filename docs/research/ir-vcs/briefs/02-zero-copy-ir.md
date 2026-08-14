# Zero-Copy IR Storage for Multi-Language Documentation IR

> **Status (Rev 3.2):** Research context, non-normative — feeds `design/IR-NATIVE-VCS-DESIGN.md` (sectional archive, IR-only yoke). The "optional dual-write during migration" suggestion in §7.1 is superseded by K1/K28 (greenfield IR plane, no dual-write; migration only via versioned formats). The `IrPatch` sketch in §4.4 was not adopted — `Change<IrAtom>` is the production delta form (K4).

**Research date:** 2026-07-16  
**Scope:** Total zero-copy options for nudox typed IR (`Index`, `Entry` enums, nested `Type` with 25+ variants, `HashMap`/`PathBuf`/`String`-heavy graphs). Decision: **rkyv vs custom POD layout vs Cap'n Proto**.  
**Extends:** [`docs/research/librarification/13-storage/PLAN.md`](/Users/philocalyst/Projects/Backend/docs/research/librarification/13-storage/PLAN.md) §3.6 / §5.4 / §7.2 (rkyv mentioned as optional for IR mmap; postcard + zstd+dict as current default).  
**Codebase anchors:** `workspace/ir/entry.rs`, `workspace/ir/kind.rs`, `workspace/ir/ty.rs`, `workspace/registry/blob/mod.rs`.

---

## 0. Problem framing (IR-specific)

Nudox IR is not a simple POD table. The live types look like:

- `Index { root_ids: Vec<NudoxPath>, entries_by_path: FxHashMap<NudoxPath, Entry> }`
- `NudoxPath` = enum over `PathBuf` + optional dependency name
- `Entry` ≈ 15 tagged variants (`Module`, `RecordType`, `Function`, `SumType`, …) wrapping `Symbol<T>`
- `Type` ≈ **25+** nested variants (`TypeReference`, `Tuple`, `BorrowedRef`, `Conditional`, `Mapped`, `TemplateLiteral`, …)
- Heavy use of `String`, `Option`, nested `Vec`, `HashMap`/`HashSet`, and recursive `Box<Type>`

Access patterns (from PLAN.md + product intent):

1. **Whole-generation load** — search indexing, lineage emit, full package open.
2. **Path → single Entry** — GUI symbol open, go-to-definition, hover.
3. **Type graph walk** — signature rendering, subtype queries (usually from one Entry outward).
4. **CAS immutability** — generation blobs are content-addressed (`blake3(raw)`); never mutate in place after publish.
5. **VCS / patch generations** — package versions are snapshots; cross-version dedupe is at file/IR-blob grain, not field-level patches inside one archive.

**Goal of this brief:** choose a wire/on-disk form that can (a) mmap, (b) resolve path→entry without full allocate-and-build, (c) stay trustworthy under validation, (d) compose with CAS + optional patch chains, without painting us into an unversionable format.

**Existing plan stance (PLAN.md):** postcard IR + zstd dictionary is the production default; rkyv is “only if IR mmap profiling demands it”; tree projections use custom POD (`.ndxt`), not rkyv. This document goes deeper on the zero-copy side and makes a sharper IR decision.

---

## 1. rkyv — total zero-copy model

### 1.1 What “total zero-copy” means

David Koloski’s architecture post ([rkyv architecture and internals](https://david.kolo.ski/blog/rkyv-architecture/), Nov 2020) and the rkyv book ([Zero-copy deserialization](https://rkyv.org/zero-copy-deserialization.html)) distinguish:

| Kind | What is borrowed | What still happens |
|---|---|---|
| **Partial** (serde + bincode/postcard with `Cow`) | Mostly string/bytes | Structure still parsed; scalars often copied; allocations for containers |
| **Total** (rkyv / Cap’n Proto / FlatBuffers class) | Entire object graph | Access = align + cast (+ optional validation); no graph reconstruction |

rkyv’s guarantee (book wording): *no data is copied during deserialization and no work is done to deserialize data* — achieved by making the on-disk layout **identical** to the archived in-memory layout. Practical consequence: `mmap` a file → (optionally validate) → cast root pointer → use `ArchivedIndex` like a normal structure.

Koloski’s core mechanism is **relative pointers**: instead of absolute addresses (invalid after reload), store an offset from the pointer’s own location to the payload. Archived strings look like `{ rel_ptr: i32, len: u32 }` pointing into the same buffer. Serialization is two-phase (`Archive` writes dependencies first; `Resolve` fills relative pointers once positions are known) so arrays of structs pack contiguously without interleaving payloads.

### 1.2 Archived types

Deriving `Archive` generates parallel types:

```rust
// Live
struct Index {
    root_ids: Vec<NudoxPath>,
    entries_by_path: HashMap<NudoxPath, Entry>,
}

// Conceptual archived form (generated)
// ArchivedIndex {
//   root_ids: ArchivedVec<ArchivedNudoxPath>,
//   entries_by_path: ArchivedHashMap<ArchivedNudoxPath, ArchivedEntry>,
// }
```

Important properties for IR:

- **Enums** get `#[repr(u8|u16|…)]` discriminant of smallest width that fits all variants ([Format](https://rkyv.org/format.html)). A 25-variant `Type` is fine; rkyv is designed for open Rust type systems, not schema IDL limits.
- **HashMap** → `ArchivedHashMap` (Swiss-table style, target-independent FxHash) with lookup/iteration performance comparable to std ([docs.rs/rkyv overview](https://docs.rs/rkyv)).
- **BTreeMap** available when ordered keys matter.
- Nested recursive enums (`Type` containing `Box<Type>` / `Vec<Type>`) archive as relative pointers into the same buffer — **well suited** to IR type graphs.
- `PathBuf` / `String` become archived strings (UTF-8 bytes + length).
- `Rc`/`Arc` supported with pooling ([Shared pointers](https://rkyv.org/shared-pointers.html)); first encounter serializes payload, later encounters reuse location. Weak pointers upgrade or become null.

**Suitability for 20+ variant Type enum:** High. Unlike FlatBuffers/Cap’n Proto, you do not rewrite a `.fbs`/`.capnp` schema and fight codegen for every IR tweak. You `#[derive(Archive, Serialize, Deserialize)]` on existing Rust types (or thin archive-facing twins). Complex enums are a *strength* of rkyv’s open type system ([Feature comparison](https://rkyv.org/feature-comparison.html)).

Caveat: the API surface becomes dual — call sites that want zero-copy deal with `ArchivedEntry` / `ArchivedType`, not `Entry` / `Type`. Full owned round-trip (`deserialize`) still exists but reintroduces allocation (benchmarks show deserialize is not free).

### 1.3 Validation

With the `bytecheck` feature ([Validation](https://rkyv.org/validation.html)):

- `access::<ArchivedT, E>(buf)` walks the archive, checking:
  - pointers stay inside the buffer
  - alignment is correct
  - enough space for pointed-to objects
  - **subtree ranges** enforce a tree ownership model (prevents recursion bombs and illegal aliasing)
- Validation is **upfront** (whole root), not lazy field-by-field like Cap’n Proto’s optional model.
- Shared pointers have extra rules: same object cannot be shared under incompatible types.

rkyv FAQ notes that even with validation, total cost is often still below full traditional deserialize. Trusted CAS blobs (self-hashing `blake3`) can skip validation after header/version checks — same pattern as filkoll (see §2.7).

**Progressive / partial validation** remains an open research area even for the author (noted in Koloski’s crate ideas as “progresso”). For IR, if we need single-Entry load, **do not rely on progressive rkyv validation of a monolithic archive** — design the file so the Entry body is a separately addressable, fully validatable blob (see §3).

### 1.4 mmap

Total zero-copy + relative pointers = mmap-native:

```rust
let file = File::open(path)?;
// SAFETY: writer uses atomic rename; file is immutable CAS content
let mmap = unsafe { memmap2::Mmap::map(&file)? };
let archived = rkyv::access::<ArchivedIndex, rancor::Error>(&mmap[..])?;
// or access_unchecked after magic/version/type_hash + blake3 verify
```

Alignment: rkyv wants properly aligned buffers; metadata prefixes must not break alignment (Iggy’s experience: 16-byte alignment typically enough; or enable `unaligned` feature at a cost). PLAN.md already marks rkyv IR as “Yes — ideal” for mmap (§5.4).

### 1.5 Cap’n Proto / FlatBuffers comparison

From rkyv’s own matrix ([Feature comparison](https://rkyv.org/feature-comparison.html)):

| Feature | rkyv | Cap’n Proto | FlatBuffers |
|---|---|---|---|
| Open type system (native Rust) | yes | no | no |
| Schema evolution | **no** | yes | yes |
| Zero-copy | yes | yes | yes |
| Random-access reads | yes | yes | yes |
| Validation | upfront | on-demand | yes |
| Schema language | `#[derive]` | custom IDL | custom IDL |
| Hash maps / B-trees | **yes** | no | no |
| Shared pointers | **yes** | no | no |
| Cross-language | no | yes | yes |
| Unset fields take wire space | yes | yes | no |

Cap’n Proto ([Kenton Varda’s comparison](https://capnproto.org/news/2014-06-17-capnproto-flatbuffers-sbe.html)): relative pointers like C structs; random access without parsing; schema evolution via field numbers; multi-language. Rust support exists but is heavier IDL-driven and historically lagged on mutation/access ergonomics vs rkyv.

FlatBuffers: vtable offsets per table; optional fields cheap; strong for games/assets; **serialization of highly structured data is notoriously slow** (Koloski’s `minecraft_savedata` bench: FlatBuffers serialize worst of the set — even behind JSON).

**For nudox IR specifically:** we are Rust-first for the producer/consumer of IR blobs; multi-language clients consume JSON/CBOR *sync views* and search APIs, not raw IR mmap. HashMap-by-path is first-class. Schema evolution is handled by **CAS + regeneration**, not in-place field defaults. That tilts strongly toward rkyv over Cap’n Proto/FlatBuffers for the *IR body*, while leaving Cap’n Proto irrelevant unless we need non-Rust zero-copy of the same bytes.

### 1.6 Performance numbers

Koloski’s [rust_serialization_benchmark](https://github.com/djkoloski/rust_serialization_benchmark) summary blog ([rkyv is faster than…](https://david.kolo.ski/blog/rkyv-is-faster-than/), Mar 2021) — still the standard public comparison (absolute numbers age; *rankings* remain directionally valid):

**`log` (string-heavy HTTP logs)** — lower better for times:

| Lib | Serialize | Access | Read | Deserialize | Size |
|---|---|---|---|---|---|
| rkyv | 423 µs | **1.36 ns** | **19 µs** | 3.25 ms | 1.07 MB |
| flatbuffers | 2.68 ms | 3.0 ns | 163 µs | n/a | 1.28 MB |
| capnp | 1.86 ms | 260 ns | 712 µs | n/a | 1.84 MB |
| postcard | 715 µs | — | — | 4.44 ms | **0.77 MB** |
| bincode | 641 µs | — | — | 4.28 ms | 1.05 MB |
| abomonation | **315 µs** | 37 µs* | 59 µs* | — | 1.71 MB |

**`minecraft_savedata` (highly structured — closest to IR):**

| Lib | Serialize | Access | Read | Size |
|---|---|---|---|---|
| rkyv | 844 µs | **1.38 ns** | **283 ns** | 725 KB |
| flatbuffers | **38.7 ms** | 2.9 ns | 4.0 µs | 849 KB |
| capnp | 863 µs | 257 ns | 5.3 µs | 836 KB |
| postcard | 774 µs | — | — | **356 KB** |

Takeaways for IR:

1. **Access** is effectively free for rkyv (pointer cast) vs µs–ms of postcard/bincode deserialize.
2. **Read traversal** on structured data is excellent for rkyv.
3. **Size** loses to postcard (often 1.5–2× larger before compression). zstd+dict (PLAN.md) largely erases size for cold store; mmap working set still cares about *uncompressed* archive size.
4. **Serialize** is mid-pack — fine for offline compile jobs, not for per-keystroke rewrite.

Apache Iggy (2025) reported production-class gains after zero-copy work ([blog](https://iggy.apache.org/blogs/2025/05/08/zero-copy-deserialization/)): consumer throughput ~2.1 → 4.0 GB/s, p99 read latency ~2.93 → 1.46 ms on i3en.3xlarge — but note they **left pure rkyv** for a custom layout (see §2.6).

### 1.7 Versioning / schema evolution pain

This is rkyv’s central weakness — intentional:

- Feature matrix: **schema evolution = no** ([Feature comparison](https://rkyv.org/feature-comparison.html)).
- Early Reddit announcement: versioning/migration intentionally out of scope.
- Issue [#164](https://github.com/rkyv/rkyv/issues/164) (“Schema evolution”) remains an ecosystem/add-on problem, not a built-in table/field default system like Cap’n Proto / FlatBuffers / Protobuf.
- uv’s Charlie Marsh notes the operational cost: *any layout change invalidates cached rkyv blobs* ([astral-sh/uv#10969](https://github.com/astral-sh/uv/issues/10969)).

**Mitigations that work for content-addressed IR:**

1. **Never migrate archives in place.** Old generation hashes keep old archive bytes forever (or until GC).
2. **Bump `format_version` + type_hash** in file header; reject mismatch → recompile IR from source.
3. **Archive-facing DTOs** decoupled from “logical” IR so you can evolve logical types and emit a new archive format version without thrashing every field rename in the public API.
4. **Do not use rkyv for long-lived cross-language interchange** — that remains postcard/JSON/CBOR sync manifests.

For nudox this is acceptable: IR is a **derived, regenerable** artifact of a producer job keyed by toolchain + grammar + source hashes (PLAN.md §9). Unlike a decade-old Cap’n Proto log that must read v1 messages forever, dead IR is reclaimed with generations.

### 1.8 Suitability verdict for complex IR enums

| Concern | Verdict |
|---|---|
| 25+ `Type` variants, nested | **Excellent** — native enums |
| `Entry` tagged union ~15 variants | **Excellent** |
| `HashMap<NudoxPath, Entry>` | **Excellent** — ArchivedHashMap |
| `PathBuf` / UTF-8 paths | Fine (as archived strings); consider interned path IDs |
| Schema evolution | **Poor built-in**; OK via CAS regeneration |
| Single-entry mmap slice | **Poor if monolithic** — need sectional layout (§3) |
| Incremental patch of one field | **Poor** — rewrite archive |
| Cross-language zero-copy | **No** |
| Validation of untrusted bytes | **Good** with bytecheck |
| Match with existing `yoke` path | Complementary (yoke for lifetime erasure of *partial* zero-copy; rkyv for *total*) |

---

## 2. Other zero-copy stacks

### 2.1 FlatBuffers

- Pros: mature, multi-language, schema evolution (deprecate/add fields), random access via vtables, optional fields omit space.
- Cons for IR: IDL tax; Rust API less ergonomic; **very slow serialize on rich graphs**; no HashMap; enum/union modeling is clumsier than Rust ADTs; generating 25 Type variants in `.fbs` is toil.
- Role: reject as primary IR format; not worth dual schema maintenance.

### 2.2 Cap’n Proto

- Pros: true zero-copy, schema evolution, RPC story, security-conscious design (Varda), multi-language.
- Cons for IR: external schema; no first-class HashMap; Rust ecosystem thinner; packing/size on bulk numeric data historically weak; still not “derive on existing IR types”.
- Role: only if a future non-Rust runtime must mmap the same IR blob. Unlikely for INDEX/REGISTRY.

### 2.3 postcard

- Current IR path (PLAN.md). Compact, serde-compatible, excellent size (best in many benches), great for embedded and CAS cold store with zstd dicts.
- Zero-copy: **partial only** (`Cow`/`&str`/`&[u8]`); containers allocate.
- Role: keep for **manifests**, sync JSON projections, small metadata; optional dual-write IR while rkyv matures.

### 2.4 bincode

- Similar niche to postcard; slightly larger; mature serde backend.
- No total zero-copy. No reason to prefer over postcard for IR.

### 2.5 abomonation

- Frank McSherry’s “terrifying” library ([Unsafe at any speed](http://www.frankmcsherry.org/serialization/2015/05/04/unsafe-at-any-speed.html)): transmute-style encode; often fastest serialize in benches.
- Requires **mutable** backing for access; non-portable; deeply unsafe; no real validation story; effectively a research/historical option.
- Role: **reject** for production IR.

### 2.6 Apache Iggy’s rkyv journey (cautionary)

Iggy ([Zero-copy (de)serialization, May 2025](https://iggy.apache.org/blogs/2025/05/08/zero-copy-deserialization/)):

1. Started with full-batch `#[derive(Archive)]` on message batches.
2. Hit a hard limit: **cannot slice a middle range of an ArchivedVec and treat it as a valid archive root** — layout is not “prefix-independent message frames”.
3. Moved to length-prefixed archived messages inside a byte bag, then **abandoned rkyv entirely** for a custom index+payload layout yielding views (closer to partial zero-copy / serde style).
4. Reasons: rkyv is “heavy,” couples network+disk schema to crate versions, and blocked their slice/send path.
5. Result: ~2× read throughput after the overall zero-copy redesign (not pure rkyv alone).

**Lesson for nudox:** if hot path is “return 10 of 100 messages as a wire slice,” pure monolithic rkyv fails. If hot path is “mmap whole package IR and probe HashMap,” rkyv shines. Our §3 layout should **not** put “random single Entry as independent rkyv root without framing” as an afterthought — Iggy’s pain is exactly that.

### 2.7 uv / Astral and rkyv

- uv depends on rkyv 0.8.x ([crates.io uv deps](https://crates.io/crates/uv)).
- Public engineering writeups: zero-copy `.rkyv` cache for package metadata — mmap, no JSON parse ([Xebia / HN discussion on “How uv got so fast”](https://news.ycombinator.com/item?id=46395919)).
- HN thread clarifies: technique isn’t Rust-only historically, but Rust (and rkyv) make *safe* total zero-copy practical; uv’s hot path is local cache touch/filter, not network.
- Issue #10969: migrate remaining msgpack → rkyv; explicit acceptance that **type layout changes bust the cache**.

**Lesson:** rkyv for **derived, disposable caches** is battle-tested at scale. nudox IR is the same class if regenerable.

### 2.8 Manish Goregaokar zero-copy series (ICU4X)

Three posts (Aug 2022) — foundational for `yoke` already in the IR path:

1. **[Not a Yoking Matter](http://manishearth.github.io/blog/2022/08/03/zero-copy-1-not-a-yoking-matter/)** — partial zero-copy via serde/`Cow`; lifetimes infect APIs; **`yoke`** erases lifetimes by cart+self-ref (“lifetime erasure”).
2. **[Zero-Copy All the Things](http://manishearth.github.io/blog/2022/08/03/zero-copy-2-zero-copy-all-the-things/)** — why `Vec<u32>` isn’t free (endianness, alignment); **`zerovec`** (`ZeroVec`/`VarZeroVec`/`ZeroMap`) for ULE types with serde.
3. **[So Zero It’s … Negative?](http://manishearth.github.io/blog/2022/08/03/zero-copy-3-so-zero-its-dot-dot-dot-negative/)** — **`databake`**: emit `static` Rust, skip runtime validation for trusted shipped data.

Manish explicitly discusses rkyv: excellent total zero-copy; ICU4X chose serde+zerovec for ecosystem fit and multi-format (JSON/postcard/baked) flexibility rather than rkyv’s parallel type system.

**Relation to nudox:**

| Crate | Already? | Use |
|---|---|---|
| `yoke` | yes (IR path) | Hold mmap/owned cart + borrowed view without lifetime spaghetti |
| `zerovec` | no | Only if staying on postcard/serde partial zero-copy for dense tables |
| `zerocopy` (Google/Fuchsia lineage, `FromBytes`) | no | POD headers, section tables — **yes for file framing** |
| `databake` | no | Optional for *shipping* standard library IR inside the binary — niche |

### 2.9 “roqs” / filkoll blog (likely match)

Search for “roqs blog post” did not surface a project by that name. The closest high-quality writeup matching the user’s intent (rkyv + string interning + mmap + type hash for a lookup cache) is **VorpalBlade’s filkoll post**:

- [Filkoll — The fastest command-not-found handler](https://vorpal.se/posts/2025/mar/25/filkoll-the-fastest-command-not-found-handler/) (2025)

Key patterns directly reusable for IR:

1. rkyv root with `HashMap<String, …>` for O(1) name lookup.
2. Custom **string interner** as `Vec<u8>` of length-prefixed strings; handles are `u32` offsets — HashMap only at *build* time.
3. `memmap2` + atomic rename writer invariant.
4. Skip rkyv validation on trusted files after **POD header**: magic + manual version + `type_hash` (crate versions mixed in).
5. Accept that compression fights zero-copy — shrink via interning instead.

This is the practical template for §5’s wire format.

### 2.10 `zerocopy` crate (not rkyv)

Google’s [`zerocopy`](https://crates.io/crates/zerocopy) (`FromBytes`, `IntoBytes`, `KnownLayout`) is for **plain byte-transmutable** types — headers, arrays of POD — not recursive IR graphs. Use it for `NdIr` section headers and offset tables; do not try to model `Type` with it.

---

## 3. File-based condensed IR layout for symbol lookup

Monolithic `rkyv::to_bytes::<Index>(…)` works for whole-load, fails the “open one symbol without touching everything” goal and Iggy’s slice lesson. Prefer a **sectioned container** where the index is tiny and POD, and each Entry (or type node) is an independently mmapable range.

### 3.1 Design goals

| Lookup | Complexity target | Mechanism |
|---|---|---|
| `NudoxPath` → Entry | O(1) avg or O(log n) | Hash table or sorted + binary search / MPH |
| Entry → nested Type nodes | O(edges touched) | Relative offsets inside type arena |
| Doc string by id | O(1) | String arena + u32 |
| Integrity | whole file or per-section | blake3 |

### 3.2 Recommended sectional layout (conceptual)

```
+------------------+
| FileHeader (POD) |  magic, version, flags, section TOC, content blake3
+------------------+
| PathIndex        |  perfect hash or swiss table: path_key → EntryRef
+------------------+
| EntryDirectory   |  parallel arrays: kind, offset, length, flags
+------------------+
| EntryArena       |  concatenated archived Entry bodies (rkyv or POD)
+------------------+
| TypeArena        |  interned Type nodes (optional split)
+------------------+
| StringArena      |  length-prefixed UTF-8 (docs, names, paths)
+------------------+
| Aux (optional)   |  name→paths inverted index, kind bitsets, etc.
+------------------+
```

### 3.3 Path index strategies

**A. ArchivedHashMap of full paths (simplest rkyv)**  
Store `ArchivedHashMap<Archivedstr, EntryRef>` or interned path ids. Good for tens–hundreds of thousands of symbols. Build cost higher; lookup excellent.

**B. Sorted path table + binary search**  
`paths: [StrRef]` sorted, `entries: [EntryRef]` parallel. O(log n), great cache locality, trivial POD, easy to mmap without rkyv. Preferred if we want zero rkyv in the index.

**C. Minimal perfect hash (MPH)**  
For a fixed key set at archive build time, MPH maps path → index with **no collisions** and ~1–2 probes ([Steve Hanov’s MPH](https://stevehanov.ca/blog/throw-away-the-keys-easy-minimal-perfect-hashing), [RecSplit](https://arxiv.org/abs/1910.06416)). Ideal for read-only package IR. Store:

```rust
struct PathMph {
    // MPH description bytes (algorithm-specific)
    desc: &[u8],
    // values[i] = EntryRef for key that hashes to i
    values: &[EntryRef],
    // optional: keys arena for negative lookup verification
    keys: StringArena,
}
```

Always **verify** candidate key equality after MPH (MPH is perfect for the build set, but arbitrary query strings still need a compare to distinguish miss vs hit).

**D. Prefix tree / trie on path components**  
Useful for “list children of module `foo::bar`” hierarchical browse. Can sit *beside* the hash index (Dash/Zeal-style browse vs exact lookup). Not required for O(1) exact path.

**Recommendation:** start with **interned path ids + sorted table or FxHash map in rkyv root**; graduate to MPH if profiles show hash overhead or memory bloat on huge packages (std-sized crates).

### 3.4 String interning tables

Follow filkoll:

```rust
#[derive(Archive, Serialize, Deserialize, Clone, Copy)]
#[repr(transparent)]
struct StrId(u32);

#[derive(Archive, Serialize, Deserialize)]
struct StringArena {
    /// Concatenated records: u16/u32 len + utf8 bytes (choose width by max)
    bytes: Vec<u8>,
}

impl ArchivedStringArena {
    fn get(&self, id: StrId) -> &str { /* bounds + utf8 check */ }
}
```

Intern: entry names, path components or full paths, docstrings, dependency names. IR is extremely redundant on paths and identifiers → large win on both disk and mmap RSS **without compression**.

### 3.5 Columnar vs row for Entry kinds

| Layout | Pros | Cons |
|---|---|---|
| **Row** (tagged Entry blob per symbol) | Matches logical model; one mmap range per symbol | Kind-specific fields scatter; harder SIMD |
| **Columnar** (all Functions together, all Records together) | Kind filters cheap; better compression; secondary indexes | Path lookup needs indirection; updates rewrite columns |

For documentation IR, **row-oriented EntryArena** with a thin **columnar side index** wins:

```rust
struct KindIndex {
    /// For each EntryKind, sorted list of entry_ids (or offsets)
    functions: Vec<u32>,
    records: Vec<u32>,
    // ...
}
```

Tantivy/search already covers free-text; the IR file should optimize **path exact match + kind fan-out**, not analytics scans.

### 3.6 mmap + partial load of single Entry

Critical protocol:

1. Map whole file **or** use `mmap` + OS paging (untouched pages stay cold).
2. Read `PathIndex` (hot, small — pin/mlock optional).
3. Get `EntryRef { offset: u64, len: u32, kind: u8, type_root: u32 }`.
4. Slice `mmap[offset..offset+len]` and `access::<ArchivedEntry>` **only that slice** — requires each Entry body to be a **self-contained rkyv archive** (own relative pointers, no pointers escaping the slice) **or** a POD/zerocopy struct with indexes into shared arenas via absolute file offsets.

**Two valid designs:**

| Design | Entry body | Type nodes | Tradeoff |
|---|---|---|---|
| **Self-contained rkyv Entry** | Full nested types inline | Duplicated type subgraphs | Simple; size↑ |
| **Arena + POD Entry head** | Entry stores `TypeId`s | Shared TypeArena | Dedup types; more engineering |

Shared TypeArena is better for IR (identical `Type` trees repeat massively across methods). Entry heads can be POD:

```rust
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
struct EntryHead {
    kind: u16,
    flags: u16,
    name: StrId,
    path: StrId,
    docs: StrId,          // 0 = none
    payload_off: u32,     // into kind-specific payload arena
    payload_len: u32,
}
```

Kind-specific payloads (Function signature, Record fields, …) can still be rkyv micro-archives **or** pure POD with TypeIds.

### 3.7 Prefix trees / perfect hashes for NudoxPath

`NudoxPath` is currently:

```rust
enum NudoxPath {
    External { path: PathBuf, dependency: String },
    Local(PathBuf),
}
```

Archive-friendly encoding:

```rust
#[derive(Archive, Serialize, Deserialize)]
struct PathKey {
    kind: u8,              // 0 = local, 1 = external
    dep: StrId,            // 0 if local
    /// Full path string OR components: [StrId] in component arena
    path: StrId,
}
```

Component-wise intern (`std`, `collections`, `HashMap`) maximizes sharing. Lookup key construction: join components or hash component id sequence.

For hierarchical UI, store optional `parent: EntryId` edges or a separate children multimap — do not force trie into the hot exact-lookup path.

---

## 4. Integration with VCS patches

### 4.1 Nature of the problem

Zero-copy archives are **write-once, read-many**. Relative pointers and Swiss tables are not designed for surgical field edits. rkyv mutation APIs exist but are limited and unsafe for concurrent readers (mmap).

Nudox already models **generations as CAS snapshots** (PLAN.md §6). That is the right default.

### 4.2 Strategies

| Strategy | Description | When |
|---|---|---|
| **A. Full snapshot per generation** | Each IR blob is complete; CAS dedupes identical IR across versions | Default; simple; matches blake3 identity |
| **B. Patch chain** | Store base IR + sequential patches (JSON-Patch / custom Entry diffs) | High churn with tiny deltas; materialize on read |
| **C. Hybrid** | Base snapshot every N versions + patches between | Cold storage optimization |
| **D. In-archive incremental update** | Patch rkyv buffer in place | **Avoid** — invalidates relative pointers, races with mmap |

### 4.3 Recommended hybrid for large multi-version retention

```
generation_g:
  ir_strategy: snapshot | materialize
  ir_snapshot_hash: blake3   # full NdIr file, if present
  ir_base_hash: blake3       # optional base snapshot
  ir_patches: [blake3, …]    # ordered patch blobs
```

Reader algorithm:

```
if snapshot present:
  mmap snapshot
else:
  load base → apply patches in memory → (optional) write materialized snapshot to CAS → mmap
```

Materialized archive is again content-addressed; multiple clients share it.

### 4.4 Patch format sketch (Entry-grain, not byte-grain)

Do **not** binary-diff rkyv bytes (fragile). Diff at logical IR:

```rust
enum IrPatchOp {
    Upsert { path: NudoxPath, entry: Entry },
    Remove { path: NudoxPath },
    // optional: type-arena-only updates if using shared arenas across gens — hard; skip v1
}
struct IrPatch {
    base_hash: ContentHash,
    ops: Vec<IrPatchOp>,
}
```

Encode patches as **postcard** (compact, evolution-friendly). Only the **materialized** form is rkyv/NdIr for mmap.

### 4.5 Why incremental rkyv update is hard

- Inserting into `ArchivedHashMap` requires rebuild (Swiss table layout).
- Growing a string arena moves offsets → rewrite all StrIds or use freelist (complex).
- Concurrent mmap readers see torn state unless COW/rename (which is just snapshot replace).

**Conclusion:** treat archive rewrite as the normal path; optimize *how often* you rewrite via snapshots+patches, not in-place rkyv surgery.

---

## 5. Concrete recommended wire format sketch

### 5.1 Decision summary

| Layer | Format | Why |
|---|---|---|
| Generation sync / manifest | postcard + optional JSON | Already shipped; evolvable |
| IR **cold** interchange / debug | postcard (+ zstd dict) | Compact; serde |
| IR **hot** local/registry mmap | **NdIr v1 sectional** (below) | Path O(1)/O(log n); partial page-in |
| Section bodies | rkyv micro-archives **or** POD+TypeId | See tradeoffs |
| Headers / TOC | `zerocopy` POD | Safe transmute |
| Lifetime ergonomics | `yoke` over mmap cart | Already in stack |

**Primary decision:** prefer **custom sectional layout (NdIr) with rkyv (or POD) entry/type bodies** over pure Cap’n Proto and over a single opaque rkyv(`Index`). Cap’n Proto loses on HashMap + Rust ADT ergonomics; pure monolithic rkyv loses on single-symbol load and Iggy-style slicing; pure custom POD loses on nested 25-variant `Type` encoding cost.

### 5.2 File header

```rust
use zerocopy::{FromBytes, IntoBytes, KnownLayout, Immutable};

pub const NDIR_MAGIC: u32 = u32::from_le_bytes(*b"NDIR");
pub const NDIR_VERSION: u32 = 1;

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Clone, Copy)]
#[repr(C)]
pub struct NdIrHeader {
    pub magic: u32,           // NDIR_MAGIC
    pub version: u32,         // NDIR_VERSION
    pub flags: u32,           // bit0: compressed sections? (prefer no for mmap)
    pub header_size: u32,     // sizeof header + toc
    pub type_hash: u64,       // hash of logical schema / rkyv type versions
    pub content_blake3: [u8; 32], // hash of payload after header (or whole file policy)
    pub section_count: u32,
    pub _pad: u32,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Clone, Copy)]
#[repr(C)]
pub struct SectionTocEntry {
    pub tag: u32,      // see SectionTag
    pub offset: u64,   // from start of file
    pub length: u64,
    pub blake3: [u8; 32], // optional per-section; zero if unused
}

#[repr(u32)]
pub enum SectionTag {
    PathIndex = 1,
    EntryDirectory = 2,
    EntryArena = 3,
    TypeArena = 4,
    StringArena = 5,
    KindIndex = 6,
    RkyvRootFallback = 7, // optional full Index for tools
}
```

**Integrity policy:**

- CAS key = `blake3(entire file bytes)` **or** `blake3(logical IR postcard)` if NdIr is a derived view — pick one and document. Prefer **file bytes hash = CAS key** for the NdIr blob so mmap readers re-verify easily.
- For trusted local CAS after verify-on-download: `access_unchecked` + header `type_hash` (filkoll pattern).
- For untrusted/network before CAS insert: full section blake3 + rkyv `bytecheck` on each micro-archive touched (or whole arenas).

### 5.3 Path index + entry directory

```rust
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Clone, Copy)]
#[repr(C)]
pub struct EntryRef {
    pub entry_id: u32,
    pub head_off: u32,   // into EntryDirectory / EntryArena
    pub head_len: u32,
    pub kind: u16,
    pub flags: u16,
}

/// Sorted path index (v1 — simple, mmap-friendly, no rkyv required)
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Clone, Copy)]
#[repr(C)]
pub struct PathIndexHeader {
    pub count: u32,
    pub str_arena_section: u32, // section index
    pub _pad: u32,
}
// followed by count × PathIndexRow
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Clone, Copy)]
#[repr(C)]
pub struct PathIndexRow {
    pub path_key_hash: u64, // FxHash of PathKey encoding for bloom-ish reject
    pub path_str: u32,      // StrId of canonical path encoding
    pub entry_id: u32,
}
// rows sorted by path_str's bytes (or by path_key_hash then verify)
```

Lookup:

1. Canonicalize `NudoxPath` → UTF-8 key (`L\x1f{path}` / `E\x1f{dep}\x1f{path}`).
2. Binary search rows by string arena compare **or** hash probe table.
3. Load `EntryRef` → slice EntryArena.

### 5.4 Type arena (handles nested Type enum)

Option A — rkyv each type node (heavy).  
Option B — **tagged POD nodes** with child indices (recommended for arena):

```rust
#[derive(Clone, Copy)]
#[repr(u16)]
enum TypeTag {
    TypeReference = 1,
    SelfType = 2,
    Primitive = 3,
    Tuple = 4,
    BorrowedRef = 5,
    // ... map all 25+ variants
    Conditional = 30,
    TemplateLiteral = 31,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Clone, Copy)]
#[repr(C)]
struct TypeNodeHead {
    tag: u16,
    child_count: u16,
    // payload interpretation depends on tag:
    // e.g. name StrId, primitive discriminant, flags for mutability, etc.
    a: u32,
    b: u32,
    c: u32,
}
// children: contiguous TypeId (u32) list in side table or immediately following
```

Builders convert live `Type` → interned `TypeId` with structural hashing (hash-consing) so identical trees share nodes — large win for method-heavy crates.

### 5.5 rkyv entry payload (optional hybrid)

For complex payloads (`Function`, `Record`, `SumType`) where POD is painful:

```rust
#[derive(Archive, Serialize, Deserialize)]
struct ArchivedFunctionPayload {
    generics: Option<ArchivedGenerics>,
    params: Vec<ArchivedParam>,
    ret: TypeId, // index into TypeArena, not nested Type
    // ...
}
```

Each payload is a length-prefixed rkyv blob in EntryArena. **Relative pointers stay inside the blob.** Type graph stays in the shared arena.

### 5.6 Safe validation story

```
open(path):
  mmap file
  check Header.magic/version
  check Header.content_blake3 == blake3(payload)   // or whole file
  check Header.type_hash ∈ supported set
  if untrusted || !cas_verified:
      for each section: verify section.blake3
      for each rkyv microblob accessed: rkyv::access (bytecheck)
  else:
      access_unchecked on demand
  wrap in Yoke<NdIrView, MmapCart>
```

Atomic publish: write temp → fsync → rename (filkoll / PLAN.md CAS). Never mutate mmap’d files in place.

### 5.7 Rust view sketch

```rust
pub struct NdIrView<'a> {
    header: &'a NdIrHeader,
    paths: PathIndex<'a>,
    entries: EntryDirectory<'a>,
    types: TypeArena<'a>,
    strings: StringArena<'a>,
    entry_arena: &'a [u8],
}

impl<'a> NdIrView<'a> {
    pub fn get_entry(&self, path: &NudoxPath) -> Option<EntryView<'a>> {
        let id = self.paths.lookup(path, &self.strings)?;
        let head = self.entries.get(id)?;
        Some(EntryView { head, ir: self })
    }
}

pub type YokedIr = Yoke<NdIrView<'static>, Rc<Mmap>>;
```

Live `Index` remains the **build-time / mutate-time** representation; `NdIr` is emit-only for storage.

---

## 6. Comparison matrix (IR decision)

| Criterion (weight) | postcard (status quo) | Monolithic rkyv(Index) | Cap’n Proto | **NdIr hybrid (recommend)** |
|---|---|---|---|---|
| Nested 25+ Type enums | excellent | excellent | poor ergonomics | excellent |
| HashMap path lookup | after full deserialize | excellent | weak | excellent |
| mmap whole package | no (alloc) | excellent | good | excellent |
| Single Entry partial touch | no | poor (pages + no slice root) | medium | **excellent** |
| Schema evolution | good (serde defaults) | poor | excellent | good (version+CAS) |
| Size before compress | **best** | medium | medium | medium–good (intern) |
| Size after zstd+dict | excellent | excellent | good | excellent |
| Patch / VCS compose | easy logical | rewrite | rewrite | snapshot+postcard patch |
| Cross-language | via serde | no | yes | no (OK) |
| Engineering cost | low | medium | high IDL | **medium–high** |
| Validation | serde errors | bytecheck | built-in | header+section+optional bytecheck |
| Fit with yoke | good | awkward Archived* | awkward | **good** |

---

## 7. Recommendations (actionable)

### 7.1 Decision

1. **Do not adopt Cap’n Proto or FlatBuffers for IR.** Wrong ergonomics for Rust ADTs and HashMaps; multi-language zero-copy is not a product requirement for IR bytes.
2. **Do not replace postcard with monolithic `rkyv::Index` as the only IR form.** Whole-load access is great; single-symbol and Iggy-style slice problems remain; schema coupling is real (uv, Iggy).
3. **Adopt a sectional NdIr container** as the hot REGISTRY/INDEX IR blob:
   - POD headers via `zerocopy`
   - String + Type arenas with intern/hash-cons
   - Path index (sorted v1 → MPH later)
   - Entry payloads as POD heads + optional rkyv micro-blobs for fat kinds
4. **Keep postcard IR** as:
   - producer intermediate
   - debug/export
   - patch encoding
   - optional dual-write during migration
5. **Keep `yoke`** for cart management over mmap.
6. **Versioning policy:** `NDIR_VERSION` + `type_hash`; never mutate; regenerate on producer bump; CAS retains old generations.
7. **Patches:** Entry-level postcard ops + materialize to NdIr; never binary-patch rkyv.

### 7.2 When pure rkyv *is* enough

If profiling shows:

- IR always loaded wholly for search/index build,
- packages fit comfortably in RAM,
- single-symbol open is dominated by UI not deserialize,

…then **monolithic rkyv(Index)** is a justified Phase-1 (uv-like cache). Still add magic/version/type_hash header. Plan migration path to NdIr sections before IR exceeds ~50–100 MB uncompressed or multi-tenant INDEX serves partial reads.

### 7.3 Implementation phasing (extends PLAN.md)

| Phase | Work |
|---|---|
| **P0** | Measure postcard IR load p95 on representative packages (std, serde, k8s client libs). |
| **P1** | NdIr header + string arena + sorted path index + entry POD heads; types still nested postcard blobs per entry. |
| **P2** | Type arena + hash-cons; rkyv micro-payloads for Function/Record/Sum. |
| **P3** | MPH path index if needed; kind columnar side index; materialize-from-patch pipeline. |
| **P4** | Drop dual postcard IR store if NdIr-only is proven (keep postcard for patches/export). |

### 7.4 Explicit non-goals

- In-place rkyv mutation of published generations.
- Cross-language mmap of NdIr (use APIs / JSON).
- Persisting tree-sitter trees (PLAN.md §3 — reaffirmed).
- Compression of mmap-hot NdIr (compress on the wire/CAS optional; decompress to raw NdIr for mmap, or store raw and rely on interning).

---

## 8. Relation to PLAN.md (delta)

| PLAN.md claim | This brief |
|---|---|
| rkyv only if profiling demands | **Affirmed**, but design NdIr *now* so we don’t box ourselves into monolithic rkyv |
| postcard + zstd dict for IR | Keep for cold/patch; not ideal sole hot format |
| `.ndxt` custom POD for trees | Same philosophy → **NdIr** for IR |
| mmap rkyv IR “ideal” | Ideal only with **sectional** layout |
| CAS + generation manifests | Unchanged; NdIr is another CAS blob class `kind=ir-ndir` |
| Optional transfer packs | NdIr can live inside packs; prefer raw for local mmap |

---

## 9. References

### Primary

- Koloski, D. — [rkyv architecture and internals](https://david.kolo.ski/blog/rkyv-architecture/) (2020)
- Koloski, D. — [rkyv is faster than {bincode, capnp, …}](https://david.kolo.ski/blog/rkyv-is-faster-than/) (2021)
- rkyv book — [Zero-copy deserialization](https://rkyv.org/zero-copy-deserialization.html), [Validation](https://rkyv.org/validation.html), [Shared pointers](https://rkyv.org/shared-pointers.html), [Feature comparison](https://rkyv.org/feature-comparison.html), [Format](https://rkyv.org/format.html)
- [rust_serialization_benchmark](https://github.com/djkoloski/rust_serialization_benchmark)
- Varda, K. — [Cap’n Proto, FlatBuffers, and SBE](https://capnproto.org/news/2014-06-17-capnproto-flatbuffers-sbe.html) (2014)
- Goregaokar, M. — Zero-copy series #1–#3 (yoke, zerovec, databake) (2022)
- Apache Iggy — [Zero-copy (de)serialization](https://iggy.apache.org/blogs/2025/05/08/zero-copy-deserialization/) (2025)
- Astral uv — rkyv cache; [HN discussion](https://news.ycombinator.com/item?id=46395919); [issue #10969](https://github.com/astral-sh/uv/issues/10969)
- VorpalBlade — [Filkoll / rkyv string interning mmap cache](https://vorpal.se/posts/2025/mar/25/filkoll-the-fastest-command-not-found-handler/) (2025) *(closest public match to “roqs”-style writeup found in search)*
- McSherry, F. — [Abomonation](http://www.frankmcsherry.org/serialization/2015/05/04/unsafe-at-any-speed.html) (2015)
- Hanov, S. — [Minimal perfect hashing](https://stevehanov.ca/blog/throw-away-the-keys-easy-minimal-perfect-hashing)
- Esposito et al. — [RecSplit MPH](https://arxiv.org/abs/1910.06416)

### Crates

- `rkyv` 0.8.x, `bytecheck`, `yoke`, `zerovec`, `zerocopy`, `postcard`, `memmap2`, `type_hash` (filkoll pattern)

### Local

- `/Users/philocalyst/Projects/Backend/docs/research/librarification/13-storage/PLAN.md`
- `/Users/philocalyst/Projects/Backend/workspace/ir/{entry,kind,ty}.rs`

---

## 10. Executive one-pager

Nudox IR is a **large, enum-heavy, HashMap-keyed, regenerable** graph. **Total zero-copy** (rkyv class) can make package open and search ingest dramatically cheaper than postcard deserialize — benchmarks show access in nanoseconds and structured reads far ahead of Cap’n Proto/FlatBuffers in Rust. **Schema evolution is deliberately weak in rkyv**; that is acceptable only because generations are CAS snapshots that get rebuilt, not migrated.

Do **not** bet the format on Cap’n Proto (IDL + no HashMap) or abomonation (unsound). Do **not** ship only a monolithic `rkyv(Index)` without learning Iggy’s lesson on non-sliceable archives. **Do** ship a sectional **NdIr** file: POD TOC, interned strings, hash-consed type arena, path index (sorted → MPH), entry heads with optional rkyv micro-payloads, blake3 + type_hash validation, `yoke` over mmap. Compose with VCS via **full snapshots + postcard entry patches + materialize**, never in-place archive edits.

**Final call:** **NdIr hybrid (custom POD shell + rkyv/POD bodies) > pure rkyv monolith > postcard-only hot path > Cap’n Proto** for this IR.

---

*End of research brief.*
