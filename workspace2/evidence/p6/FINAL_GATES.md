# P6 clean-gate receipt

The exact closure-review commit was `40dd84e0fa836dfb6da706006788f915b727469b`.
Both independent clean gates ran from
`/private/tmp/nudox-prototype-protocol-registry-dispatch/workspace2` with no uncommitted changes.

| gate | command | result |
| --- | --- | --- |
| 1 | `cargo fmt --check && cargo test --workspace --all-targets && git diff --check && test -z "$(git status --porcelain)"` | exit 0 |
| 2 | `cargo fmt --check && cargo test --workspace --all-targets && git diff --check && test -z "$(git status --porcelain)"` | exit 0 |

The closure-specific nested checks also passed before these gates:

| workspace | command | result |
| --- | --- | --- |
| compiler | `cargo test --workspace --all-targets` | 2 integration tests passed |
| index | `cargo test --workspace --all-targets` | 4 integration tests passed |

No codegen, assembly, compile-time comparison, monomorph text comparison, or allocation/layout
measurement was run for a new registry candidate, because admission failed before a candidate existed.
Those fields are intentionally UNVERIFIED rather than implied by the retained manual baseline.
