# Stylo derive compatibility patch

Upstream crates.io package: stylo_derive 0.23.0 (Servo), MPL-2.0.
Package archive SHA-256 (verified against Cargo.lock): `d7fdb4416be784fd22b2bc3d1c7be82f6bb55e58af99b0e1e7e975b5575be8bb`.
Original source license headers retained.

Generated ToCss success results explicitly use `Result<(), std::fmt::Error>`.
The desktop also links serde_fmt, whose additional error conversion otherwise
makes function serialization fail E0282/E0283 inference. Serialization behavior
is unchanged. A competing conversion in the regression test reproduces it
without extra runtime dependencies. Native support identity includes this tree.

Validation: Rust/Cargo 1.97.1 isolated crate regression passes 2/2.
The exact unpatched upstream expressions fail the same regression with
E0282/E0283; the restored patch passes. Root workspace package-selection
triggered a Cargo resolver panic, so this focused test uses an isolated copy
of the vendored crate; the full desktop build is a separate required gate.
