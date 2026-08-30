# P5 C0 pre-edit release control

## Specimen

- Card digest: `4ec5cb024e3ddc84d07e6dd318d7d0e29d15986694a6786d9ab94c31f1662332`.
- Candidate baseline: `44c22154fd5238e4769562590420371979306050` plus card commits through
  `73799fad8e327cd186a9c75dfbb2fb394a4e3690`.
- Retained executable control: `planes/compiler/target/release/deps/dispatch-ebe57be877ebbbb9`.
- Command: `RUSTC_WRAPPER= cargo test --locked --release --no-run --manifest-path planes/compiler/Cargo.toml -p nudox-compile-registry --test dispatch`.
- LLVM command: `RUSTC_WRAPPER= cargo rustc --locked --manifest-path planes/compiler/Cargo.toml -p nudox-compile-registry --test dispatch --release -- --emit=llvm-ir`.

## Raw host and text observation

```text
rustc 1.97.1 (8bab26f4f 2026-07-14)
host: aarch64-apple-darwin
LLVM version: 22.1.6

/usr/bin/size -m dispatch-ebe57be877ebbbb9
Segment __TEXT: 655360
    Section __text: 494508
total 4295917568

LLVM artifact: planes/compiler/target/release/deps/dispatch-2b8c6cf37f396d83.ll
```

The `.d` and `.ll` side artifacts are intentionally not passed to `size`; macOS `size` reports them as
non-object files. This executable is the retained manual-control artifact for the post-edit named
consumer comparison. It does not by itself prove source forwarding; that proof remains the public
pointer-and-length journey and its input-removal mutant.
