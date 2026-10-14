# Time-bounded test exclusions

Every excluded test needs a filter, category, observed failure reason, accountable
owner, review reference, review date and expiry. An exclusion lasts at most30 days
from review. The expiry date is exclusive: an entry expiring November13 blocks
on November13 UTC. Fix and remove it, or review current evidence and commit an
explicit renewal with an explanation. Never automatically roll dates forward.

Run `python3 .config/ci/quarantine.py` to check both emulated lanes, or append
`windows` or `arm64`. The fast lane checks the actual UTC date before compilation;
each emulated lane checks again before selecting tests. Expired or malformed
entries fail the lane, rather than silently skipping more tests or automatically
reenabling a known failing test. Cached structural checks test validator behaviour;
they cannot substitute for the runtime date check.

Review evidence should identify the native/emulated build, error, whether the
failure is a product defect or emulator limit, and the work to remove it. Owners
may be accountable maintainer teams; the initial owner is the NuDox Backend
maintainers, with Robert coordinating triage. A future named owner replaces that
team when the work is assigned. A quarantine is neither deletion approval nor a
claim that native coverage has passed.

## Initial migration 2026-10-14

This administrative migration keeps every existing Wine/ARM filter and failure
reason, including each lane's existing compile-fail harness exclusion. It assigns
the Backend maintainer role and a November13 review deadline. No fresh platform
pass is claimed: the current native attempts fail disk admission before tests.
Existing `unclassified` exclusions still require diagnosis; timing and emulator
limitations still require evidence on native platforms. Renewal must link a new
review rather than cite this migration indefinitely. No tests were deleted and
no new failure was hidden. Individual historical build references remain in the
original reason fields where available; missing historical evidence is review
work, not a reason to invent a passing result.
