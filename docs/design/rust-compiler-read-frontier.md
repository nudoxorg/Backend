# Rust compiler read-frontier boundary

## Admission status

The Rust authority cannot currently mint `VerifiedUnitReadClosure`. The
V2 workspace manifest correctly reports `CompilerReadFrontierStatusV2::Unproven`,
and `RustWorkspaceSessionLane` opens a fresh Cargo/rust-analyzer workspace for
each operation. Keep both behaviors.

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
`SourceDatabase::file_text`, so a caller can hash positive buffers after load;
that still says nothing about missed candidates or inputs that bypassed VFS.
There is also a concrete negative-lookup seam in
`ra_ap_hir_def/src/nameres/mod_resolution.rs`: `ModDir::resolve_declaration`
constructs up to two candidates (`name.rs` and `name/mod.rs`, or the explicit
`#[path]`) and calls `DefDatabase::resolve_path` for each. That call reaches
`ra_ap_base_db::RootDatabase::resolve_path`, which asks the anchored
`SourceRoot`/`FileSet` for membership and returns `None` when a candidate is
absent. No public callback reports those candidate strings or misses. The
crate graph can also be created from Cargo and project-model data before VFS
source loading begins. Project-model code directly uses filesystem
metadata/read calls and child processes for manifest discovery, Cargo queries,
and sysroot selection. A VFS walk is therefore a useful positive file
inventory, but it is neither the process read frontier nor a negative lookup
proof.

The API-level integration points are the `ra_ap_load_cargo` load boundary and
the RA module resolver, not only the semantic callback in `RustAuthority`. A
future adapter needs an event hook around
`ModDir::resolve_declaration`/`RootDatabase::resolve_path` to retain the exact
positive and negative module candidates. It also needs the `loader::Handle`
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

Passing only the VFS-positive-file checks is insufficient. Until all supported
read classes have evidence and unsupported classes are rejected, the workspace
manifest and Rust reuse lane remain fresh-only.
