# P1 C1a12B2 targeted behavioral proof repair

Base: isolated repair branch `codex/prototype-canonical-root-hydration-c1a-repair` at
`afa5751c`. Independent C1a12B review passed formatting, all-target clippy, the existing test,
and compile fail, but blocked exactly seven proof gaps. This card is limited to those gaps; it
does not rewrite tests, change production code, or start the C1a12C allocator lab.

Allowed paths remain exactly the C1a12B root test and two UI source/stderr pairs. Preserve its
strict all-target clippy cleanliness.

1. Assert byte-for-byte canonical output equality and equivalent validated borrowed facts for
   two arbitrary-order input permutations, not identity equality alone.
2. Add a small collision table proving the complete global phase ordering: descriptor before
   order; order before all parent phases; an early missing parent cannot beat a later invalid
   presence marker; and a later absent-parent key cannot be masked by a subsequent missing
   parent. Assert checked header/count and overflow/geometry behavior without enormous
   allocation.
3. Assert root `bytes` pointer and extent lie within the supplied canonical byte buffer—not only
   slice-value equality.
4. Add a valid branch which reaches an already destructively memoized tail and an invalid cycle
   reached by an incoming branch, alongside the existing direct self-cycle.
5. Assert exact ordered `get`/full-scan generation entries, locality-only identity stability
   across different valid localities, and every public `BorrowedGenerationView::new` mismatch
   rejection relevant to its `generation` and `root_count` facts.
6. Retain public semantic success proof for zero/one/100k shallow/deep and test checked
   header/count geometry/sentinel theorem. Allocation/drop claims are intentionally C1a12C lab
   evidence and must not be faked here.
7. Make the UI fixture's expected stderr specifically contain private-field inaccessible errors
   for `ValidatedRoot` and `BorrowedGenerationView` literal attempts. If compiler recovery masks
   one behind reassignment errors, split literal attempts into separately compiling-fail fixture
   statements/files within the same allowed fixture contract; preserve root and view escapes.

Run formatting, root all-target clippy, exact integration test, and hydration compile fail; commit
the isolated proof result. Fresh independent Terra review is mandatory before C1a12C.
