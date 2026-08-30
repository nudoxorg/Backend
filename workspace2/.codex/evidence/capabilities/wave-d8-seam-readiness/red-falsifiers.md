# Red and fake falsifiers

The normal terminal always executes real Cargo metadata. The mutation harness may replace only the
metadata executable through the named, test-only `WAVE_D8_METADATA_COMMAND` seam; it never changes
the documented package contract. Each case must run the normal command with `--require-ready` and
must fail when its particular source fact is injected.

| case | mutation | required killed behavior |
| --- | --- | --- |
| `added-package` | Fixture adds one package to a fixed workspace inventory. | A constant output or ignored actual package set is rejected as `UNEXPECTED_PACKAGE`. |
| `removed-package` | Fixture removes `nudox-compile-driver`. | C0 vocabulary alone cannot pass; `MISSING_DOCUMENTED_PACKAGE` identifies the driver. |
| `duplicate-package` | Fixture repeats one package name. | Parser rejects duplicate package inventory with a `DUPLICATE_PACKAGE` source fact. |
| `malformed-metadata` | Fixture writes invalid JSON. | Parser reports `MALFORMED_METADATA`, `BLOCKED`, and nonzero require-ready. |
| `metadata-command-fails` | Fixture exits nonzero. | Command reports `METADATA_COMMAND_FAILED`, does not substitute an empty/constant inventory, and exits nonzero with require-ready. |
| `real-current` | Actual pinned metadata reports only C0/I0 vocabulary for nested plans. | Record is `BLOCKED` with every missing documented package and every undocumented public seam. |

The self-test additionally runs the unmodified record twice and byte-compares both outputs. It is a
tooling mutation proof, not an assertion of compiler/index/graph/vector product behavior.
