# Sol Cargo browsing stopping checkpoint — 2026-10-02

Status: source-only checkpoint, incomplete acceptance. No further work is running. No Cargo/build/check/metadata/test execution, push or native capture was performed in this slice. The original GUI635 tree/evidence is intact. The canonical Downloads/backend tree was not touched.

## Exact frozen state

- Main tree: /Users/mileswirht/Documents/ChatGPT/backend-sol-cargo-route-dependencies
- Branch: codex/sol-cargo-route-dependencies
- Clean HEAD: b7ec91ed775f1f6a19228109b10fd8aba1e1f56f
- Parent: eefd5b38841ea0ed0a8d92fbd529e50423a0816b
- README checkpoint: 30 source files, 931 insertions, 129 deletions. No tracked or untracked WIP remains.
- Second isolated queue tree: /Users/mileswirht/Documents/ChatGPT/backend-sol-cargo-queued-admission, codex/sol-cargo-queued-admission, clean eefd5b38841ea0ed0a8d92fbd529e50423a0816b.
- Preserved original: /Users/mileswirht/Documents/ChatGPT/backend-sol-cargo-source-gui, clean 635a43deb3f8a6239be0db660dab58525e6f7765.
- Scoped WIP backup stash 969822983036ca88b6d0d5121681df86b61bca12 remains retained; it was already applied before this commit. Do not reapply it.

## Atomic lineage awaiting integration/runtime gating

1. e788ce69e6dc78bff7e83dfbf8d9c8fc0250ee0d: unified RouteDependencies for ensure/residency/watch/gather; Cargo file and inventory paired.
2. ee13d6121d65ff2c236d2036919b1c548f4f07bc: opaque OwnerAttachment, direct serving gate admission.
3. 8f4bf78ca8c9cc448af203a05ac93f0515ff4ca0: approved exact primary core admission prerequisite.
4. 8af71da90d6ce782e3c5b7c7e7062ff8527e961a: lazy generic read revocation and retained nonactionable predecessors.
5. 33c9abee6c2b2225beee0b3c7b52f85c8afa1c86: distinguish first owner admission from replacement; preserve prior served epoch across Starting/Failed.
6. 5c23141565cc71f79873aa156648d22b82294898: seed entry has unserved value_root/asked_at by construction; keeper claim remains separate.
7. 2b63b4ba8885171cb9b3687b544fabbbb5740d0c: faulted seed recovery requires fresh typed success; no-redraw confirmation limited to healthy complete Rest.
8. 091723de6f15f177e770272d79b313181eba6ef7: strengthen failed-boot Package fault oracle.
9. Reviewed native prerequisites were cherry-picked before P1: 7e588acef4, 72f318a11, 56d6252075, a6c171f201, 8fbc143b76, 2491100eb875b15bac10386bd931c48ea5cd6fdf. Parent owns reconciling later native/Find changes.
10. 6ba6a08a30bc467cf035ba9e9db08ebfcd37d5ce: P1 explicit CargoBrowseContext end to end. Frozen independently, 44 files; source-only.
11. a30cd0b0fcfaa865fce5a738caa0d9f5f582a10d: local package fallback uses load_with_cancel plus immediate cancellation fence; source-only.
12. eefd5b38841ea0ed0a8d92fbd529e50423a0816b: selected read lease survives queue only while exact route/overlay/visit, same authority, OwnerAttachment, PageKey/Stamp and shared resource admission remain current; pure reducer stays independent of runtime. Four deterministic owner/visit/root schedule regressions written, not executed.
13. b7ec91ed775f1f6a19228109b10fd8aba1e1f56f: current README source checkpoint described below.

## README checkpoint behavior represented in source

