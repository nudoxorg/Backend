# Typed identity shipping repair card

## Capability and observable consumers

Repair the typed-identity boundary across `nudox-id`, validated root locality,
object-pack, and workflow decoding. Consumers must receive a validated typed
identity or an exact source-bearing rejection; trusted locality projections must
be infallible over immutable witnessed bytes.

## Frozen cold-review baseline

Baseline commit: `789974d47387ca42eae45f56377db5aef55cd0d2`.

That shared baseline is clean and already contains the integrated identity/locality rewrite, public
identity falsifiers, minimal index vocabulary, and its historical salvage ledger. This reissued card
owns only the final cold-review corrections: remove the public compact-payload rebranding route,
preserve branchless projection behind checked header authority, assert complete nested sources, and
replace stale layout/SIMD evidence. The new `authority.rs` path is absent at the baseline.

The card owns the following exact writable set:

| Area | Exact writable paths |
| --- | --- |
| identity | `workspace2/crates/nudox-id/src/{authority,content,lib}.rs` |
| locality | `workspace2/crates/nudox-root/src/locality/cursor.rs`, `workspace2/crates/nudox-root/src/locality/artifact/{descriptor,errors,validate,view}.rs`, `workspace2/crates/nudox-root/tests/locality_validation.rs` |
| measurement | `workspace2/layout-lab/src/{main.rs,bin/locality-validation-size.rs}`, `workspace2/layout-lab/raw/baseline-aarch64-apple-darwin.tsv`, `workspace2/layout-lab/evidence/locality-validation-m3-pro.jsonl`, `workspace2/LAYOUT_AUDIT.md` |
| evidence | `workspace2/.codex/evidence/{typed-identity-shipping-card,salvage-ledger,typed-identity-shipping-closure}.md` |

## Preserved law and selected construction

`ContentId`, `ArtifactId`, `GenerationId`, `ProviderSet`, and `ObjectRef` remain
checked at raw ingress. A locality validation owns a private compact borrowed
witness that contains closed, typed views of every identity-bearing lane; trusted
cursor/view projection borrows those facts and returns no `LocalityReadError`.
No public unchecked raw identity constructor is permitted.

One artifact-header domain code owns descriptor authority for the complete
artifact. Each descriptor carries only the remaining 31 payload bytes;
validation converts the observed header cell into a checked zero-sized
`ContentAuthority<DomainTag>` proof, casts every lane once, and only then mints
the borrowed witness. Projection requires that proof and must not reconstruct,
transmute, or dynamically revalidate identity authority.

The public domain law is closed rather than generically revalidated: only the
registered domain markers are accepted and the generic parameter is retained
solely where it prevents cross-authority reconstruction. One registry
declaration emits each code enum and each marker binding. Enum discriminants are
literal numeric codes so duplicates fail in the compiler with E0081.

## Required evidence and falsifiers

| Law | Artifact or command | Expected evidence | Hard stop |
| --- | --- | --- | --- |
| validated locality | root integration `validated_locality_projection_is_infallible` plus source scan | one validation; no `GenerationId`/`ProviderSet`/`ObjectRef` raw decode in trusted projection; iterator item is not `Result` | any projection fallibility/redecode |
| domain coherence | two real marker consumers, compile-fail cross-authority reconstruction, and compact-payload compile-fail proof | mismatched authorities cannot compile; arbitrary 31-byte payload cannot bind without checked header authority | unused generic/only one authority path |
| closed registry | `nudox-id` compile-fail fixture | duplicate numeric code produces E0081; markers do not carry `$code:expr` | handwritten duplicate-prone match |
| identity falsifiers | top-level tests in regular owning crates | N-1/N/N+1 widths; exact expected/observed/raw/source; all authority pairs; literal wire goldens; all split points including empty; routing mutation killer | a success-only or source-losing assertion |
| workflow diagnostic | workflow integration test | `UnexpectedOutput` contains the complete `[u8; 32]` observed operand | output bytes omitted |
| public surface | grep + consumer tests | no unknown-code public errors or `TryFrom<u8>` unless a dynamic consumer/falsifier exists | retained unused API |
| collision budget | public identity docs and object-pack docs | content 248-bit/124-bit birthday; artifact 240-bit/120-bit birthday; no false raw-key claim | inaccurate or absent documentation |

## Resource and scope budgets

No new dependency, unsafe, `Box`, `Vec`, or `Arc`. Trusted projection must make
zero allocations and zero repeated 32-byte identity decodes. The repair may
delete obsolete errors/results. Production net change target is at most 240
formatted lines and test net change target at most 360, each retaining at least
25 unused lines of reserve. Any new public item requires a named current
consumer and falsifier.

## Explicit negative space

No test-only crate, dynamic registry parser, public raw rebranding constructor,
compatibility aliases, erased errors, suppressed formatter, or broad unrelated
layout-lab edits. Do not alter durable wire bytes except to add documented exact
error operands already present in the record.

## Final decision

Accept only a clean committed candidate with all focused gates, two clean
workspace fmt/test/doctest/clippy `--locked --offline` runs, independent Terra
review with zero blockers/majors, changed-file/LOC and salvage ledgers, and a
tripwire scan. Otherwise issue one named falsifier-bound repair card.
