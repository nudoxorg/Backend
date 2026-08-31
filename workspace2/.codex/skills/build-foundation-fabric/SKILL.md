---
name: build-foundation-fabric
description: Scope rules for workspace2 typed identities, canonical binary records, borrowed views, and packed/range-addressable artifacts. Use with deliver-reviewed-rust-slice for nudox-id, schema, frame, view, or an approved protocol/pack crate.
---

# Foundation fabric scope

Read `../deliver-reviewed-rust-slice/SKILL.md` completely first. It is authoritative for Rust idioms,
review checkpoints, ownership, protocols, SIMD, unsafe, diagnostics, errors, and tests. This skill adds
only foundation scope.

## Routing and ownership

Own `nudox-id`, `nudox-schema`, `nudox-frame`, `nudox-view`, and a parent-approved protocol/pack
crate. Ask before changing object, root, store, runtime, adapters, or manifests. Read
`PROTOCOL_TOOLING.md` only for a parser/record choice and `PACKED_COLLECTIONS.md` only for a packed
collection/range task.

## Required packet additions

- exact record diagram: endian cells, derived widths/offsets, canonical order, version owner;
- semantic versus physical identity preimages;
- borrowed-view lifetime/mutation model and caller-output preflight;
- hostile-input error priority and scalar work per record/range;
- next-version rejection/compatibility story;
- why zerocopy, BLAKE3, slices, or a parser crate does/does not own each edge case.

## Foundation invariants

- Foundation owns all domain/encoding tags; applications never hash string literals.
- Serialized typed identities carry enough closed authority to reject same-byte cross-domain or
  cross-encoding decode. Registry codes are assigned once and checked for uniqueness at compile time;
  no runtime map owns the protocol. `TryFrom` validates representation bytes, while a deliberate
  digest projection has a named constructor—`From<[u8; N]>` never rewrites caller bytes.
- Reserving identity cells changes collision budget and routing geometry. Record the permanent
  security tradeoff and prove routing/partition words consume digest entropy rather than fixed
  authority/version prefixes.
- One typed record declaration owns fixed wire geometry.
- Complete physical identity is derived after the byte stream is final and lives in its receipt or
  content-addressed name; it is never a field inside the same all-byte preimage.
- A complete validated view borrows; ownership is an outer adapter.
- Sparse packs do not require absent root objects. Typed consumers join root descriptors to present
  objects without re-identifying either.
- Public format tests live under top-level `tests/` and include golden bytes, every truncation and
  structural-cell mutation, N-1/N/N+1 immutable output, range/order/duplicate faults, exact errors,
  pointer identity, allocation/copy evidence, and explicit next-version behavior.
- Negative API laws use external compile-fail evidence: runtime rejection cannot prove that an
  unchecked constructor or cross-authority conversion is absent.

## Closure

Return the shared handoff plus record diagram, error matrix, identity/trust boundary, range evidence,
and remaining protocol decision. Never claim transport, store, or distributed completion.
