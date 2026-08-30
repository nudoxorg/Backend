# Wave B4 immutable-index proof matrix

| ID | Law | Weakened implementation | Falsifier / required evidence | State | Owner |
| --- | --- | --- | --- | --- | --- |
| B4-01 | A typed query pins the named snapshot, never a mutable head. | Read current head after planning. | Pin S1, publish S2 then compact S3; S1 still gives `alpha=v1`, S2 gives `v2`. | RED | Sol public journey / future query owner |
| B4-02 | Snapshot identity binds immutable typed exact/lexical segment references and stable recipe/input facts. | Mix family IDs, route, or tier facts. | Header/directory mutation and cross-family negatives; IDs equal only for identical canonical bytes. | RED | future format/view owner |
| B4-03 | Views validate once and borrow original bytes with exact truncate/mutation errors. | Deserialize/retain rows or reparse trusted fields. | Every-byte truncation, field mutation, pointer containment, repeated-access allocation and scan counters. | RED | future format/view owner |
| B4-04 | Sealed delta chronology distinguishes insert, update, delete, and tombstone. | Append updates or omit tombstones. | D1/D2: S1 alpha v1/beta visible; S2 alpha v2/beta complete zero-hit; repeat D2 proves idempotency/conflict. | RED | future exact owner |
| B4-05 | Exact lookup/prefix work is bounded by selected segments and output, not corpus size. | Scan all docs or build IDs in comparisons. | Count selected segments, comparisons, ranges, decoded rows at empty/one/boundary/density cases. | RED | future exact owner |
| B4-06 | Lexical score, tie order, and provenance are deterministic recipe facts. | Unordered merge or unconstrained backend float. | Input/worker/segment permutation gives identical rows, typed scores, tie keys, provenance. | RED | future lexical owner |
| B4-07 | Selected absent segment/range is exact `Partial`, not zero hits. | Swallow leaf failure/return empty. | E2 missing yields `Partial { missing: [E2] }`; present no-match is `Complete` zero rows. | RED | future terminal owner |
| B4-08 | Requests name snapshot/segment IDs; rendezvous is advisory and lost/stale route can retry. | Route determines truth or silently changes snapshot. | First E2 route absent; retry equals local; no retry names E2/attempt/range. | RED | future horizontal adapter owner |
| B4-09 | Local/RAM/NVMe/object/remote byte owners preserve IDs, plans, and semantic results. | Tier leaks into ID or backend changes merge. | Equal bytes from independent owners give equal plan/rows/order/score/provenance; tier move preserves IDs. | RED | future range/query adapter owner |
| B4-10 | Compaction is independently built, equivalence-checked, atomically replaces explicit input. | Local repack or head update before verification. | Independent E3/L3 from E1/E2/L1/L2 matches S2 exact+lexical sets before S3 publication; S2 pin survives. | RED | future build/publish owner |
| B4-11 | Hot paths have measured ownership, allocation, work, branch, range, and text controls. | Hidden cache/heap/branch/second scan. | Safe-control comparison; isolated non-empty allocation; retained bytes/copies/ranges/branches/release text. | RED | Terra plus future owners |
| B4-12 | Compiler/probe/backend availability cannot alter data truth. | Query compiles/rescans or formats/allocates disabled probe. | Sealed deltas work with compiler unavailable; disabled typed probe builder not evaluated; adapter returns typed partial/degraded. | RED | future integration owner |

Existing I0 aliases are a prerequisite identity vocabulary only. They prove none of this capability's
manifest, delta, query, publication, lease, route, or compaction rows.
