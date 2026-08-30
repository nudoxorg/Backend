# Interrupted Luna symbol-level salvage ledger

Worker `/root/canonical_byte_local_closure_terra/luna_vertical_2` was interrupted without a commit after its final visible-write deadline. Its partial diff is retained unstaged in the shared checkout only for the rows below; it is not an accepted checkpoint.

The manager reproduced `cargo test -p nudox-root --locked --offline`. It fails before tests with `E0446`: `RootWireRecord` derives `KnownLayout` and `TryFromBytes` while its `parent_present` field names private `ParentWire`; rustc rejects the leaked private type. The resulting dead-code warnings show that the new closure-scratch and row-index helpers have not yet become behavior.

| Symbol / mechanism | Previous owner / proposed destination | Useful proof to retain | Replayed falsifier / acceptance condition | Disposition |
| --- | --- | --- | --- | --- |
| `RootWireRecord` nesting `ObjectDescriptorWireRecord` | root canonical grammar -> root borrowed-view validator | Reuses the object-owned typed descriptor record, preserves the fixed 63-byte root wire layout, and makes the nested schema type-cast available at ingress | Existing canonical writer and ID tests must retain byte-for-byte output; a descriptor-field duplication or changed wire width fails the layout/golden checks | retain and repair |
| `ParentWire` closed `repr(u8)` tag | root canonical grammar -> root borrowed-view validator | Typed `0/1` parent presence avoids treating an arbitrary byte as a boolean/sentinel | An invalid parent tag must reject at typed cast and trigger only an error-path diagnostic rescan; `ParentWire` must be sibling-visible to `root_view` and zerocopy derives | retain and repair (`pub(crate)` visibility only) |
| `FixedCanonicalRecord` rename to `RootWireRecord` | root writer -> same root grammar owner | Writer/hash use exactly the same nested record as borrowed ingress | The root gate named above currently kills the incomplete rename; after visibility repair writer/hash compatibility must pass | retain and repair |
| `RowIndex::from_validated_borrowed_root_position` | packed coordinate owner -> private borrowed-row projection | Retains one named provenance bridge from validated wire ordinal; avoids raw coordinate construction in sibling code | Root-view selection must use it; if no direct use exists after root checkpoint, remove it rather than retaining preparation API | conditional retain |
| `ClosureScratch` visibility/helpers | closure owner -> only a directly consuming borrowed selection | Would allow the borrowed root to reuse existing caller-owned marking/selection scratch rather than allocate a second selection lane | Borrowed root selection must call these exact helpers and preserve existing scratch-capacity/work tests; otherwise delete all added helper surface | conditional retain |

No code is accepted from this worker until the new root-only card proves the retained rows, runs the focused root gate, and commits a coherent checkpoint. No row authorizes an object, hydration, operation, manifest, chief, or prerequisite-control edit.

## Terminal disposition

The stale partial writer was never committed as-is. `ParentWire` received only sibling visibility;
the nested `ObjectDescriptorWireRecord`, `RootWireRecord` rename, `RowIndex` provenance bridge, and
`ClosureScratch` helpers were retained because the final borrowed validator/selection consumes
them. The private borrowed root module, exact transient parent lane, authority projection, and
caller-scratch selection passed the owning gates. No native row arena, descriptor copy, second
parent owner, magic depth sentinel, unsafe cast, or zero-allocation quadratic walk was salvaged.

The legacy read-only reference
`/Users/mileswirht/Downloads/backend/workspace2/crates/nudox-root/src/root_view.rs` was consulted as
evidence only; its SHA-256 was
`37e4c839dfb3f44974a66beb6d5459da5a57e22520b9cb67b959e32380e572e9`. The current implementation
was rederived against the branch's `ParentWire`,
`ContentAuthority`, locality, and error contracts. No file under that saved checkout was modified.
