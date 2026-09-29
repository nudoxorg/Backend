# Rust compiler read-frontier boundary

## Admission status

The Rust authority cannot currently mint `VerifiedUnitReadClosure`. The
V2 workspace manifest correctly reports `CompilerReadFrontierStatusV2::Unproven`,
and `RustWorkspaceSessionLane` opens a fresh Cargo/rust-analyzer workspace for
each operation. Keep both behaviors.

The manifest also exposes the typed partial reason
`NoRegisteredCompleteReadAdapter`. This is diagnostic state only: it does not
change admission, register a protocol, or skip compiler work. No safe reuse
subset has been established from `RustWorkspaceSessionKey`; its selected source
paths are an editor-buffer overlay, not a complete Cargo package, dependency,
or toolchain read closure. In particular, omission from that list does not
delete a disk-visible module, and an unchanged list cannot establish that
negative module candidates, Cargo/project-model reads, sysroot inputs, or
child-process observations stayed unchanged. Reusing an RA database from that
key would still risk stale results.

The internal trace builder in
`crates/engine/src/compiler_unit_read_closure_v2.rs` is a typed sink, not a
read observer. Its production trust registry is empty. Test fixtures can call
`observe` directly with facts they invent, so those tests prove canonical
encoding, ordering, binding, and workspace-witness validation only. They do
not establish that rust-analyzer observed the supplied paths or that a caller
reported every observation.

The fact schema already has useful distinctions: `PresentFile`, `AbsentPath`,
`DirectoryListing`, present/absent environment variables, generated inputs,
toolchain inputs, and external inputs. `verify_read_frontier_with_work` checks
positive file identities, absent workspace paths, and workspace directory
listing digests against the captured workspace tree. It currently accepts
environment, generated, toolchain, and external records without resolving them
against a separate captured authority. A trusted adapter must supply those
facts from observation, and a future verifier must bind their root identities
and content witnesses before this protocol can be registered. Merely making
the registry nonempty would not close those gaps.

## What pinned rust-analyzer exposes

The Rust frontend pins `ra_ap_*` version `0.0.341`. Its package open path calls
`ra_ap_load_cargo::load_workspace_at` from
`frontends/rust/src/legacy/authority.rs`. That call discovers the Cargo
manifest, loads `ProjectWorkspace`, invokes Cargo for workspace location and
metadata, discovers sysroot inputs, then populates a `RootDatabase` and VFS.
The frontend sets `load_out_dirs_from_check: false`, disables the proc-macro
server, and uses the crate graph and project-folder loader for source files.
Those options narrow the authority; they do not make its read set complete.

The pinned `ra_ap_vfs::Vfs` API exposes `iter()` and `file_path()` for files
whose contents reached the VFS. Its own module documentation says VFS does not
perform I/O; a separate `loader::Handle` does. The public VFS state has no
event for a failed read, a directory enumeration, or an environment lookup.
`RootDatabase` can expose the text associated with a `FileId` through
`SourceDatabase::file_text`, so an opt-in caller can hash the positive VFS
snapshot after load; that still says nothing about failed loader probes or
inputs that bypassed VFS.
There is also a concrete negative-lookup seam in
`ra_ap_hir_def/src/nameres/mod_resolution.rs`: `ModDir::resolve_declaration`
constructs up to two candidates (`name.rs` and `name/mod.rs`, or the explicit
`#[path]`) and calls `DefDatabase::resolve_path` for each. That call reaches
`ra_ap_base_db::RootDatabase::resolve_path`, which asks the anchored
`SourceRoot`/`FileSet` for membership and returns `None` when a candidate is
absent. The resolver itself has no callback, but the crate's `DefMap` retains
`UnresolvedModule` diagnostics with candidate strings when every candidate
fails. The observer reads only those name-resolution diagnostics through
`ra_ap_hir_def::nameres::crate_def_map`; it does not call the broader
`ra_ap_hir::Module::diagnostics`, which computes unrelated type and MIR
diagnostics. This captures rejected module candidates, not every internal
`resolve_path` call or unrelated failed lookup. The crate graph can also be
created from Cargo and project-model data before VFS source loading begins.
Project-model code directly uses filesystem
metadata/read calls and child processes for manifest discovery, Cargo queries,
and sysroot selection. A VFS walk is therefore a useful positive file
inventory, but it is neither the process read frontier nor a negative lookup
proof.

