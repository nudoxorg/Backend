# Padding-free C1-FORMAT detached control

The padding-free control was compiled outside the repository at
`/private/tmp/p5-c1-format-v6`; it is a calibration specimen, not production source.

```text
wire header: 23 bytes
golden: 35 bytes, entity coordinates [7, 11], type coordinate [3]
command: cargo test --manifest-path /private/tmp/p5-c1-format-v6/Cargo.toml
result: 3 integration tests passed; doc/unit tests passed
validator SHA-256: 1f04163d2330586e5a268892b6afeceeec2ec9243b452d124d03022bfadda8f6
test SHA-256: cc5ad5cffce379d11d6c5ba8e613a7d4cffe727ad4f271a23169adc7c11740fc
```

The executed tests cover the golden borrowed-lane path, every truncation prefix, entity-count-three
priority (`Type Start expected 35 observed 31`), and late-type-start priority (`Type End expected 39
available 35`). It does not yet supply the required actual-rlib negative API fixture or the complete
repository mutation deck, so it grants no implementation authority.
