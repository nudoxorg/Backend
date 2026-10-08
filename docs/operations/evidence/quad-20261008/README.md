# Quad-language checkpoint evidence, 2026-10-08

These packets bind narrow native/build/phase-experiment results to exact sources and raw output. They do not certify production readiness, installed real-application success, or all-language performance. See the current acceptance ledger in `docs/architecture/briefs/production-readiness.md`.

- SDK8809 Linux: fresh release trio, full source-input manifest, actual Cargo events/observations and retired owned build. The original warnings-report incorrectly says zero; raw stderr is authoritative. Root independently rehashed the three remote images.
- SDK8809 macOS: fresh engine compilation and72 passed/one ignored host/TypeScript/classifier controls, raw logs/source inputs/graph return. Root independently rehashed the immutable image. The audit script's checkout/archive paths reflect the original Root workspace; relocate those inputs explicitly to rerun it.
- Lexical admission:103 passed/one ignored native suite plus six paired isolated allocation runs. Baseline differs only by restoring delta.rs from8809; unchanged tests/lock. This is relation-admission allocation/time evidence, not query latency/RSS or isolated posting-validation speed.

`artifacts.json` records exact packet bytes. No test images are embedded here. Original failed attempts remain in owned audit directories and the ledger; these compact successful packets do not erase them.
