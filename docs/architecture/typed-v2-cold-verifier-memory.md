# Typed V2 cold-verifier memory and I/O model

The cold V2 history verifier no longer retains one `Vec<u8>` per semantic
segment. It stages the exact, unique FileStore closure in a private temporary
spool, then lends each segment through one reusable buffer capped at
`MAX_SEMANTIC_SEGMENT_BYTES` (1 MiB). Unix unlinks the owner-only spool
immediately; Windows marks it delete-on-close. No segment slice escapes the
source borrow, and the typed inventory proof is minted only after row roots,
the seven-family joins, jumbo closure, canonical cuts, and all counts succeed.

The payload-buffer bound is therefore independent of the total segment
payload: one 1 MiB segment buffer, the FileStore verifier's 16 KiB buffer,
the jumbo verifier's bounded leaf buffer (at most 256 KiB), and at most one
64 KiB relation-object accumulator are live during their respective phases.
The first semantic pass admits jumbo references and the second computes row
roots and cross-family facts. Both passes read the spool through the same
lending source; they do not reopen FileStore objects.

This is a payload-residency bound, not a claim that total verifier RSS is
1 MiB. The large-package tier still admits at most 65,536 segment descriptors,
200,000 physical closure objects, 512 MiB of semantic segment bytes, 2,000,000
rows, 8,000,000 semantic references, and 16,000,000 aggregate reference
scratch entries. The row index retains one fixed-width key/payload identity
slot per row while building one family at a time. Cross-family catalogs retain
bounded keys, edges, owner facts, and reference vectors. Those counts are
finite and each entry has a fixed-size representation, but their simultaneous
heap capacity, allocator slack, tree node metadata, and host RSS have not yet
been measured. Treat the joins as potentially hundreds of MiB at the large
tier; do not infer a low total-RSS ceiling from the segment-buffer bound.

## I/O accounting

Let `U` be the byte sum of unique physical objects in the admitted closure,
`S` the byte sum of logical segment descriptors (including any repeated
physical mapping), and `J` the reference-weighted jumbo traversal bytes,
including the exact interior-node wire bytes charged by the semantic budget.
Each is independently bounded by its applicable tier; `U` and `S` are at most
512 MiB at LargePackage and `J` is at most 512 MiB plus 65,536 node wires.

Before the bounded spool path, FileStore payload reads were `2U + 2S + 2J`:
the closure-member scan hashed every unique object (`U`), stored-closure
reopen hashed the closure again (`U`), and each segment and jumbo visit hashed
its object and range-read its payload (`2S + 2J`). The semantic pass kept all
logical segment payloads resident together. `J` is reference-weighted, so
repeated references to one leaf count repeatedly. The sum of independent tier
maxima is about 3 GiB of payload reads; this is a conservative envelope, not a
jointly attainable-workload claim.

The first bounded-spool implementation reduced FileStore reads to `2U`: it
hashed each object, then copied its payload to the spool from the verified
reader. It wrote `U` bytes to the spool and later read `2S + J` bytes for the
two segment passes and reference-weighted jumbo traversal.

With callback-scoped tentative chunks, FileStore reads are `U`: each unique
object is hashed and sent to the spool during that same fixed-buffer pass.
The caller rolls back the provisional spool tail if the object hash, typed
version, relation admission, exact length, or mapped schema fails. The spool
index receives an admitted member only after all those checks return success.
Spool traffic remains `U` written and `2S + J` read. Total payload I/O,
counting spool writes and reads, is now `2U + 2S + J`; the initial
bounded-spool path was `3U + 2S + J`. Closure B-tree membership lookups and
semantic descriptor lookups add metadata I/O not included in these totals.

## Evidence and limits

The streaming aggregate tests compare its seven-family proof and row roots
with the materialized verifier and reject reordered and truncated sources.
Replication tests cover partial and extra closure membership and wrong jumbo
schema, plus a nonempty cold FileStore restart with all seven family slots, four
typed segments, and a complete multi-leaf documentation rope. Its expected
roots are derived independently from the public stable-row index before the
cold lending proof is compared. A deterministic sparse-spool shape test
exercises a full 512 MiB segment tier while checking that the same one-segment
buffer is reused. These source-level tests do not report process RSS. Cargo
validation and a measured RSS run remain blocked on the repository's shared
four-build slot; no timing or RSS numbers are claimed here.

The temporary spool needs disk space up to the exact closure byte limit (512
MiB for LargePackage) in the system temp directory. Unix unlinks the owner-only
file immediately after `create_new`, so a crash closes and reclaims its
anonymous inode. Windows opens with `FILE_FLAG_DELETE_ON_CLOSE`; the spool is
removed when its last cloned handle closes, including after process exit. On
other targets, the implementation still uses a create-new path and ordinary
drop cleanup, so a crash can leave an orphan there. Directory permission,
quota, and free-space exhaustion surface as admission errors. The spool is an
intermediate verified-payload cache only; it carries no authority after the
enclosing cold-verification call returns.