- navigation/cargo_source_target.rs:12,72: closed PackageFile versus ReadmeLink target. Link stores complete origin, authored href, producer-derived path/fragment. Key hashing/ordering preserves package, full binding, root scope, README path/selection/content digest and href. Identical relative spelling cannot collapse package and inherited workspace reads.
- navigation/route.rs:286: readme_link constructor checks exact qualified package/full binding; saved link is an address and cannot admit bytes.
- model/browse/cargo_readme.rs:12,45,93: exact README key independent of semantic PackageDossier; bounded worker-prepared navigation; separate Read versus owner-proven Absent.
- runtime/cargo_readme_reads.rs:12,41: requested package plus full expected requested/effective binding. Closed unbound AuthorityUnavailable is only a cold hint for one bounded exact Tree observation, then one identical retry. Tree must match full saved binding and source-qualified package. Final Read/Absent/Stale/Unavailable all require exact selectors; no unbound final reply becomes content, absence or action.
- runtime/cargo_readme_reads.rs:66,97: relative link requests retain original package/workspace scope and exact origin/href; final reply must match origin, root scope, path and fragment. Exact-origin ObservationUnavailable may trigger one bounded Tree retry. No package-file or inventory contract was loosened.
- runtime/store/dependencies.rs:129,189: Package route renew/watch includes optional semantic dossier, selected Tree and exact owner README; page readiness still does not wait for semantic indexing. Current README factory uses direct current-serving Cargo admission and exact package/full binding, returning a transient projection and selected dependency.
- shell/bodies/package.rs:67,277: owner README appears independently in pending/faulted/retained semantic pages; qualified Cargo packages do not use the ordinary local README fallback.
- shell/bodies/package/cargo_readme.rs:33,72,148,161: current Markdown, heading/link controls and disclosure rows are leased; retained README text is disclosed as inert Label. Relative source navigation uses Links.dispatch_read with fresh selected README PageKey/Stamp, so queue flush rechecks OwnerAttachment/current read. Prepared destinations do not assert target bytes or indexing. External destinations reuse ordinary Markdown spelling admission.
- model/local_package/readme.rs:251,279: ordinary and owner README share AST navigation extraction/external spelling admission; owner extraction opens no local paths. At most 512 headings and 512 links are retained; 32 rows disclosed at a time.
- model/persistence.rs:453,576,602,1099: new CargoReadmeLink persisted route carries exact browse address/origin/href and restores only through typed address constructors. PackageFile legacy restore stays separate. Invalid link restores Orbit with the existing unread-source notice; no history fallback or scope conversion.
- Source views, inventory highlight, titlebar, Jump, held labels, debug and harness display mechanically migrate file to target.path. Inventory selection is only PackageFile; inherited links remain distinct.

## Checks actually performed

- All 30 changed Rust source files parsed successfully using rustfmt --edition 2024 --emit stdout --config skip_children=true. This is syntax parsing only; it cannot prove imports, trait bounds, exhaustive cross-worktree matches or type correctness.
- git diff --check and staged diff --check passed before commit. Worktree clean after commit.
- Three README worker regressions are written at runtime/cargo_readme_reads.rs:197,220,278: exact selector/absence and negative mismatch; inherited workspace origin/scope/path/fragment and package-file key distinction; exact requested-member cold retry, changed Tree binding refusal and no retry loop. They have not run.
- Earlier P1/owner/read/seed/queue regressions are written but likewise not executed in this branch.
- No screenshots, Accessibility/AccessKit journey, real owner/IPC read, memory responsiveness test or native focus gate was run. Synthetic fixtures and no-gate harness tokens do not establish actual owner acceptance.

## Unresolved risks and required next gates

