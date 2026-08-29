# Progressive swarm stages

## Stage 1: scout

Read only the common craft skill, the domain skill, the capability's direct code/tests, and its
architecture contract. Establish the call graph and baseline before following supplementary links.
This prevents novelty-driven design.

## Stage 2: discriminate

Load a supplemental source only for a named choice:

- allocation/ownership: compare the actual lifetime against borrow, caller scratch, inline, arena,
  slab, mmap/lease, exact `Vec`, thin/reference-counted region, and persistent structures;
- static dispatch: require two shipping interpreters and inspect cross-crate optimized output;
- SIMD: require a profiled contiguous kernel and compare scalar/crossover/tail/error behavior;
- concurrency: model the production transition and cite its progress/ordering/reclamation law;
- observability: prove disabled laziness, bounded export, correlation, shutdown, and portable-client
  dependency exclusion.

Write the decision table before implementation. Reject any option whose benefit is only novelty.

## Stage 3: build

Give the builder the accepted decision, exact paths, budgets, and falsifiers. Do not ask it to repeat
the architectural search. Stop after the smallest public vertical proof.

## Stage 4: break

Give the breaker the frozen contract, baseline, resulting source, and raw commands. Omit the builder's
defense. Require concrete counterexamples and forbid silent fixes.

## Stage 5: learn

Separate task facts from reusable process failures. A generalized skill change must prevent a
plausible future worker from repeating the same failure without forcing one past solution onto a new
shape. Forward-test the revised instruction with raw artifacts.
