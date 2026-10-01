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
RA from disk, overlay the selected source buffers, lower the requested unit,
then drop that RA database. A failed, cancelled, or unwound operation drops
its mutated database. Publication state stays owned by the compiler's separate
transaction. `workspace_reuses` therefore remains zero; no cache authority or
end-to-end delta speedup is claimed.

`PackageSourceSet` and `RustWorkspaceFile` carry caller-selected buffers, not a
complete package inventory. The constructor validates path spelling, order,
cardinality, and target membership; it does not prove that every Rust module,
generated file, ignored file, or symlinked path is represented. The local
discovery caller also filters some ignored/generated paths. Therefore an
omitted `.rs` path is never interpreted as a deletion. A future editor delete
or rename needs a separate typed tombstone bound to its editor authority.
Existing buffers replace VFS text, and paths absent from disk can be added as
virtual VFS files. Added and existing paths keep their lexical VFS identities
while canonical resolution is used to reject paths escaping the package root.
Selected existing paths may span multiple RA source roots; each is updated by
its own VFS file ID. A new virtual file is attached to its nearest unambiguous
local source root, and admission fails closed if that path has no unique owner.

The pinned RA change API replaces the complete source-root vector when a
virtual file is added, so the path sets are rebuilt. That work is bounded at
250,000 copied root entries and counted in `overlay_root_entries_rebuilt`; wall
time is included in `source_update_nanos`. No file membership is removed by
omission, and `overlay_sources_removed` remains zero until a typed tombstone
path is implemented. These counters make setup cost visible, but no benchmark
has run and this change makes no performance claim. `selected_source_roots_touched`
accumulates the number of distinct local roots with selected files per operation,
including multi-crate workspaces.

This selected-buffer list does not carry Cargo metadata or generated-file
editor buffers. `Cargo.toml`, Cargo configuration and lock state, build-script
outputs, proc-macro products, and unselected files still come from the fresh
disk/Cargo load. Changing a crate target root in an unsaved manifest cannot be
applied by this interface because the crate graph was already built from the
disk manifest. Those edits need an explicit typed manifest/build input and
crate-graph update before they can be claimed as editor-authoritative.
Non-`.rs` module targets likewise need explicit editor inputs.

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
exercise unsaved nested module creation, omitted `#[path]` module preservation,
symlinked module buffers, and selected updates across workspace roots. They also
verify that source-list omission does not remove disk files. Explicit
delete/rename authority remains unimplemented, and cross-call reuse stays
disabled.
