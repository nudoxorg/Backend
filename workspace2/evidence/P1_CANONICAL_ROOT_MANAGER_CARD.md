# P1 C1a destructive-memoized typed-slice borrowed-root full-view experiment card

Baseline: `44c22154fd5238e4769562590420371979306050`; manager branch
`codex/prototype-canonical-root-hydration`; card base
`948c458cc131fbd8d75490cc955b6669c9713a65`. This is the sole C1a edit-authority
body. It supersedes only the C1a card retained in Git history.

## Capability and first terminal

C1a adds `ValidatedRoot<'bytes, DomainTag>` and a distinct
`BorrowedGenerationView<'root, 'locality, DomainTag>`. Caller-retained current canonical
root bytes validate once into a checked `&'bytes [RootWireRecord]`, then pair privately
with existing `ValidatedLocality` for infallible root projection and fallible
locality-composed full scan/exact-key lookup (`LocalityReadError` remains the existing
post-validation drift-containment result). C1a has no selection, closure scratch, hydration,
`Need::bind`, plan,
`VerifiedGeneration`, locality construction, receipt, `Published`, or P2 behavior.
Existing C0 `GenerationRoot` and `GenerationView` remain byte-for-byte/API unchanged.
`ValidatedLocality` preserves all ordinary field-read expressions through immutable facts
`Deref`, but deliberately removes external post-validation fact assignment; this required
compatibility tightening closes a field-mixing forgery route and is covered by downstream
compile-fail evidence.
C1b, if separately justified later, owns the source-neutral selection/hydration vertical.
The calibrated allocation-free Floyd subcandidate is retained as losing evidence: its
deep-chain validation cost is quadratic and therefore fails this candidate's whole-operation
work law. This replacement admits one fallible transient standard-library allocation inside
`TryFrom`; it retains no hierarchy sidecar after validation succeeds or fails. The first
split-parent/2-bit-state implementation checkpoint is also retained as a losing artifact:
its claims and proof surface did not meet the card gates, so none of its source changes are
integrated. This card supersedes only its hierarchy-scratch representation.

## Control, grammar, and typed-slice precedent

C0 retains `Box<[RootRow]>` (64-byte asserted rows), arbitrary-order fallible build, and
the full current locality/selection/hydration control. It writes `8 + 63*N` bytes. C1a
uses the accepted `nudox-object-pack` route: explicit descriptor-schema provenance, then
`try_ref_from_bytes_with_elems` retains a checked typed slice; projection calls
`ObjectRef::from(&row.descriptor)` rather than another `TryFrom`. `TryFrom` accepts only
an exact `8 + 63*N` input: after checked geometry it rejects both short and trailing input
through `RootReadError::Extent`, retains exactly that input in `ValidatedRoot.bytes`, and
sets `ValidatedRoot.id` to `GenerationId::from_canonical_bytes(bytes)`. Thus a successful
borrowed root has the same identity as C0's `write_canonical` output and never hashes or
retains an ignored suffix.

`encode.rs` is the single shared private wire-grammar owner: it declares `RootHeaderRecord`
and `RootWireRecord`, and both the writer and `root_view.rs` import those records. There is
no second wire declaration or parser. C1a preserves the current 8-byte header plus 63-byte
row grammar and C0 identity exactly.

The private root record is exactly:

```rust
#[repr(C)]
#[derive(Clone, Copy, Immutable, IntoBytes, KnownLayout, TryFromBytes, Unaligned)]
struct RootWireRecord {
    key: U64<BigEndian>,
    parent_present: u8,
    parent_key: U64<BigEndian>,
    descriptor: ObjectDescriptorWireRecord,
}
const _: [(); 63] = [(); size_of::<RootWireRecord>()];
```

The 8-byte header becomes a private `FromBytes`/`Unaligned` count record. Both record
types are borrowed only from the supplied bytes; no parser dependency or unsafe code is
introduced.

## Exact C1a public boundary and immutable facts

