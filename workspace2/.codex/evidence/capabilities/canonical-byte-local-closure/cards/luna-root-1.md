# Frozen Luna card: canonical borrowed root validation checkpoint

Registered role: `nudox_luna_implementer`; expected runtime `gpt-5.6-luna` / `max`; config `.codex/agents/nudox-luna-implementer.toml`.

Baseline: chief commit `b062d77e9805d8be1a4ae85f7efb4b28402e700d`, tree `85881e08979c0d54911f605a8dbfa2f08932cc33`; capability card/evidence baseline `31333fa1761d8c242c589886dbecb1b0680cde78`. Allowed paths are **only** `crates/nudox-root/**` and this capability evidence directory. Do not edit hydration, operation, object, object-pack, store-memory, any manifest, chief test/brief, controller/shared roadmap/skills, compiler/index, or durability paths.

## One terminal

Make the caller-owned canonical root bytes validate into a non-forgeable, zero-retained-row/backing `ValidatedRoot`, and pair it with existing validated locality as `BorrowedGenerationView`. This root-only checkpoint must expose sufficient private/public root surface for the chief terminal, but must not start borrowed hydration/planning or operation binding.

## Exact retained salvage and implementation law

Start from the uncommitted symbols recorded in `salvage-ledger.md`; they are a rejected partial diff, not a clean baseline. Retain only the rows that meet the checks below.

- `RootWireRecord` embeds public `ObjectDescriptorWireRecord`, retaining exact 63-byte layout. `ParentWire` is a closed typed `repr(u8)` tag with `pub(crate)` visibility sufficient for sibling root code and zerocopy derives; invalid tag/schema diagnostics may rescan only after typed cast rejection.
- Root ingress typed-casts header and rows once. It validates every `descriptor.content[0]` with `ContentAuthority<DomainTag>`, retains one authority only after every row agrees, and privately projects `ObjectRef` from typed fields by binding the array-destructured observed 31-byte payload. Never use `ContentId::from_digest`, `ObjectRef::try_from`, a duplicate descriptor reader, raw authority construction, `Option` fallback, `unwrap`, `expect`, panic, or unsafe.
- Model empty versus populated rows with a closed state. A populated state retains the checked authority; empty selection never needs an absent authority branch.
- Resolve parents and validate hierarchy in linear work using exactly one fallible `Vec<u32>` lane, cap `4 * declared_rows` bytes before reservation, preserve `TryReserveError` as the typed source, and drop the lane before `ValidatedRoot` returns. No retained lane, second row/descriptor arena, or warmed allocation is allowed.
- The borrowed witness keeps the input slice and typed wire-row borrow only. It owns no root-row/backing bytes. Its byte/row projections remain within caller input. Existing `GenerationRoot` remains unchanged.
- Keep `RowIndex::from_validated_borrowed_root_position` and new `ClosureScratch` helpers only if root-view selection directly uses them. Otherwise delete those helpers in the same checkpoint.

## Required owning tests and gate

Add exact `nudox-root` tests for valid pointer containment; header/row truncation; trailing bytes; invalid parent tag; invalid nested schema; invalid object domain with observed operand/source; duplicate/unsorted key; absent/self/cyclic/missing parent; root/locality generation/count mismatch; and borrowed selection parity. Include an adversarial 100,000-row arbitrary-parent chain plus one-row control measuring linear passes/row reads and transient scratch high water versus constant-memory control. Add compile-fail/visibility coverage if a downstream construction could forge the witness.

Run at minimum:

```sh
cargo test -p nudox-root --locked --offline
cargo fmt --check
```

Commit a coherent root-only passing checkpoint. Return the exact commit/tree, changed paths, raw gate output, validation error precedence/source/operands, layout/offset facts, scratch measurement, worker receipt, retained/deleted salvage rows, strongest counterexample, and remaining red rows. Do not claim CBC-03 through CBC-09 closed.
