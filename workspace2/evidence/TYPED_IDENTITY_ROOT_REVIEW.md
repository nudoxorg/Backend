# Typed identity root review deck

This is an independent root-review deck, not a representation prescription. Run it against the
integrated candidate after the manager cycle. Any shipping edit made to satisfy it creates a new
candidate and requires a fresh blind Terra review.

## Primary-source constraints

- Rust evaluates free constants at compile time and surfaces a panic there, so a closed marker table
  can prove code uniqueness without a runtime map or unit-test-only assertion:
  <https://doc.rust-lang.org/reference/items/constant-items.html#evaluation>.
- BLAKE3 incremental `update` plus `finalize` is one streaming hash; `finalize_xof` can expose a
  chosen-length suffix without a second hash. Ordinary `update` is single-threaded and already uses
  the crate's architecture-specific SIMD paths:
  <https://docs.rs/blake3/latest/blake3/struct.Hasher.html>.
- A default BLAKE3 output is 256 bits. Reserving one or two bytes therefore changes the collision
  budget and every golden identity; that is a permanent protocol choice, not a refactor detail:
  <https://github.com/BLAKE3-team/BLAKE3>.
- Rust expects `From` to be lossless, value-preserving, and obvious. An implementation that silently
  replaces authority cells is not a raw-byte conversion; it needs a named digest projection while
  raw representation uses checked `TryFrom`:
  <https://doc.rust-lang.org/std/convert/trait.From.html#when-to-implement-from>.

## Mandatory falsifiers

| law | executable hostile attempt | rejection condition |
|---|---|---|
| negative API | external compile-fail example attempts the former `From<[u8; 32]>` and same-byte cross-authority reconstruction | a runtime test alone cannot prove the trait is absent |
| conversion semantics | pass an array whose authority cell disagrees with the marker and compare the input to the result | `From` silently overwrites or discards a byte; a named digest projection is required instead |
| registry closure | replace one registered domain or encoding code with a duplicate and run a compile check | a runtime uniqueness test or lookup table survives the duplicate |
| routing entropy | hash many values in one domain, derive the public routing word, and inspect every byte/bit used by bucket selection | an authority cell occupies routing entropy or makes low-bit buckets constant |
| raw decode | test N-1/N/N+1 lengths and every wrong domain/encoding pair; inspect expected authority, observed code, raw bytes when present, and structural source | an error erases the source, input extent, or observed authority |
| consumer closure | search all workspace and nested-workspace raw constructors, golden arrays, durable decoders, pack readers, and routing consumers before/after migration | an unchecked constructor remains or a consumer silently reinterprets the new layout |
| streaming identity | compare one-shot with every split point, including empty chunks, while counting allocations and retained owners | a second hash, staged payload, allocation, or changed chunk semantics appears |
| fixed representation | assert size 32, alignment one, direct borrowed pointer identity, and exact canonical record bytes | a wrapper/backing owner or hidden materialization appears |

## Counterexample mutants

The final tests must fail at least these plausible weakened implementations:

1. Decode checks length but ignores the authority cell.
2. Content checks domain, while artifact checks only encoding.
3. The public routing word still consumes the authority prefix as hash entropy.
4. Duplicate marker codes compile because uniqueness exists only in a unit test.
5. `TryFrom<&[u8]>` is checked, but a public array `From` recreates the old bypass.
6. One-shot construction applies the authority projection while incremental finalization does not.

## Integration decision

Do not rebase the Index I0 deletion candidate until every applicable row is reproduced on the typed
identity candidate. After rebase, its exact/lexical compile-fail proof must cover raw reconstruction,
not merely assignment or `Into`, and the integrated index tree is reviewed as a fresh candidate.
