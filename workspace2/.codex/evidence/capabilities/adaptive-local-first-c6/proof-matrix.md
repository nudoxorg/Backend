# Adaptive local-first C.6 proof matrix

| ID | Law | Weakened implementation | Falsifier / required evidence | State | Owner |
| --- | --- | --- | --- | --- | --- |
| C6-01 | The policy is deterministic regardless of input slice permutation. | Scan order becomes action order. | Permute local/remote/demand/bundle facts; require identical streamed actions and least-canonical duplicate errors. | RED | Luna policy |
| C6-02 | A hot, remotely pinned fact missing locally expands during unavailable or inconsistent remote health. | Treat outage as no-op or fetch cold/unpinned facts. | Exact hot/outage and inconsistent-generation traces; cold demand produces no fetch. | RED | Luna policy |
| C6-03 | Identity is byte-for-byte `FactKey` across move/fetch/evict and tiers are not truth. | Reconstruct an ID, use only object, or change generation. | Compare exact keys in every emitted action and move the same fact across RAM/NVMe/object-store inputs. | RED | Luna policy |
| C6-04 | Healthy recovery contracts only against a matching remote generation/fact. | Evict on stale/inconsistent/missing remote evidence. | Mismatched health/fact generation and missing remote-key traces emit no eviction. | RED | Luna policy |
| C6-05 | RAM/storage credits, pressure, latency, demand, CPU, and battery gate the appropriate action. | Ignore one physical credit or demand qualifier. | Boundary table at zero/exact/one-below bytes and every pressure/battery/demand level. | RED | Luna policy |
| C6-06 | Pinned/in-use facts and unverified bundles are protected. | Evict protected local fact or acquire/release rejected bundle. | Exact no-action tests for each retention and each non-verified bundle form. | RED | Luna policy |
| C6-07 | Cursors stream exact actions once; start/replay/resume/cancel have no duplicate or lost action. | Constant body, skipped cursor, prefix reservation drift, or terminal that is not fused. | For every action prefix, resume/replay with the unchanged snapshot; repeated completion is `Complete`; changed input restarts; invalid cursor is exact rejection. | RED | Luna policy |
| C6-08 | Pure policy makes no allocation and has bounded action work. | Allocate/sort/cache or scan unbounded state. | Isolated non-empty allocation control and work counter over 0/1/limit/limit+1; public caps are 64/64/64/32 facts and 224 effects. | RED | Terra measurement |
| C6-09 | Corrupt/mismatched bundle outcomes are inert; exact verified `Available` may acquire and exact verified `Active` may release. | Match only capability, accept bad identity/signature, or infer hidden residence. | Same capability with every outcome/residence; exact action trace and no hidden fallback. | RED | Luna bundle faults |
| C6-10 | Long outage and recovery remain finite and do not re-evict local proven capability. | Runaway fetch/action stream or recovery deletes hot/protected facts. | 100-step bounded fact profile plus recovery trace and exact termination count. | RED | Luna policy |
| C6-11 | Public outage/recovery journey kills inert policy. | Return `Complete` unconditionally. | Chief exact ignored journey passes after implementation and is red at baseline. | RED | Terra + chief boundary |
| C6-12 | The release base consumer is reproducible, under 50 MiB, and has no optional/server graph. | Test-only target, unused manifest edge, nondeterministic binary, or accidental optional dependency. | Chief script compares two isolated release hashes/sizes, runs self-check, and validates the exact direct-edge allowlist plus forbidden graph. | RED | Terra release control |
| C6-13 | The release base graph genuinely exercises roots, descriptors, schemas, operations, minimal store, runtime, and a concrete runtime-independent transport seam. | Link only the policy, add unused package edges, or invent an SDK/runtime transport. | Binary runs the exact `ObjectRef` → `GenerationRoot` → `LocalObjectProvider`/pinned operation → `InlineRuntime` → `InlineMemoryStore` route and translates `Fetch` to an identity-preserving offset-zero `RangeRequest`. | RED | Luna base consumer |
| C6-14 | A remote fact is intrinsically pinned proof; unpinned remote state is unrepresentable. | `pinned: bool`, an optional pin, or an ignored false branch survives. | Remove the field; downstream construction and policy tests prove the only remote-fact form carries the pinned authority. | RED | Luna policy |

## Coupling and resource skeleton

| Module | Invariant owner | Public terminal | Dependencies | State/control boundary |
| --- | --- | --- | --- | --- |
| `nudox-local-first::policy` | One `PolicyInput` snapshot plus cursor | `PlanStep` | `nudox-id` only | pure effect ordinal |
| `nudox-local-first::types` | Typed fact/credit/bundle vocabulary | facts/actions | `nudox-id` only | closed enums, no adapter state |
| `tests/policy` | policy falsifiers | exact action trace | public crate API | restart/cancel/action prefix |
| `tests/adaptive_journey` | chief | outage/recovery terminal | public crate API | one chronological public trace |
| `src/bin/local_first_base_client.rs` | shipping measurement consumer | concrete release executable | typed base graph only | policy, local owner, fetch-range seam |

| Resource | Baseline | Bound to establish | Measurement | Rollback |
| --- | --- | --- | --- | --- |
| policy allocation | inert no-alloc `next_action` | zero allocations per non-empty call | isolated allocator control | delete any heap/cache mechanism |
| policy action memory | `ActionCursor` + one `PlanStep` | no retained plan collection; O(1) state | `size_of` and trace count | retain rescanning safe control |
| policy work | inert O(1) | caller facts capped at 64/64/64/32 and effects at 224; no recursive/global scan | named scan counter at 0/1/limit/limit+1 | simplify rule ordering |
| release binary | no binary yet | reproducible under 50 MB at chief gate with required base graph | Nix release executable bytes, `cargo tree`, and consumer trace | remove accidental package edges |
