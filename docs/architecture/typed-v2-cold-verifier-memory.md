# Typed V2 cold-verifier memory and I/O model

The cold V2 history verifier no longer retains one `Vec<u8>` per semantic
segment. It stages the exact, unique FileStore closure in a private temporary
spool, then lends each segment through one reusable buffer capped at
`MAX_SEMANTIC_SEGMENT_BYTES` (1 MiB). The spool is removed on success and on
ordinary error returns. No segment slice escapes the source borrow, and the
typed inventory proof is minted only after row roots, the seven-family joins,
jumbo closure, canonical cuts, and all counts succeed.

The payload-buffer bound is therefore independent of the total segment
payload: one 1 MiB segment buffer, the FileStore copy loop's 16 KiB buffer,
and the jumbo verifier's bounded leaf buffer (at most 256 KiB) are live during
semantic verification. The first semantic pass admits jumbo references and
the second computes row roots and cross-family facts. Both passes read the
spool through the same lending source; they do not reopen FileStore objects.

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

Before spooling, FileStore payload reads were `2U + 2S + 2J`: the closure
admission opened and hashed every unique object (`U`), the stored-closure
reopen hashed the closure again (`U`), every semantic segment open hashed and
then range-read its payload (`2S`), and every jumbo object traversal open
hashed and then range-read it (`2J`). `J` is reference-weighted, so repeated
references to one leaf count repeatedly. The loose independent-limit sum is
about 3 GiB; it is intentionally not presented as a jointly attainable
workload because closure uniqueness and segment/rope membership constrain
the terms.

With the current private-spool implementation, FileStore payload reads are
`2U`: one full verification pass in `open_object_limited`, followed by one
bounded copy into the spool. Spool traffic is `U` written, `2S` read for the
two semantic passes, and `J` read for jumbo closure. This removes repeated
FileStore opens during semantic verification while retaining exact physical
closure admission. A FileStore API that hashes and copies each payload in one
tentative streaming pass could reduce FileStore reads to `U`; the current
reader verifies the full payload before exposing ranges, so the separate
copy pass remains until that API is added.

## Evidence and limits

The streaming aggregate tests compare its seven-family proof and row roots
with the materialized verifier and reject reordered and truncated sources.
Replication tests cover partial and extra closure membership and wrong jumbo
schema. A deterministic sparse-spool shape test exercises a full 512 MiB
segment tier while checking that the same one-segment buffer is reused. These
source-level tests do not report process RSS. Cargo validation and a measured
RSS run remain blocked on the repository's shared four-build slot; no timing
or RSS numbers are claimed here.

The temporary spool needs disk space up to the exact closure byte limit (512
MiB for LargePackage) in the system temp directory. A process crash can leave
its file behind because ordinary Rust drop cleanup cannot run after a crash;
stale-file scavenging is not implemented yet. Directory permission, quota,
and free-space exhaustion surface as admission errors. The spool is an
intermediate verified-payload cache only; it carries no authority after the
enclosing cold-verification call returns.
