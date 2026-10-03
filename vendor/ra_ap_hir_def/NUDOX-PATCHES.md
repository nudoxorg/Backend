# Nudox patch notes

This crate is vendored from rust-analyzer 0.0.341, pinned to upstream commit
`7ae18ed96a90dbb8972fc886321651fdda80f7de` (`crates/hir-def`) and the Cargo
registry checksum `2ca8a7a983347be37e971c3d57dfdb6fdd937575afa27cad85bccd870046281f`.
The original MIT and Apache-2.0 license texts are retained here.

`attrs::Docs::find_ast_range` returns `None` when a Rustdoc range has no source
map line at or before its start. Macro-expanded text can have no AST mapping,
so this is a valid unmapped-text result; subtracting one from a zero partition
index previously panicked during Rust compilation. The checked predecessor
preserves the docs text and lets callers retain it as owned, unmapped text.