```text
pub struct ValidatedRootFacts<'bytes> {
    pub bytes: &'bytes [u8],
    pub id: GenerationId,
    pub entry_count: RootEntryCount,
}
pub struct ValidatedRoot<'bytes, DomainTag> {
    private facts: ValidatedRootFacts<'bytes>,
    private rows: &'bytes [RootWireRecord], domain witness
}
impl Deref<Target = ValidatedRootFacts> for ValidatedRoot
impl<'bytes, DomainTag> TryFrom<&'bytes [u8]> for ValidatedRoot<'bytes, DomainTag>

pub struct BorrowedGenerationViewFacts {
    pub id: GenerationId,
    pub entry_count: RootEntryCount,
}
pub struct BorrowedGenerationView<'root, 'locality, DomainTag> {
    private facts: BorrowedGenerationViewFacts,
    private root/locality borrows
}
impl Deref<Target = BorrowedGenerationViewFacts> for BorrowedGenerationView
impl BorrowedGenerationView {
    pub fn new(&ValidatedRoot, &ValidatedLocality) -> Result<Self, LocalityError>;
    pub fn len(&self) -> usize;
    pub fn get(&self, EntryKey) -> Result<Option<GenerationEntry>, LocalityReadError>;
    pub fn closure(&self) -> BorrowedGenerationScan;
}
impl Iterator for BorrowedGenerationScan {
    type Item = Result<GenerationEntry<DomainTag>, LocalityReadError>;
}
```

`ValidatedLocality` is simultaneously tightened with the same source-compatible pattern:
`ValidatedLocalityFacts<'bytes> { pub bytes, pub generation, pub root_count }` becomes its
private `facts` field and `ValidatedLocality` implements `Deref<Target =
ValidatedLocalityFacts<'bytes>>`, without `DerefMut`. Its public construction/validation and
ordinary reads such as `locality.generation` remain source-compatible, but no caller can
rewrite any fact after validation. Both root/view private representations and the immutable
facts pattern prevent downstream literals and root/locality field mixing. The facts types add
no construction authority: only their respective validated witness owns the private typed
rows or locality layout needed for trusted traversal.
`BorrowedGenerationView` is intentionally a different type from `GenerationView`; it
cannot satisfy `Need::bind` and therefore cannot falsely advertise C1b selection or
hydration authority. C0 constructors, `Deref`, scan, selection, overlay propagation, and
all direct consumers remain unchanged. The two C1a public types have current external
consumers in the root integration test and layout-lab public-API control; they have no
generic dispatch, trait, or owner adapter.

`RootReadError` is C1a's public `TryFrom` error, in this exact priority:

1. `HeaderTruncated { required: MetadataBytes, available: MetadataBytes }`.
2. `CountOutOfRange { declared: u64, source: TryFromIntError }` from `u32::try_from`.
3. `LayoutOverflow { count: RootEntryCount, record_bytes: MetadataBytes }` from checked
   geometry.
4. `Extent { count: RootEntryCount, required: MetadataBytes, available: MetadataBytes }`
   when and only when `bytes.len() != required` (including trailing input).
5. `Descriptor { ordinal: RootEntryCount, key: EntryKey, source: ObjectDescriptorDecodeError }`.
6. `KeyOrder { ordinal: RootEntryCount, previous: EntryKey, current: EntryKey }`.
7. `ParentPresent { ordinal: RootEntryCount, key: EntryKey, observed: u8 }`.
8. `AbsentParentKey { ordinal: RootEntryCount, key: EntryKey, observed: EntryKey }`.
9. `MissingParent { child: EntryKey, parent: EntryKey }`.
10. `HierarchyScratch { entries: RootEntryCount, requested_bytes: MetadataBytes,
    source: TryReserveError }` from the one `Vec<u32>::try_reserve_exact` request. The
    observation is the checked exact `4 * N` byte request, never the element count.
11. `HierarchyCycle { key: EntryKey }`.

The global decoder phase order is frozen: header; count conversion; checked geometry; exact
extent; raw per-row descriptor/schema provenance; typed-slice retention; key order for every
row; parent-presence for every row; absent-parent-key for every row; all missing-parent binary
searches; one hierarchy reservation/fill; and only then cycle validation. The raw provenance
phase is the established object-pack pattern: it passes each exact 46-byte descriptor subrange
to `ObjectRef::<DomainTag>::try_from`, deriving its `ordinal` and big-endian `key` from that
row's exact fixed offsets, and maps the returned `ObjectDescriptorDecodeError` directly to
`Descriptor`. It is not a second root parser or a retained owner. Only after every descriptor
passes does `<[RootWireRecord]>::try_ref_from_bytes_with_elems` retain the checked typed slice;
its nested validation is guaranteed by the preceding exact-geometry/schema phase, and its
otherwise-impossible failure maps consistently to the existing structural `Extent` error rather
than panicking. The mutation matrix must include collisions that demonstrate this global
priority, not merely isolated failures. Each error retains exact observations/source.
`TryFrom<&[u8]>` never takes ownership: on every error the caller retains the original bytes,
and its temporary scratch is dropped before the error returns.
C1a uses no `RootBuildError`.

