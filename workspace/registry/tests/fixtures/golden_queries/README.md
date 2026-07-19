# Golden query fixtures (nDCG ranking gate)

These JSON files feed `tests/ranking_ndcg.rs`. Each file is a
`Vec<GoldenQuery>` (see `registry::search::eval`) for one ecosystem. CI requires
`mean_ndcg(k=10) >= 0.95` for primary ecosystems (`rust`, `typescript`,
`python`) and currently for `go` / `java` as well.

## Query classes

| Class | Shape | Intent | What to exercise |
| --- | --- | --- | --- |
| **Navigate** | Single token / package id (`serde`, `lodash`, `@scope/pkg`) | Exact-name lookup | Exact-name bonus beats keyword spam; related packages gain 1–2 |
| **Explore** | Multi-word prose (`http client`, `yaml parser`, `web framework`) | Topic browse | Quality + popularity outrank land-grab spam names |

Author at least a few of each class per ecosystem so the gate is not only
navigate-path noise.

## Gain scale (graded relevance)

| Gain | Meaning |
| --- | --- |
| **3** | Canonical / blessed answer for this query |
| **2** | Strong alternative users would accept |
| **1** | Weakly related / satellite package |
| **0** | Irrelevant or adversarial distractor (spam, land-grab) |

Unjudged pool entries contribute **0** gain in nDCG (they still affect ranking
order via their synthetic signals).

## Pool signals

Each `pool` entry needs synthetic retrieval signals the pure ranking pipeline
replays:

- `bm25` — raw text score (spam often high)
- `quality` — `0.0..=1.0` (spam typically `≤ 0.08`)
- `downloads` / `dependents` — optional popularity
- `keywords` — diversity pass
- `withdrawn` — graded demotion when true

Make the pool **winnable**: the ranking pipeline must be able to put gain-3
packages above gain-0 spam with realistic signals. If a fixture is unwinnable,
the nDCG gate fails forever.

## Adversarial / authoring rules

1. **Spam never gets gain 3.** Names containing `spam` or `landgrab` must not
   be judged gain ≥ 3 (`fixture_lint_spam_never_gets_gain_three`).
2. Every judged name **must appear in the pool**
   (`fixture_lint_all_judged_names_in_pool`).
3. Explore queries (`http client`, `yaml parser`, `web framework`) must include
   at least one gain-3 blessed package and one gain-0 distractor.
4. Prefer corpus-like package names (real crates/pypi/npm) with synthetic but
   plausible download/dependent scales — not random strings.
5. Do not “fix” a failing gate by raising spam quality or lowering the
   blessed package’s quality; fix ranking or the signal geometry.

## Adding a query

1. Pick ecosystem JSON (or add a new file + register it in `ranking_ndcg.rs`).
2. Add a `GoldenQuery` object with `query`, `ecosystem`, `judgments`, `pool`.
3. Run:

   ```bash
   cargo test -p registry --test ranking_ndcg -- --nocapture
   ```

4. Confirm per-query nDCG and the ecosystem mean stay ≥ 0.95.

## Multi-ecosystem note

Fixtures are **synthetic but corpus-like**: they mirror how real packages would
look after retrieval (BM25 + facets), not live index dumps. Cross-ecosystem
parity (same explore topic in python + typescript) is encouraged so ranking
regressions surface in more than one gate.
