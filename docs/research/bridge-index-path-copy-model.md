# Typed history bridge-index path-copy model

**Base:** `8de13e6dc` (`codex/index-compiler-tentpole`). **Scope:** isolated, source-only model; no production changes and no Cargo command. The companion [`bridge_index_path_copy_model.py`](bridge_index_path_copy_model.py) uses only Python's standard library and prints the measurements below.

## Finding

The current typed-V2 history locator repeats both the c005 manifest bytes and the complete semantic-to-FileStore bridge table in its per-commit sidecar. At the allowed 65,536 segment descriptors, that is 18,875,306 canonical payload bytes in the simplest no-jumbo case when counting the c005 object once and the locator sidecar once. A typed path-copy bridge tree can make the bridge projection local, but its benefit depends on not embedding a second complete c005 copy in the locator. The c005 object itself remains O(all segment descriptors) per changed generation; replacing that wire object is a separate format migration.

The model finds a useful one-row-edit opportunity, but does not justify transplanting old VCS storage wholesale. Existing `backend_version::LazyTree` updates each sorted `TreeChange` in sequence. Replacing one content-addressed segment means removing its old 32-byte semantic ID and inserting the new one, so the model charges two single-key path copies. At 100 replacements in one prepared update, the accumulated intermediate canonical nodes can exceed the current flat bridge vector. A batched multi-key tree rewrite or smaller descriptor pages would need separate evidence before making a 100-edit claim.

## Source facts and accounting

The current [`TypedV2HistoryLocator`](../../crates/replication/src/ir_generation_store/history/v2.rs) stores raw canonical manifest bytes, sorted segment mappings, and sorted jumbo mappings. A segment mapping serializes a 32-byte semantic segment ID, 32-byte FileStore object ID, and u64 byte length: 72 bytes. A jumbo mapping adds a one-byte kind to its 32-byte rope ID, FileStore object ID, and byte length: 73 bytes. The locator body has a six-byte header, a u64-sized manifest field, and two u32 counts; its durable sidecar adds a 32-byte identity and 32-byte checksum. Admission currently decodes and canonicalizes the c005 manifest, sorts manifest segment IDs, checks the exact segment ID/length bridge, and checks jumbo kind-specific lengths. Those checks are a correctness contract, not overhead to drop.

The c005 size follows [`SemanticTypedPlaneManifestV2::encoded_length`](../../crates/semantic/src/ir/typed_plane_manifest_v2.rs): 108 bytes per segment descriptor, plus fixed build/input/root/family framing. The model uses a 426-byte fixed portion for `Shared` authority, unavailable provenance, and the seven required family headers; it places all 65,536 descriptors in one family. The resulting c005 payload is 7,078,314 bytes. A production distribution among families changes only the small fixed header term, not the 108-byte descriptor cost.

The proposed map is a typed relation with a fixed 32-byte semantic ID key and a 40-byte value (`FileStore ObjectId` plus u64 byte length). The relation schema is a placeholder in the model. It must be a closed, versioned schema in production. Segment and jumbo IDs should use separate typed roots or a kind-tagged key, because equal 32-byte IDs from different object kinds must not alias. The node width is 88 bytes per leaf entry and 88 bytes per branch child, including canonical length frames. Node headers and the 64 KiB / 64-256-1024 cut policy match the current `backend_version` source.

The model compares one c005 object plus one per-commit locator file. The locator itself currently embeds another full c005 copy, so the current accounting is:

```text
current = c005 object + locator header/checksum + embedded c005 + flat bridge table
        = 7,078,314 + 86 + 7,078,314 + (72 * segment_count) + (73 * jumbo_count)
```

The sidecar wrapper and FileStore object envelopes are excluded; this compares canonical payload and locator bytes. At 65,536 segments and no jumbo objects, the flat bridge is 4,718,592 bytes and the total is 18,875,306 bytes. If a caller's accounting already excludes the embedded c005 copy, subtract 7,078,314 bytes from both current and conservative path-copy totals; do not silently count it twice or omit it.

## Measurements

