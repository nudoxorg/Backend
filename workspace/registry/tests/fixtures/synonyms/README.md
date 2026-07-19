# Synonym CSV fixtures (`tag-synonyms.csv`)

Runtime loads `tag-synonyms.csv` from a data directory via
`registry::metadata::Synonyms::new`. Tests write temporary CSVs with the same
layout.

## Formats

### Global (3 columns)

```csv
# term,canonical,votes
http-client,reqwest,4
yaml,serde-yaml,3
```

### Ecosystem-scoped (4 columns)

When the first column is a known `Language` token (or alias: `py`, `ts`,
`golang`, …), the row is ecosystem-specific:

```csv
# ecosystem,term,canonical,votes
python,http-client,httpx,5
typescript,http-client,axios,4
rust,web-framework,axum,4
```

Lookup order in `normalize_for(Some(eco), term, min_votes)`:

1. Eco-specific map for `eco`
2. Global map

`normalize(term, min_votes)` always uses the global map only.

## Votes

Integer `1..=5` (higher = stronger consensus). Runtime expansion typically uses
`min_votes = 3`. Scores above 5 log an error but still load.

## Comments and flexibility

- Lines starting with `#` are comments
- Columns are trimmed
- Flexible column counts; non-language first column + 4 fields still parses as
  global using columns 0–2
