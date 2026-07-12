//! Pipeline part: **VCS version resolution** (`compiler::languages::*::traversal`).
//!
//! Specs for finding the commit that declares a requested version and
//! materializing its tree. Mirrors the old `producers::vcs` guarantees,
//! including the "scan every ref tip, newest→oldest" behaviour.
//!
//! Fixtures are built with the system `git` CLI (offline, local-only). Clone
//! tests use `file://` remotes so no network is required.

use std::fs;
use std::path::Path;
use std::process::Command;

use compiler::languages::rust::traversal::find_commit_for_version as find_rust_commit;
use compiler::languages::typescript::traversal::find_commit_for_version as find_ts_commit;
use compiler::languages::vcs::{materialize_commit, open_or_clone_repository};
use semver::Version;
use tempfile::TempDir;
use url::Url;

// ─── Git fixture helpers ─────────────────────────────────────────────────────

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git {:?}: {e}", args));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_repo(dir: &Path) {
    fs::create_dir_all(dir).expect("create repo dir");
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    // Global commit.gpgsign can be on; fixtures must never require a key.
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["config", "tag.gpgsign", "false"]);
}

fn write_file(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(&path, contents).expect("write file");
}

fn commit_all(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn file_url(path: &Path) -> Url {
    Url::from_file_path(path)
        .unwrap_or_else(|_| panic!("path must be absolute for file URL: {}", path.display()))
}

// ─── Specs ───────────────────────────────────────────────────────────────────

/// The commit for a version is found by reading `Cargo.toml` from the tree.
///
/// Arrange: a git fixture whose HEAD `Cargo.toml` is `mycrate@1.2.0`.
/// Act: `rust::traversal::find_commit_for_version(&repo, "1.2.0")`.
/// Assert: returns the HEAD commit.
#[test]
fn finds_rust_commit_for_version_at_head() {
    let root = TempDir::new().expect("tempdir");
    let fixture = root.path().join("fixture");
    init_repo(&fixture);
    write_file(
        &fixture,
        "Cargo.toml",
        r#"[package]
name = "mycrate"
version = "1.2.0"
edition = "2021"
"#,
    );
    write_file(&fixture, "src/lib.rs", "pub fn f() {}\n");
    let head = commit_all(&fixture, "mycrate 1.2.0");

    let cache = root.path().join("cache");
    let repo = open_or_clone_repository(&cache, &file_url(&fixture)).expect("clone fixture");
    // After clone, HEAD may differ in object id? No — same content, same commit
    // objects when cloning. The oid of the cloned HEAD equals the source HEAD.
    let version = Version::parse("1.2.0").unwrap();
    let found = find_rust_commit(&repo, &version, "mycrate", None)
        .expect("version 1.2.0 must resolve");

    assert_eq!(
        found.to_hex().to_string(),
        head,
        "resolved commit must be the fixture HEAD"
    );
}

/// Resolution scans all refs, not just HEAD's first-parent history.
///
/// Arrange: the target version is only reachable on a non-HEAD branch.
/// Assert: it is still found (walks every peeled ref tip).
#[test]
fn scans_all_refs_not_just_head_history() {
    let root = TempDir::new().expect("tempdir");
    let fixture = root.path().join("fixture");
    init_repo(&fixture);

    // main @ 1.0.0
    write_file(
        &fixture,
        "Cargo.toml",
        r#"[package]
name = "mycrate"
version = "1.0.0"
edition = "2021"
"#,
    );
    write_file(&fixture, "src/lib.rs", "pub fn f() {}\n");
    commit_all(&fixture, "mycrate 1.0.0");

    // side branch @ 2.0.0 (not on main history)
    git(&fixture, &["checkout", "-b", "release-2"]);
    write_file(
        &fixture,
        "Cargo.toml",
        r#"[package]
name = "mycrate"
version = "2.0.0"
edition = "2021"
"#,
    );
    let side_head = commit_all(&fixture, "mycrate 2.0.0");

    // Return to main so HEAD history does not include 2.0.0
    git(&fixture, &["checkout", "main"]);

    // Mirror all branches into a bare remote, then clone — so the side branch
    // is present as a remote-tracking ref the walker can peel.
    let bare = root.path().join("bare.git");
    git(root.path(), &["clone", "--bare", fixture.to_str().unwrap(), bare.to_str().unwrap()]);
    // Ensure the side branch tip is advertised (bare clone of a worktree keeps
    // branches present as refs/heads/*).
    let side_in_bare = git(&bare, &["rev-parse", "release-2"]);
    assert_eq!(side_in_bare, side_head);

    let cache = root.path().join("cache");
    let repo = open_or_clone_repository(&cache, &file_url(&bare)).expect("clone bare");

    let version = Version::parse("2.0.0").unwrap();
    let found = find_rust_commit(&repo, &version, "mycrate", None)
        .expect("version on side branch must still be found");

    assert_eq!(
        found.to_hex().to_string(),
        side_head,
        "must resolve to the side-branch tip, not something on main"
    );
}

/// Workspace-inherited versions (`version = { workspace = true }`) resolve.
///
/// Assert: a member crate inheriting the workspace version resolves to the
///   commit where the workspace root declared it.
#[test]
fn resolves_workspace_inherited_versions() {
    let root = TempDir::new().expect("tempdir");
    let fixture = root.path().join("fixture");
    init_repo(&fixture);

    write_file(
        &fixture,
        "Cargo.toml",
        r#"[workspace]
members = ["member"]

[workspace.package]
version = "3.1.0"
"#,
    );
    write_file(
        &fixture,
        "member/Cargo.toml",
        r#"[package]
name = "member"
version = { workspace = true }
edition = "2021"
"#,
    );
    write_file(&fixture, "member/src/lib.rs", "pub fn f() {}\n");
    let head = commit_all(&fixture, "workspace member 3.1.0");

    let cache = root.path().join("cache");
    let repo = open_or_clone_repository(&cache, &file_url(&fixture)).expect("clone fixture");
    let version = Version::parse("3.1.0").unwrap();
    let found = find_rust_commit(&repo, &version, "member", None)
        .expect("workspace-inherited version must resolve");

    assert_eq!(found.to_hex().to_string(), head);
}

/// TypeScript resolution reads `package.json` name/version from the tree.
///
/// Act: `typescript::traversal::find_commit_for_version(&repo, "1.2.3")`.
/// Assert: returns the commit whose `package.json` declares `1.2.3`.
#[test]
fn finds_typescript_commit_for_version() {
    let root = TempDir::new().expect("tempdir");
    let fixture = root.path().join("fixture");
    init_repo(&fixture);
    write_file(
        &fixture,
        "package.json",
        r#"{
  "name": "mypkg",
  "version": "1.2.3"
}
"#,
    );
    write_file(&fixture, "index.ts", "export const x = 1;\n");
    let head = commit_all(&fixture, "mypkg 1.2.3");

    let cache = root.path().join("cache");
    let repo = open_or_clone_repository(&cache, &file_url(&fixture)).expect("clone fixture");
    let version = Version::parse("1.2.3").unwrap();
    let found =
        find_ts_commit(&repo, &version, "mypkg", None).expect("package.json version must resolve");

    assert_eq!(found.to_hex().to_string(), head);
}

