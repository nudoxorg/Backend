# Final reachable publication frontier

The actual 18ed law
`publication_worker_removes_native_selection_after_staging` failed its original
30-second terminal bound after genuine native Python publication and a separate
6000-row unavailable-descriptor transition. The terminal wait was
30.000797494 seconds; total test time was 306.62 seconds. At refusal, the original
publication thread was unfinished, had no completion, and last entered
`prepare-workspace` with `granted=false`. Owner sequence remained 3. The earlier
native operation published in 4.091 seconds. These are the retained results of
source `18ed5c7de1db5a84f5db88d8e8b481cd202b398f`, tree
`d06764fac95b37dbc9acc8f1609aa88cfb3d7ab8`; they are not passes for this repair.

The observed phase encloses model preparation and store durability inside
`ReadHeadWriter::prepare`. No owner grant event is sent until that call and view
preparation return. This state therefore precedes the actor grant wait; it does
not show an undrained completion or a pending grant. Worker close joins the
original work after test unwind, so total test duration includes later drain;
there is no measured per-subphase timing in this receipt.

The source trace exposes avoidable durable-write amplification. A nonempty
`LazyTree::prepare_update_inner` accumulates every path-copy node emitted by
every sequential edit. Intermediate roots and ancestors remain in its returned
frontier even after later edits supersede them. Builtin transition preparation
forwards that entire vector to the typed workspace closure, whose stored
membership rebinding installs the physical relation frontier. Each such node
is admitted and written to immutable object CAS with a durable schema/version
reference. The 6000 changes miss the empty-tree bulk shortcut because the base
already contains the genuine native publication. This is a source-confirmed
work amplification; the coarse actual phase does not prove how much elapsed
time each step consumed.

The repair keeps every intermediate node in the existing admitted overlay while
the batch is constructed. After the final root is known, it marks that root and
its new child edges within the overlay, stopping at unchanged durable subtrees.
It retains only marked nodes in the original child-before-parent order, once
per commitment. No unchanged subtree is loaded and no second complete index is
allocated. The single overlay marker is included through the actual overlay
struct size in bounded metadata accounting. A borrowed child-wire iterator on
the already typed admitted root follows only fixed-width commitments, with no
decoded key or child-vector allocation and no new byte-order requirement on
generic relation keys. Empty bulk construction and the stable replacement
batch fast path already emit final frontiers and are unchanged.

The checked target, all before/after delta rows, base identity, and publication
authority are unchanged. Work counters still measure every constructed node
and byte, including discarded intermediate copies; the documentation now
distinguishes that construction work from the smaller durable frontier. No
timeouts, admission limits, workload size, retries, publication grants, or
selection/removal assertions are changed.

Controls use independent canonical trees and actual storage boundaries:

- The existing mixed insert/replace/remove law retains its original root, delta,
  row-count, work, and invalid-order controls, and adds final-node reachability
  plus a complete cold-loader read from base CAS and the final frontier.
- A new nonempty-base/6000-addition law checks the exact canonical target bytes,
  all 6000 delta entries, unique final nodes, every new target-reachable node,
  child-before-parent order, truthful larger construction counters, and a paged
  cold read of all 6001 rows.
- A typed child-wire law uses little-endian keys whose logical ordering differs
  from encoded byte ordering. It proves the borrowed commitment walk retains
  typed admission, performs no key decoding, and handles an empty leaf iterator.
- The existing bounded store law retains its metadata, loader, and construction
  ceilings. Its old construction-count-equals-frontier assertion is replaced
  by the strict construction-count-greater-than-final-frontier contract, with
  independent exact canonical bytes, final-node reachability, full delta,
  actual relation CAS publication, and a cold store read of all 656 members.
- The original local-service native-selection removal law, its 6000 descriptors,
  actual native compiler setup, and 30-second assertion remain byte-identical
  to the tested 18ed source. The actual selection-removal outcome must still be
  demonstrated on the composed source.

Source checks are Rust formatting with edition 2024, manual call/type review,
and `git diff --check`. Compilation, all tests, and improved runtime/performance
after this change are **UNRUN**. Sole-broker gates must include the original
local-service law, version batch and bounded-path controls, and store closure
composition tests against the exact composed source. No historical run is
relabeled as current.

Actual raw:
`/private/tmp/sol61-ilo-real-cli-mcp-20261011/core18ed-terminal-minimal-03/current18ed-focused-controls-01/test-25.stdout.log`.
The immutable packet pins its SHA-256 and the unchanged producer/test sources.
The separate semantic fixture `InvalidConfig` correction is not part of this
atom.
