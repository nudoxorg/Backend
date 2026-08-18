#!/usr/bin/env nu

# The product semantic smoke tests run only with the Nix-pinned model and ONNX
# runtime. Keeping this tier separate prevents ordinary workspace tests from
# silently acquiring a 641 MB model or a large inference dependency graph.
^cargo generate-lockfile
^env RUSTC_BOOTSTRAP=1 cargo nextest run --locked -p nudox-engine \
  --features fixtures,onnx --test engine_semantic_real_model --test-threads 1
