# P5 C0 exact-falsifier repair card

Base candidate: `328f890ab96fb5df9bd4975eea7e552dad5ba386`.

This is a deletion-first repair card. It does not change the canonical capability terminal,
ownership, or authority boundary. The rejected checkpoint remains inspectable.

Allowed paths are the six builder paths in `P5_C0_MANAGER_CARD.md`; no other path, manifest,
dependency, allocation, unsafe code, macro, or public API is authorized.

Required repair falsifiers:

1. Replace shadow compiler fixtures with dependency-free current-toolchain compilations of source
   importing the actual public `nudox_compile_registry` or `nudox_ir_vocab` artifact. Each source has
   exactly one forbidden expression and asserts its stipulated code and symbols; a source replacement
   with the legal member or correctly typed coordinate must fail the expected-diagnostic assertion.
   Do not use `unwrap`, `expect`, `panic!`, or discarded cleanup. Restore a positive public consumer
   for `RustSubsetOnly::parse`.
2. Make the private matrix non-positional with named language fields, consume the named const in the
   compile-time witness/test, and retain row-owned lower behavior. Preserve exactly one language
   match and the one `drive` stage match.
3. Make the release executable return typed, source-preserving errors and nonpanic pointer/length
   mismatch results; it still drives Rust Parse, Rust LowerIr, and TypeScript Parse on three distinct
   nonempty slices.

Budget reconciliation: the prior registry forecast (80 formatted LOC) was exceeded at 118. Delete
redundancy first and record actual formatted delta. The hard limits and literal unused reserves in the
canonical card remain binding; stop if they would be consumed.

Focused gates: the two C0 test files, registry doc tests, both format checks, and `git diff --check`.
Commit only the repaired allowed paths. The manager separately produces all raw evidence artifacts.