## Hierarchy and work contract

Three safe standard-library-only functional-graph mechanisms were compared on the sole
transient-scratch axis. All make the exact `MissingParent` pass first, retain no state after
validation, use no unsafe, and have a one-allocation target.

| Shape | One allocation | Parent searches | Temporary logical bytes | Decision |
|---|---:|---:|---:|---|
| A: packed 2-bit tri-state only, re-search parent during visiting and cleanup | `Vec<u32>` state words | ≤`3*N` | `4*ceil(N/16)` | reject: smallest memory but repeats every resolved edge and adds an avoidable third search pass |
| B: parent-coordinate words plus packed 2-bit tri-state words | one split `Vec<u32>` | ≤`2*N` | `4*N + 4*ceil(N/16)` | reject: credible but 2-bit state code and 100k overhead add complexity without a whole-consumer win |
| C: parent coordinates destructively memoized to terminal | one `Vec<u32>` | ≤`2*N` | exact `4*N` | selected: no packed-state accessor, one allocation, lower peak, bounded memoized Floyd/destruction work |

C first verifies every present parent key by binary search. One `Vec::new`, one fallible exact
`try_reserve_exact(N)`, then in-capacity `resize(N, u32::MAX)` supplies parent coordinates; a
second binary-search pass fills every present parent coordinate. `u32::MAX` is both the original
root terminal and the destructive DONE terminal; it cannot be a valid coordinate because
`N <= u32::MAX` makes valid positions `0..N` (exclusive). The checked `4*N` byte observation is computed
before reserve. The maximum representable count is source-proved without allocation: a count of
`u32::MAX` has valid coordinates `0..u32::MAX - 1`, so the sentinel is unreachable; on a
32-bit target the earlier checked `8 + 63*N` geometry rejects that count. Tests cover zero,
one root, one self-cycle, and the arithmetic/sentinel theorem without attempting an enormous
allocation.

For each position whose parent is not DONE, Floyd runs only in the remaining functional graph.
It advances the slow cursor one and fast cursor two resolved coordinates, ending acyclic when
either cursor reaches DONE and rejecting when they meet. Only after that check succeeds, a
destruction walk saves each next coordinate, overwrites its current parent slot with DONE, and
then advances. A later branch reaches a previously destroyed tail at its first DONE slot; it
does not revisit that tail. A cycle exits before destructive writes affect its component. Thus
each formerly non-DONE row is probed a bounded number of times by Floyd and destruction, and
no traversal indexes or follows DONE. The vector is explicitly dropped before `ValidatedRoot`
is returned; it is setup scratch, never a retained owner/sidecar or per-row heap owner.

The implementation must make the sentinel guards mechanically obvious in a short private helper
and this pseudocode is the required review invariant (each `parent_at` call indexes only a known
coordinate):

```text
for start in coordinates:
    if parents[start] == DONE: continue
    slow = start; fast = start
    loop:
        slow_next = parents[slow]; if slow_next == DONE: break acyclic
        slow = coordinate(slow_next)
        fast_next = parents[fast]; if fast_next == DONE: break acyclic
        fast = coordinate(fast_next)
        fast_next = parents[fast]; if fast_next == DONE: break acyclic
        fast = coordinate(fast_next)
        if slow == fast: return HierarchyCycle(key_for(slow))
    current = start
    loop:
        next = parents[current]
        parents[current] = DONE
        if next == DONE: break
        current = coordinate(next)
```

No destructive write occurs until the Floyd pass has reported acyclic; the destruction loop
saves `next` before overwriting and tests DONE before converting it to a coordinate. Required
tests include a branch that later joins a destroyed tail and a cycle first reached through an
incoming branch, in addition to the direct self-cycle.

The frozen C validation cap is: descriptor/order/presence checks at most `N` row visits;
at most `2*N` binary parent searches, each with at most `ceil(log2(N))+1` key comparisons;
and at most `6*N` direct resolved-parent probes across memoized Floyd and destruction. This is
O(N log N), including parent searches, for every valid or invalid non-structural profile. Its
allocation-count ceiling is one (`N=0` requests none), and its exact logical payload is `4*N`
bytes. Root-internal code invariants and source review establish the structural/schema/typed and
hierarchy bounds; the public release lab records only observable allocation/deallocation calls,
requested bytes, peak requested bytes, allocator status, and elapsed time. `TrackingAllocator`
does not measure ordinary copies or final live bytes, so pointer containment and no-traversal-copy
are separately demonstrated by public root tests and reviewed source; they are not lab columns.
Allocator rounding is reported rather than treated as canonical storage. Any second allocation,
wrong scoped drop count, retained scratch after return, missing measured row, cap violation,
synthetic counter, or post-validation get/scan allocation rejects this candidate.