1. Producer composition: this GUI lineage still requires reviewed library/client producer prerequisites (Cargo observation boundary b915f361663b5d373ea3ddb216d2699baf0db747, README producer e58ee6df75ac06f8d7a8d8b218e511c1abe1ffe3 and primary source authority lineage). The README request DTO uses requested digest plus Some(expected effective digest), created through from_tree; it is not a field named full binding. Final replies are checked against the full V1 binding. Do not replace this with the CLI shortcut or relax strict file/inventory negative selectors.
2. Cold hint: the closed None-binding reply only permits one Tree observation and no rendering/action/absence. Shape-valid and matching package is necessary but cannot prove content. Real attached/embedded cold-cache tests must verify exact member versus workspace paths, producer cache observation retention, identical request count, changed full binding, cancellation and final malformed/unbound negatives. No live claim has been made.
3. Read authority and restart: new README keys share direct current-serving admission and generic owner revocation. Retained bytes have inert disclosure in source. Add actual DataStore and mounted Shell tests for Starting/Failed/coalesced Ready, owner replacement at unchanged root, late inflight result, absent semantic dossier and newly painted controls. Add explicit README queue-to-flush owner-change regression; the existing frozen queue suite tests the shared lease with Cargo resolution but not this new Markdown path. Verify external owner publication integration preserves stable attachment identity and current root invalidation.
4. Markdown links: worker index and exact-origin callbacks are present, but mounted native Markdown/action bubbling, heading scroll offsets, long documents, duplicate/reference/autolinks, unsupported hrefs and external open behavior need runtime coverage. Existing Markdown view may still parse/render source on UI; prepared navigation does not by itself establish bounded UI work or queue responsiveness. Bounded ReadPool result/drain work remains a separate unimplemented slice.
5. Restore/focus: new persisted link variant has no new roundtrip/forged origin/full-binding/scope/href/line regressions yet. Check saved package/workspace link roundtrip, legacy package-file restore, invalid edited address notice, cold origin replacement and no history fallback. README row IDs are ordinal; current restore recalls an index for the Package place. Changed README origin/content may restore focus to a different row at that ordinal; semantic endpoint/receipt-aware restoration needs review before native acceptance. Anchor callbacks are admitted immediately but their deferred scroll/focus path also needs owner/visit interruption testing.
6. Native composition: reconcile later native focus/read lease, inert wrapper/A11y, Ctx.native_input_active/Ask and LibraryStateMemory patches on parent integration. Original635 Tree fixture must gain a real request binding before producer shape projection while retaining per-package source unavailability; native agent owns it. No syntax parsing result can validate these merge effects.
7. Native pixels: original evidence remains preserved. At 1440x900 the earlier Ask f07 plate extends to about x440 while reader title/table begins around x424, producing about16px overlap/cropping. That is captured evidence, unrelated to a headless green probe. Source/code gutters, README heading/link native keyboard and accessibility roles/focus, source line gutters and geometry all need actual mounted/live-owner screenshot+AccessKit review.
8. Parent stopping-point instruction is active: do not expand or run Cargo until the root/user resumes and grants the next gate. This checkpoint is not production acceptance, not a claim that all requested work is done.

## Artifacts

- .local/reviews/cargo-readme-checkpoint.patch: exact HEAD^..HEAD README diff.
- .local/reviews/cargo-readme-checkpoint-paths.txt: all 30 changed source paths.
- .local/reviews/readme-stopping-brief.md: this stopping brief.
- .local/reviews/cargo-browse-context-p1.patch and cargo-browse-context-p1-paths.txt: frozen P1 source diff and inventory.
- .local/reviews/package-fallback-cancellation.patch: tiny cancellation correction.
- Queue tree .local/reviews/cargo-queued-read-admission.patch: exact eefd queue repair diff.
- .local/reviews/readme-scope-wip-before-queue.patch and readme-scope-wip-stash.txt: pre-checkpoint recovery artifacts.

## Complete changed-file inventory

- apps/desktop/src/harness/journey/look.rs
- apps/desktop/src/model/browse/cargo_readme.rs
- apps/desktop/src/model/browse/mod.rs
- apps/desktop/src/model/local_package/mod.rs
- apps/desktop/src/model/local_package/readme.rs
- apps/desktop/src/model/pages/cargo_source.rs
- apps/desktop/src/model/pages/key.rs
- apps/desktop/src/model/pages/store_tests.rs
- apps/desktop/src/model/persistence.rs
- apps/desktop/src/navigation/cargo_source_target.rs
- apps/desktop/src/navigation/mod.rs
- apps/desktop/src/navigation/route.rs
- apps/desktop/src/runtime/browse_reads.rs
- apps/desktop/src/runtime/cargo_readme_reads.rs
- apps/desktop/src/runtime/debug_page.rs
- apps/desktop/src/runtime/mod.rs
- apps/desktop/src/runtime/reads.rs
- apps/desktop/src/runtime/store.rs
- apps/desktop/src/runtime/store/cargo_context_tests.rs
- apps/desktop/src/runtime/store/cargo_tests.rs
- apps/desktop/src/runtime/store/dependencies.rs
- apps/desktop/src/shell/bodies/package.rs
- apps/desktop/src/shell/bodies/package/cargo_readme.rs
- apps/desktop/src/shell/bodies/package/readme_links.rs
- apps/desktop/src/shell/bodies/source/cargo.rs
- apps/desktop/src/shell/bodies/source/cargo/inventory.rs
- apps/desktop/src/shell/jump.rs
- apps/desktop/src/shell/reader.rs
- apps/desktop/src/shell/side/hold.rs
- apps/desktop/src/shell/titlebar.rs
