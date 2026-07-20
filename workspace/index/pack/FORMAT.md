# NDPK v1 — the ObjectPack container format

One format for source trees, VM goldens, and compile stages (INDEX-PLAN
§6, ID-13/ID-16). Range-addressable, highly compressed, content-addressed.
Every byte is specified here; nothing in a pack depends on wall-clock time,
filesystem order, or insertion order.

All multi-byte integers are **little-endian**.

## File layout

```
┌───────────────────────────────────────────────────────────────┐
│ Header            fixed 24 bytes                               │
├───────────────────────────────────────────────────────────────┤
│ Member frames     zstd frames, one or more chunks per member   │
│                   (members concatenated in TOC-key order)      │
├───────────────────────────────────────────────────────────────┤
│ Table of contents postcard-encoded, at header.toc_offset       │
└───────────────────────────────────────────────────────────────┘
```

## Header (24 bytes, fixed)

| Offset | Size | Field         | Value                                    |
|-------:|-----:|---------------|------------------------------------------|
| 0      | 4    | `magic`       | ASCII `NDPK` (`0x4E 0x44 0x50 0x4B`)     |
| 4      | 2    | `version`     | u16 = `1`                                |
| 6      | 2    | `flags`       | u16; known mask `0x0000` in v1           |
| 8      | 8    | `toc_offset`  | u64 byte offset of the TOC from file start |
| 16     | 8    | `toc_length`  | u64 byte length of the TOC               |

- A reader accepts versions in `[NDPK_VERSION_MIN_READ ..= NDPK_VERSION_CURRENT]`
  (N/N-1 support, INDEX-PLAN ID-22). Both are `1` today.
- Any flag bit outside the known mask ⇒ `PackError::UnknownFlags`
  (forward-incompatible pack, fail closed).

## Members and chunking

A member's uncompressed bytes are split into **chunks** of
`SOURCE_CHUNK_SIZE_BYTES = 128 KiB` (the final chunk may be shorter). Each chunk
is compressed into an **independent** zstd frame. A member smaller than one
chunk is a single frame; an empty member is one empty chunk.

Each chunk is compressed at `ZSTD_COMPRESSION_LEVEL = 19` — a **format
constant**. The frames of a member are stored contiguously; members are stored
in ascending `MemberKey` order.

### Why chunk?

INDEX-PLAN §6.2 requires byte-range gets *within* a member (single-snippet
serve). A monolithic zstd frame cannot be randomly accessed — a range read
would decompress the whole member. Independent per-chunk frames let a range get
decompress only the chunks its `[start, end)` overlaps.

**Tradeoff.** Smaller chunks improve range locality (a single function body
almost always lands in one 128 KiB chunk, so a snippet serve decompresses ~128
KiB regardless of file size) but cost zstd per-frame header overhead and a
larger chunk table; they also forgo cross-chunk compression context. 128 KiB is
the chosen balance and is fixed by the format (changing it changes every pack's
bytes and its `ObjectPackId`).

### Bao outboards (verified range streaming)

A member whose uncompressed length is ≥ `BAO_OUTBOARD_THRESHOLD_BYTES = 1 MiB`
sets `MemberRecord.bao_outboard = true` and gets a **Bao outboard** generated at
seal time (INDEX-PLAN §6.2/§7.2). An outboard is a BLAKE3 Merkle tree
(`bao-tree`, `BlockSize::ZERO` → 1 KiB chunks, the iroh-blobs default) that lets
a peer request an arbitrary byte range and receive, with the range bytes, the
interior hashes needed to verify that slice against the member's single root
hash — the basis of "fetch one snippet, prove it's genuine" over iroh.

**Generated over the uncompressed bytes.** The outboard spans the member's
*uncompressed* content, not its zstd-framed on-disk form, because (1) verified
range streaming addresses ranges of logical content — a snippet range must map
to a verifiable subtree; (2) the member's `content` digest is already BLAKE3 of
the uncompressed bytes, so the outboard's **root hash equals `content`**, making
one identity anchor both the pack integrity check and the streaming proof; and
(3) zstd frame boundaries are a storage detail that must not leak into the
verification tree, or re-chunking would invalidate proofs.

