# MCP overlap admission and durable failure lineage

Linux report 18 describes actual failures on historical source `f30aae099`:
after a CLI compiler refusal, `backend.index` followed by an overlapping
unkeyed `backend.index_start` left the default operation unresolved. Either
start alone and two competing CLI adds did not reproduce it. The preserved
user states have not been opened, altered or reproduced by this source work.
This atom is based on `cf05cb021bb20b0ed6c4e6bfca1e4b19673bede5`, tree
`c23600500a359e932043a74a4ab12cd6c5d4bfeb`.

Both versions already serialize mutating owner work. The second unkeyed start
waits behind the first job and then replaces the selected package/profile
capture markers. The first durable Failed row can retain a fully closed,
original source receipt, but its status route previously demanded that those
current markers still carry the old operation key. A legitimate replacement
therefore turned the exact terminal refusal into WorkspaceEvidenceMismatch.
The default caller journal retained that uncertainty and kept observing the
same key; removing current project rows could not repair that projection.

The journal decoder validates the saved key/request digest, original capture
key and source sequence relative to the accepted base, failure tuple, and the
absence of Published profiles in Failed. A closed Failed row now returns that
retained evidence. Only a receipt with Pending profiles consults the current
capture relation for exact authenticated advancement. The shared typed
pending-source predicate is also used by journal observation, preserving the
private durable failure/capture gap and its explicit recovery requirement.
There is no success inference from a newer root, no receipt/key reset and no
failure-to-success rewrite. Legacy uncertainty without an admitted closed
failure or exact current capture proof remains unresolved.

An unkeyed IndexStart for the same exact live package spelling and execution
intent joins the existing owner-issued ticket before new-mutation queuing.
This shortcut applies only with no earlier queued mutation; an already queued
remove or other writer remains an ordering barrier for a later start.
The shared start encoder retains the new transport request id. Joining changes
no job, claim, cancellation flag, progress sequence, ticket counter or compiler
work. Distinct persisted operation keys, intents and project identities retain
their independent FIFO writer admission. Owner reads remain immediate; the
default caller lock remains scoped to the canonical request digest, allowing
another project to reach its own start while the first reconciliation lock is
held.

Four local-service controls dispatch real typed owner requests, scan real
Python files, and obtain actual source captures and closed-host compiler
refusals. They cover joined starts with one ticket/terminal, a later actual
unkeyed capture replacing the selected markers while the original failure
stays byte-for-byte equivalent, cold owner/journal reopen, ordinary removal,
fresh keyed retry and unrelated package/intent/key identity. The added
queued-remove control preserves the earlier writer barrier and checks ordered
correlated replies with a distinct post-remove start ticket. The existing
Pending-capture/foreign-evidence/recovery controls remain required.

The MCP control pipelines ordinary stdin initialize/index/index_start lines
through the production actor, SessionProduct, Session, caller journal, reply
admission and complete line serializer. It copies the returned exact poll,
retains the joined ticket and visible terminal refusal, acknowledges only the
matching default claim, and checks a distinct durably saved retry identity
with EOF closed before its final response. Its admitted command-transport
observations are constructed: it proves routing/journal/wire contracts, not
native compiler acquisition or a real daemon overlap. The separate owner
controls and a matched current normal MCP runtime reproduction are required.

Rustfmt parses the new source and `git diff --check` passes. Compilation and
all new tests are **UNRUN**. Only the sole ILO broker may run the composed
source. Actual reproduction on the report's preserved states and a matched
current CLI/MCP package remain **UNRUN**. The original 6000-record/30-second
publication failure and all cancellation/deadline gates are unchanged.
