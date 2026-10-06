# Staged queue and cold replay source checkpoint

This builds on membership checkpoint `5bcbed3f16186feb9dc0c4300306a88eb31714aa`
and imports the root-owned compiler-fault exhaustiveness dependency as
`bfc9dbba9f` (original `c66060c948`). No compiler authority implementation was
otherwise edited.

`finish_add` now lends the admitted canonical source root and job cancellation
flag to `prepare_builtin_intent_at`. Intents above 1 MiB are written into typed
64 KiB SourceRows and CompleteFacts pages. Actual available changed source
files are additionally streamed into RawSource pages. The producer recomputes
the exact length-prefixed InputContentSchema digest and checks file metadata
before/after reading. The 292-byte staged manifest binds owner epoch, exact
workspace sequence/root/closure/binding proof, and original command identity.

The queue carries a canonical 180-byte BPS1 pointer, shared staged admission,
and bounded semantic/capture metadata; source rows and complete facts are
removed from the queued intent. Queue accounting includes the retained
metadata. The existing 4 MiB queue limit is unchanged. Owner preparation reads
and validates the bounded staged streams, reconstructs the exact canonical
intent and original request digest, then uses existing typed source, facts,
capture, workspace, transaction, and commit validators. The complete typed IR
and witnesses are retained without truncation.

Selection composes the fresh staged scope with the small typed control
frontier and checks exact union cardinality and membership. A new staged
selection starts from its own evidence scope, so prior staged pages do not
accumulate in the new generation. Selected auxiliary relation roots replace
their previous schema family; changed nodes remain in the physical CAS
publication frontier. Stored Prepared journal closures use SCM1 plus exact
membership ID and encoded controls. Recovery uses the fixed authenticated
pack to fetch only controls, reopens the selected root-only durable index,
validates canonical staged pages, source digests and original request, and
checks the authenticated predecessor and original transaction owner fence.

Cancellation is checked between staging reads, source chunks, page visits,
member envelope verification and the pre-HEAD membership check. Cancellation
before selection becomes a cancelled index outcome. A persistent marker
catalog and filesystem lock charge actual Unix allocated blocks for staged
page/manifest CAS objects; dropping a live Arc reservation does not erase
their storage charge. Missing markers are reaped only after ordinary store GC
has removed their CAS files. No owned CAS reclamation is introduced. Physical
allocation adapters on other platforms currently refuse with Bounds.

This is an uncompiled source checkpoint. It is not an actual Zod success
claim. The previous checkpoint's remote check was blocked by the root-owned
compiler-fault match and its retry had a stale fleet observation, so neither
is validation evidence. Current pending work: resolve the selected global GC
pin lifetime contract with root, compile this checkpoint under fresh fleet
admission, run meaningful queue/fence/identity/quota/cancellation/restart and
missing-page gates, and reproduce the actual 596-file Zod 8,092,105-byte red
through the public path. Staged page/manifest allocation is charged; durable
index node allocation accounting was still outstanding at this checkpoint.
Later source checkpoints add reader closure leases and a physical allocator
primitive. Root rejected applying a fixed staging quota to all selected CAS;
the producer now derives its admission budget from the existing configured
source policy and transfers selected members out of pending accounting.
