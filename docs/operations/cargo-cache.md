# Cargo build cache lanes

The Nix shell wraps Cargo with `.config/scripts/cargo-shared-cache.sh`. Cargo
1.97's `build.build-dir` is assigned from a bounded pool of reusable warm
directories. Each directory has an exclusive lease, so two worktrees can
compile in parallel without mutating the same intermediate graph. A slot has a
`.nudox-worktree-root` stamp. Reuse by that same canonical worktree is warm;
when the slot changes owners, the wrapper retires only that managed slot graph
while holding its lease. Cross-worktree compiler reuse comes from `sccache`,
which avoids stale `rmeta` and public-API leakage.

The final target directory defaults to `<worktree>/.local/target` and is
stamped with its canonical worktree root. An explicitly supplied
`CARGO_TARGET_DIR` keeps its exact path when it is empty or already stamped for
that worktree. A non-empty unmarked target, symlink, or stamp for a different
root is refused without changing its contents. An explicit
`CARGO_BUILD_BUILD_DIR` is a role root. New invocations reuse only four stamped
graphs (`slot-0` through `slot-3`) under `.nudox-cargo`, independently from the
host-wide permit number. Higher-numbered graphs left by an older wrapper are
ignored and left untouched.
The wrapper finds a graph stamped for the current worktree before selecting an
empty or replaceable graph, and holds a graph-specific lease while inspecting,
resetting, or compiling it. Two different role roots may both use their own
`slot-0` concurrently under distinct host permits. A non-empty unmarked graph
is refused without deleting its contents.

The host permit pool caps simultaneous Cargo invocations sharing
`NUDOX_BUILD_CACHE_ROOT`. Use the same cache root for every lane on a host;
different roots have separate leases. When every lane is busy, another caller
waits for `NUDOX_CARGO_SLOT_WAIT_MS` (five minutes by default) and exits with
status 75 if no lane opens. It never creates an overflow lane. Explicit role
graphs have their own four-slot retention bound; graph slots do not consume or
name host permits. These bounds cap invocations, not compiler memory: an
explicit `CARGO_BUILD_JOBS` is preserved, and each invocation's workload has
its own RAM cost.

Compiling commands also acquire a lease keyed by the canonical git worktree
path. For the default pool, the first command records the warm lane it used in
the cache affinity map. A second compile from that same worktree waits for the
first command and reuses the remembered lane; it cannot silently create a
second warm or overflow graph. Explicit role graphs follow their worktree
stamp across changes in the host permit number. Waiting is bounded by
`NUDOX_CARGO_WORKTREE_WAIT_MS` (five
minutes by default) and returns status 75 when the bound expires. If all warm
lanes are occupied by other worktrees, the scheduler waits up to
`NUDOX_CARGO_SLOT_WAIT_MS` and then returns status 75 without starting Cargo.

The wrapper bypasses both the compiler cache daemon and build-dir leasing for
read-only commands that do not compile: `metadata`, `tree`,
`locate-project`, `read-manifest`, `search`, `help`, and `version`. `fmt` is
build-free but can mutate source files, so it stays on the worktree-serialized
path to avoid racing a compile. Manifest-changing commands such as `update`,
`generate-lockfile`, `add`, `remove`, and `vendor`, along with artifact commands
such as `package` and `clean`, also stay on the conservative path. Unknown
commands use the compiling path so a new Cargo subcommand cannot accidentally
weaken isolation. Explicit build-directory values are role roots, not exact
graph paths: build graphs use stamped, leased children. Explicit target
directories keep their caller-selected path under a matching worktree stamp.
An override cannot start a fifth compiler when the four configured slots are
occupied.

`.config/scripts/cargo-in.sh` is the tracked source for the local
`.local/devenv/cargo-in` helper. Refresh that ignored helper with
`install -m 0755 .config/scripts/cargo-in.sh .local/devenv/cargo-in`. It sets a
role-specific build root and leaves the target directory worktree-local by
default; the shared wrapper derives the leased build child. Existing caller
Cargo arguments and explicit target paths are preserved.

By default, `RUSTC_WRAPPER` is a small gate that sends only `lib`/`rlib`
compilations whose source is under Cargo's registry or git-checkout directories
to `sccache`. Workspace crates, vendored sources, build scripts, proc macros,
and unknown invocations run the selected `rustc` directly. The shared cache
therefore reuses compiler results for unchanged external dependency inputs
without sharing a Cargo fingerprint graph or workspace artifact directory.
An explicitly supplied `RUSTC_WRAPPER` is preserved and bypasses this gate.
The `sccache` cache key still controls whether an external compilation can be
reused; unsupported rustc modes compile normally.

Legacy `.local/build/<role>` and `.local/target-<role>` directories are never
imported or stamped automatically. The exact standard target path
`<worktree>/.local/target` is treated as worktree-owned even when the Nix shell
pre-exports it; the wrapper stamps that path without deleting its final
artifacts. Any other explicit target path must be empty or already stamped for
the current root. Keep old role directories available for read-only comparison
until their owners review and retire them; mixed build graphs cannot be
promoted by adding a stamp.

Each compiling invocation writes a private JSON provenance record beneath
`CARGO_TARGET_DIR/.nudox-provenance/`. It includes the canonical root, HEAD and
dirty-tree digests, lockfile digest, Cargo/Rust versions, wrapper hashes,
feature/target selection, effective build and target paths, Cargo exit status,
and hashes of executable outputs written during that invocation. Failed Cargo
commands still receive a record and retain Cargo's exit status.

Lease directories contain the owner PID, process start token, and canonical
worktree path. A dead owner is reclaimed by an atomic rename before its lock
is removed; when `ps` is available, a reused PID is rejected unless its start
token still matches. Signal handlers forward cancellation to Cargo and release
host-capacity, role-graph, and worktree leases through the single exit cleanup
path. The default-pool affinity map is updated by a same-worktree lease and an
atomic rename, so a killed process can leave only a
harmless temporary file. Warm lanes are retained for reuse, which bounds both
the number of warm build graphs and simultaneous wrapped Cargo invocations.

The protocol tests are intentionally shell-only and run without Nix or a
workspace build:

```sh
./tests/cargo-shared-cache.sh
```

They use fake Cargo, git, and sccache processes to prove same-worktree
serialization and reuse, independent-worktree parallelism, metadata bypass,
explicit role-root namespacing, mismatch refusal/isolation, provenance fields
and output hashes, external-only rustc cache routing, exit-code/argument
preservation, dead-owner recovery, cancellation cleanup, and the shared-root
invocation ceiling. They do not measure real compiler memory or arbitrate
callers using a different cache root or unwrapped Cargo.