At 100k C uses 400,000 logical bytes, saving 25,000 bytes over B and 100,000 bytes over separate
parent plus byte-state storage. Removing B's state-word splitting makes C smaller in both text
and scratch while its destructive memoization prevents the rejected Floyd-per-row quadratic
case. The lab covers zero, one, 100k shallow, and 100k deep chains with actual validation/view
calls; it must not derive or print algorithmic counters from N. The original `3*N*N` Floyd
subcandidate is never executed.

After a successful validation, trusted C1a root projection inside `get` and scan only
binary-searches or iterates the retained typed rows and calls `ObjectRef::from(&row.descriptor)`;
the locality composition can still return its existing `LocalityReadError`. Neither path redoes
schema validation, raw tag matching, hierarchy validation, allocation, copying, panic,
fallback, or omission. Full scan is O(N); exact lookup has at most `ceil(log2(N))+1`
root comparisons. C1a retains `8 + 63*N` caller bytes and O(1) view metadata with pointer
depth zero from the root view to those bytes. C0 remains `64*N` boxed rows plus backing.

## Exact writable surface and skeleton

| File | Items/methods | Errors/docs | Forecast added formatted LOC |
|---|---|---|---:|
| `crates/nudox-root/src/root_view.rs` | typed validator; destructive parent-only transient scratch; immutable root/view facts with `Deref`; `ValidatedRoot`; C1a view/scan/get | full public/error/scratch/trusted-projection docs | 480 |
| `crates/nudox-root/src/encode.rs` | sole shared private header/row wire declarations and byte writer reuse | grammar comments/width assertions | 40 |
| `crates/nudox-root/src/packed.rs` | required crate-private `RowIndex::from_validated_borrowed_root_position` coordinate bridge | exact borrowed-root ordinal provenance docs; no new public item | 14 |
| `crates/nudox-root/src/locality/artifact/view.rs` | private immutable `ValidatedLocalityFacts` storage plus `Deref`, no `DerefMut` | preserve public field reads; prohibit fact reassignment | 34 |
| `crates/nudox-root/src/locality/artifact/mod.rs` | reexport locality facts type | no validation change | 2 |
| `crates/nudox-root/src/locality/cursor.rs` | make only the no-work sequential step crate-private | permits C1a O(N) scan through existing `ValidatedLocality::scan`; no public API | 2 |
| `crates/nudox-root/src/locality.rs` | reexport locality facts type and crate-private cursor name for O(N) borrowed scan | no locality semantic change | 3 |
| `crates/nudox-root/src/lib.rs` | module and root/locality fact/type public reexports | reexport docs | 16 |
| `crates/nudox-root/tests/canonical_root_view.rs` | public C0/C1a bytes, pointer, permutations, locality, scan/get, fact-read source compatibility, all mutations/truncations, scratch/drop, 0/1/100k | exact fixtures and errors | 320 |
| `crates/nudox-hydration/tests/ui/forged_validated_root.rs` | C1a literal root/view forge and root/locality fact-reassignment failure | checked stderr | 28 |
| `crates/nudox-hydration/tests/ui/escaped_borrowed_root.rs` | C1a root/view lifetime escape failure | checked stderr | 20 |
| `crates/nudox-hydration/tests/compile_fail.rs` | include exactly the two C1a UI cases alongside existing witness | harness change | 5 |
| `layout-lab/src/bin/p1-canonical-root-control.rs` | public C0/C1a tracking-allocator validation and warmed paired-view get/scan/repetition TSV control | actual counters/TSV docs | 260 |
| `layout-lab/raw/p1-canonical-root-control.tsv` | three release repetitions, target/compiler/workload rows | raw artifact only | 0 |

All numbers in the table and stop rule are **added formatted lines**, not total pre-existing
file length. New files use their full formatted length; existing files use `after - baseline`.
Production forecast is 591 added lines with 29 unused reserve under a 620-line ceiling. Test
forecast is 373 lines with 47 unused reserve under a 420-line ceiling. Lab forecast is
260 lines with 40 unused reserve under a 300-line ceiling. Format and recount after every
physical delta; stop if any delta exceeds its forecast by 20% or 25 lines, a new item is
needed, or C1b behavior appears.

