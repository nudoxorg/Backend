//! Pipeline part: **VCS version resolution** (`compiler::languages::*::traversal`).
//!
//! TDD specs for finding the commit that declares a requested version and
//! materializing its tree. Mirrors the old `producers::vcs` guarantees,
//! including the "scan every ref tip, newest→oldest" behaviour.

/// The commit for a version is found by reading `Cargo.toml` from the tree.
///
/// Arrange: a git fixture whose HEAD `Cargo.toml` is `mycrate@1.2.0`.
/// Act: `rust::traversal::find_commit_for_version(&repo, "1.2.0")`.
/// Assert: returns the HEAD commit.
#[test]
fn finds_rust_commit_for_version_at_head() {
    todo!("build a git fixture and assert the resolved commit");
}

/// Resolution scans all refs, not just HEAD's first-parent history.
///
/// Arrange: the target version is only reachable on a non-HEAD branch.
/// Assert: it is still found (walks every peeled ref tip).
#[test]
fn scans_all_refs_not_just_head_history() {
    todo!("assert a version on a side branch is found");
}

/// Workspace-inherited versions (`version = { workspace = true }`) resolve.
///
/// Assert: a member crate inheriting the workspace version resolves to the
///   commit where the workspace root declared it.
#[test]
fn resolves_workspace_inherited_versions() {
    todo!("assert workspace-inherited version resolution");
}

/// TypeScript resolution reads `package.json` name/version from the tree.
///
/// Act: `typescript::traversal::find_commit_for_version(&repo, "1.2.3")`.
/// Assert: returns the commit whose `package.json` declares `1.2.3`.
#[test]
fn finds_typescript_commit_for_version() {
    todo!("assert package.json-based TS commit resolution");
}

/// Materializing a commit checks its tree out into a workspace dir.
///
/// Assert: `materialize_commit(commit, dest)` populates `dest` with the source
///   tree at that commit.
#[test]
fn materializes_commit_tree_into_workspace() {
    todo!("assert the materialized worktree contents");
}

/// A cached clone whose origin URL differs is re-cloned, not reused.
///
/// Assert: `open_or_clone_repository` wipes + re-clones when the cached remote
///   URL no longer matches the requested source.
#[test]
fn reclones_when_cached_remote_differs() {
    todo!("assert stale cached clones are replaced");
}
