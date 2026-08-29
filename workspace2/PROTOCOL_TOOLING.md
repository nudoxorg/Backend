# Binary protocol tooling decision

The Wave 1 frame is a fixed envelope and fixed-width directory over borrowed section bodies. Its
central requirement is not merely declarative parsing: read and write must share one layout, validation
must create an allocation-free witness, and repeated access must borrow without reparsing scalars.

## Decision for the fixed frame

Use one `zerocopy` wire module containing endian-aware `Header` and `Descriptor` records. Constrained
values remain raw integers in the all-bit-pattern-valid wire record and convert once into closed
semantic enums during validation. Derive only the traits each record actually satisfies, assert exact
size/alignment, borrow the directory as a typed slice, and instantiate the same record types in the
encoder.

This design deletes:

- mirrored header/descriptor offset constants;
- handwritten `read_u16`/`read_u32` and `write_u16`/`write_u32` forests;
- repeated fallible descriptor reparsing in a validated iterator;
- a second independent encoder description that can drift from the decoder.

Relevant primary documentation:

- [`zerocopy::byteorder::U32`](https://docs.rs/zerocopy/latest/zerocopy/byteorder/struct.U32.html)
  is unaligned, endian-parameterized, and implements the byte conversion traits needed for shared
  read/write records.
- [Zerocopy byte-order records](https://rust.docs.kernel.org/zerocopy/byteorder/index.html) show the
  `FromBytes`/`IntoBytes`/`KnownLayout`/`Immutable` fixed-record pattern.

## Where the alternatives fit

- [`binrw`](https://docs.rs/binrw/latest/binrw/) is declarative and supports `no_std`, but its natural
  abstraction is a reader/writer with seek semantics. It is a candidate for a cold seekable archive or
  importer whose values must be materialized, not the default for the hot borrowed fixed envelope.
- [`nom`](https://github.com/rust-bakery/nom) supports streaming partial input and can run without
  `alloc`. It is a candidate for a future genuinely variable grammar or incremental transport parser.
- [`winnow`](https://docs.rs/winnow/latest/winnow/) emphasizes declarative parsers with predictable
  performance and supports binary streams. It is likewise a candidate for variable control/query
  grammar. It does not by itself provide a shared encoder layout.

No parser framework enters the core merely to make imperative code appear declarative. A candidate
must demonstrably reduce total code and branches, preserve exact structured errors, allocate zero on
the borrowed path, compile without `std` where required, and provide equal or smaller code size and
validation work on the representative frame corpus.

## Ownership shape

```text
&[u8]
  -> checked Header reference + checked Descriptor slice
  -> semantic validation once
  -> ValidatedFrame<'bytes> borrowing the original records and bodies
  -> exact lending section iterator with no fallible internal reparse
```

An owning adapter may later bind a mapped/boxed buffer to this borrowed view with `ouroboros` when the
view must escape the construction scope. The borrowed core remains primary; self-reference is not
introduced into every caller merely because an owning convenience is possible.
