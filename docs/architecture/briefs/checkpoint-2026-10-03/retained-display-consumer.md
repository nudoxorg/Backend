# Retained display consumer: exact visit, no owner authority

The private NXSNAP4 projection is a bounded historical reading, not a Resource. `DataStore::retained_display(route)` returns it only for the mounted snapshot's exact typed route. The Reader additionally rejects overlays and checks the requested pane's loaded Resource before painting the projection. A loaded value with a different embedded project, query, package, target, binding or content digest is an identity refusal, not an empty pane that a cache may fill.

The one pane selection path is `shell/bodies/retained.rs`. Its plain `Reading`, `Source` and `Markdown` renderers append only saved words, observed root/epoch and coverage. Existing pending, fault and unavailable messages remain in the destination body. A current or exact retained Resource takes precedence, even when it offers a narrower historical view. Saved rows have no destinations or semantic targets; Markdown stays plain text with no relative-link resolution. Saved source uses the same UTF-8 bounded `SourcePage` calculation as current source, with local paging keyed by exact route and saved content digest. A paging callback checks the same Reader visit and the same retained projection again before changing local cursor memory. Saved content never populates Pages, creates a `VersionedRoot`, or enters the current source generation and action lease.

| Route | Cold display policy |
| --- | --- |
| World | When no Orbit value exists, the Reader replaces the otherwise empty graph surface with saved rows; owner failure is still shown. |
| Tree | Saved reading only when no exact Tree Resource exists; requested/effective binding checks remain mandatory. |
| Find and FindHome | The local Find input remains mounted. Saved rows appear below it only when its exact query Resource is absent. |
| Compare | Saved rows appear only when no Resource for the same ordered package selection exists. |
| Cargo source | The exact bound package, source target, binding and digest are checked. Saved bytes are plain, locally paged text and cannot open editor, compile, semantic or inventory actions. Unbound legacy addresses are ineligible. |
| Cargo README | Only a successfully read exact package/binding/origin may be cached. Saved Markdown is plain read-only text alongside the package's independent dossier and current README status. |
| Indexed package/symbol/source, local settings/inbox, graph declaration | Their existing Resource or local contracts remain the source of their visible content. No NXSNAP4 display projection is selected. |

Source-only validation in this isolated composition: rustfmt parsing and Git whitespace checks. Mounted tests save and restore an actual private display file, install it through the real Shell/Reader while the owner is Starting, advance to Failed, navigate away and Back, and verify a Find projection yields to local Settings. Root compilation, test execution and native GUI acceptance are separate pending gates.
