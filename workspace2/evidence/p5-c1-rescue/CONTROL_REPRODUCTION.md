# C1 rescue detached control reproduction

This control was created and passed before the rescue manager card.  It is an isolated Cargo workspace
at `evidence/p5-c1-rescue/format-control`, deliberately excluded from the production workspace by its
own empty `[workspace]`.  Its only dependency is the real C0 vocabulary crate at
`domains/ir/crates/nudox-ir-vocab`; no helper declaration replaces `EntityId` or `TypeId`.

## Observable control

The control accepts a padding-free envelope of `4 + 4 * entity_count + 4 * type_count` bytes:

```text
0: magic 0xc1 | 1: schema 1 | 2: entity count (0..=2) | 3: type count (0..=2)
4..: entity lane, little-endian u32 dense IDs | type lane, little-endian u32 dense IDs
```

`FragmentView::validate(&[u8])` is the sole construction path.  Its private correlated fields retain
the caller envelope and its two checked sub-lanes.  Validation order is literal: envelope truncation,
magic, schema, entity cardinality, type cardinality, then exact geometry.  Its only cursors yield the
existing `nudox_ir_vocab::EntityId` and `nudox_ir_vocab::TypeId`.

`tests/control.rs` proves golden 0/1/2 lane envelopes; each strict truncation; each header tag/count
mutation; one-cell excess geometry; payload-cell survival; and an internal pointer-containment check.
The library is `#![no_std]`, borrows the input, contains no owner or allocation API, and has no copying
loop. Allocation/copy claims are therefore limited to this control's source inspection plus test-path
review; a production candidate needs a whole-consumer allocator measurement before claiming zero cost.

## Actual-rlib negative harness

The integration test locates **exactly one** fresh
`libnudox_ir_format_control-*.rlib` beside its own executable. It invokes the configured `RUSTC` (or
`rustc`) through a real child process using `--edition 2024 --crate-type lib --emit=metadata=- -L
<deps> --extern nudox_ir_format_control=<rlib> -`, with piped stdin, `stdout(Stdio::null())`, and
captured stderr. Its seven independent invocations have these exact primary predicates:

| fixture | required primary diagnostic | required symbols |
|---|---|---|
| downstream struct literal | one uncoded private-field primary | `FragmentView`, `envelope` |
| raw `.into()` | one `error[E0277]` | `From`, `FragmentView` |
| raw `.try_into()` | one `error[E0277]` | `TryFrom`, `FragmentView` |
| format `EntityId` import | one `error[E0603]` | `EntityId`, `private` |
| format `TypeId` import | one `error[E0603]` | `TypeId`, `private` |
| builder import | one `error[E0432]` | `FragmentBuilder` |
| third lane method | one `error[E0599]` | `atom_ids`, `FragmentView` |

The legal companion is a separate actual-rlib compilation that imports `FragmentView`, calls
`validate`, and consumes `entity_ids`; it exits successfully and does not use a test helper.

## Recorded run

Worktree: `/private/tmp/nudox-prototype-real-compiler-ir` at `e106aa49` before this control commit.
Temporary dependency-cache paths are intentionally not durable; the harness discovers its executable's
parent each run and fails if rlib cardinality is not exactly one.

```text
cd workspace2/evidence/p5-c1-rescue/format-control
cargo fmt --check                                      # status 0; stdout/stderr empty
cargo test                                             # status 0; stdout captured by Cargo as below
```

The test command reported 1 unit test, 4 integration tests, and 0 doctests; all passed.  The compiler
children have null stdout. Their stderr is captured and asserted only through the table's exact coded
or one-uncoded-primary cardinality plus the listed causal symbols, avoiding unstable spans and
temporary paths.

Per-run external SHA-256 custody (`shasum -a 256`; never `DefaultHasher`) was:

```text
6fee2e5cd1436b0741d9f3c74268e38bc8a3814879c5d56f4b570c74efcc1393  Cargo.toml
b57e1f577bf2615dbc21906a65d7a7f5d89cb85dea3b601377a8011fb68f77ce  Cargo.lock
7973008fcfb89bf5bd566b33a5625b1b53bbb6f6d9b2c9a967da8aabe717d49e  src/lib.rs
823f7b82f552bcc824ddbe70c546ccc1655c8fe194af589c2872a9f4bc187b35  tests/control.rs
2470230102424a34892369204ce20c5a164cec25d894ce8eee45331e636e5a78  ../../../domains/ir/crates/nudox-ir-vocab/src/lib.rs
2301c5db86828346b6f68faee6cd902eaad1c9900f08b3302ba1703371d7722e  target/debug/deps/libnudox_ir_format_control-15fbf719b54c7165.rlib
```

Replay the two commands above from a clean checkout of this commit. The test itself rechecks the
actual-rlib cardinality, child status, stdout disposition, stderr predicates, and legal control.
