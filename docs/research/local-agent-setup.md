# Local agent and validation setup

Scope: read-only inspection of `/Users/mileswirht/Downloads/backend`, the
repository's local control plane, and relevant Codex state under
`/Users/mileswirht/.codex` and `/Users/mileswirht/.config/codex`. Secret-bearing
files were identified by name only; credentials, tokens, private keys, auth
contents, and persisted command values were not read or reproduced. No source,
configuration, build, or test was changed.

## Repository instruction boundary

The product checkout has no root `AGENTS.md`, `ORCHESTRATION.md`, `TESTING.md`,
or `TEST_INFRA.md` in its tracked tree. The only discovered repository
`AGENTS.md` is `server/index/turso/AGENTS.md`, inside the explicitly excluded
vendored Turso tree. Its instructions therefore apply only when intentionally
working in that vendored subtree; they are not product-workspace instructions.
The file recommends Cargo build/test/fmt/clippy and Turso-specific `make` and
conformance commands, and requires regression tests, invariant assertions, and
no unearned assumptions. Do not apply its Turso commands to the parent product
workspace.

The current product checkout does carry `.envrc` (`.envrc:1`) which sources
`.config/direnv/entrypoint`. The workspace-root flake delegates its pinned
inputs and outputs to `.config/flake.nix`; the entrypoint watches both the
workspace wrapper and the control-plane flake, lockfile, Nix and
Nu files, control-plane fixtures, Dylint sources/UI fixtures, and rustfmt config
(`.config/direnv/entrypoint:4-20`), disables nix-direnv fallback, and enters
`path:.#development`. This makes environment drift visible when those
inputs change, but the checkout must be entered through direnv/Nix before using
the `backend` command.

Historical planning instructions exist in prior Codex review snapshots under
`/Users/mileswirht/.codex/wave-b5-review-source.1trdyC/workspace2/`. The useful
ones are `AGENTS.md`, `ORCHESTRATION.md`, `TESTING.md`, and `TEST_INFRA.md`.
They describe a five-role custody model (Sol architecture/integration, Terra
orchestration/research, Luna implementation, hostile review, rubric writing),
require role/model/effort/sandbox/baseline/checkout evidence, and forbid a
worker from changing the contract or declaring completion. They are historical
review material for the older snapshot, not files currently governing the
Downloads checkout; use them as migration constraints only after revalidating
paths and package names.

## Product control plane and usable commands

`.config/flake.nix:1-23` pins nixpkgs, Fenix, Nuenv, and treefmt-nix by revision
and delegates outputs to `.config/nix`. `.config/nix/default.nix:4-143`
constructs per-system toolchains, generated artifacts, backend command, role
bundles, shells, checks, packages, apps, and formatter. The flake exposes
`development`, `compiler`, `services`, `verification`, `observability`, and
`complete` shells (`.config/nix/shells.nix:8-76`).

The stable Fenix toolchain is Rust 1.97.1 with cargo, clippy, rust-src, rustc,
and rustfmt; a latest nightly includes cargo, clippy, llvm-tools, rust-src,
rustc, rustc-dev, and rustfmt (`.config/nix/toolchains.nix:10-38`). Native
compiler tools are explicit: Clang, .NET 8, Go, JDK, Node 22, Python, and
TypeScript (`.config/nix/tools.nix:45-53`). Quality tools include ast-grep,
cargo-audit, cargo-deny, cargo-nextest, Git, Koji, formatters, Nu, and jq;
verifier tools add cargo-bloat, Dylint, cargo-llvm-cov, hyperfine, samply, and
Linux valgrind (`.config/nix/tools.nix:54-79`). This is the intended tool source;
ambient PATH tools should not be treated as equivalent evidence.

The generated command surface is discoverable with:

```text
nix develop path:.#development
backend
backend catalog --json
backend doctor
backend scope changed --json --base <base>
```

`backend doctor` checks ast-grep, cargo-nextest, Git, Nix, and Nu and reports
repository/config/local roots plus active role (`.config/nu/main.nu:21-47`).
`backend catalog --json` is the stable command inventory. `backend scope changed`
maps Git paths to owning Cargo packages without running a gate
(`.config/nu/main.nu:50-62`, `.config/nu/scope/cargo.nu:1-44`).

The normal implementation-feedback command is `backend test`; changed-package
verification is `backend test changed --base <base>`; expanded package proof is
`backend test affected --base <base>`; and complete closure is
`backend test workspace` (`.config/nu/quality/test.nu:1-160`). Fast source checks
are `backend lint structure ...`, `backend lint changed`, and `backend lint self-test`;
semantic Dylint is privileged as `backend lint semantic --package <name>`
(`.config/nu/quality/lint.nu:56-185`). Formatting is
`backend format changed --check --base <base>` or the write form without
`--check` (`.config/nu/quality/format.nu:6-63`). These command names should be
used by implementation/cutover automation instead of raw script aliases.

