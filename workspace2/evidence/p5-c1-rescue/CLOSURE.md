# C1 rescue closure

Production checkpoint: `f8ebf5b6b23e73ebd4ff05980f80d20787a5beef`.
Its predecessor `3193cd098568d03b2f3405508b6e41256c9d3efc` is retained as a rejected repair checkpoint:
the manager independently observed its process-global allocation count change from 284 to 287 during
the measurement, and postbuild review also found a scenario-fixture `unwrap`. The repair uses
const-initialized thread-local tracking, an independent `PreparedFragment` absence invocation, and a
`match`-based legal fixture.

## Closure review

Separate Terra postbuild/closure task
`/root/p5_c1_format_rescue_manager/c1_terra_postbuild` (explicit `gpt-5.6-terra`, `fork_turns=none`)
returned CLEAR on `f8ebf5b6`. It repeated the focused suite five times with eight test threads and
confirmed the tracker sees only the measuring thread. It also checked the prepared absence primary,
the legal-source conversion, scope, manifest, LOC, no-reexport/conversion surface, and no C2 leak.

## Two independent clean manager gates

Both gates used a distinct fresh `CARGO_TARGET_DIR` and the locked domain workspace. No target path is
required for replay; each is a portable fresh temporary directory.

| gate | result | target description |
|---|---|---|
| `cargo fmt --check`; `cargo test --locked`; `cargo clippy --locked --all-targets -- -D warnings`; both diff checks | 0 / pass | fresh target one |
| `cargo test --locked`; `cargo clippy --locked --all-targets -- -D warnings`; diff/status check | 0 / pass | fresh target two |

Each test gate passed 1 format unit, 5 format integration, 0 vocabulary unit, 2 vocabulary integration,
and 1 vocabulary doctest. The actual-rlib integration test independently performs eight isolated
negative compiler invocations plus a legal validation invocation. The fresh target's ephemeral rlib
path is deliberately not recorded; the test resolves exactly one artifact beside its executable.

Current per-run external SHA-256 custody (`shasum -a 256`, not `DefaultHasher`) from the second gate:

```text
afeb037e49f08d479c39a866e06ec6ca8ffbbaeace5a52978682615bfcc1cb9c  crates/nudox-ir-format/Cargo.toml
be96b5efb144a6a10cf6860d49866fb2c226e2d1e24c903403161ff6947a28ed  crates/nudox-ir-format/src/lib.rs
37384fcb636fefad86e4def52801c85cc47068329c322f48de8312f98f8bc3b7  crates/nudox-ir-format/tests/fragment.rs
784e273c39b4e56de4c8f4eaffff7ea525abeadcd094e7cd4fceb0bac3c04bc7  Cargo.lock
```

Measured claims are deliberately narrow: validation plus full two-lane cursor consumption makes zero
allocations on the measuring thread; production has no `alloc` dependency or owned backing and forms
only subslices plus scalar four-byte decodes. This is not a whole-consumer zero-cost claim. Release
text, non-host platforms, Miri, and broader compiler/IR performance remain UNVERIFIED.

The only future integration consideration is a separately calibrated C1 builder/prepared-output card;
it is not part of this result and must not preserve this prototype API by default.