The compiler has an opt-in, borrow-scoped observation seam in
`RustWorkspaceSessionLane::begin_with_read_frontier_observer`. It observes the
selected editor overlay, synchronously walks `Vfs.iter()` and reads each
representable loaded file through `SourceDatabase::file_text`, reads the
name-resolution diagnostics retained by each loaded crate's `DefMap` and emits
unique crate-root/declaration/candidate identities from
`UnresolvedModule.candidates`, and observes exact byte buffers successfully
read by the Rustdoc `include_str!` preloader. The frontend reports independent
visited-versus-acknowledged totals for the VFS, Rustdoc, and module-candidate
events; the engine seals the selected-editor stream against the selected-file
count and the VFS and module producers against their source-side visited
counts. A rejected callback, recorder failure, unsupported path, or truncated
scan cannot appear sealed. The Rustdoc filesystem producer remains unsealed
because its callback starts only after a successful open/read; it does not
capture the earlier canonicalize, metadata, or open attempts. Under
the `compiler.read_frontier` debug target, the production Rust package
compiler feeds these events to the bounded, attempt-fenced diagnostic
recorder. With that target disabled (the default), the compiler takes the
original `begin` path without the VFS walk, HIR diagnostics, Rustdoc observer,
or hashing.

The callbacks are synchronous and use no per-read `Arc<Mutex<_>>` or retained
event row. File contents are hashed as RA holds them; module-candidate
deduplication retains fixed-size digests. The observer caps VFS/candidate
events and examined DefMap diagnostics at 250,000 each, and VFS content/path
evidence at 512 MiB. Rustdoc reads inherit the preloader's per-file and
aggregate byte limits. Overflow leaves the affected producer unsealed. This
is diagnostic evidence only: a VFS snapshot is not a loader
event stream, unresolved-module diagnostics do not expose all successful or
failed resolver attempts, and later semantic queries can still discover other
reads. No work or validation scan is skipped.

The recorder has a fixed required channel set. A channel token is tied to one
monotone operation attempt; accepted events are sequenced into a rolling
per-channel transcript, and a producer seal must match the observed sequence
count. Wrong-attempt tokens, channel/class mismatches, unsupported paths,
sequence gaps, byte/event limits, and terminal-count mismatches poison the
report. The diagnostic production path registers `editor_overlay`,
`ra_vfs_loader`, and `rust_module_resolver`; the latter two are sealed only
when the scans are untruncated and all paths are representable. Every other
required channel remains missing, so its report remains partial. Seals
establish administrative producer closure only:
the same adapter can still omit an event and report a self-consistent count.
The recorder has no method that mints `VerifiedUnitReadClosure`, and the V2
trust registry remains empty.

## Current channel coverage

| Required channel | Current evidence | Remaining blocker |
| --- | --- | --- |
| Selected editor overlay | Captured bytes read back from RA `SourceDatabase` after overlay application | Does not cover disk-loaded source files or filesystem reads |
| RA VFS loader | Diagnostic post-load snapshot of VFS paths and exact `SourceDatabase` text | No loader event receiver; failed reads, directory results, and later changes are invisible |
| Rust module resolver | `DefMap` `UnresolvedModule` candidate pairs for declarations whose candidates all failed | No hook for successful resolution or every individual `resolve_path` attempt; candidate data is a post-load scan |
| Authority filesystem | Successful bounded Rustdoc include reads observed after `read_to_end` | Earlier `canonicalize`, `metadata`, `symlink_metadata`, and open attempts bypass the callback; other frontend filesystem reads remain unobserved |
| Directory enumeration | None | No complete child-set/error callback for RA loader or project model |
| Cargo project model | None | Manifest discovery and metadata/config reads use direct filesystem calls and Cargo |
| Environment | None | RA, Cargo, rustup, and toolchain environment observations are not routed through one sealed policy observer |
| Process tree | None | Cargo/rustc/rustup children and descendants lack a joined event broker and independent terminal count |
| Toolchain/sysroot | None | Sysroot discovery, standard-library inputs, helper probes, and symlink identity are not captured as a closed root |
| Generated outputs | None | Build outputs are disabled in this RA configuration, but no observer proves that every generated input path is absent |
| External dependencies | None | Registry/path dependency source and metadata reads are not bound to verified external roots |

