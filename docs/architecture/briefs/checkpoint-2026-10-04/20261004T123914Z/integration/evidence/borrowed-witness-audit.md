# Borrowed source-state witness API audit

Read-only audit of frozen candidate `d21562d4bfd5e7648b0aeb6bdd8bab6fc8d5555d` (tree `70a4bcb2cb8ba0624f15ee9295fea23b4234dc27`). The paired-candidate manifest is `/private/tmp/nudox-typed-graph-candidate-20261004T121200Z/candidate.json`; its acceptance is recorded as unrun. No source files, indexes, or worktrees were changed, and no build/check/test command was run.

## Current path and repeated work

`extensions/turso/src/graph.rs:828-930` reads one persisted source's exact typed key, source witness, optional state/reason, and edge-existence bit in one query. It decodes the stored coordinate with `PackageReferenceKind::try_from` plus `PackageReference::from_kind` (`graph.rs:993-1029`) and decodes authority kind/ID (`graph.rs:1313-1334`), then compares the decoded source key to the requested key. For Unknown/Unavailable it admits the stored reason with `decode_exact_product_text` before hashing. The known-empty case hashes a temporary `DependencyFacts::Known(Vec::new().into_boxed_slice())` at `graph.rs:893-900`.

Both branches call `checked_source_witness` (`graph.rs:953-964`), which clones the source key into a one-element `Vec`, calls `CheckedPackageGraphFacts::new`, then copies one digest back out. The gap-state branch additionally clones `ProductText` to make the owned `DependencyFacts` at `graph.rs:918-923`. The constructor allocates its uniqueness set, source-witness vector/Arc and owned facts Arc (`crates/library/package_graph.rs:289-330`) even though this caller has exactly one source and no edges.

The desired v1 digest itself is already streamed without an encoded source/edge vector (`crates/library/package_graph.rs:577-667`). Its domain is `nudox.package-dependency-source-facts.v1\0`. The coordinate reference tag and spelling, authority tag and ID, state tag, and reason or ordered edge-version pairs are fed directly into BLAKE3. For Known-empty, the Known state framing is emitted with zero edge pairs; for Unknown/Unavailable, the same state framing is followed by field 4 containing the borrowed reason bytes.

## Smallest API cutover

Keep this API specific to the three states Turso is actually checking. A general borrowed `DependencyFacts` API would either need to sort arbitrary Known rows (the current unchecked fallback allocates a temporary vector at `package_graph.rs:625-645`) or impose new ordering semantics. The state reader has no reason to hash nonempty Known rows; full edge facts continue through `CheckedPackageGraphFacts` and its existing row/source/authority/target validation.

Suggested public, strongly typed surface in `package_graph.rs`:

```rust
pub enum BorrowedPackageGraphSourceState<'a> {
    KnownEmpty,
    Unknown(&'a ProductText),
    Unavailable(&'a ProductText),
}

pub struct CheckedPackageGraphSourceWitness([u8; 32]);

impl CheckedPackageGraphSourceWitness {
    pub fn admit(
        source: &PackageGraphSourceKey,
        state: BorrowedPackageGraphSourceState<'_>,
    ) -> Result<Self, ProductAdmissionError>;

    pub const fn digest(&self) -> &[u8; 32];
}
```

Keep the newtype field private and export only the enum and checked constructor/result needed by Turso. Implement one shared private encoder view, such as `SourceWitnessState<'a> { Known(&'a [PackageDependencyRecord]), Unknown(&'a ProductText), Unavailable(&'a ProductText) }`. Adapt the existing owned-facts witness wrappers to this view, then map `KnownEmpty` to `Known(&[])` and pass Unknown/Unavailable reasons by reference. The public `admit` path validates its inputs, calls that same v1 encoder, and returns the checked digest wrapper. This shares the existing grammar; it must not duplicate the preimage sequence or make the current unchecked private hash helper public.

