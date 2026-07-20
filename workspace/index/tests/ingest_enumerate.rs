//! Direct-git enumeration tests (REGISTRYLESS-PLAN §7.4): a real `git init`
//! fixture repo (tagged + untagged) driven through **both** real adapters —
//! the default [`GritAdapter`] and the subprocess fallback
//! [`GitCommandAdapter`] — plus the git-monitor ref-diff and pseudo-version
//! path. The two subprocess-hardening regression tests remain bound to
//! [`GitCommandAdapter`] specifically (they test that adapter's argv/config
//! discipline); the grit adapter's hostile-URL refusal has its own test.

mod common;

use std::process::Command;

use common::FakeGitRepository;
use index::protocol::CatalogOp;

use index::ingest::enumerate::{cpp_stem_id, enumerate_git_versions};
use index::ingest::git::{GitCommandAdapter, GitRepository};
use index::ingest::grit::GritAdapter;
use index::ingest::monitor::{GitMonitor, TickOutcome};

/// `git` command in a directory with deterministic identity, arg-vector only.
fn git(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// Build a bare-ish local repo we can `ls-remote` via a `file://` URL.
fn init_repo(tagged: bool) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "-q", "-b", "main"]);
    std::fs::write(path.join("README"), b"hello").expect("write");
    git(path, &["add", "README"]);
    git(path, &["commit", "-q", "-m", "initial"]);
    if tagged {
        git(path, &["tag", "v1.0.0"]);
        std::fs::write(path.join("README"), b"world").expect("write");
        git(path, &["add", "README"]);
        git(path, &["commit", "-q", "-m", "second"]);
        // Annotated tag exercises the peeled-oid path.
        git(path, &["tag", "-a", "v1.1.0", "-m", "release 1.1.0"]);
    }
    let url = format!("file://{}", path.display());
    (dir, url)
}

/// The tagged-repo enumeration contract, generic over the adapter under test
/// so both the grit default and the subprocess fallback are held to it.
fn assert_tagged_repo_enumerates_one_version_per_tag(adapter: &impl GitRepository) {
    let (_dir, url) = init_repo(true);
    let slug = "example.test/lib";

    let ops = enumerate_git_versions(adapter, slug, &url, 20250101000000)
        .expect("enumerate tagged repo");

    let versions: Vec<&CatalogOp> = ops
        .iter()
        .filter(|op| matches!(op, CatalogOp::UpsertVersion { .. }))
        .collect();
    assert_eq!(versions.len(), 2, "one op per tag (v1.0.0, v1.1.0)");

    for op in versions {
        if let CatalogOp::UpsertVersion { coordinates, source, .. } = op {
            assert!(
                coordinates.version_canonical.starts_with("v1."),
                "tag preserved as canonical: {}",
                coordinates.version_canonical
            );
            let source = source.as_ref().expect("git source acquisition");
            let rev = source.source_rev.as_ref().expect("peeled oid pinned");
            assert_eq!(rev.len(), 40, "full commit oid pinned, got {rev:?}");
        }
    }
}

#[test]
fn tagged_repo_enumerates_one_version_per_tag_with_pinned_oid_via_git_command() {
    assert_tagged_repo_enumerates_one_version_per_tag(&GitCommandAdapter::default());
}

#[test]
fn tagged_repo_enumerates_one_version_per_tag_with_pinned_oid_via_grit() {
    assert_tagged_repo_enumerates_one_version_per_tag(&GritAdapter::default());
}

/// The untagged-repo pseudo-version contract, generic over the adapter.
fn assert_untagged_repo_yields_one_pseudo_version(adapter: &impl GitRepository) {
    let (_dir, url) = init_repo(false);
    let slug = "example.test/untagged";

    let ops = enumerate_git_versions(adapter, slug, &url, 20250101000000)
        .expect("enumerate untagged repo");

    let versions: Vec<&CatalogOp> = ops
        .iter()
        .filter(|op| matches!(op, CatalogOp::UpsertVersion { .. }))
        .collect();
    assert_eq!(versions.len(), 1, "exactly one pseudo-version from HEAD");

    if let CatalogOp::UpsertVersion { coordinates, source, .. } = versions[0] {
        // Go pseudo-version form 1: v0.0.0-<ts>-<hash12>.
        assert!(
            coordinates.version_canonical.starts_with("v0.0.0-20250101000000-"),
            "pseudo-version form 1: {}",
            coordinates.version_canonical
        );
        let rev = source.as_ref().and_then(|s| s.source_rev.as_ref()).expect("HEAD oid");
        assert_eq!(rev.len(), 40);
    }
}

