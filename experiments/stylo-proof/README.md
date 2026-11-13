# Stylo native API proof

This standalone proof uses `stylo` 0.23.0 with Servo features, selectors 0.42.0,
and cssparser 0.38.0. `Cargo.lock` captures the tested dependency graph. Run with
Rust 1.97.1:

```sh
CARGO_BUILD_JOBS=2 cargo test --manifest-path experiments/stylo-proof/Cargo.toml --locked --offline
```

The tests exercise Stylo's real stylesheet parser, media/selector/declaration
AST, structured custom-property references and fallback, parse diagnostics, and
selector matching on a small explicit synthetic element. A separate cssparser
observer test demonstrates UTF-8 host byte positions and distinct duplicate
raw tokens. It does not establish token-to-Stylo semantic correspondence.

Stylo deduplicates declarations, exposes rule start line/column but not exact
host token endpoints, and reports approximate diagnostic locations. Ordinary
property `WithVariables` does not expose its structured substitution references
through the public 0.23 API. The fixture does not calculate cascade or computed
styles. Those need explicit stylesheet order, origin, layers, DOM, pseudo/state,
media/device and inheritance inputs; source-only product results must not infer
those runtime facts. Imports are disabled in this synthetic parser context.
