# Cargo build cache lanes

The Nix shell wraps Cargo with `.config/scripts/cargo-shared-cache.sh`. Cargo
1.97's `build.build-dir` is assigned from a bounded pool of reusable warm
directories. The final target directory remains inside the calling worktree.
Each warm directory has an exclusive lease, so two worktrees can compile in
parallel without mutating the same intermediate graph. A warm directory is
reused without cleaning only by the same canonical worktree; handing it to a
different worktree first resets the old Cargo graph. Cross-worktree compiler
reuse comes from `sccache`, which avoids stale `rmeta` and public-API leakage.

Compiling commands also acquire a lease keyed by the canonical git worktree
path. The first command records the warm lane it used in the cache affinity
map. A second compile from that same worktree waits for the first command and
reuses the remembered lane; it cannot silently create a second warm or
overflow graph. Waiting is bounded by `NUDOX_CARGO_WORKTREE_WAIT_MS` (five
minutes by default) and returns status 75 when the bound expires. If all warm
lanes are occupied by other worktrees, the scheduler waits up to
`NUDOX_CARGO_SLOT_WAIT_MS` (five seconds by default), then gives the current
worktree an isolated overflow directory that is deleted on exit. The overflow
path is never shared with a live owner and is cleaned after normal completion
or cancellation.

The wrapper bypasses both the compiler cache daemon and build-dir leasing for
read-only commands that do not compile: `metadata`, `tree`,
`locate-project`, `read-manifest`, `search`, `help`, and `version`. `fmt` is
build-free but can mutate source files, so it stays on the worktree-serialized
path to avoid racing a compile. Manifest-changing commands such as `update`,
`generate-lockfile`, `add`, `remove`, and `vendor`, along with artifact commands
such as `package` and `clean`, also stay on the conservative path. Unknown
commands use the compiling path so a new Cargo subcommand cannot accidentally
weaken isolation. An explicit
`CARGO_BUILD_BUILD_DIR` is always preserved verbatim and bypasses the lease
selection while retaining the existing compiler-cache setup.

Lease directories contain the owner PID, process start token, and canonical
worktree path. A dead owner is reclaimed by an atomic rename before its lock
is removed; when `ps` is available, a reused PID is rejected unless its start
token still matches. Signal
handlers forward cancellation to Cargo and release the slot and worktree
leases through the single exit cleanup path. The affinity map is updated by a
same-worktree lease and an atomic rename, so a killed process can leave only a
harmless temporary file. Warm lanes are retained for reuse; overflow lanes are
invocation-local, which bounds cache growth by the configured warm pool plus
currently running overflow commands.

The protocol tests are intentionally shell-only and run without Nix or a
workspace build:

```sh
./tests/cargo-shared-cache.sh
```

They use fake Cargo, git, and sccache processes to prove same-worktree
serialization and reuse, independent-worktree parallelism, metadata bypass,
explicit override preservation, dead-owner recovery, cancellation cleanup,
and bounded overflow lifetime.
