# Packed immutable collection boundary

Roots, locality maps, manifests, and immutable indexes are canonical artifacts first and Rust
collections second. Their portable API borrows validated regions; it never requires one heap owner per
region.

## Shape

```text
artifact header
  generation identity
  exception/promise/overlay/present counts

derived semantic lanes
  sorted exception rows
  class bits + rank directory
  provider values
  overlay-presence bits + rank directory
  present object-descriptor records
  optional shared overlay basis
```

The verified request carries the centrally registered `LocalitySortedEncoding`; the artifact does not
repeat magic, version, kind, offsets, lengths, or lane descriptors. Counts and fixed record widths are
sufficient for one checked monotone layout derivation that both validation and writing consume.
External range-verification/outboard data authenticates partial fetches without changing these bytes.
Records use unaligned explicit-endian fields and closed discriminants, so `zerocopy` can lend typed
slices from received bytes. Semantic APIs reconstruct compact values such as `ObjectRef` by value;
they do not retain native-layout structs as a second authority.

A complete primary view borrows every derived lane from one artifact owner:

```text
ValidatedLocality<'artifact, ObjectDomain>
  artifact: &'artifact [u8]
  layout: one proved LocalityLayout
```

Lane access is a range reborrow into that one stack buffer, pooled network slab, mmap, or caller
arena—not one owner per lane. A later partial clone binds separately authenticated leased ranges
through an explicit typestate; missing regions never masquerade as empty complete slices.

## Locality normalization

One coherent overlay layer names its remote base generation once. Mixed remote bases are separate
layers or a precise construction rejection; they do not justify repeating a 32-byte generation
identity in every exceptional row.

The first reference representation is one packed record per exceptional row, but that is not the
default winner. Row coordinates are a monotone set and payload classes are closed, so the stronger
candidate is a succinct row set plus parallel semantic lanes:

```text
exception rows: rank/select dictionary over root-row coordinates
class bits:     promise | overlay, indexed by exception rank
providers:      non-empty ProviderSet values, indexed by promise rank
overlay-present bits, indexed by overlay rank
present bases:  ObjectRef records, indexed by present-overlay rank
overlay basis:  one GenerationId for the complete coherent layer
```

Membership returns both presence and exception rank. Rank then projects directly into the class and
payload lanes; no stored payload index is needed. A canonical scan advances one monotone set cursor,
and homogeneous payload iteration does not test unrelated variants per row. The artifact header names
the row-set grammar once. Cold validation dispatches to a concrete view and the hot traversal remains
monomorphized—never `dyn` and never a backend enum match inside the row loop.

The semantic projection is expressed with non-interchangeable coordinates, not raw integers:

```text
ExceptionRows::rank(RootRow) -> Option<ExceptionOrdinal>
ClassBits::overlay_rank(ExceptionOrdinal) -> PromiseOrdinal | OverlayOrdinal
OverlayPresence::present_rank(OverlayOrdinal) -> Absent | PresentOverlayOrdinal
```

The `Option` above represents genuine row membership absence. Once an ordinal exists, payload access
is infallible: validation proved lane cardinalities and ranks together. Internal index conversion must
not use `.ok()?`, `filter_map`, defaults, or an `Internal` public error to shorten a valid stream.

Compare at least four row-set grammars:

- plain sorted `u32` coordinates for tiny cardinalities;
- a dense bit vector with rank samples for dense exceptions and branch-light word scans;
- Elias–Fano for large sparse monotone sets;
- restart-block delta/bitpacking for sequential scans and SIMD block decode.

The comparison includes the rank directory and padding, not just the coordinate payload. Candidate
13-byte packed and 16-byte aligned array-of-struct route records remain baselines. Freeze no adaptive
wire choice until empty, one, density-cliff, 100k, sequential, random, local partial-fetch, and remote
scan measurements establish exact retained/transferred bytes and total work.

## Construction

A `PreparedLocality<'facts, Domain>` borrows immutable sorted `LocalityException` input, performs every
ordering/coherence/count/layout check once, and exposes an exact typed output length. Its consuming
write first proves the output prefix exists, then emits every lane directly and infallibly into caller
output. It does not stage `Vec` arrays and convert them into `Box` slices. A convenience allocation,
when genuinely needed, allocates one exact byte artifact only after preparation and is outside the
borrowed semantic view.

- Root preparation accepts caller-owned mutable entries/scratch, sorts and validates once, and emits
  deterministic compact parent coordinates.
- Locality preparation accepts canonical facts or an in-place sortable caller slice, merge-validates
  them against the root, normalizes the shared overlay basis, and emits routes/exception payloads in a
  second pass.
- Overlay propagation uses caller marks and two root scans; it writes final regions directly instead of
  materializing intermediate locality facts.

Every failure retains its rejected field/value and allocation/output source. Output-too-small and
invalid input fail before the first byte is modified.

## Ownership adapters

The primary view has no owner and no allocator. When an owner must escape, an adapter is generic over
the concrete immutable byte owner and lends the view with an HRTB callback. A self-referential wrapper
is considered only when a dependent view must itself be stored and measurement proves the extra heap
owner removes a copy or repeated validation.

Const-inline, caller-arena, pooled-lease, mmap, and boxed-byte owners remain adapter choices. The
semantic root/locality crates do not expose their allocation classes as domain state.

## Acceptance evidence

- zero artifact-owned allocations with caller output and borrowed validation;
- exact pointer containment in supplied regions and no payload copy;
- identical canonical bytes/identity from local and remote builders;
- full, header+routes-only, and lazily completed views with honest typestate;
- 0/1/100k promised, absent-overlay, present-overlay, and mixed profiles;
- exact retained/transferred bytes, allocator calls, cache lines, comparisons, branches, and code size;
- exact rank/select directory overhead and density crossover among sorted, dense, Elias–Fano, and
  block-bitpacked row sets;
- truncation/mutation at every region boundary and cross-region overlap/alias rejection;
- optional owner adapter drop/cancellation and Miri coverage;
- range-fetched regions bind only when their count, digest/proof, and artifact identity match the
  validated header.
