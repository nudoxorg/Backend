# Syntaqlite source subset

The `syntaqlite`, `syntaqlite-common` and `syntaqlite-syntax` trees are from
upstream syntaqlite v0.13.5, commit `4e4de76312429fb2235ae093ab2125914320d5bd`.
Each upstream `LICENSE` symlink is replaced by the identical root license
bytes. The root Cargo workspace manifest here lists only the three included
crates and retains their upstream workspace package metadata and lint policy.
The actual parser and tokenizer include SQLite-derived C source. The upstream
license and SQLite public-domain notice are preserved in `LICENSE`.

Local read-only instrumentation adds `analysis/engine/nudox_observer.rs` and
exports its bounded native walk observations through `analysis/engine/mod.rs`
and `analysis/mod.rs`. It is intended not to modify native parsing, name resolution,
diagnostics, or lineage; equivalence requires executable verification.
The observer file carries item documentation (the upstream lint policy denies missing docs
and the file did not compile under it), and it reports each occurrence as the written token:
the parser's name spans exclude the enclosing quote, bracket or backtick, so the observer
widens a quoted name's range by one byte on each side (read from the span's quote flag). Consumers must validate ranges against the original
host bytes and cannot infer access mode or a deployed provider from an event.

This source is selected only for the SQLite profile; it does not approve or
package the separate PostgreSQL pgrust dependency.
