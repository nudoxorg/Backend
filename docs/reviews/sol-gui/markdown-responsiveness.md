# Markdown responsiveness slice

Source-only implementation; no Cargo or owner pixels have been run in this
worktree. The integration owner must compile the exact admitted commit, run the
component tests and capture a real package README before accepting the slice.

The desktop continues to use GPUI CE component TextView for Markdown, selection,
layout and pointer links. `background_parse()` opts its document and nested
heading views out of the component's small-document synchronous parser. Keyed
state receives the configured extensions before its first publication, removing
the initial default parse followed by a custom-heading reparse. Until that first
parse arrives the Markdown view is empty; no default-extension content is painted.

One pending input slot and one pending completed-result slot coalesce complete
publications; the input slot preserves ordered
append text using the existing update reducer. The worker holds at most one
current parse and one pending input publication. The busy UI retains one pending
completed result, with incompatible selection requirements carried across skipped
results (synchronous baseline acknowledgements are already applied). New complete snapshots supersede
queued old snapshots. Append deltas after a snapshot are concatenated in order;
append-only publications continue the worker's previous document and reuse its
stable prefix blocks. The worker yields after each parse. Exact source revision
checks reject stale successes and failures before updating paint or selection.
Dropping the entity cancels its worker/receiver tasks; cancelled receivers refuse
and release later pending publications.

Width and text scale remain layout inputs and do not enter parser options.
Theme remains a paint/style input and does not enqueue parsing. Heading element
IDs remain source-offset based across reflow, and their IDs/inline source are
allocated in the worker projection and cloned during rendering. Native outer
Reader scroll ownership, focus and typed destination authority remain with the
existing Reader and README link plan.

The current owner supplies bounded complete README content (512 KiB), not a
streaming producer. This change improves that real flow and retains the existing
component append API; it does not introduce a streaming endpoint or claim that
all Markdown reference definitions can be resolved by suffix-only append parsing.
These are bounds on publication count, not cumulative source bytes. The generic
component append API preserves its existing unrestricted byte semantics: merging
ordered deltas can grow the single payload with the actual document. The desktop
producer's existing 512 KiB admission bounds complete snapshots; this slice adds
no new byte-limit contract to the vendor API. A future owner stream needs an
explicit cumulative byte admission bound and revision protocol before storage
bounds can be claimed for it.

Added source tests cover deferred parsing even for small Unicode headings,
custom-heading AST and exact local link destinations, unchanged-source/options
reuse, A held while B/C supersede it, stale success/failure rejection, ordered
append reduction, cancelled receiver retention, a README near the real size
limit, and heading Unicode/setext/link preservation. Existing native component
layout/link/selection tests remain the reflow and pointer regression gate.

Still needs real-owner proof: first frame and completed README geometry, width
and 200% text-scale reflow, heading jumps before/after parse, inline local and
external links, keyboard link rows and Back restoration, selection/copy, and idle
frame settling. No performance or pixel acceptance is inferred from source tests.
