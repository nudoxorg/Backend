# Resource controls

| control | bound | enforcement | rollback condition |
| --- | --- | --- | --- |
| metadata work | exactly four fixed manifests | literal manifest list; one `cargo metadata --no-deps --locked --offline` each | any dynamic discovery, build, or dependency traversal |
| package data | package names only, sorted | jq extracts `packages[].name`, rejects malformed/duplicate output | source includes dependency target/package data or unsorted output |
| environment | pinned Nix quality closure plus jq | `nix run` app invokes only tooling script | a new package reaches any Cargo shipping graph |
| output | one JSON record with no timestamp/PID/path | fixed schema and `LC_ALL=C`; self-test byte comparison | nondeterministic bytes or prose-only status |
| failure | no fallback inventory | command/parse errors emit source facts and force `BLOCKED` | error becomes empty package array or `READY` |
| test fixtures | temporary, process-local fake metadata executable | self-test trap removes it and command default remains cargo | fake command reachable without explicit test environment |

No allocation/copy/codegen claim is made. The bounded input is Cargo metadata names; the result has
no retained process state, cache, mutable domain state, service, or network owner.