#[test]
fn untagged_repo_yields_one_pseudo_version_from_head_via_git_command() {
    assert_untagged_repo_yields_one_pseudo_version(&GitCommandAdapter::default());
}

#[test]
fn untagged_repo_yields_one_pseudo_version_from_head_via_grit() {
    assert_untagged_repo_yields_one_pseudo_version(&GritAdapter::default());
}

/// The nonexistent-repo contract, generic over the adapter.
fn assert_nonexistent_repo_is_typed_error(adapter: &impl GitRepository) {
    let result =
        enumerate_git_versions(adapter, "x/y", "file:///nonexistent/repo/path", 20250101000000);
    assert!(result.is_err(), "nonexistent repo must be a typed error");
}

#[test]
fn nonexistent_repo_is_typed_error_not_panic_via_git_command() {
    assert_nonexistent_repo_is_typed_error(&GitCommandAdapter::default());
}

#[test]
fn nonexistent_repo_is_typed_error_not_panic_via_grit() {
    assert_nonexistent_repo_is_typed_error(&GritAdapter::default());
}

/// The two real adapters must agree ref-for-ref on a `file://` fixture —
/// object ids, ref names, peeled `^{}` companions, and HEAD — so the grit
/// swap is verified behaviorally, not assumed.
#[test]
fn grit_and_git_command_adapters_agree_on_fixture_refs() {
    let (_dir, url) = init_repo(true);
    let grit_adapter = GritAdapter::default();
    let command_adapter = GitCommandAdapter::default();

    let mut grit_refs = grit_adapter.list_remote_refs(&url).expect("grit ls-remote");
    let mut command_refs = command_adapter.list_remote_refs(&url).expect("git ls-remote");
    grit_refs.sort_by(|a, b| a.reference.cmp(&b.reference));
    command_refs.sort_by(|a, b| a.reference.cmp(&b.reference));
    assert_eq!(grit_refs, command_refs, "adapters must advertise identical ref sets");
    assert!(
        grit_refs.iter().any(|entry| entry.reference.ends_with("^{}")),
        "fixture's annotated tag must produce a peeled entry"
    );

    let grit_head = grit_adapter.head_object_id(&url).expect("grit HEAD");
    let command_head = command_adapter.head_object_id(&url).expect("git HEAD");
    assert_eq!(grit_head, command_head, "adapters must agree on the HEAD oid");
    assert!(grit_head.is_some(), "fixture has a HEAD");
}

/// The grit adapter must refuse a hostile `ext::` URL with a typed error and
/// perform no I/O at all (it has no subprocess, so nothing can run — this
/// asserts the refusal is the *typed* [`UnsupportedTransport`] variant).
///
/// [`UnsupportedTransport`]: index::ingest::git::GitRepositoryError::UnsupportedTransport
#[test]
fn grit_adapter_refuses_ext_transport_url_with_typed_error() {
    let adapter = GritAdapter::default();
    let result = adapter.ls_remote_bytes("ext::sh -c date");
    assert!(
        matches!(
            result,
            Err(index::ingest::git::GitRepositoryError::UnsupportedTransport { ref scheme, .. })
                if scheme == "ext"
        ),
        "ext:: must map to the typed UnsupportedTransport variant, got {result:?}"
    );
}

