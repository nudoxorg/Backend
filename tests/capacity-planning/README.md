# Capacity-planning benchmark

This standalone custom-harness benchmark exercises the public compiler,
publication, index, Tantivy, vector, and store paths. Its source and helper
modules were moved intact out of `backend-store` so ordinary store tests do not
build benchmark-only engine and semantic dependencies.

Run it with:

```sh
cargo bench -p backend-capacity-planning --bench capacity-planning -- --mode warm --samples 5 --warmups 1
```

The harness writes raw stage samples as `result.json`; its terminal summary
reports arithmetic means. It does not use Criterion or bootstrap statistics.
Cold-process measurements require one sample per invocation. See `--help` for
the full option list.
