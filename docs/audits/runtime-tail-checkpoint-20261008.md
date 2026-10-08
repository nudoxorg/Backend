# Focused runtime and compiler checkpoint — 2026-10-08

This checkpoint preserves equal TypeScript, Python, Go and Rust priority. It contains four reviewed source commits on canonical `52bee7f1b0b46db1ee7d07f62aaefa69c8f51c30`. The seven changed production paths are byte-identical to the corresponding frozen, natively tested candidates. Cargo.lock remains `de731929bbf72c5220e59c0543aaddd06fcc2bc16899741a9d6cb5546e60780e`.

## Changes

- New Unix CAS inodes receive their immutable permissions before the existing pre-publication file sync. Publishing and parent-directory durability are unchanged; existing legacy objects still receive the original admission checks. This avoids a second chmod/file-sync on the first read of a newly published object.
- TypeScript errors after project authority admission retain the actual project compiler recipe instead of reporting the unavailable global compiler. Pre-admission configuration failures retain their original meaning.
- Go authority capture shares stable file digests within one capture, while charging each consumer's independent budget. Path/open-file identity, size, modification time and a platform change witness are revalidated before reuse. Platforms without a change witness stream fresh bytes. Cancellation, incomplete reads and failed captures do not publish reusable digests. The existing independent authority recaptures remain.

## Actual verification

Root read the production changes and controls, then independently verified the raw archives against their extracted members, receipts, named successful test rows, executable hashes where copied, compiler input hashes and exact integrated source blobs. See the [Root audit](../operations/evidence/runtime-tail-checkpoint-20261008/nudox-root-runtime-tail-audit-20261008.json), its [replay script](../operations/evidence/runtime-tail-checkpoint-20261008/nudox-root-runtime-tail-audit-20261008.py), and the [raw-file manifest](../operations/evidence/runtime-tail-checkpoint-20261008/manifest.json).

| Frozen candidate | Actual focused results | Scope |
| --- | --- | --- |
| CAS `930dbc9e734c6a38de3c998592f22347b33b4067` | 4 passed, 0 failed, 0 ignored | New immutable publication and existing object admission controls; actual macOS test image independently hashed. |
| TypeScript diagnostic `c05549365b139e5c189cb98ffdef961c017b1336` | 4 passed, 0 failed, 0 ignored | Admitted project recipe, typed native authority failures and terminal projection. The sealed engine image remains remote; Root verified the raw receipt, tool hashes and all 17,751 tracked source-tree entries, without claiming to have locally hashed that image. |
| Go capture `6c58535f63fd31d65aafe18ae4219077753f6f07` | 20 passed, 0 failed, 0 ignored | Eight primitive, three consumer, eight existing dependency and one selected-dependency-loader tests. Actual Go artifacts and ten compiler inputs were independently hashed; the generated oracle source is explicitly recorded as generated, not a Git blob. |

The Go selected-loader test used genuine dependency setup and exact foreign-call/source/type assertions, including cold offline authority. Its test body took 109.56 seconds; the whole Cargo invocation took 126.13 seconds. Those figures are validation timings, not a product indexing benchmark. Digest counters show shared physical hashing with independent logical budget charges; they do not imply removal of Go list or toolchain work.

Total: **28 actual focused passes**, not a combined workspace suite. All three supervisors recorded owned child retirement, empty managed leases and preserved source/graph state. The archives retain failed predecessor attempts separately; none is silently converted to a success.

## Open product gates

No installed binaries, installer, release or Homebrew formula are updated by this source checkpoint. Rust virtual-workspace package discovery still needs repair and native/public replay. TypeScript semantic/cache-admission work remains in a separate unvalidated cohort. Current installed GNU tests have exposed slow cold-owner startup and oversized MCP output; the combined startup/packed-output successor is being validated separately.

Two current installed real Python applications produced 796 complete Python artifacts, but this is not a four-language or all-tool product pass: Mealie published partially because its JavaScript lane failed, its known declaration search ranked inferred rows poorly, and its cold cursor test never reached MCP after startup timed out. Current Excalidraw also published partially with a compiler refusal; a truthful diagnostic is not a native compilation fix. Existing native GUI captures and delivered binaries are not proof of a live GUI walkthrough.

Root's verified production/test tree before adding these documents and evidence is `8dad71746a73bb913b547a71e9734734681b1288` (`3d0a85ec66731249085d085d63eda088b14cb824`). The four source commits are kept atomic for review and rollback. Other authors' unfinished work and the user's preexisting Nix configuration edit are untouched.
