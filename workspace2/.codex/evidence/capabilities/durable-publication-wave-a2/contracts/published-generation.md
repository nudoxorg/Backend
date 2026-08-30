# Frozen published-generation contract

This pre-edit contract is a public-surface constraint, not implementation source. It supplies no
constructor and names no future backend. It applies only to the current adapter-owned authority.

## Required representation

`PublishedGeneration` is an adapter-owned `#[non_exhaustive]` struct with exactly these readable
facts:

```text
pub pinned_root: nudox_id::GenerationId
pub dep_set: nudox_object::DepSetId
pub publication: PublicationFacts
```

It has one additional private seal field of an adapter-private type. It has no public constructor,
no public conversion from raw facts, receipts, heads, or verified generations, and does not implement
`Clone` or `Copy`. `PublicationFacts` is also `#[non_exhaustive]`; it exposes `pub stable:
ReceiptFacts` and named immutable-publication and head identities, while its construction remains
adapter-private.

The only construction path consumes one `VerifiedGeneration` and an adapter-private validated stable
publication/head result. The validation result exists only after the named journal receipt has been
matched to immutable checksum-valid publication bytes and a checksum-valid head. Reopen reconstructs
that same private validation result only after independently reducing the journal and validating the
immutable bytes and visible head. No caller may bind root/dependency facts to an unrelated stable fact.

## Required downstream falsifiers

The adapter must add two adapter-owned downstream compile-fail doctest fixtures, requiring no new
manifest or lockfile dependency:

1. `published_generation_literal_is_rejected` attempts an external `PublishedGeneration` literal
   with every required readable field. It must fail because the type is non-exhaustive/private-sealed.
2. `mixed_owner_published_generation_is_rejected` names root/dependency facts derived from verified
   generation B and `PublicationFacts` obtained from published generation A, then attempts the same
   literal. It must fail at construction before any runtime path can observe the mixed facts.

Each fixture runs through `cargo test --doc` for the durable-journal manifest in the pinned
environment. The test text is compiled as a downstream crate; it must import only the adapter's public
items and cannot use a test-only constructor. A later implementation may choose a stronger existing
repository harness only if it introduces neither a new package source nor a new normal dependency and
retains these two attacks verbatim.

## Adjacent runtime falsifier

`tests/publication_recovery.rs::reopen_rejects_mixed_head_and_immutable_facts` must substitute B's
head or immutable bytes under A's receipt/publication identity and assert the exact typed validation
error. This test proves the constructor seal is paired with validation rather than merely hiding a
struct literal.