/// Materializing a commit checks its tree out into a workspace dir.
///
/// Assert: `materialize_commit(commit, dest)` populates `dest` with the source
///   tree at that commit.
#[test]
fn materializes_commit_tree_into_workspace() {
    let root = TempDir::new().expect("tempdir");
    let fixture = root.path().join("fixture");
    init_repo(&fixture);
    write_file(
        &fixture,
        "Cargo.toml",
        r#"[package]
name = "mycrate"
version = "0.4.0"
edition = "2021"
"#,
    );
    write_file(
        &fixture,
        "src/lib.rs",
        "pub const MARKER: &str = \"materialize-me\";\n",
    );
    let head = commit_all(&fixture, "materialize fixture");

    let cache = root.path().join("cache");
    let repo = open_or_clone_repository(&cache, &file_url(&fixture)).expect("clone fixture");
    let version = Version::parse("0.4.0").unwrap();
    let commit =
        find_rust_commit(&repo, &version, "mycrate", None).expect("version resolves");
    assert_eq!(commit.to_hex().to_string(), head);

    let dest = root.path().join("materialized");
    materialize_commit(&repo, commit, &dest).expect("materialize succeeds");

    let lib = dest.join("src/lib.rs");
    assert!(lib.is_file(), "src/lib.rs must exist in materialized tree");
    let contents = fs::read_to_string(&lib).expect("read lib.rs");
    assert!(
        contents.contains("materialize-me"),
        "materialized source must match the commit tree: {contents}"
    );
    assert!(dest.join("Cargo.toml").is_file(), "Cargo.toml must be materialized");
}

/// A cached clone whose origin URL differs is re-cloned, not reused.
///
/// Assert: `open_or_clone_repository` wipes + re-clones when the cached remote
///   URL no longer matches the requested source.
#[test]
fn reclones_when_cached_remote_differs() {
    let root = TempDir::new().expect("tempdir");

    // Remote A
    let remote_a = root.path().join("remote_a");
    init_repo(&remote_a);
    write_file(&remote_a, "marker.txt", "from-a\n");
    commit_all(&remote_a, "remote a");

    // Remote B (different content + URL)
    let remote_b = root.path().join("remote_b");
    init_repo(&remote_b);
    write_file(&remote_b, "marker.txt", "from-b\n");
    commit_all(&remote_b, "remote b");

    let cache = root.path().join("cache");

    // Seed cache from A
    let _repo_a =
        open_or_clone_repository(&cache, &file_url(&remote_a)).expect("initial clone of A");
    assert_eq!(
        fs::read_to_string(cache.join("marker.txt")).unwrap().trim(),
        "from-a"
    );

    // Request B against the same cache path — must wipe + reclone
    let _repo_b =
        open_or_clone_repository(&cache, &file_url(&remote_b)).expect("reclone of B over stale cache");
    let marker = fs::read_to_string(cache.join("marker.txt")).expect("marker after reclone");
    assert_eq!(
        marker.trim(),
        "from-b",
        "cache must contain remote B after URL mismatch reclones"
    );
}
