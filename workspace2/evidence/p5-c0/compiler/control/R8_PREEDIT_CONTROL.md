# P5 C0-COMPILER R8 direct same-source control replay

`seeded/run-same-source-control.sh` was replayed successfully before R8 calibration using a detached
`fac5b709d4596f889735a773a6dbd6a9c7c822f5` control worktree and a detached candidate worktree at
`94edb2e360df2d35940af090a3c2cad626e97d91`.  The runner first copied the frozen candidate skeleton into
only the four permitted package paths in the detached candidate.  It then independently cleaned each
dedicated release target, asserted zero registry/vocab rlibs, built, and asserted exactly one fresh
registry plus one fresh compile-vocab rlib in each target.

Both direct `rustc` invocations succeeded with exactly the same consumer source, crate name
`release_consumer`, `--edition 2024`, `-C opt-level=3`, target `aarch64-apple-darwin`, and
`--emit=llvm-ir,asm`.  Each used its own `-L dependency=<fresh release/deps>` plus the exact pair of
explicit `--extern nudox_compile_registry=<fresh rlib>` and
`--extern nudox_compile_vocab=<fresh rlib>`.  No Cargo-example artifact was used.

| retained raw artifact | SHA-256 |
| --- | --- |
| `R8_PREEDIT_CONTROL.ll` | `3869ffadfbb8522c12b8aba284c8659cd40c0e6e34d2a8f9b78726c2b783efbb` |
| `R8_PREEDIT_CONTROL.s` | `433833b864e6b5dcf1b04c291ff11fb7cd3304cb4f26e5110332a331351b3f54` |
| `R8_PREEDIT_CANDIDATE.ll` | `b7a2164baf9cf5af9bc910594b7762028c321b24a3a2348d4b86d76a83fe002e` |
| `R8_PREEDIT_CANDIDATE.s` | `eae37d8a363641a1db86e44a6c6ec37c53d042933a65af17b1bb749cd6fb29c6` |
| `R8_PREEDIT_CUSTODY.txt` | `00061ad231fa2ff6dafca84d4355d97d4179c72895ad8aa9ad294dd0c76739f7` |

The raw LLVM for each named wrapper contains a direct call to the registry dispatch symbol and forwards
its pointer and length arguments.  That residual direct call is recorded as baseline-equivalent cost;
it is not an erasure or zero-cost claim.  This replay establishes that the previously missing direct
compile mechanism is executable.  It is pre-edit skeleton calibration evidence only; the real candidate
must rerun it after a builder copies the frozen source.
