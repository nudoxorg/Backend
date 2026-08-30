# Wave B4 immutable-index Phase 0 closure receipt

## Status

Status: `EVIDENCE_BLOCKED`.

Phase 0 is not a production candidate. The final read-only calibration is incomplete (one final
cold reader was interrupted), and the required registered source-isolated Terra reviewer could not
be started or returned by the separate sidecar after three bounded dispatch variants plus the
requested resume continuation. No matrix row is proved; B4-01 through B4-13 remain red.

## Exact planned Nix commands

```text
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" fmt --manifest-path workspace2/planes/index/Cargo.toml --all -- --check'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" test --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --all-targets --no-fail-fast'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" test --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --doc --no-fail-fast'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" clippy --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --all-targets -- -D warnings'
nix develop ./workspace2 --command zsh -lc 'RUSTC="$NUDOX_STABLE_TOOLCHAIN/bin/rustc" RUSTC_WRAPPER= "$NUDOX_STABLE_TOOLCHAIN/bin/cargo" doc --manifest-path workspace2/planes/index/Cargo.toml --locked --offline --workspace --no-deps'
```

These are read-only checks for existing I0 vocabulary. Future B4 code needs focused commands in a
frozen worker card and cannot cite them as segment/query proof.

## Toolchain failure and recovery receipt

Ambient `cargo` under `nix develop` is inadmissible: Sol's raw checkout receipt observed a nightly
`rustc` on `PATH`, `NUDOX_STABLE_TOOLCHAIN=1.97.1`, a shared `RUSTC_WRAPPER` sccache setting, and
`E0514` even with a fresh target. The manager's bounded ambient retry emitted only Nix's initial
`fetching git input` line before the 30-second command window expired, so it is recorded as an
inconclusive alternate attempt and not as a claimed reproduction. The commands above explicitly pick
the pinned stable `cargo`/`rustc` and clear the wrapper; their raw successful receipt is required
before Phase 0 closes. `workspace2/tools/quality.sh` remains an equivalent full-workspace fallback
because it registers pinned rustup toolchains.

## Raw checks and reviewer custody

The raw calibration and sidecar transcripts are in `packets/`. Final reader A and the final
misreader retained read-only receipts; final reader B was interrupted without a final receipt. The
registered reviewer has no valid runtime task receipt, no effective-sandbox event, and no findings:
this is a failure to satisfy review custody, not a clear review. All four sidecar attempts kept the
sole build root disposable and the source snapshot separate; the aggregate source digest stayed
`13e7bf55ad0ca948e2cf5867f72192d03f58b0c2ac675bc2edc7221477cfd03e` before and after.

| Attempt | Distinct bounded diagnostic | Raw result | Disposition |
| --- | --- | --- | --- |
| 1 | Luna-low parent, copied registered config, no writable roots/TMP/`/tmp` grants | sidecar `01a0514e-3d25-7201-836c-33473be48957`; `Unknown model gpt-5.6-luna for spawn_agent` | no reviewer child |
| 2 | Terra-low parent with unchanged copied config | sidecar `01a0514f-3c9c-76d1-bd36-c32a380a2768`; same unavailable Luna default during child dispatch | no reviewer child |
| 3 | Terra-low parent with disposable copied config's default child model changed to Terra | sidecar `01a05150-31ac-7353-bef9-213f969a5443` started, emitted child `wait` with no receiver IDs, then exited with no child/result | no reviewer child |
| continuation | supported `codex exec resume` from attempt 3's same build root | `thread/resume failed: no rollout found` | no recoverable reviewer receipt |

The stable-toolchain gate is also unproved. Sol's independent raw receipt observed ambient nightly
`rustc`, `NUDOX_STABLE_TOOLCHAIN=1.97.1`, inherited sccache wrapper, and `E0514`. The manager's two
bounded Nix invocations (ambient and explicit-stable forms) each emitted only `fetching git input ...`
before the 30-second command window ended, so neither is claimed as a pass or E0514 reproduction.

## Strongest pre-edit counterexample

A stale rendezvous route sends declared E2 to a lost node after S2 is pinned. An implementation that
reads latest head, trusts assignment, or makes the fault empty success returns wrong facts or empty
`Complete`. The seam requires exact E2 absence then equivalent retry, or `Partial { missing: [E2] }`.

## Remaining red work

No manifest/segment format, sealed-delta builder, exact/prefix/lexical query, publication/head, range
lease, placement adapter, compactor, allocation/work control, compiler/probe integration, or bounded
Tantivy differential adapter exists. Deterministic owner parity cannot claim file/NVMe/object-store
durability/outage proof. The external owner must repair sidecar reviewer spawning/resume and stable
toolchain Nix execution, then re-run the complete frozen cold deck and source-isolated pre-edit
review. No production writer is authorized before that repair.
