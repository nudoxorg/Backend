# C1 builder detached control reproduction

The detached control at `/private/tmp/p5-c1-builder-control` is outside the repository and uses the
frozen baseline `nudox-ir-format` validator plus the unchanged typed IDs.

```text
command: cargo test --manifest-path /private/tmp/p5-c1-builder-control/Cargo.toml
result: 3 tests passed
writer source SHA-256: b9e1c1d611546a126b25655f9f798dadc1e77734e8aab6833dda53d43f47a8d3
test source SHA-256: 0fc7ab2e9fa04b692b2fefd208708e27f0fa3c206a5bfa779a7c89d8978ec71f
writer/test LOC: 77 / 53
```

It proves the selected direct-write mechanism can produce the existing 2/1 golden, validate through
the existing validator, preserve a suffix, reject every shorter output with untouched sentinels, and
report lane count errors. It does not prove private construction, actual-rlib type/lifetime failures,
returned-view authority, source mutation strength, or comparable release codegen; those remain card
requirements.