This is not a complete self-contained compile recipe: there is no existing
Rust authority path that proves its dependency, sysroot, environment, Cargo,
and process inputs are preadmitted as one immutable owner-controlled tree. No
VFS validation scan, manifest scan, source-root rebuild, or fresh RA load is
skipped on the strength of this observation. The precursor provides actual RA
positive-file snapshots and actual DefMap-reported negative module candidates,
with independent callback-loss checks, and a recorder contract that a future
adapter can extend channel by channel without introducing per-read locking.

The API-level integration points are the `ra_ap_load_cargo` load boundary and
the RA module resolver, not only the semantic callback in `RustAuthority`. A
future adapter still needs an event hook around
`ModDir::resolve_declaration`/`RootDatabase::resolve_path` to retain every
successful and failed module attempt; the DefMap scan only recovers the failed
candidate sets. It also needs the `loader::Handle`
boundary to retain successful file buffers and complete directory enumeration
results. In one scope it must cover manifest/project discovery, Cargo and
rustc subprocesses, sysroot resolution, project-folder enumerations, and every
successful or failed loader request. An OS-level broker or audited
process/filesystem shim is needed for the current project-model code because
its filesystem and environment reads bypass the VFS loader. After load, it must
also account for later lazy lookups that rely on file-set membership. The
current public RA API does not expose enough dependency information to do
that. Specifically, pinned `ra_ap_load_cargo::load_workspace_into_db`
constructs `vfs_notify::NotifyHandle` internally and returns only the finished
database, VFS, and optional proc-macro client; it has no injectable loader
factory or event receiver. A first RA-facing patch therefore needs an audited
loader factory plus a completion/error channel, likely as a local patch to the
pinned load-cargo crate, and an explicit callback in `ModDir::resolve_declaration`
for the candidates it tries. These hooks cover separate layers: the loader
reports I/O and enumeration evidence, while the module resolver reports
semantic absence in the already-loaded `FileSet`.

## Bounded typed path when RA supports it

Keep one mutable observer owner borrowed across a single load-and-lower
transaction. Have the RA adapter append typed events into a bounded event log
owned by that operation; do not put an `Arc<Mutex<_>>` on every file. If loader
workers need to report concurrently, use one bounded channel feeding that
owner. The owner assigns a monotonically increasing sequence as events are
accepted, and channel overflow, worker failure, cancellation, panic, or an
unjoined producer poisons the trace. Only a successful end marker after every
producer joins may call `CompilerReadTraceBuilderV2::end`.

The adapter protocol should emit:

- A positive file event with bytes or a verified content identity from the
  exact buffer RA consumed.
- A negative path event for each absent candidate emitted by
  `ModDir::resolve_declaration`, plus every other failed file/path probe that
  can affect the admitted result.
- A directory event containing the exact direct-child set and a completeness
  marker for each enumeration RA or Cargo used. Exclusions and errors must be
  represented; a scanner inventory cannot fill in an unobserved listing.
- Environment presence/value events from the authority's explicit environment
  policy. A full inherited process environment is not inferred from selected
  Cargo variables.
- Generated, toolchain, and external input events bound to typed root identities
  and content digests. Do not treat a short path string as a root identity.
- Process events binding executable/toolchain identity, arguments and relevant
  output digests for Cargo, rustc, and any helper admitted by the protocol.

The engine should continue to construct a `ReadFrontier` tree and verify
workspace-relative facts with `verify_read_frontier_with_work`. Add separate
typed external-root snapshots or closure members for non-workspace facts, then
verify every environment/generated/toolchain/external witness against those
snapshots. A protocol digest should commit to the exact observer implementation
and coverage contract. Register it only after the adversarial cases below pass
on the actual authority path. Any unsupported read class or dropped event must
return an incomplete trace and preserve `Unproven` status.

