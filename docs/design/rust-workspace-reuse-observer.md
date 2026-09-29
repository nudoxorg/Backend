# Rust workspace reuse and editor frontier boundary

Cross-call rust-analyzer workspace reuse remains disabled. The pinned RA APIs
do not expose a complete positive and negative read frontier for lazy HIR
module lookup, directory enumeration, Cargo probes, build outputs, symlink
targets, or environment reads. A workspace source crawl cannot substitute for
that frontier. The pinned `notify` 8.2.0 adapter reports some backend rescan
conditions, but drops the FSEvents event IDs and does not expose a stable
checkpoint/barrier. Its documentation also warns that event delivery can be
lost in some environments. A notification generation counter therefore does
not establish the no-loss fence needed to reuse semantic output. The pinned
`ra_ap_vfs-notify` 0.0.341 crate describes its watcher support as untested and
keeps watching disabled by default.

The only admitted workspace lifetime is one compiler operation: load Cargo and
RA from disk, apply the complete selected source frontier, lower the whole
package, then drop that RA database. A failed, cancelled, or unwound operation
drops its mutated database. Publication state stays owned by the compiler's
separate transaction. `workspace_reuses` therefore remains zero; no cache
authority or end-to-end delta speedup is claimed.

Within that fresh operation, `RustWorkspaceFile` is the complete current
package source frontier, matching the `PackageSourceSet` contract and the
engine's full `package.sources` handoff. Existing buffers replace VFS text.
Paths absent from disk can be added as virtual VFS files, and RA source roots
are rebuilt to include them before HIR queries. A package-local `.rs` path
loaded from disk but omitted from the current frontier is removed from the
operation's source root and VFS, so deleting or renaming a module does not
leave stale disk text available to module resolution. If RA does not expose one
unambiguous local source root for the selected package, admission fails closed.
Source paths resolve existing symlink components and reject any canonical
result outside the package root; a missing suffix is kept lexically beneath
the canonical root.

The pinned RA change API replaces the complete source-root vector, so a path
membership edit rebuilds its path sets. That work is bounded at 250,000 copied
root entries and counted in `overlay_root_entries_rebuilt`; wall time is
included in `source_update_nanos`. These counters make the setup cost visible,
but no benchmark has run yet and this change makes no performance claim.

This current-state source list does not carry Cargo metadata or generated-file
editor buffers. `Cargo.toml`, Cargo configuration and lock state, build-script
outputs, proc-macro products, and files outside the package's admitted Rust
source list still come from the fresh disk/Cargo load. Changing a crate target
root in an unsaved manifest cannot be applied by this interface because the
crate graph was already built from the disk manifest. Those edits need an
explicit typed manifest/build input and crate-graph update before they can be
claimed as editor-authoritative. Non-`.rs` module targets are also outside the
current omission-based delete reconciliation.

A future `WorkspaceReuseAllowed` admission token must come from a trusted,
no-loss filesystem observer or broker. It must subscribe before Cargo/RA load,
cover every actual read root and positive/negative probe, bind the owner key and
observer epoch, and provide a stable checkpoint at each reuse boundary. Any
overflow, stream gap, dropped-event/rescan flag, watcher error, cold restart,
root or mount change, symlink uncertainty, unobserved read root, or environment
change must revoke the token and force a fresh load. The generic `notify`
callback does not currently provide enough sequence/checkpoint evidence to
mint that token. Observer work must be measured separately from workspace
load, source-root update, and lowering work; a cache benefit is accepted only
after an end-to-end benchmark shows fewer total reads and lower wall time.

Before any future reuse admission is enabled, a live RA oracle must cover at
least: creation below a pre-existing empty module directory; same-size file
replacement with restored modification time; deletion and rename; symlink
retarget; manifest and build-script changes; observer overflow/rescan; cold
restart; and exact unsaved editor create/delete/rename patches. Current tests
exercise per-operation module create/delete/rename against live HIR resolution
and retain the explicit zero-reuse counters.
