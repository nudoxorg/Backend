# Colm and Ragel adoption boundary

Closed state machines should have one declarative authority when generation removes handwritten
transition duplication without weakening Rust ownership, payload types, errors, or concurrency proofs.

The current Colm Suite describes Ragel as a finite-state compiler with Rust output and table, flat
table, and goto backends. Colm is a typed transformation language, but its normal tool/runtime path
generates C, invokes GCC, and links the Colm runtime. Sources:

- <https://github.com/adrian-thurston/colm-suite>
- <https://github.com/adrian-thurston/colm-suite/tree/main/examples/rust>

## Architecture fit

| Machine | Fit | Decision |
|---|---|---|
| Incremental byte/control-envelope parser | Strong | Build a Ragel Rust pilot when chunked transport lands; compare table/flat/goto output with the zerocopy slice validator. |
| Workflow phase × event control | Partial | Pilot the control projection. Keep `StageOutput`, failure equality, ownership, effects, and exact errors in typed Rust host actions. |
| Local object cursor | Mechanically strong, economically weak | Four typed states are clearer and smaller until generated code proves otherwise. |
| Hydration publication typestate | Weak | Rust typestate makes invalid publication unconstructible; a runtime numeric state would regress the proof. |
| Runtime slot/waiter/credit atomics | Rejected | A DFA does not specify memory ordering, linear permits, wake races, ABA, or reclamation. Production transitions remain Rust exercised by Loom. |
| Source-language analysis/transformation in compiler workers | Potentially strong | Evaluate Colm only in the vertically scaled compiler/tool workspace; never link its C/GCC runtime into the portable client. |

## Promotion gate

A generated machine enters production only when all of these are true:

1. The `.rl`/`.lm` source is the reviewed authority and the generator is pinned to an exact upstream
   revision or release artifact.
2. Generation is deterministic. CI regenerates into a temporary location and requires a byte-identical
   diff; developers do not need the generator to consume a release crate.
3. Generated Rust is checked in, formatted or isolated, warning-clean, and inspectable. It introduces
   no runtime dependency, allocator, trait object, FFI, or hidden panic path.
4. Public state, event, action, payload, and error types remain descriptive Rust types. A numeric
   generated `cs` is a private control coordinate, never durable or public semantic state.
5. Exhaustive case tests compare generated execution to a small independent Rust model. Fuzz/model
   replays exercise the generated path, not an analogous handwritten transition.
6. Optimized text size, read-only table bytes, branches, instruction/cache counts, compile time, and
   source complexity beat the typed enum/match baseline for representative local and server profiles.
7. The generated transition remains compatible with tracing probes and, for sequential machines,
   deterministic fault replay. Concurrent atomics still require Loom and a written memory-order proof.

The repository's readily available Nix package is Ragel 6.10. It is useful only as an experimental
control; no production source may be migrated from that old generator without proving equivalence to
the current Colm Suite Rust backend.