The model replaces 1, 10, or 100 existing segment keys with new content-addressed keys in one target commit. Each replacement is two sorted single-key changes: remove the old semantic ID, insert the new ID. It reconstructs canonical tree states after each change and counts the changed canonical node bytes absent from the immediate parent root. This mirrors the sequential update loop in [`LazyTree::prepare_update_inner`](../../crates/version/src/persistent/lazy.rs). The `SHA-256` stand-in replaces BLAKE3 only for content-defined cuts and node commitments; source byte widths and frame lengths are copied from the Rust implementation. The values therefore estimate realistic path locality, not exact Rust node counts.

| Logical segment replacements in one commit | One-key changes | Path-copy nodes touched | Path-copy canonical bytes touched | Current c005 + locator | Path copy, c005 still inline | Path copy, locator references c005 |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 2 | 8 | 204,968 B | 18,875,306 B | 14,361,722 B | 7,283,424 B |
| 10 | 20 | 61 | 1,033,264 B | 18,875,306 B | 15,190,018 B | 8,111,720 B |
| 100 | 200 | 627 | 10,617,024 B | 18,875,306 B | 24,773,778 B | 17,695,480 B |

The last column assumes the locator names the exact c005 FileStore object already pinned by the commit closure, using a 32-byte object claim, and retains a 40-byte bridge-root/count descriptor. It still counts the c005 object once per changed generation and every path-copy node emitted by the modeled update. It does not assume c005 descriptors themselves are persistent. At 100 replacements the path-copy tree alone emits about 10.6 MB, more than the flat bridge list; the savings in the last column come mostly from removing the duplicate embedded c005 manifest. Keeping c005 inline turns that case into a regression. Jumbos are excluded, so 4,718,592 bytes is a lower bound on the flat bridge-list cost when ropes are present.

## Safe integration shape

Treat this as a two-part change with separate proofs:

1. Replace locator-owned vectors with typed segment and jumbo bridge roots. Bind each root to its relation schema/cut-policy version, exact entry count, and the exact c005 object claim (or equivalent manifest identity). Keep the current exact c005 descriptor, payload, length, kind, and FileStore-envelope checks. Use `LazyTree`/`PersistedTreeRoot` for bounded path copies, CAS the commit against its exact parent roots, and admit update budgets before allocating edit/frontier state. The current `LazyTree` path-copy mechanics are reusable, but the source shows sequential single-key steps; do not market it as O(k log n) output bytes without a batched update that shares ancestors.
2. Make the locator resolve c005 through the commit's pinned FileStore closure instead of copying raw c005 bytes into the locator. On cold verification, obtain the exact manifest object from that closure and compare every descriptor with the authenticated bridge index. The current manifest order is family/range order while the bridge index is semantic-ID order; a safe verifier needs the same sort-and-compare contract (the current implementation materializes and sorts up to 65,536 IDs), or a changed canonical order/proof. A root claim alone does not prove bridge completeness.

The index root must itself be retained by history GC. Either include its immutable node closure in the commit's payload closure or make the GC mark walker traverse its nodes from every retained commit root. Leaf FileStore IDs must remain covered by the existing exact artifact closure; a bridge tree root is not permission to skip payload closure verification. Pin roots while replay/range hydration is in flight. For range hydration, use c005's family and stable-key intervals to choose semantic segment IDs, then resolve those IDs through the bridge tree; the bridge tree is keyed by object ID and cannot replace a row-range index. Verify each fetched c004/jumbo envelope and semantic content claim before returning bytes.

The immediate safe win is eliminating duplicated locator bytes and using the tree for local bridge updates while retaining full cold set verification. A later manifest-tree format could reduce the remaining c005 O(N) rewrite, but it changes the semantic manifest contract, range lookup path, and content/generation root admission. That is distinct from this bridge-index prototype.

## Limits

This is deterministic model evidence, not a benchmark of `LazyTree` or FileStore. SHA-256 changes the exact cut positions versus BLAKE3; `LazyTree` may emit transient spill nodes that a whole-tree closure comparison does not count, and global FileStore CAS reuse can lower physical writes. The model assumes fixed 40-byte bridge values, 32-byte keys, 65,536 segment descriptors, no jumbo objects, no cold verification work, and no object-envelope overhead. It does not count GC traversal, locator decode allocations, commit metadata, or range-read costs. Validate these figures with an isolated Rust benchmark when a Cargo slot is assigned; retain exact cold verification regardless of the modeled savings.