## Runnable macOS observation experiment

This experiment measures filesystem system calls made by one real
rust-analyzer authority test. It is diagnostic only: `fs_usage` does not expose
the environment values RA consulted, prove that all process descendants were
selected, or establish a no-loss completeness boundary. It must never be used
to promote V2 to a complete closure.

Build the test executable before capturing so Cargo's build traffic is outside
the trace:

```sh
cargo test -p backend-frontend-rust --test semantic_authority --no-run
```

Use the executable path Cargo printed (or the matching executable under the
configured target directory) and run only the real package-load test:

```sh
tools/observe-rust-ra-reads.sh /tmp/rust-ra-reads \
  /path/to/semantic_authority-EXECUTABLE \
  borrowed_authority_preserves_hir_types_resolution_macros_and_exact_spans \
  --exact --nocapture
```

The helper pauses the test process before `exec`, attaches `fs_usage`, resumes
the test, and writes separate authority output, filesystem trace, and capture
metadata files under the chosen prefix. It selects the test process by PID and
also selects processes named `cargo` and `rustc` for Cargo metadata and toolchain
queries. `fs_usage` requires macOS administrator privileges, so the helper asks
`sudo` to authenticate once. Run the experiment without unrelated Cargo or
rustc processes if possible, since command-name filters can include those
processes too.

Inspect the trace for successful opens/reads, failed path lookups (including
their OS error), and directory enumeration calls around the temporary fixture
root. The test has a Cargo manifest, a crate root, and a sibling module, so it
exercises actual project discovery and module resolution. Repeat after adding
an absent candidate or an empty directory to the fixture if the trace needs a
specific negative-probe case. The trace contains filesystem metadata and paths,
not file contents. It still over-approximates semantic dependencies and does
not observe values returned by `getenv`; that difference is exactly why this
experiment is evidence for designing an observer, not a production proof.

## Adversarial admission gates

The frontend's workspace-session fixture drives the real pinned RA workspace,
reads an included Rustdoc file, and introduces `mod absent;`; it receives the
VFS root/sibling buffers, the exact canonical Rustdoc path and bytes, and
DefMap-reported `absent.rs`/`absent/mod.rs` candidate pair. Its observer
deliberately rejects one Rustdoc callback and one candidate callback and
asserts that the independent source-side visited totals exceed acknowledged
deliveries. The engine's
read-closure tests separately include an independent-oracle fault injection
for omitted negative facts and a lost child terminal event. These tests check
the real RA surface and broker failure behavior separately; neither proves
global frontier completeness or authorizes production reuse.

Before a Rust adapter protocol is registered, its tests must show that changing
each of these changes the captured closure or rejects admission:

1. Create a new module under an already-existing empty directory; remove it and
   rename it to the alternate Rust module spelling.
2. Retarget a symlink used by a selected module and change a file while
   preserving both byte length and modification time.
3. Change Cargo manifest, lockfile, `.cargo/config.toml`, feature selection,
   `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, and a relevant environment variable
   from absent to present.
4. Change a build output, sysroot source, external crate source, and helper
   executable used while loading the same fixture.
5. Force observer overflow, callback loss, worker failure, cancellation, and
   panic. Each case must fail closed without an end marker or a verified
   closure.

The current broker tests include an independently authored expected negative
module candidate and an independently sealed child-process terminal count. The
first fault-injection case shows that a producer can omit a negative candidate
and still seal its own internally consistent count; only the separate oracle
detects the omission. The second drops the child's final event and is rejected
because the process supervisor's terminal count disagrees. These validate the
failure contract, not actual RA capture. A future experiment must drive the
actual loader/module/project/process hooks through those cases before any
protocol is registered.

Passing only the VFS-positive-file checks is insufficient. Until all supported
read classes have evidence and unsupported classes are rejected, the workspace
manifest and Rust reuse lane remain fresh-only. The debug summary is
observability only; it does not change compiler readiness or authorize skipped
work.
