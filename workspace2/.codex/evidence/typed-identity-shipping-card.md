# Typed identity shipping repair card

## Capability and observable consumers

Repair the typed-identity boundary across `nudox-id`, validated root locality,
object-pack, and workflow decoding. Consumers must receive a validated typed
identity or an exact source-bearing rejection; trusted locality projections must
be infallible over immutable witnessed bytes.

## Frozen baseline

Baseline commit: `f96ef0c0529f055a3903822f30d84f20012c6d7f`.

The baseline is clean. The selected baseline ledger is:

| Path | Formatted LOC | SHA-256 |
| --- | ---: | --- |
| `workspace2/crates/nudox-id/src/marker.rs` | 356 | `f203c1956b579e1618aba5af50d7246f35e9449180e50ff2d5385654d109dd7b` |
| `workspace2/crates/nudox-id/src/content.rs` | 443 | `4caa70e9715b382fb8b2af8c3c84469c8b1e0dd55f0c2a5bdc252061294d4c53` |
| `workspace2/crates/nudox-id/src/artifact.rs` | 346 | `673f9a2f2f41060c03cc7f61ca941ebc1b447a3fc2e1b690d9f732ea57f3d427` |
| `workspace2/crates/nudox-root/src/locality.rs` | 249 | `2c35a2933ba792552e9810618555e00fc494d544ef10898dc5e97ca47686d619` |
| `workspace2/crates/nudox-root/src/locality/view.rs` | 489 | `7a4158d1d5f469168df6b52c0632685a8b928b93740448b9c193314f3368e1fd` |
| `workspace2/crates/nudox-root/tests/locality_validation.rs` | 155 | `d54216c5cf91800843e1996880f52fb02bf5e6a2424bc70f21dae1abbadcaba7` |
| `workspace2/crates/nudox-object-pack/src/index.rs` | 202 | `af6a7be4757d70075db5111990cef476515b8d43d14f893cb75a92d62ce630c8` |
| `workspace2/crates/nudox-object-pack/tests/object_pack.rs` | 211 | `90336b3680e1eed422117b7457d3b16e91da587af779fcb632ec7e94f2a01fd8` |
| `workspace2/crates/nudox-workflow/src/durable.rs` | 328 | `1a46c4d1acf5b0860f9bfc2959963a07bd3a3d7001ffe554110893b846d08f34` |
| `workspace2/crates/nudox-workflow/src/tests.rs` | 323 | `a3135b3999fed41a32e3e39fe56cb4f39b1c0b9843f0de12eff090e932042fe1` |

The card owns the following writable set:

| Area | Exact writable paths |
| --- | --- |
| identity | `workspace2/crates/nudox-id/src/{marker,content,artifact,lib}.rs`, `workspace2/crates/nudox-id/tests/*` only if an existing owning test tree is created with a valid manifest consumer |
| locality | `workspace2/crates/nudox-root/src/locality.rs`, `workspace2/crates/nudox-root/src/locality/{cursor,error,view}.rs`, `workspace2/crates/nudox-root/src/locality/artifact/{mod,descriptor,errors,header,layout,rank,rows,validate,view,write}.rs`, `workspace2/crates/nudox-root/src/tests.rs`, `workspace2/crates/nudox-root/tests/{locality_validation,identity_contracts}.rs`, `workspace2/crates/nudox-root/Cargo.toml` only for an already-locked test dependency
| object pack | `workspace2/crates/nudox-object-pack/src/{index,view,write,lib}.rs`, `workspace2/crates/nudox-object-pack/tests/{object_pack,index,view,identity_contracts}.rs`
| workflow | `workspace2/crates/nudox-workflow/src/{durable,tests,lib}.rs`, `workspace2/crates/nudox-workflow/tests/{durable_shared,identity_contracts}.rs`
| evidence | `workspace2/.codex/evidence/{typed-identity-shipping-card,salvage-ledger,typed-identity-shipping-closure}.md`

## Preserved law and selected construction

`ContentId`, `ArtifactId`, `GenerationId`, `ProviderSet`, and `ObjectRef` remain
checked at raw ingress. A locality validation owns a private compact borrowed
witness that contains closed, typed views of every identity-bearing lane; trusted
cursor/view projection borrows those facts and returns no `LocalityReadError`.
No public unchecked raw identity constructor is permitted.

One artifact-header domain code owns descriptor authority for the complete
artifact. Each descriptor carries a typed 31-byte `ContentPayload`; validation
checks the header authority once, casts every lane once, and only then mints the
borrowed witness. Projection uses safe `From` conversions from that payload and
must not reconstruct, transmute, or dynamically revalidate identity authority.

The public domain law is closed rather than generically revalidated: only the
registered domain markers are accepted and the generic parameter is retained
solely where it prevents cross-authority reconstruction. One registry
declaration emits each code enum and each marker binding. Enum discriminants are
literal numeric codes so duplicates fail in the compiler with E0081.

## Required evidence and falsifiers

| Law | Artifact or command | Expected evidence | Hard stop |
| --- | --- | --- | --- |
| validated locality | root integration `validated_locality_projection_is_infallible` plus source scan | one validation; no `GenerationId`/`ProviderSet`/`ObjectRef` raw decode in trusted projection; iterator item is not `Result` | any projection fallibility/redecode |
| domain coherence | two real marker consumers and compile-fail cross-authority reconstruction | mismatched authorities cannot compile; no fake generic dynamic decode | unused generic/only one authority path |
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
