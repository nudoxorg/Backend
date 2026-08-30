# Typed identity and locality closure receipt

Verdict: **ACCEPTED — shared typed identity/locality boundary closed**

## Identity and scope

```text
cold-review baseline: 789974d47387ca42eae45f56377db5aef55cd0d2
implementation candidate: 6d23b21c
final source/test candidate: 93e394ffc3e27434fb74a9131fa677223ec121fc
capability: checked compact identity authority and infallible locality projection
unsafe: none
new shipping dependency: none
```

The baseline already contained the integrated typed-identity/locality rewrite. This candidate owns
only the four independent cold-review findings and their exact measurement repair.

## Finding closure

| Finding | Correction | Falsifier/evidence |
| --- | --- | --- |
| Public 31-byte `ContentPayload` could inject caller-selected authority | Delete the wrapper and both raw `From` paths. `TryFrom<u8>` now mints a zero-sized `ContentAuthority<DomainTag>` proof; only that proof binds compact payloads. | compile-fail arbitrary 31-byte conversion and dependency-set-to-object binding, exact wrong-authority unit test, second-domain writer/parser round trip |
| Original card did not authorize the actual cold repair | Reissue one canonical card at clean shared baseline `789974d4` with exact source, test, lab, and evidence paths. | changed-path comparison against this receipt and clean status at candidate commit |
| Layout/SIMD JSONL measured the old 136-byte/48-byte grammar | Regenerate baseline inventory, three release timing runs, and scalar/SIMD size binaries against the exact candidate. | JSONL reports 200-byte witness and 49/217/353-byte grammar; source fixture is the current 353-byte format |
| Provider/schema mutation assertions elided nested sources | Compare `ProviderSetError::Empty` and `UnknownSchemaId(u32::MAX)` explicitly. | full `LocalityError` pattern assertions in the public locality integration test |

## Surgical salvage ledger

| Previous mechanism | Disposition | Current owner |
| --- | --- | --- |
| One shared domain byte rather than one byte per present descriptor | retain | `HeaderWireRecord::content_domain` plus 31-byte descriptor payload |
| No authority branch or raw identity decode in random lookup/cursor hot paths | retain | checked zero-sized `ContentAuthority` stored in `ValidatedLocality` and copied into projection |
| 45-byte descriptor and caller-owned canonical artifact | retain | unchanged descriptor grammar and direct writer |
| Safe typed projection | strengthen | proof-gated `ContentAuthority::bind`; no unsafe/transmute/fallback |
| Public `ContentPayload` conversion vocabulary | reject as a leaky owner | deleted; compact payload bytes have meaning only beside validated artifact authority |
| Exact provider/schema diagnostics | strengthen | complete nested sources asserted at the sole validation boundary |

The replacement preserves the useful compact wire and branchless traversal instead of reverting to a
32-byte descriptor, repeating authority checks per row, or erasing the rejected implementation. The
old implementation remains inspectable at baseline `789974d4`.

## Source and resource delta

From `789974d4` to `6d23b21c`:

```text
production Rust: +147 / -125 = +22 lines
public integration test: +23 / -16 = +7 lines
layout drivers: +22 / -8 = +14 lines
total source: +193 / -150 = +43 lines
ValidatedLocality<ObjectDomain>: 200 bytes (unchanged)
ContentAuthority<ObjectDomain>: 0 bytes, alignment 1
descriptor: 45 bytes (unchanged)
empty / 32-row / 64-row absent-overlay artifacts: 49 / 217 / 353 bytes
allocation in projection: 0 by construction; no allocation owner exists in the path
```

The public type and error are current-consumer bound: locality validation constructs the proof,
lookup/cursor projection consumes it, the second registered domain exercises the generic, and public
compile-fail/exact-error tests kill unchecked reconstruction.

## Measurement

On the recorded Apple M3 Pro release runs, the stable SIMD threshold remains 32 rows. Median complete
validation at 32 rows is 33,223 µs scalar versus 11,966 µs detected SIMD for 250,000 parses (2.78×).
At 16,384 rows it is 22,088 µs versus 1,786 µs for 488 parses (12.37×). The detected binary adds
324 bytes of Mach-O text, 16 bytes of const data, and 16,544 bytes of padded file size. Raw runs remain
in `layout-lab/evidence/locality-validation-m3-pro.jsonl`; no timing is inferred from a unit test.

## Tripwires

The exact changed-Rust-path scan finds no panic/unwrap/expect/unreachable, unsafe, `dyn`, production
`Box`/`Vec`/`Arc`/`Rc`, one-letter generic, unit namespace struct, or new public tuple field. The six
`Vec` identifier hits (`rg -n '\bVec\b'` over the changed Rust set) are caller/test fixture storage.
Existing `map_err` sites either retain their structural
source or re-walk zerocopy's non-semantic cast error into the exact provider/schema error asserted by
the public mutation tests. The only new public items are the checked proof and its complete two-operand
error.

## Gates

Passed at final source/test candidate `93e394ff`:

- root `nudox-id`/`nudox-root` all-target locked/offline tests and strict Clippy;
- root `nudox-id`/`nudox-root` locked/offline doctests, including the compact-payload compile failure;
- layout-lab all-target locked/offline tests, strict Clippy, and formatting;
- exact release benchmark and scalar/SIMD size-driver execution.
- two complete shared-root formatting, strict all-target Clippy, all-target test, and doctest passes;
- formatting, strict all-target Clippy, all-target tests, and doctests in durable-journal,
  observability, IR, layout-lab, compiler, and index nested workspaces.

A fresh explicit `gpt-5.6-terra` read-only review found one missing cross-domain UI falsifier and one
incorrect tripwire count. Commit `93e394ff` closes both; the same reviewer reproduced the focused
trybuild failure, the six-hit scan, clean status, and found no new P0–P2 findings. The strongest
surviving attack—binding a dependency-set authority then assigning it as an object identity—fails
with E0308 at the public boundary.
