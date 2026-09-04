# Card L6b-r3: finish R12 — forward nominal survives publish->reopen->render

registered role: nudox_luna_implementer (expected `luna`/max; L6b session,
card 3). This is ONLY the two skipped card items — small and bounded.
baseline: d75e2936 (your L6b-r2 result).

MANDATORY environment (unchanged):
  export PATH=/opt/homebrew/bin:$PATH
  export NODE_PATH=/Users/mileswirht/Downloads/backend/node_modules
  export CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-typescript/.local/target

## Required behavior (matrix row R12 legs 3-4, from the L6b addendum)

Extend `forward_nominal_checker_and_lowering_keep_the_later_class` in
compiler/driver/tests/typescript_lower.rs:

- Leg 3: the same forward-nominal source (`const a = new B(); class B {}`
  exported shape) goes through `compile` -> `publish_compiled` into a fresh
  `DurablePublisher` -> `DurablePublisher::reopen` -> `open_published`, and
  the REOPENED fragment's decoded computed fact for `a` still carries the
  nominal tag naming `B` (mirror the inline choreography of
  typescript_purl_lifecycle.rs; ALL buffers heap `vec![]`/`Box` — the test
  must pass inside `std::thread::Builder::new().stack_size(2 * 1024 * 1024)`,
  which you assert by actually running the body in such a thread).
- Leg 4: in typescript_render.rs, one render-truth assertion for the
  forward-nominal source: the display for `a` names `B` (the canonical
  spelling of the class), whatever form the canonical lattice uses
  (`struct B`-style), pinned as an exact golden.

owned paths: compiler/driver/tests/typescript_lower.rs,
compiler/driver/tests/typescript_render.rs. Everything else forbidden.

## Exact focused commands (lane-local CARGO_TARGET_DIR mandatory)

- cargo test -p compiler-driver --test typescript_lower
- cargo test -p compiler-driver --test typescript_render   (14 + new = 15)
- cargo check -p compiler-driver --lib

## Checkpoint protocol

One commit, prefix `test(typescript):`. Return: commit sha, the two new
assertions verbatim, focused outputs, smallest remaining red row, card
digest verified.
