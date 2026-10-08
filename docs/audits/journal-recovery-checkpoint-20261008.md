# Bounded journal recovery and truthful publication

The index owner now observes committed operation-journal changes through a bounded coalesced notification worker. A quiet healthy reader performs no SQL polling. Native watch failure, exhausted notification sequences and disconnected local wakes degrade coverage explicitly; finite user retries still refresh and recover work. SQL commit succeeds independently of an advisory wake failure.

Prepared work remains serialized through the publication barrier. Foreign prepared work, busy SQL writers, lost hints and cold recovery retain the exact accepted operation and candidate until recovery or an explicit retry can proceed. All terminal refused source captures can no longer become Published simply because their selected workspace commit matches. A live typed compiler refusal remains the primary failure; cold recovery reports its actual worker failure without inventing a cause.

Root reviewed the complete six-file production/test slice and the later selected-refusal correction. The isolated 12-atom stack was applied to fetched canonical `f92d4d0ff05bcb721ba69fc2d2ed43fc7ecff750`, preserving the typed-absent adapter work from PR67. The only lock change is the existing `notify 7.0.0` dependency edge; canonical's Tantivy allocation-counter edge is retained.

## Native evidence

Native09 at `1b46051a4bbc69440092bc7be41d80c918419282` / tree `248880a7e3e77203b23da485f8a1efac6a0b43ae` passed 50 controls with zero failures or ignored tests in 47.61 test seconds (276.526 seconds overall). These cover quiet operation, foreign commits/WAL close, wake loss, SQL contention, explicit retry, prepared recovery, cold reopen, truthful native Python/TypeScript partial publication, selected refusal, and source-capture terminalization failures. Earlier native08's 45 passes and three publication failures are retained by the worker and are not relabeled passes.

Root independently verified all 17,647 tracked source archive entries against Git blob hashes and modes, all six changed-file preimages/postimages, raw receipt/log seals, actual 50 unique test names/results, source/lock identity before and after, fresh fleet admission and recorded kernel-wait retirement. The supervisor sealed native test image SHA256 `402f453f816c9b7f782a93b41b98f027bfe9331bf06da57a13037192acb47703`. Root did not independently reread that remote executable.

Compact raw logs and [the integration seal](../operations/evidence/journal-recovery-20261008/integration-seal.json) are committed. The complete 174,824,806-byte source archive remains in the local audit directory, sealed by SHA256 `0ec975be39a7726eaf0442d85396354b2e974efe2fa014419cc1967b0ce64d23`.

## Remaining gates

This is a source-bound native regression checkpoint. A new matched CLI/MCP/local-service trio must still exercise real Docs indexing, truthful Partial completion, responsive progress/cancellation, remove/re-add, cold replay and quiet-owner CPU. The merged composition with PR67 has not yet had a complete native rerun. The later Python dependency-graph admission repair and pinned-rustix FIFO test correction are separate gates; their compile-failed native10 attempt ran zero tests and receives no pass credit here.
