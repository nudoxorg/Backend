# Wave A.1 foundation integration — frozen Phase 0 brief

## Custody and frozen inputs

- Capability baseline: `f2565a9fb33af06053bd19721d4dc2753ec09ed5`
  (`8477cb2ab93763c468d5431740cfb1d5e4c4cf82` tree).
- Chief-owned executable red journey: `7f90b60d8ee3efa27c7d60f6819781fab2327f71`
  (`f7d52a72a8f906495b7a0f7841b50d5dbd6b6227` tree),
  `crates/nudox-operation/tests/wave_a1_foundation.rs`, SHA-256
  `31c92918c4318cf1d5092504a16e34724e2f9c8e772e85e2e0a894ebab9db5df`.
- Manager checkout: `/private/tmp/nudox-wave-a1-manager-clean-7f90b60d`, branch
  `codex/wave-a1-foundation-manager-clean-7f90b60d`, rooted at clean Phase 0
  checkpoint `8007d7702d797b5ed8711050015e9cddd41fb9f5` with only the chief
  test patch transplanted as ordinary single-parent commit `e121f82365d835e5afa6bc569ed1cded595d2152`.
  Manager role/config: `nudox_terra_orchestrator`,
  `.codex/agents/nudox-terra-orchestrator.toml`, `gpt-5.6-terra`/`xhigh`.
- The chief retains the shared `codex/wave-a1-foundation-integration` worktree
  and final integration. This manager may commit only its isolated branch and
  must not merge into or edit the chief worktree. `/Users/mileswirht/Downloads/backend`
  is outside this capability's authority.

The pre-edit reviewer is source-isolated under a distinct dispatch-only parent:
`gpt-5.6-sol`/`low` must spawn registered `nudox_terra_reviewer` at
`gpt-5.6-terra`/`xhigh` with `fork_turns = "none"`. The sidecar may report a
review only after the runtime returns a nonempty reviewer child task ID. An
empty receiver/wait, direct manager reviewer, full-history fork, or Luna parent
is a custody failure, never a partial review.

## Public terminal

The public integration test must execute this call graph using no transport,
publisher, cache, UI, runtime-policy, compiler, or test-only crate surface:

```text
GenerationRoot construction/control
  -> canonical root bytes -> ValidatedRoot<'bytes>
  + canonical locality bytes -> ValidatedLocality<'bytes>
  -> GenerationView<'root, 'locality>
  -> Need { untrusted generation, projection }.bind(view) [one demand check]
  -> pure plan / Fetch { exact ObjectRef }

PreparedObjectPack inputs -> exact canonical whole-pack bytes
  -> ArtifactId<ObjectPackEncoding, ObjectDomain> for that full stream
  -> proof_len(range) / write_proof(range, caller output)
  -> authenticate_range(expected full ArtifactId, ReceivedRange, ObjectPackProof)
  -> authenticated header -> ObjectPackHeader
  -> authenticated header+directory -> AuthenticatedObjectPackIndex
  -> BodyRangeSelection::{Missing, Selected}
  -> Selected.verify_owner(exact fetched ObjectRef, authenticated owned body)
  -> VerifiedObjectOwner
  -> MemoryStore::insert_verified

replay pure plan -> StagedGeneration::verify
  -> non-forgeable VerifiedGeneration borrowing the validated view
  -> LocalObjectRun::from_verified(VerifiedGeneration, { generation, key })
  -> real batch effect -> fused terminal summary
```

`GenerationRoot` remains native construction/control only. `ValidatedRoot` is
the borrowed canonical semantic owner used by planning. The verified operation
consumes the capability; it does not reconstruct a generation/object equality
check after verification.

## Non-negotiable laws and owner boundaries

| Boundary | Sole invariant owner | Required result / error priority |
| --- | --- | --- |
| canonical root | `ValidatedRoot` | exact root grammar and generation identity; zero-panic validation |
| demand | `Need::bind` | compares untrusted demand generation to the view exactly once and retains `DemandBindError` |
| pack physical identity | range verifier | expected **full-pack** `ArtifactId<ObjectPackEncoding, ObjectDomain>` plus the explicit requested range and proof; an index-prefix hash is not authentication |
| header/directory | authenticated range then parsers | unauthenticated bytes cannot create a header/index witness; structural errors stay `ObjectPackError` |
| selection | authenticated index | closed `MissingBodyRange` or a selected exact `PackRange`, never `Option<&[u8]>`/empty success |
| selected body | selected-range verifier | first authenticates this exact body range against the full artifact, then compares the complete `ObjectRef`, then verifies its `ContentId`; rejection returns error and body owner |
| store | generic `MemoryStore` | consumes only `VerifiedObjectOwner`; first-write policy returns the verified owner unchanged on rejection, does not import pack or rehash semantic bytes |
| hydration | pure planner and `StagedGeneration::verify` | no external effect, store, or transport policy; capability cannot be forged |
| operation | `LocalObjectRun::from_verified` | consumes the verified capability, binds its one `{generation,key}` request, yields a genuine body batch and fused terminal state; no later generation/object equality recheck |