Measurements are explicitly named and verifier-owned:
`backend observe build [target]`, `backend observe benchmark <target>`,
`backend observe binary <target>`, `backend observe profile <target>`, and
`backend observe baseline <run> <name>` (`.config/nu/quality/observe.nu:133-260`).
Declared targets include changed debug build timings, heart-root capacity
planning, interface CLI symbol size, and interface CLI CPU profile
(`.config/nix/control.nix:284-350`). No measured result is implied by the
target declaration.

Production agent dispatch has one durable owner: `backend-control` in
`tools/control`. The `backend cutover control ...` commands are thin Nu
adapters over its bounded JSON CLI; they admit checked work specifications,
exact reusable receipts, fenced leases, evaluator/reviewer/Sol verdicts, and output-sensitive
expiry/dependency transitions into the shared `backend-version`/`backend-store`
head. Candidate, evaluator, reviewer, and Sol receipts form one schema-typed
chain; only the final Sol receipt enables reuse. The older
`cutover lease|candidate|receipt|context` commands are the separately rooted Git
promotion and audit oracle. They must not be used as dispatch state or treated
as a second work authority.

## Cargo and nextest setup

The root Cargo workspace uses resolver 3, edition 2024, Rust 1.97.1, and
explicitly excludes `server/index/turso` (`Cargo.toml:4-24`). The root profile
sets incremental dev builds with debug level 1, and release uses one codegen
unit, thin LTO, abort-on-panic, and stripped symbols (`Cargo.toml:99-113`).
`.config/nix/shells.nix:17-21` places Cargo output at `$PWD/.local/target` and
sets `CLIPPY_CONF_DIR=$PWD/.config`; generated backend applications default
`CARGO_TARGET_DIR` to `.local/target` (`.config/nix/commands.nix:46-60`). For
parallel worktrees, override this per checkout with an isolated target directory
before any build or measurement; historical evidence shows shared targets create
phantom compiler artifacts and false failures.

Nix generates `.local`-independent Koji, nextest, and OpenTelemetry declarations
from the immutable control plane (`.config/nix/artifacts.nix:1-13`). Nextest is
version-pinned in control (`.config/nix/control.nix:141-243`), uses zero retries
and flaky-result failure in the default profile, enforces leak/global/slow
timeouts, and groups allocator, service, process, display, concurrency, native,
and telemetry tests. `backend test changed` invokes `cargo nextest run --locked
--no-tests=fail` with `-E kind(lib)`; `affected` selects packages without that
library-only expression; `workspace` selects `--workspace`
(`.config/nu/quality/test.nu:88-160`). `--no-tests=fail` is an empty-selection
guard, not permission to pass with no tests. A cutover plan must still enumerate
integration, binary, example, and benchmark targets because the quick changed
command intentionally covers libraries only.

Every test invocation creates a unique run-scoped nextest configuration and
store under `.local/nextest` (`.config/nu/quality/test.nu:71-86`). This prevents
parallel JUnit races. Use the returned run/evidence directory as the test receipt;
do not collapse a failed process into a success narrative. The control-plane
tests assert zero default retries and valid group references
(`.config/tests/control-plane.nu:165-185`).

## Nushell custody and evidence

`configuration-root` has live and immutable modes and rejects missing policy
snapshots (`.config/nu/core/root.nu:14-38`). `active-role` resolves a declared
role or human, and `require-command` verifies command ID, role contract digest,
tool binding, and allowed capability before execution
(`.config/nu/core/capability.nu:5-43`). Role bundles expose only their assigned
short tools; `.config/nix/role-tools.nix:9-83` derives candidate identity from
Git status, tracked diff, untracked paths, and object hashes before forwarding
to the generated backend command. This is the key concurrency safeguard: keep
writers on disjoint paths or isolated worktrees, and preserve the candidate
digest in evidence.

External processes are captured through bounded files, with a 16 MiB combined
stdout/stderr cap and typed failure manifests (`.config/nu/core/process.nu:5-106`).
Tooling events omit source/path/query/identifier/error-message data and record
hashed run/card/change identities, status, elapsed time, and byte counts
(`.config/nu/core/telemetry.nu:1-48`, `.config/nix/control.nix:246-283`). The
collector is a separate validated check. Future reports may cite event schema,
run IDs, and digests; they must not include raw captured output or environment
credential values.

## Agent model defaults and local Codex state

The active Codex home is `/Users/mileswirht/.codex` in this session. Its
`config.toml` records the active default model as `gpt-6-astra`, medium general
reasoning, high plan-mode reasoning, and agent defaults of 15 concurrent threads,
`gpt-5.6-luna`, medium effort. The same file enables multi-agent v2 with a
15-thread limit and marks the Downloads backend project trusted. These are
configuration facts, not authorization to broaden a task; role custody still
comes from the repository control plane when using generated role tools.

The separate legacy-looking `/Users/mileswirht/.config/codex/config.toml` has a
different default model (`gpt-5.6-sol`) but the same 15-thread and Luna medium
agent defaults. Do not mix those two configuration roots when comparing historical
receipts. Record the resolved config path with every delegated candidate, as the
historical orchestration instructions require.

