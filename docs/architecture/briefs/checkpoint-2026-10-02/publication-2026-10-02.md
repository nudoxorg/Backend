# Publication checkpoint — 2 October 2026

After the stopping checkpoint, the user explicitly requested publication to canonical on `dev.nudox.org`. The target is `origin/canonical` in `ssh://forgejo@dev.nudox.org:2222/Nudox/Backend.git`.

The composed continuation candidate and preserved primary checkpoint are combined in source merge `5799fdd4c941b3cb6bf430f3432a111b76b32d61`. The capture-harness conflict was resolved by retaining both the candidate's text-size/motion fields and the primary's exact after-paint frame ledger. Rustfmt parsing of that file and diff hygiene passed; no Cargo build or native capture was run. This source merge descends from the fetched canonical head `f9c158af0dfc229c4567bf5cd725c5c0828043fa`.

The publication includes the combined checkpoint on canonical and the saved work on seventeen explicit source branches. Late isolated patches remain on their separate branches; they are not silently integrated into canonical. Full branch identities at preparation are recorded in [the publication inventory](publication-2026-10-02.json). The candidate branch also gains this documentation commit before publication.

The canonical Downloads checkout's external `.config/nix/tools.nix` edit is preserved separately, SHA-256 `fd113ac31e641595fe98d876133b62330a1a2cc9fbcafdba6e88890b9b07cc72`. It is not included in this publication. That checkout can remain behind the remote canonical head to avoid overwriting its local edit. Continue from the clean candidate branch until the local edit is deliberately reconciled.

This is Git source publication, not an application deployment or production-readiness certification. The goal remains paused. The composed source still needs compilation, full suites, actual native journeys, joined live ingest/restart/restore, and the broader index/compiler/history gates documented in [the exhaustive stopping brief](README.md). Remote publication success must be verified by comparing the remote canonical and saved branch heads with the pushed objects; do not infer it from this document alone.
