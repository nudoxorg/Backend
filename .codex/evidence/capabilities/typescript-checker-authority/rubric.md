# Corpus fidelity rubric (v1) — typescript-checker-authority

Scored per package, 0–10, iterated until every corpus package scores >= 9
with zero blocker rows. Any single blocker caps the verdict at FAILED
regardless of other scores.

## Blockers (any one => FAILED)

- B1: a decode panic, hang, or unbounded child on the package's entry source.
- B2: a fragment that fails `FragmentView::validate`.
- B3: a reference occurrence attributing a use-site to a wrong same-file
  declaration or wrong foreign module (spot-checked against source truth).
- B4: a computed type row contradicting the checker's own printed type for
  the same declaration (deep-review comparison on the sampled entities).
- B5: an emitted fact whose name/owner is not a source slice of the package
  (borrowed-report-bytes class).

## Scored dimensions (0/1/2 each)

1. **Declaration coverage**: all exported declarations present as facts;
   missing exported declaration => 0; all present incl. nested namespaces
   and re-exports => 2.
2. **Type fidelity**: declared types carry exact lattice records (union/
   intersection/tuple/generic application/literal/mapped/conditional/
   template/this/self-nominal); each honest `TypeReason` must be justified
   against source truth in the review note; unexplained fallback (`other`
   text) per entity loses one point each, capped at 2 lost.
3. **Checker computed fidelity**: injected/package-run computed cells match
   the checker's printed types for sampled entities (>= 10 samples or all
   exports if fewer); mismatches => 0–1.
4. **Reference resolution**: oracle-confidence call sites name exact
   overload targets; foreign origins module-qualified (node_modules scope
   and `typescript` lib distinguished).
5. **Docs & extensions**: JSDoc fragments retained with link targets;
   TypeScript extension facts (type parameters, declared/computed cells)
   present for every generic and typed declaration.
6. **Render truth**: Signature/Type/Docs displays for sampled entities match
   a hand-written expected rendering (2 samples minimum per package).

## Verdicts

- 9–10, no blocker: CLEAN.
- 7–8, no blocker: CLEAN-WITH-NOTES (notes become repair-card candidates).
- < 7 or any blocker: FAILED => one Luna repair card per defect class, then
  re-scored. Two failures of the same defect class => back to decomposition.