/// A hostile feed URL naming git's `ext::` transport — which *executes a shell
/// command* — must be refused by the adapter, never run. The adapter force-pins
/// `protocol.ext.allow=never`, so this holds independent of the host's git
/// version or inherited global/system config.
#[test]
fn ext_transport_url_is_refused_and_command_never_runs() {
    // A sentinel the injected command would create if the transport ran.
    let sentinel = std::env::temp_dir().join(format!("ingestor_ext_pwn_{}", std::process::id()));
    let _ = std::fs::remove_file(&sentinel);
    let hostile = format!("ext::sh -c touch>&2 {}", sentinel.display());

    let adapter = GitCommandAdapter::default();
    let result = adapter.ls_remote_bytes(&hostile);

    assert!(result.is_err(), "ext:: transport must be refused, got {result:?}");
    assert!(
        !sentinel.exists(),
        "the injected shell command must never execute (sentinel present)"
    );
    let _ = std::fs::remove_file(&sentinel);
}

/// A URL crafted to look like a git option (`--upload-pack=…`) must be consumed
/// as the positional repository argument, never as a flag — the adapter places
/// the URL after a `--` separator. git rejects it as a strange pathname, and no
/// `--upload-pack` command runs.
#[test]
fn option_shaped_url_is_positional_not_a_flag() {
    let sentinel = std::env::temp_dir().join(format!("ingestor_up_pwn_{}", std::process::id()));
    let _ = std::fs::remove_file(&sentinel);
    let hostile = format!("--upload-pack=touch {}", sentinel.display());

    let adapter = GitCommandAdapter::default();
    let result = adapter.ls_remote_bytes(&hostile);

    assert!(result.is_err(), "option-shaped URL must fail as a repo, not inject a flag");
    assert!(!sentinel.exists(), "no --upload-pack command may run");
    let _ = std::fs::remove_file(&sentinel);
}

#[test]
fn monitor_reports_unchanged_when_ref_digest_matches() {
    let url = "https://example.test/repo.git";
    let ls_remote = b"aaaa\trefs/tags/v1.0.0\n";
    let fake = FakeGitRepository::new().with_ls_remote(url, ls_remote);
    let monitor = GitMonitor::new(fake);
    let stem = cpp_stem_id("example.test/repo");

    // First tick with no watermark → Moved (records the digest).
    let first = monitor.tick(stem, "example.test/repo", url, None, 1000, 20250101000000).unwrap();
    let digest = match first {
        TickOutcome::Moved { rev, ops } => {
            assert!(
                ops.iter().any(|op| matches!(op, CatalogOp::SourceMoved { .. })),
                "first observation emits SourceMoved"
            );
            rev
        }
        TickOutcome::Unchanged { .. } => panic!("first tick must be a move"),
    };

    // Second tick with the same digest as last_rev → Unchanged, no ops.
    let second =
        monitor.tick(stem, "example.test/repo", url, Some(&digest), 2000, 20250101000000).unwrap();
    assert!(matches!(second, TickOutcome::Unchanged { .. }), "matching digest is unchanged");
}

#[test]
fn monitor_emits_source_moved_and_versions_on_change() {
    let url = "https://example.test/repo.git";
    let fake = FakeGitRepository::new().with_ls_remote(url, b"bbbb\trefs/tags/v2.0.0\n");
    let monitor = GitMonitor::new(fake);
    let stem = cpp_stem_id("example.test/repo");

    // Stale watermark (old digest) → move.
    let outcome = monitor
        .tick(stem, "example.test/repo", url, Some("stale-digest"), 3000, 20250101000000)
        .unwrap();
    match outcome {
        TickOutcome::Moved { ops, .. } => {
            assert!(matches!(ops[0], CatalogOp::SourceMoved { .. }), "SourceMoved leads the batch");
            assert!(
                ops.iter().any(|op| matches!(op, CatalogOp::UpsertVersion { .. })),
                "move re-enumerates versions"
            );
        }
        TickOutcome::Unchanged { .. } => panic!("stale watermark must read as a move"),
    }
}

#[test]
fn monitor_git_failure_is_typed_error() {
    let url = "https://example.test/gone.git";
    let fake = FakeGitRepository::new().with_ls_remote_error(url, "repository not found");
    let monitor = GitMonitor::new(fake);
    let stem = cpp_stem_id("example.test/gone");
    let result = monitor.tick(stem, "example.test/gone", url, None, 4000, 20250101000000);
    assert!(result.is_err(), "adapter failure surfaces as a typed MonitorError");
}
