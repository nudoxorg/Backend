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


## Source intake 2026-10-14

The source registry `.config/ci/source-ignores.json` owns126 existing Rust ignore
attributes in the current versioned code tree, including vendored/fork sources.
Documentation examples and the retired `workspace/` prototype are outside this
code policy. This is a static attribute inventory, not126 tests proved to run in
the current nextest selection. Existing ignores and their literal reasons stay
unchanged; this intake adds no exclusion, removes no test and claims no pass.
The accountable owner is the NuDox Backend maintainers, with the same November13
UTC deadline and30-day maximum as emulation filters. The intake is administrative
ownership; bare ignores and semantic reasons still need classification/evidence.

`python3 .config/ci/source_ignores.py` prints current source sites for review; it
never updates ownership or dates. The lexer skips Rust comments/strings/characters
and retains direct/conditional ignores. Tracked Rust symlinks are refused. Source
keys include the file, nested module and function, with explicit occurrence keys
for generated/unsupported sites. Historical intake lines aid navigation but do
not make unrelated line movement invalidate an owner. Attribute text changes,
new ignored sites and stale registry entries require an explicit metadata change.
Unknown source directories are included; only the two named non-code roots are
excluded. Generated macro behavior that has no written ignore attribute requires
the separate runtime inventory and is not claimed covered by this scan.

Fast and emulated entrypoints check the actual UTC expiry and source-registry
consistency before heavy work. A cached Nix check cannot replace this runtime
check. No expired entry is automatically renewed or silently reenabled. Removing
an ignore requires removing its metadata too; renewing one requires current
observations and an accountable review. Source inventory/lexer/expiry controls
run in the Nix sandbox, without compiling or executing the ignored Rust tests.


## Wine 72 triage 2026-10-15

Build484603/windows-wine2 on e2c295b3e ran603 cases:531 passed,72failed,
134skipped. The source-bound [72-case ledger](wine-72-triage.json) records every
failure identity and diagnostic site. Fifty-three explicitly report missing
protected DACLs before the behavior under test. Those exact Wine-only filters
have Backend maintainers as owner and expire November14 UTC. Linux/native Windows
selection and production private-storage admission remain unchanged.

The independent Win32 probe documented in historical commit
1cf43173d0a657183d99520bc76916a0d96343a1 applied an owner-only protected DACL with
SetSecurityInfo, then read it successfully without SE_DACL_PROTECTED
(control0x8004, missing0x1000). The current53 explicit errors match that boundary.
This is emulator coverage loss, not native Windows passing evidence. Removal
requires a Wine version preserving the bit or independent native Windows proof.

Nineteen cases lack precise captured causes and remain selected. Fixture helpers
now print actual errors. Diagnose them on a fresh source-bound Wine run before
further exclusion. This is not a new green result; no thresholds or admission
checks are relaxed.

Fresh build490495/92868fc11 ran550 cases:531passed,19failed,187skipped.
Eighteen retained store failures now explicitly report the same protected-DACL
rejection; their exact Wine-only filters have owner and November14 expiry.
One cache fixture still returns an opaque None; expose the directory-capability
error before deciding its disposition. No native-platform coverage is removed.

Build490924/10b8ef947 exposes the final cache cause: opening the protected
fixture directory returns InvalidData, workspace object does not have a protected
DACL. All historical72 are now classified as this emulator limitation, with
exact Wine-only owner/expiry filters. A fresh full candidate run is still needed;
no native Windows pass or production guard relaxation is implied.

## Managed native intake 2026-10-16

Canonical9ae0eca9d contains five new ignore attributes not registered by its
language integration commits. Fast2297 flags all five; no new ignore attribute
or runtime filter is added here. The Backend maintainer team owns review by
October30 UTC (14 days, no extension of existing entries).

Three Python environment controls require genuine managed CPython3.12/3.14
and, for the transitive-installation control, pinned uv. They exercise ordinary
venv selection, closed native-default host capture and offline wheel/change
invalidation. Two TypeScript controls require pinned Node and TypeScript API
paths: rooted-specifier negative-observation containment, and production checked
forest reuse plus source/dependency/options invalidation. Their bodies use these
explicit prerequisites and exercise real native tools.

This is ownership of existing skips, not acceptance of those native behaviors.
Before expiry, run them explicitly with their required pinned tool inputs in a
managed deep lane and either remove the ignore or retain current reviewed native
evidence and a specific integration task. No native pass is claimed from the
metadata/lexer checks; no guard is bypassed or ignored test silently enabled.
