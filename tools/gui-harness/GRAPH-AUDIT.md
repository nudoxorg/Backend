# Native graph audit

The graph is rendered by GPUI in every command below. The web prototype supplies a visual reference and fixture; it is not the test renderer.

Build separately, then freeze source and stop compiler work before acceptance measurements:

```sh
.local/devenv/cargo build -p backend-facet --features gallery --release --bin facet-gallery
python3 tools/gui-harness/test_graph_verify.py
python3 tools/gui-harness/graph_verify.py \
  --binary .local/target/release/facet-gallery \
  --mode correctness --out .local/graph-correctness
python3 tools/gui-harness/graph_verify.py \
  --binary .local/target/release/facet-gallery \
  --mode perf --out .local/graph-perf
```

The verifier derives the repository path from its own location and copies the executable into the output directory. Both consecutive runs use that exact copy. `REPORT.json` records executable/tool/fixture hashes, every command, exit status, complete logs, assertion failures, explicit skips, all-frame measurements, and two-run state/pixel comparisons. Correctness commands have a 90-second watchdog; performance commands have a 180-second watchdog. A timeout is a failure and incomplete PNGs never enter the atlas. `--scenes a,b` records a limited scope; it does not claim a full audit. Both full and limited audits require two runs.

`atlas.html` and `ATLAS.json` link real capture images to the exact captured focus, hover, selected identity, camera, exploration, measured card/scroll/prism geometry, motion and frame requests. Cases cover idle input, interrupted navigation, actual pointer picking, native zoom/drag anchors, keyboard selection and follow, same-frame hover jitter, parked pointer weather changes, reduced motion, search, chain, reach and tour. Responsive cases include translated viewports, narrow widths, short heights, larger text and both palettes. Pinned fixtures provide exact semantic assertions; live fixtures supply topology/scale invariants, and absent live targets are explicit skips.

Measured native scrolling can explain intrinsic offscreen text only when its actual viewport remains inside the card and its content extent proves the text reachable. Missing metadata and cross-axis clipping remain failures. Hover must pick an actual symbol without focus; the live relation count must preserve the independently projected fixture neighbourhood while counted routes are visibly submitted.

Draw timing and input dispatch are distinct. Official `perf` measures `Window::draw` without JSON state probes. Detailed reports preserve every draw and report cold, warm, requested and input subsets, including p50/p95/p99, maximum and every over-budget sample. Input timing covers real dispatch, adapters and immediate foreground work; it excludes background readiness waits and draw. Input statistics are per-draw batches plus event count and maximum single dispatch. CPU timing is not displayed cadence or GPU completion. Timing values are excluded from deterministic equality; event counts, coverage, geometry and state are retained.

The Naive and AllPaths controls use the same world/hover input and viewports. They are explicitly comparative alternatives: their budget failures remain recorded without redefining the chosen Batched renderer's release gates. The acceptance runner rejects concurrent compilers. Any diagnostic run performed under native activity or other load must be labelled separately and must not stand in for quiet acceptance.

To capture actual native films with 32ms capture cadence and a 16ms draw loop:

```sh
python3 tools/gui-harness/graph_films.py \
  --binary .local/target/release/facet-gallery --out .local/graph-films
```

The exporter writes and releases each native RGBA frame, preserves exact time/state sidecars, and generates MP4s plus contact sheets with ffmpeg. There is no interpolation. `FILMS.json` records provenance, exact capture times and complete commands/logs; a separate motion report observes the finer draw cadence. Film/probed timing is diagnostic and PNG/encoding work is excluded from draw timing.

A live native window can optionally stream bounded timing evidence without requesting redraws:

```sh
.local/target/release/facet-gallery window --scene graph-world \
  --native-trace .local/graph-live.jsonl
```

This trace reports actual native draw CPU, invalidations, intervals between draws, and dirty-to-draw latency. It does not measure compositor presentation or callbacks that run before the window is marked dirty. Normal quit drains and syncs the writer. Same-process memory replay is provided separately by `graph_memory_soak.py`; it uses streaming observations and actual retained-state boundaries rather than inferring memory behaviour from process restarts.
