# Independent review packet: Wave A.1 pre-edit, hardened red journey

## Review input identity

- Reviewed source is a fresh path-limited, no-Git export of the clean manager
  revision recorded in the runtime receipt. Its only chief-owned source change
  is the ordinary single-parent transplant of red commit
  `7f90b60d8ee3efa27c7d60f6819781fab2327f71`.
- The export excludes every legacy `MANAGER_EVIDENCE.md`, closure card,
  prototype evidence, historical packet, and builder-rationale artifact.
- The only non-source additions are the exact review inputs below, copied into
  `.review-inputs/` with their displayed SHA-256 values:

  | Snapshot path | SHA-256 |
  | --- | --- |
  | `.review-inputs/brief.md` | `5e76cd7b56ca63976ed1ac580e4a4cbfa7eb4ef7ca2002a1c62a5ceac7482598` |
  | `.review-inputs/proof-matrix.md` | `df73753823a8c69feda1a95a74f7249e18265e673b7a3d1431f9ee9e9ad39090` |
  | `.review-inputs/index.toml` | `f6b1eed6267d8e21d59b5c21717e468c4646c1285789f0ca12ad6a3d96426374` |
  | `.review-inputs/TESTING.md` | `c29ae328a26117dd347c9b4952b24774c0cd7a5c6b8cc0b28ae2d7f22fda8e7b` |

- Chief public red test:
  `workspace2/crates/nudox-operation/tests/wave_a1_foundation.rs`, SHA-256
  `31c92918c4318cf1d5092504a16e34724e2f9c8e772e85e2e0a894ebab9db5df`.
- Intended boundaries are `nudox-root`, `nudox-object-pack`, `nudox-object`,
  `nudox-store-memory`, `nudox-hydration`, and `nudox-operation`. No test-only
  crate is permitted.

## Required terminal

Canonical root bytes validate to a borrowed root view. A sparse object pack is
authenticated only by the expected full-pack artifact identity plus an explicit
range proof for header, directory, and selected body. The selected body matches
the exact root descriptor and semantic content identity, then transfers into a
generic immutable store. Hydration replay creates a non-forgeable verified
generation capability that a real operation consumes.

The sole untrusted demand generation comparison occurs at binding to the view.
The later verified operation may bind `{generation,key}` but must not repeat a
generation/object equality check. Missing selection remains typed; rejection
retains exact causal error and owner.

## Required ownership, dependency, and resource boundaries

- Native `GenerationRoot` is construction/control only; `ValidatedRoot` and
  `ValidatedLocality` are borrowed canonical owners used by planning.
- Full-artifact range proof owns physical-range authority; `ContentId` owns
  semantic body verification. Pack never becomes a store dependency.
- Store accepts only a non-forgeable verified owner; hydration stays pure and
  narrowly feeds operation. No publication, cache, transport, UI, compiler, or
  runtime policy is in scope.
- Validation and trusted projection are zero-panic and do not repeat successful
  raw semantic decode. At 100,000 rows root bytes are `8 + 63*N` and hierarchy
  scratch is reusable caller-owned `u32` storage (`4*N`), without a retained
  native sidecar. No unearned `Box`/`Vec`/`Arc`/`dyn`, cache, unsafe/SIMD, or
  dependency is permitted.

## Hardened public falsifiers

The chief test asserts exact requested identity for missing selection;
corrupt-body returned owner bytes plus saved pointer/capacity identity and exact
nested `HashMismatch`; empty store and no capability after rejection; wrong
full-artifact identity rejection for header/index/body; header/index truncation
and mutation errors; and stale `Need` generation rejection at binding. Review
the literal ABI/card and existing consumers against those falsifiers, seeking
an invalid-state leak, false authentication claim, duplicate owner, lost error
or owner, hidden allocation/scan, dead marker, unearned surface, or simpler
standard-library representation. Return findings, tripwire table, cleared
suspicions, and verdict only; do not edit, change the contract, or prescribe an
unfrozen API.

The reviewer is read-only over the separate source snapshot. A dispatch-only
sidecar must spawn the registered reviewer with `fork_turns = "none"`; the
runtime child ID, resolved role/model/effort, and effective build-only sandbox
are required receipt fields, not inferred from a task label or wait result.
