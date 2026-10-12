# Exact durable Add wait

Linux Report 18 describes historical f30 Add returning acceptance/exit 0, then
the same operation refusing about 21 seconds later. Acceptance is still the
ordinary asynchronous Add contract. Explicit `nudox add . --wait` now waits
for the exact durable operation's terminal receipt. The CLI-specific caller
options are held by the shared grammar and its generic parser/help; MCP's
product argument schema and lowering do not falsely advertise a wait flag.

Observation starts after the admitted Add reply, under one absolute caller
context. Its default/max bound is 15 minutes, using the existing durable
work lease duration; `--wait-timeout SEQUENCE` chooses 0–900 seconds. Every
conditional read uses the existing exact-status fingerprint policy, at most
500 ms probes within 10-second slices. Slices measure their own start relative
to the retained clock; a later slice cannot become an unpaced poll loop.
Neither a new slice nor an owner reply renews the overall context.

Only Published/PartiallyPublished/Failed is terminal. Every changed reply
must retain the initial key, package, intent and canonical request digest.
Unknown, unresolved or expired evidence stops with an incomplete observation;
it cannot infer success or authorize a new mutation. The shared caller journal
acknowledges only an exact terminal key. Waiting does not hold its request lock
across observation delays and does not submit start/cancel/remove commands.

Terminal refusal and partial publication use the existing exact product
fault/receipt projection and return exit 2. Timeout, disconnect, cancellation,
proof refusal or uncertain evidence returns exit 75, preserving the last
admitted operation in complete output with its exact progress command. The
readable note explicitly labels that as the last observation, not a current
owner state or cancellation. Immediate terminal replies require no extra read,
including a zero wait budget. Ordinary Add still returns acceptance/exit 0.
Progress uses a four-message nonblocking sink and a single best-effort stderr
reporter; redirected stderr cannot delay the operation budget. Generic client
callbacks are not claimed to be preemptable.

Four client controls retain the original waveform identity through a 21-second
virtual-clock wait, count 45 bounded reads, refuse a foreign same-key request,
retain last evidence on caller stops, and accept immediate terminal evidence.
Four CLI controls execute the production plan/driver/session/caller journal
with admitted constructed owner replies. They check later failure/exit 2,
exact journal acknowledgment, ordinary acceptance, zero-timeout retained claim
and literal resume, disconnect identity, immediate failure, and CLI-only
grammar options. These are adapter/identity controls, not live native compiler
or daemon acquisition proof. Compilation and all new controls are **UNRUN**.

Death-race investigation confirms Session does not automatically reconnect,
despite the former shared disconnected sentence claiming it did. That sentence
now preserves ambiguity truthfully. Automatic owner-death recovery remains a
separate source task: it must prove a fresh authenticated connection and
reconcile the same saved operation before any same-key replay. This atom adds
no retry of ambiguous mutations or pinned queries. Current matched CLI/runtime
and the report's real later refusal remain **UNRUN**, broker-owned gates.
