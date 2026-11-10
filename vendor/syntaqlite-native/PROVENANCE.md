# Syntaqlite source subset

The `syntaqlite`, `syntaqlite-common` and `syntaqlite-syntax` trees are from
upstream syntaqlite v0.13.5, commit `4e4de76312429fb2235ae093ab2125914320d5bd`.
Each upstream `LICENSE` symlink is replaced by the identical root license
bytes. The root Cargo workspace manifest here lists only the three included
crates and retains their upstream workspace package metadata and lint policy.
The actual parser and tokenizer include SQLite-derived C source. The upstream
license and SQLite public-domain notice are preserved in `LICENSE`.

This source is selected only for the SQLite profile; it does not approve or
package the separate PostgreSQL pgrust dependency.