#### Placement decision — sidecar, never inside the pack

Outboards are stored in a **separate sidecar file** beside the pack:
`<id-hex>.ndob` next to `<id-hex>.ndpk` in the store, holding a postcard-encoded
`OutboardSidecar { members: Vec<MemberOutboard> }` (sorted by `MemberKey`).

The outboard is **not** inside the pack, and its presence does **not** affect the
`ObjectPackId`, for three reasons:

1. **Frozen pack bytes / stable id.** The `ObjectPackId` commits only to the TOC
   + policy. Embedding outboards would change every large pack's bytes and id,
   and would couple identity to a `bao-tree` version. Keeping them out means the
   id is a pure function of content, and the outboard is a *regenerable
   projection* of the pack.
2. **Rebuildable.** An outboard can be regenerated at any time from the pack's
   own member bytes (the pack is the durable anchor; the sidecar is derived), so
   a lost or version-skewed sidecar is never a data-loss event — matching the
   "disposable/rebuildable projection" posture of INDEX-PLAN.
3. **Optional / large-member-only.** Sub-threshold members carry no outboard;
   a pack with no large members writes no sidecar at all. A sidecar file's
   absence is normal and reads back as an empty `OutboardSidecar`.

The store writes the pack first, then the sidecar, each atomically
(tempfile + rename). The pack is the atomicity anchor: if the sidecar write
fails the outboard can be regenerated, so no partial-outboard state is durable.

## Table of contents

Serialized with **postcard** (deterministic, self-describing binary; no
map-iteration nondeterminism; stable field order). The TOC is:

```
TableOfContents {
    policy:  PackPolicy { version, zstd_compression_level, source_chunk_size_bytes },
    records: Vec<MemberRecord>,   // strictly ascending by key
}
```

Each `MemberRecord` carries: `key`, first-frame `offset`, `compressed_length`,
`uncompressed_length`, `content` (BLAKE3 of the whole uncompressed member),
`chunks: Vec<ChunkEntry>` (each `{frame_offset, compressed_length,
uncompressed_length}`), and `bao_outboard`.

The `records` vector **must** be strictly ascending by `MemberKey`. This is
enforced on both encode and decode; a pack whose TOC is unsorted or has
duplicate keys is rejected with `PackError::TocNotSorted`.

## ObjectPackId (identity)

```
ObjectPackId = BLAKE3( "ndpk-id-v1"
                     ‖ "ndpk-policy-v1" ‖ version ‖ zstd_level ‖ chunk_size
                     ‖ u64_le(len(toc_bytes)) ‖ toc_bytes )
```

Because each TOC row commits (via `content` and frame offsets) to its member's
decompressed bytes, hashing the canonical TOC transitively commits to the whole
pack. The **policy bytes** are folded in so two packs that differ only in a
format constant get different ids.

## Determinism guarantees

The same input tree produces **byte-identical** pack bytes and thus the same
`ObjectPackId`. Enforced by:

1. **Fixed zstd level** (`19`) — no adaptive/level drift.
2. **No timestamps** — nothing in any byte comes from the clock or filesystem
   metadata.
3. **Sorted iteration** — members are held in a `BTreeMap<MemberKey, _>` and the
   TOC is strictly key-sorted, so insertion order is irrelevant.
4. **Stable codec** — postcard with fixed struct field order.
5. **Fixed chunk boundaries** — `128 KiB` uncompressed, so frame boundaries
   depend only on content, not on how bytes were fed in.

## Error taxonomy (never panic)

Adversarial or corrupt input maps to a typed `PackError`: `Truncated`,
`BadMagic`, `UnsupportedVersion`, `UnknownFlags`, `BadStructure`, `TocDecode`,
`TocHashMismatch`, `TocNotSorted`, `MemberHashMismatch`, `FrameDecode`,
`MemberNotFound`, `RangeOutOfBounds`, `AbsolutePath`, `UnsafePathSegment`,
`DuplicateMember`, `TooLarge`, `Io`, plus the Bao/transport variants:
`BaoEncode`, `BaoDecode`, `NoOutboard`, `Transport`, `NotEnrolled`,
`FetchedIdMismatch`, `Codec` (and the legacy `TransportNotWired`).
