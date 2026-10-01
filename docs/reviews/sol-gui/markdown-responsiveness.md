# Markdown responsiveness slice

Source-only implementation; no Cargo or owner pixels have been run in this
worktree. The integration owner must compile the exact admitted commit, run the
component tests and capture a real package README before accepting the slice.

The desktop continues to use GPUI CE component TextView for Markdown, selection,
layout and pointer links. `background_parse()` opts its document out of the
component's small-document synchronous parser. The heading extension prepares
its native inline projection on the same parent worker. An immutable prepared
TextView consumes that projection immediately, with no heading parser/receive
task or additional empty publication frame. Its native marks preserve inline
links and selectable/copyable text; a changed heading snapshot replaces its
projection directly. If preparation fails the parent parser retains its native
heading conversion. Keyed
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

Added source tests cover prepared heading native marks/copy and immediate
replacement without child worker tasks, deferred parsing even for small Unicode headings,
custom-heading AST and exact local link destinations, unchanged-source/options
reuse, A held while B/C supersede it, stale success/failure rejection, ordered
append reduction, cancelled receiver retention, a README near the real size
limit, and heading Unicode/setext/link preservation. Existing native component
layout/link/selection tests remain the reflow and pointer regression gate.

Still needs real-owner proof: first frame and completed README geometry, width
and 200% text-scale reflow, heading jumps before/after parse, inline local and
external links, keyboard link rows and Back restoration, selection/copy, and idle
frame settling. No performance or pixel acceptance is inferred from source tests.

## Source repair invariants

The parser worker separates the current logical source from its last checked
projection. Any failed replacement or append marks the basis unparsed. Recovery
parses the actual complete logical source and resets incompatible selection;
no append may borrow an older successful page as its basis after a failure.

Checked source snapshots use the existing Ropey dependency; appending inserts
only the new bytes and shares the prior rope. Checked block snapshots use a
private persistent balanced tree: publication clones the root, suffix replacement
copies a logarithmic path, and stable prefix AST leaves retain their addresses.
Full source materialization is lazy for source-copy consumers and full recovery.
The source-copy oracle measures ingestion, suffix slicing and parse-buffer source
storage over 1,024 real parser appends while retaining an old snapshot. This is
an operation counter, not an allocator profiler or a release timing measurement.
Tree tests check prefix identity and bounded height after 4,096 appends/pops.

A barrier-driven test holds the actual Markdown worker on A while a replacement
and repairing append are published. Only its latest cumulative result remains
queued when the UI resumes; this exercises the real reducer and parser.

Prepared headings derive native inline nodes from their parent's AST and
reference context. Derived projection offsets are local, distinct projections
always publish even when source text matches, and prepared/live keyed states
have separate identities. Duplicate definitions deliberately use CommonMark's
first-definition-wins rule, correcting the component's previous last-insertion
priority. A same-key switch test also requires a later live background result.

Generic append semantics still have limits: an ever-growing unfinished last
paragraph/fence requires reparsing that growing suffix; references introduced
later can require reparsing older prose. This slice removes unconditional checked
prefix copies, not those Markdown grammar dependencies. Generic cumulative bytes
remain unrestricted; the complete README producer remains admitted at 512 KiB.
All new repair oracles remain source-only until the integration gate runs them.
