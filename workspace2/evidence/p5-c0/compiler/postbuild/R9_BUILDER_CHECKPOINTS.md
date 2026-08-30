# R9 exact-path Luna builder custody

All builders were explicit `gpt-5.6-luna` tasks with `fork_turns="none"`, commissioned only after the
R9 four-role deck and separate hostile pre-edit review cleared the frozen card digest. Each had one
path, its source skeleton, and exact SHA-256; none could change a manifest, dependency, second source
file, or frozen artifact.

| task | destination | frozen SHA-256 | LOC | resulting commit |
| --- | --- | --- | ---: | --- |
| `c0_compiler_builder_lib` | `planes/compiler/crates/nudox-compile-registry/src/lib.rs` | `bbe0152ce639ee7f92a9b72e26dd6b040133a28f3700f04aee302abd1c4fe867` | 80 | `58949077d6ef7befb79d8663343f5482f28506ec` |
| `c0_compiler_builder_dispatch_tests` | `planes/compiler/crates/nudox-compile-registry/tests/dispatch.rs` | `588ee2b4c5ec3ad7847970ff316640a7ca05343ab66e75aea890b8880acdd9ab` | 48 | `3d3d099d584c59a653815dc6af607dc190f2e333` |
| `c0_compiler_builder_subset_tests` | `planes/compiler/crates/nudox-compile-registry/tests/subset.rs` | `3cefa53b7b6d56951bef3c3548945d6f8a3fc097a702aa9462f65d500d115894` | 101 | `489f2d396e073c27fcec8285ba9c7933cfad1a7d` |
| `c0_compiler_builder_release_consumer` | `planes/compiler/crates/nudox-compile-registry/examples/release_consumer.rs` | `b86ecd9c1c15f855e8523883250b89ab96f042708b54e4ccd21f01edfd4a76c8` | 70 | `5017ca393b9662b8e7fa2e32e0c88caaeabf8e81` |

The manager checked `cmp -s`, destination SHA, `git diff --check`, and a single changed path after every
checkpoint before making the listed commit. Every checkpoint cleared; no repair agent was commissioned.
