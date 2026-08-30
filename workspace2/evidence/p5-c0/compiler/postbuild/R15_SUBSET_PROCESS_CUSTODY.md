# R15 fresh subset compiler-process custody

Falsifier-bound Luna evidence task: `c0_compiler_r15_subset_custody_repair`, explicit
`gpt-5.6-luna`, `fork_turns="none"`. It changed no repository path. The checkout was
`e82f7a7b714037f18f582eaae8d532b8b2b48c03`; target was an isolated temporary directory.

```text
rustc 1.97.1 (8bab26f4f 2026-07-14)
cargo 1.97.1 (c980f4866 2026-06-30)
registry cardinality=1
registry SHA-256=8445b7ce05b00c9bf482fc4a2ac2b64abbe6aa140e8485e4b55c32530515946e
vocab cardinality=1
vocab SHA-256=35b5f3c9d9606248824aa8d650c11223caa202e654eebb00bab215b0a1197864
registry source SHA-256=bbe0152ce639ee7f92a9b72e26dd6b040133a28f3700f04aee302abd1c4fe867
vocab source SHA-256=a26c48dcc3cbe71b35b83ddf324cc4c961b4a7ffd42e11abcc390e9e60233d55
```

The exact rlibs passed to the original process remain at these audit paths:

```text
/private/tmp/nudox-r15-target.PvjaFz/debug/deps/libnudox_compile_registry-40046a88b049e9db.rlib
/private/tmp/nudox-r15-target.PvjaFz/debug/deps/libnudox_compile_vocab-22d4a1f8346a23ec.rlib
```

The original forbidden and legal probes each used exactly:

```text
rustc --edition 2024 --crate-type lib \
  -L dependency=/private/tmp/nudox-r15-target.PvjaFz/debug/deps \
  --extern nudox_compile_registry=/private/tmp/nudox-r15-target.PvjaFz/debug/deps/libnudox_compile_registry-40046a88b049e9db.rlib \
  --emit=metadata=- - 1>/dev/null
```

Child stdout is bound to `/dev/null`, so metadata creates no file and cannot reach the terminal. The rlib
hash binds the exact artifact passed to this recorded process; it is not a cross-directory reproducible
build identifier. A later clean build must prove fresh zero/one cardinality and record its own artifact
hash, not compare byte-equal to this captured run.

## Independent later per-run verification

A Terra reviewer independently rebuilt from the same pinned source/toolchain in fresh empty target
`/private/tmp/r15-custody.XAxica`, with dependency directory
`/private/tmp/r15-custody.XAxica/debug/deps/`. It proved exactly one rlib of each kind and retained this
second run's separate artifact identities:

```text
registry /private/tmp/r15-custody.XAxica/debug/deps/libnudox_compile_registry-40046a88b049e9db.rlib
registry SHA-256 5f8b9ecb1df84661fefd0cfb3c17c53ecbfeedeb7649ee3e1b1cb3a23b9ac25e
vocab /private/tmp/r15-custody.XAxica/debug/deps/libnudox_compile_vocab-22d4a1f8346a23ec.rlib
vocab SHA-256 b33d615ed074092986834bccbec22ee41fcaa1dbe88c49f5fd53ba7817bb70f4
```

It used the same edition, crate type, explicit fresh registry extern, dependency-path form, metadata
stdout sink, and forbidden/legal stdin sources. Forbidden exited 1 with the same sole E0599; legal exited
0; no metadata artifact was created. Its isolated `cargo test --test subset -- --nocapture` passed one
test with clean ordinary terminal text. The two different rlib SHA-256 pairs are retained as identities
of two exact fresh runs. The causal invariant is pinned source hashes plus toolchain/flags, zero/one
cardinality, and the exact rlib path/hash bound to that run's command and output—not cross-run byte
equality.

Forbidden stdin source:

```rust
use nudox_compile_registry::TypeScriptSubset;
fn main() {
    let subset = TypeScriptSubset;
    let _ = subset.lower(&[]);
}
```

It exited `1` with raw stderr:

```text
error[E0599]: no method named `lower` found for struct `TypeScriptSubset` in the current scope
 --> <anon>:4:20
  |
4 |     let _ = subset.lower(&[]);
  |                    ^^^^^ method not found in `TypeScriptSubset`

error: aborting due to 1 previous error

For more information about this error, try `rustc --explain E0599`.
```

Legal stdin source differs only in `subset.parse(&[])`. It exited `0`; stderr had only rustc's unused
`main` warning. The isolated `cargo test --test subset -- --nocapture` also exited `0` with this clean
terminal result:

```text
running 1 test
test typescript_lower_is_a_causal_absent_member ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

This records the fresh command inputs, status, source/rlib hashes, forbidden raw stderr, legal result,
and observability terminal without a rustc metadata filesystem artifact.