The pack's range-proof representation is deliberately not selected by the
public ABI. Phase 0 compares only concrete private controls behind
`PreparedObjectPack::{artifact_id, proof_len, write_proof}` and
`authenticate_range`; any selected scheme must authenticate arbitrary explicit
ranges against the exact full-pack artifact and preserve caller-output
preflight. `ObjectPackProof` is a borrowed opaque carrier, not a transport or
publisher abstraction.

## Dependency direction and explicit negative space

```text
nudox-id <- nudox-object <- nudox-root <- nudox-hydration <- nudox-operation
       ^           ^              ^
       |           |              +-- borrowed canonical/locality views
       |           +-- VerifiedObjectOwner capability
       +-- nudox-object-pack (physical sparse proof; no store dependency)
nudox-store-memory (generic verified-owner consumer; no pack dependency)
```

No reverse dependency from store to pack; no hydration dependency on store or
operation; operation may depend narrowly on hydration. Do not introduce
`Box`, `Vec`, `Arc`, `dyn`, caches, a protocol/publisher abstraction,
transport runtime policy, default allocation escape hatch, unsafe/SIMD, a new
dependency, or an unconsumed public API. A new allocation must have a recorded
lifetime/bound and a current consumer.

## Resources and safe controls

- Root read path: canonical `8 + 63*N` bytes and one caller-owned/reusable
  `u32` hierarchy scratch (`4*N` bytes); at 100,000 rows retain no native
  `GenerationRoot` sidecar after canonical bytes are validated.
- Pack header/index read, binary selection, authenticated witness projection,
  body verification, and warmed proof generation must account for allocations;
  borrowed header/index/witness paths may not allocate or rescan successful
  structural input.
- Store keeps its existing preallocated accounting and must neither copy nor
  rehash an already verified accepted owner. All rejection paths retain the
  exact owner and causal error.
- Clean proof requires a fresh empty Cargo target and Dylint cache under the
  pinned Nix environment; shared target artifacts are inadmissible.

## Mandatory falsifiers

1. A full pack where the authenticated directory names a demanded descriptor
   but the selected received body is same-length attacker data must fail before
   store mutation or `VerifiedGeneration`/operation capability issuance.
2. A proof valid for a different full pack, shifted range, short range, extra
   range, or corrupted proof must not authenticate a header, index, or body.
3. A missing selected range must remain typed `Missing`, not an empty body or
   a fabricated present object.
4. A stale untrusted demand generation must fail at `Need::bind`; a verified
   operation must not depend on a second generation/object-equality guard.
5. The red journey's real operation must consume the verified capability and
   actual selected descriptor. The strongest plausible substitute is a
   constant-body/constant-object operation after a nominal verification path;
   a two-distinct-object/key fault oracle must make that mutant emit the wrong
   batch and fail.

## TESTING.md map

| Clause | Matrix row or evidenced exclusion |
| --- | --- |
| allocation/layout shared | A1, A2, A9; existing root/pack/store allocation suites provide baseline control, new sparse proof path must add its own warmed measurement |
| universal negative/exact errors | A2–A8 |
| foundation fabric truncation, mutation, immutable output, borrowed proof | A2–A5; no top-level format crate because the public cross-crate operation test is the chief-owned terminal |
| object/root/store/hydration | A1, A6–A9 |
| operation/runtime/workflow | A8; runtime/concurrency excluded: terminal is synchronous `LocalObjectRun`, no atomics/queue/future state changes |
| end-to-end | A10; no frame or durable recovery in the frozen chief terminal, so those clauses are out of scope rather than claimed |

## Frozen worker lanes after pre-edit review

- **Luna pack lane:** `nudox-object-pack` and its existing tests only:
  full-artifact range proof, authenticated header/index, typed missing/selected
  range, exact error/owner preservation, and resource controls.
- **Luna root/hydration lane:** `nudox-root`, `nudox-hydration`, and their
  existing tests only: borrowed `ValidatedRoot`, one-bind `GenerationView`,
  verified capability borrowing/no forgeability, 100k/warmed controls.
- **Luna store/operation lane:** `nudox-object`, `nudox-store-memory`,
  `nudox-operation` production sources and bounded tests only: verified-owner
  handoff and operation consumption. This lane may not alter the frozen chief
  red test.

No worker starts until the independent pre-edit review clears this card and
matrix. Any disagreement that changes permanent pack-proof semantics or adds a
new dependency/unsafe authority is a product-authority fork.
