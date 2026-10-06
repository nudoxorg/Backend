# Explicit workspace membership source checkpoint

Base: `834c5b4dc741abbf639f7c46504858b59bd6031e`.

The workspace closure distinguishes an in-memory complete manifest from stored
membership. Stored membership holds an authenticated durable index and an
Arc-shared affine receipt plus local reader GC pin. `control_manifest` exposes
only already hydrated typed controls; `in_memory_manifest` returns `None` for
stored membership. `membership_id`, fallible `contains`, and bounded `visit_ids`
are the complete membership APIs. There is no implicit full object hydration.

The separate workspace composer verifies external relation children against
durable CAS. The existing complete closure composer still requires every
relation child to be a member. Control rebinding preserves stored evidence by
ID-only composition and preserves the typed physical publication frontier.
Store publication retains the closure pin until HEAD and streams current
member envelopes again immediately before HEAD selection.

The product composition change retains only selected auxiliary relation roots
and pointers in control membership. All changed capture/facts/semantic nodes
stay in the checked physical frontier. Ordinary GC remains responsible for
unselected staged content after pins drop. Existing staged storage source was
imported verbatim from `2f86f9aaf6872e3bde69c266b8eed4893ac3b573`.

This checkpoint is source only. It does not yet connect producer queue ingress
to staged storage, and is not a claim that the actual Zod enqueue is repaired.
No Cargo compile, unit test, or runtime result is credited. Queue transport,
staged recovery, persistent allocation quota, and cancellation remain the next
implementation slice.