No other path may change: all manifests/dependencies; `nudox-id`/`nudox-object`; active
identity paths; `locality/view.rs`, `closure.rs`, `overlay.rs`, hydration source, P2; all
operation/runtime/I/O types; mapped/leased owners; self-reference; unsafe; SIMD; `Arc`;
per-row heap owners; test-only crates; and shipping scenarios are forbidden.

## Required evidence and exact gates

| Law | Artifact/command | Expected evidence | Stop trigger |
|---|---|---|---|
| trusted typed projection | public root test plus source tripwire | retained typed row projects by `ObjectRef::from`; zero traversal `TryFrom`/schema match | any reparse/panic/fallback/omission |
| decoder | root test mutation table | all truncations, count/short-extent/trailing-extent, schema/order/parent/missing/cycle errors with exact priority | missing operand/source |
| hierarchy work | root source invariant plus public 0/1/100k shallow/deep/branch-tail/incoming-cycle test | two parent-search phases and the guarded destructive Floyd/destruction pseudocode only; no N-derived lab counter | repeated Floyd tail/recurse/extra allocation or failed deep profile |
| identity/locality | root public test | permutations canonical; C1a ID equals C0 exact writer bytes; locality-only ID stability; mismatch rejects | identity coupling/field forge |
| lifetime/coherence | `cargo test -p nudox-hydration --test compile_fail` | root/view literals, root/locality fact reassignment through `&mut` witness, and root/view escape cases fail | compilable forge/reassignment/escape |
| allocation/drop/pointers | `cargo run --release --manifest-path layout-lab/Cargo.toml --bin p1-canonical-root-control -- --repetitions 3 > layout-lab/raw/p1-canonical-root-control.tsv` | C0 root construction, canonical encoding, and `PreparedLocality::prepare(&c0_root, &[])`/write/validate all finish before every measurement. One C1a validation/drop scope starts immediately before `ValidatedRoot::try_from`, builds the paired borrowed view against that prevalidated locality, drops the borrowed view and then `ValidatedRoot` before scope close: `N>0` must report allocations/deallocations `1/1`, requested/peak `4*N`; `N=0` must report `0/0`, `0/0`. A separate warmed paired `get`/`closure` scope must report `0/0` allocations/deallocations. Public root tests prove root and locality pointers lie within their caller bytes and source review proves no trusted traversal copy. Every successful TSV row declares `allocator_failure_source=UNVERIFIED`. | missing measured raw row/UNVERIFIED declaration, synthetic counter, wrong scoped alloc/drop facts, >1 validation allocation, retained scratch, or warmed get/closure allocation |
| C0 preservation | `cargo test -p nudox-root -p nudox-hydration` | all existing C0/package tests pass, no P2 diff | any regression/surface |

C1a's `TryFrom` carries the exact `TryReserveError` source and checked requested-byte
observation from its one fallible hierarchy reservation. The error-path source-preservation
claim is code/error-structure evidence; because a valid `TryFrom` input large enough to
deterministically force ordinary allocator failure is not portable, actual allocator-OOM
injection is an explicit platform UNVERIFIED rather than a synthetic public-API claim. Every
successful lab row must write `allocator_failure_source=UNVERIFIED`, while every actual
reservation error leaves the caller’s borrowed bytes usable. A C0 root and its exact empty
locality artifact are prepared, written, and validated before the first successful-path tracking
scope starts. That scope begins immediately before C1a validation, constructs the paired view
only from that prevalidated locality, drops the view and `ValidatedRoot`, and then closes; the
distinct warmed-view scope surrounds only `get` and `closure`. Neither scope includes fixture
setup, canonical encoding, locality preparation, output, or a counter inferred from `N`. C0
builder allocation-failure/drop evidence remains the safe construction control. The raw lab must
name the host compiler/target/release profile, every workload, and C1a's three setup categories
(schema provenance, typed-slice, hierarchy).

The immutable-facts, destructive-memoization, and real-evidence corrections spend 287
additional forecast production lines versus the first typed-slice C1a card and raise the
production ceiling to 620. They change no semantic C0 root/locality reads or canonical bytes:
the former closes post-validation fact rewriting, and the latter converts an infeasible
quadratic validator into bounded fallible setup with no retained state or synthetic evidence.

## Plan closure

Fresh explicit Luna and independent Terra roles must calibrate this exact card. A passing
card authorizes one isolated Luna C1a implementation branch. The reviewer then attacks
that artifact; only a passing C1a could authorize a separate C1b card. No final verdict,
publication capability, or merge authority is implied.