At the Turso call site, replace the owned generic enum with `BorrowedPackageGraphSourceState::KnownEmpty` or `Unknown(&reason)` / `Unavailable(&reason)`. Compare `witness` to `*checked.digest()`. `source` and `reason` stay borrowed throughout; there is no source/reason clone, one-source `Vec`, `BTreeSet`, facts `Arc`, or source-witness `Vec`. The BLAKE3 hasher and the returned 32-byte value are stack data. No database schema, cursor recipe, or witness version changes.

## Admission constraints

The fast API cannot stand in for general graph admission. It has no row variant, so it cannot certify edge hashes, source/source-authority correspondence, authority/evidence compatibility, duplicate edge IDs, row bounds, or `PackageDependencyTarget::admit`. Those remain on `CheckedPackageGraphFacts::new` (`package_graph.rs:294-317`) and `PackageDependencyTarget::admit` (`package_graph.rs:761-796`); Turso's edge decoder also explicitly rechecks `target.admit` and source/evidence compatibility (`extensions/turso/src/graph.rs:1336-1419`). If the API ever grows to accept Known rows, it must run those same checks and preserve canonical row sorting before producing a checked witness.

A small allocation-free source/reason shape check is still warranted before returning a *checked* witness. The public fields and constructors expose two non-admitted escape hatches: `PackageUrl::ordering_floor()` is explicitly an empty PURL ordering sentinel (`crates/semantic/src/vocabulary/package.rs:145-180`), and `ProductText::from_static` relies on `debug_assert!` for its shape (`crates/library/surface.rs:53-71`). `PackageGraphSourceKey` and `PackageReference` have public enum/struct construction paths. Validate the source coordinate and reason in place: reject the empty PURL floor; for local text/reason require the same nonempty, bounded, NUL-free, already-trimmed shape `ProductText::new` would retain. No allocation or reparse is needed. Authority is a closed typed enum with fixed-size IDs; there is no untyped authority parameter to the API. On the Turso path, stored authority tags and IDs are independently checked by `decode_source_authority` (`graph.rs:1313-1334`), including the zero ID rule for `Unattributed`.

Known-empty and gap states carry no rows, so there is no target/evidence validation to perform for that specialized API. The persisted state reader also validates state/reason pairing, rejects Unknown/Unavailable alongside an edge, and validates the source row against the exact typed primary key before invoking the helper (`graph.rs:859-929`). Preserve those checks and the current Known-empty/`has_edge` branch logic exactly.

## Validation plan for the eventual change

1. Before replacing the current wrapper, freeze v1 source-witness goldens from `d21562d` for one deterministic typed source and each state: Known-empty, Unknown(reason), and Unavailable(the same reason). Add an assertion that the checked borrowed API matches those independent byte constants. Also compare its output with the existing one-source `CheckedPackageGraphFacts` path for the same valid inputs; treat that as a secondary parity check because the encoder should be shared after refactoring.
2. Test digest distinctions across Known-empty/Unknown/Unavailable, changed reason, coordinate kind with equal display spelling, and authority kind/ID. Include a valid Local coordinate whose text starts `pkg:` to ensure it is not prefix-reinterpreted.
3. Test rejection of `PackageUrl::ordering_floor()`, a release-bypass Local `ProductText::from_static(" padded ")`, and a malformed borrowed reason. Ensure Known rows cannot be passed to this API by construction. Keep the existing malformed row target, evidence, source, and facts-version admission tests on the full checked path.
4. Add/extend Turso tests for Unknown, Unavailable, Known-empty, state-with-edge, reason-without-state, wrong stored typed key, malformed coordinate kind, and invalid authority kind/ID. Verify returned source state and refusal behavior remain unchanged.
5. Validate allocation reduction with a warmed, isolated counter-allocator harness around the new call and the old `checked_source_witness` baseline. Count allocations and cloned bytes for Known-empty and reason-bearing states separately; expect zero allocations in the new API itself. Keep DB row decoding/query allocations outside the measurement window. Do not use elapsed time alone as proof.
6. Run the normal paired library/Turso gates when available. This design requires no schema or cursor protocol changes.