`/Users/mileswirht/.codex/models_cache.json` contains model metadata; only model
identifiers are useful for a safe setup report. `/Users/mileswirht/.codex/auth.json`,
the `.config/codex/auth*.json` files, Codex databases, session/history JSONL,
attachment files, and shell snapshots are credential or private-session stores;
they were not read. `/Users/mileswirht/.codex/rules/default.rules` contains
persisted command approval rules and may contain sensitive command text; it was
identified but not used as task evidence. Hooks, MCP server declarations,
plugins, and browser/computer-use configuration are local integration state;
the command paths and environment names are not product build dependencies.

The model cache lists `gpt-6-astra`, `gpt-reserve`, `gpt-5.6-sol`,
`gpt-5.6-terra`, `gpt-5.6-luna`, `gpt-5.5`, `gpt-5.4-mini`,
`gpt-5.3-codex-spark`, and `codex-auto-review`. Availability in cache is not
proof that a model/effort pair is valid for a particular host; preserve the
resolved runtime receipt rather than inferring it from this list.

## Worktrees and concurrency

The current Git repository is `canonical`, with a remote configured over SSH;
the remote host/user were redacted here. `git worktree list --porcelain` reports
106 entries, mostly prunable historical `/private/tmp` or Codex worktrees, plus
the current checkout and several active Downloads/.local worktrees. This is a
high collision risk for shared `.local/target`, nextest stores, native tool
outputs, and mutable evidence. Before a cutover, prune only with explicit user
authority; for routine work use a fresh isolated worktree and target/evidence
directories, and do not infer that a listed prunable entry is safe to delete.

The role implementation bundle is intentionally narrow: Luna receives `test`
only; Terra receives inspection, lint, testing, measurements, and integration
tools; reviewer and Sol roles receive broader verification. In the historical
role contract, Luna must not format, lint, benchmark, profile, bless snapshots,
or broaden scope; Terra owns formatting/lint repair and measurements; Sol owns
cross-crate integration. Preserve this division for regression-free migration.

## Current test setup and safe validation sequence

The current checkout is dirty: root Cargo files are modified and interface
crates have extensive staged/unstaged edits and additions, with untracked
`node_modules/` and excluded Turso source. This is not a valid clean baseline.
No test result should be attributed to a clean branch until a separate clean
worktree is selected.

For a safe read-only inspection/cutover preparation sequence:

1. Enter `nix develop path:.#development` and run `backend doctor`.
2. Run `backend catalog --json` and `backend scope changed --json --base <base>`.
3. Use stable `cargo metadata --locked --format-version 1` to enumerate 53
   workspace members, features, dependencies, and Cargo targets; use
   `cargo nextest list --locked --workspace --no-tests=fail` only after the
   generated config exists.
4. Run `backend format changed --check --base <base>`, then
   `backend lint changed`, then `backend test changed --base <base>` for a
   narrow candidate. Expand to `backend test affected --base <base>` and finally
   `backend test workspace` only at the assigned closure boundary.
5. For unsafe/concurrency claims, run `backend lint semantic`, then the scoped
   trybuild/Loom/Miri/property tests in isolated target directories. For cost
   claims, use only the declared `backend observe ...` targets and retain their
   sealed environment/artifact records.

The sequence intentionally keeps diagnosis separate from integration. A command
that is unavailable, skipped for a typed native prerequisite, or blocked by a
dirty/shared worktree must be recorded with its exact typed failure and owner;
it is not a green result.

## Setup implications for the v2 cutover

The control plane is reusable, but the cutover should first add a generated
manifest that binds: resolved config path and model/effort; repository revision
and dirty status; worktree and target roots; Cargo member/feature/target graph;
role/tool contract digests; native prerequisites; nextest profile/group; and
measurement environment. Store only hashes and bounded metadata. The manifest
should reject mixed Codex homes, shared target paths, missing contract digests,
unregistered skips, and stale immutable configuration.

The highest-risk regressions are known: an older snapshot's 19-crate nextest
assumptions do not describe this 53-member workspace; the only product AGENTS
file belongs to excluded Turso; historical worktrees are mostly prunable; and
the current product checkout is dirty. Revalidate every copied command and path
against the current `.config/nu` catalog and Cargo metadata before automation.

No credentials, private command contents, or current test/build success are
included in this report.

## Implementation checkout note — 2026-09-09

The active implementation is now
`/Users/mileswirht/Documents/ChatGPT/backend/implementation`. Its `.git` file
still names `/Users/mileswirht/Downloads/backend/.git/worktrees/implementation`,
which is absent, so Git-derived revision and dirty-state commands are not valid
for this checkout. The cutover scripts deliberately accept
`BACKEND_WORKSPACE_SNAPSHOT`; the pinned `path:$PWD/.config#verification`
environment used that explicit source root and passed both control-plane and
end-to-end gates. Structural measurements consequently report
`revision=unknown` and must not be labeled as clean-commit evidence.

Do not repair this by inventing a new repository or mutating the Downloads
checkout during validation. Before production promotion, restore the intended
worktree registration from the repository owner, then rerun the same pinned
gates so receipts bind the real commit and tree. No secret-bearing local Codex
state is needed for either the snapshot validation or that later Git repair.
