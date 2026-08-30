# Fresh Luna plausible misreader — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_plausible_misreader`
Requested model: `gpt-5.6-luna`
Fork mode: `none`
Role: read-only plausible misreader

Calibration evidence — plausible misreader (read-only, no source/history inspection)

Cheapest interpretation that could look compliant:

- Edit only `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs`, staying within the two builder paths.
- Add a roughly 53-line test that writes a temporary Rust fixture and invokes `rustc`.
- Make the fixture define a local/shadow `nudox_ir_vocab` module containing lookalike `EntityId` and `TypeId` types, then compile an `EntityId` passed where the fixture’s `TypeId` is required.
- Assert `E0308`, both type names, and one compiler error.
- Mutate the expression to use `TypeId`, rerun, and assert the diagnostic predicate no longer matches.
- Keep production unchanged and rely on the existing layout assertions.

This is the cheapest apparent patch because it avoids locating/linking the built public rlib and avoids proving the actual exported crate boundary. A slightly less aggressive variant would compile the fixture against crate source or an arbitrarily discovered artifact while treating that as “the public artifact.”

Permission gaps or ambiguities that could enable this reading:

- “Dependency-free compiler-process fixture” does not explicitly define whether dependency-free means no Cargo dependency, no `--extern`, or merely standard-library-only fixture code.
- “Actual public `nudox_ir_vocab` artifact” does not specify the exact rlib-resolution procedure or prohibit source inclusion, local recreation, or shadow modules in that sentence alone.
- The legal-mutant operation is required but its exact mutation command and source transformation are unspecified.
- The 53-line forecast and 22-line reserve do not explicitly state whether generated temporary fixture text, embedded raw strings, or helper code count toward the test budget.
- Temporary files used by the compiler-process test are not explicitly named among writable paths, leaving uncertainty about whether ephemeral filesystem writes are permitted.
- The card does not explicitly state that the fixture must be compiled as a downstream consumer with the real crate linked externally, rather than as an in-tree/module-level recreation.

The card does rule out this misread. Literal rules include:

- “one dependency-free current-toolchain compiler-process fixture imports the actual public `nudox_ir_vocab` artifact”
- “local/shadow types … [are] inadmissible”
- “The test is terminal only when it proves the actual exported crate rejects the exact kind mismatch”
- “a local recreation … [is] inadmissible”
- “The fixture is an ordinary `tests/coordinates.rs` public consumer”
- “no … unlisted path is writable”
- “A compiler invocation failure remains causal rather than being converted to green success.”

Therefore the shadow-module patch is explicitly falsified, and the source/arbitrary-artifact variant is also excluded unless it demonstrably imports and links the actual exported public artifact. The remaining artifact-resolution and temporary-file details are procedural ambiguities, not permission to weaken the public-boundary proof.
