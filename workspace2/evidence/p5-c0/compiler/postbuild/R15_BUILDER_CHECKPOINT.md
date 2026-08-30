# R15 one-path Luna builder custody

| task | model | destination | frozen SHA-256 | LOC | source commit |
| --- | --- | --- | --- | ---: | --- |
| `c0_compiler_r15_builder_subset` | `gpt-5.6-luna` | `planes/compiler/crates/nudox-compile-registry/tests/subset.rs` | `05bc6f455335106bac3812f4cde299d97890592fddd1c07a6ec1043213c77b8a` | 111 | `8aa30cdd57b32209ad185a2bb23f83307292d96c` |

The task used `fork_turns="none"`, changed only this named destination after R15 hostile clearance, and
reported a byte-identical source, `git diff --check`, and one-path proof. The manager independently
checked the same SHA and committed that one-path repair. The three other authorized destinations were
not modified in R15.
