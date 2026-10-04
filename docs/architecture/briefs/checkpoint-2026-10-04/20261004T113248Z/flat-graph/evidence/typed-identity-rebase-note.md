# Package-reference spelling collision and page-selection audit

Date: 2026-10-04  
Scope: source analysis for the private flat-index experiment at `/private/tmp/nudox-flat-package-graph-20261004`, based on immutable baseline `9a896bf197aed9028e9dbf14f0532eb00daad077`. This note does not implement the separate typed-identity cutover.

## Reproducible collision

`PackageReference::parse("pkg:cargo/collision@1.0.0")` creates `Purl(PackageCoordinate)`. The public enum also permits `PackageReference::Local(ProductText::new("pkg:cargo/collision@1.0.0")?)`, so two distinct typed references can have the same `as_str()`. `PackageReference` derives equality and ordering over its enum variant, so the values are not equal even though their spellings match.

In this baseline, `CheckedPackageGraphFacts::new` sorts source facts by coordinate spelling, authority kind tag, and authority bytes. It does not use the reference variant to break a tie. The production `PackageGraphIndex` then has two different forward identities: `by_source` is keyed by exact `PackageGraphSourceKey`, while `by_coordinate` is keyed only by `source.coordinate.as_str()`. Consequently, exact-source lookup distinguishes the Purl and Local entries, but coordinate-only lookup groups them. The private flat candidate preserves that behavior in `flat_and_map_preserve_spelling_grouping_and_exact_package_variant_lookup`; this is intentionally a baseline behavior fixture, not the desired typed-identity policy.

The source-facts witness encoder explicitly emits reference tags `1` for Purl and `2` for Local, so those two source witnesses differ. `CheckedPackageGraphFacts::source_witnesses()` remains positionally aligned with the canonical fact vector. The complete graph witness hashes the source-witness set after sorting the digests, so reordering just these tied inputs should leave the complete witness unchanged. The fact vector and its aligned per-source witness array may still retain input-order influence while the sort comparator treats the keys as equal. The added witness probe checks exact key-to-digest alignment and complete-witness equality without asserting which tied entry appears first.

## Page selector mismatch

`select_checked_source` first takes the range whose coordinate *spelling* matches the request. Its cursor branch compares the full `PackageGraphSourceKey`, including `PackageReference` variant. Its authority branch, however, selects the first source in the spelling range whose authority matches; it does not also require `source.coordinate == request.package`. Without an authority selector, it returns every source in that spelling range as ambiguous.

The page contract subsequently compares source coordinates using enum equality. An authority-selected Local key can therefore be selected for a Purl request with the same spelling, then rejected by page admission. An unqualified ambiguous response containing both variants also fails the page-shape check that requires every ambiguous source coordinate to equal the request package. Cursor request admission already uses exact coordinate equality, while the current query recipe hashes coordinate spelling and selected source spelling/authority without a reference-kind tag. This is a real identity-policy inconsistency across selection, page admission, and cursor identity; the performance patch leaves it untouched.

## Probe for the typed-identity follow-up

Use one Purl and one Local reference with the same spelling and the same `Unattributed` authority. Give each a known-empty state (the reference-kind hash tag alone should distinguish their source witnesses). Build `CheckedPackageGraphFacts` twice, once with Purl first and once with Local first. For both snapshots, pair each `facts()[i].0` variant with `source_witnesses()[i]` and independently recompute the source witness. Record the ordered variant tags, both typed-key/witness pairs, and complete witness. After the canonical-order cutover, require the same Purl-before-Local order for both input permutations, preserve the tag-distinct per-source digests, and preserve the complete witness. Also probe a Purl lookup and a Local lookup separately, same-authority and multi-authority variants, an authority-selected forward page, an ambiguous page, and a cursor round-trip; compare page admission and recipe identity as well as the selected rows.

The planned typed cutover is a separate correctness change: add the reference-kind tag to canonical source order, use exact typed coordinate equality for forward lookup and page selection, and bump the page recipe/domain version so cursor identity binds the variant. When the flat experiment is rebased on that change, update its collision oracle from spelling-grouped ambiguity to typed-coordinate lookup. Do not silently fold any of those behavioral changes into the memory-layout comparison.

## Benchmark interpretation

The current flat candidate and its paired tests intentionally match the old map baseline, including spelling-grouped coordinate ambiguity. Its output must be treated as baseline-only until rebased on the typed-identity correction and revalidated. No Cargo tests or benchmark runs were performed while preparing this note.
