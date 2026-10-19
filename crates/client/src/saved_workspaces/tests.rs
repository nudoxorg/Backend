#![allow(clippy::expect_used)]
use super::*;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    projects: PathBuf,
    first: PathBuf,
    second: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = backend_runtime::normalize_surface_path(
            std::env::temp_dir()
                .canonicalize()
                .expect("canonical temp parent"),
        )
        .join(format!(
            "nudox-saved-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        backend_platform::durable::ensure_private_directory(&root).expect("private fixture");
        let projects = root.join("projects");
        backend_platform::durable::ensure_private_directory(&projects).expect("private projects");
        let first = root.join("first-project");
        let second = root.join("second-project");
        for project in [&first, &second] {
            backend_platform::durable::ensure_private_directory(project).expect("project");
        }
        Self {
            root,
            projects,
            first,
            second,
        }
    }
    fn store(&self, project: &Path, secret: u8, index: &[u8]) -> PathBuf {
        let data = backend_runtime::default_project_workspace_in(project, &self.projects);
        backend_platform::durable::ensure_private_directory(&data).expect("private store");
        let cap = DirectoryCapability::open(&data).expect("store handle");
        for (name, bytes) in [
            ("authority.secret", vec![secret; 32]),
            ("OWNER.state", index.to_vec()),
            ("retained-index-fixture", index.to_vec()),
        ] {
            let mut file = cap.create_file_exclusive(name).expect("private owned file");
            file.write_all(&bytes).expect("fixture bytes");
            file.sync_all().expect("fixture flush");
        }
        data
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn existing_legacy_stores_without_terminal_claims_remain_isolated() {
    let f = Fixture::new();
    let first = f.store(&f.first, 11, b"retained first index");
    let second = f.store(&f.second, 22, b"retained second index");
    assert!(!first.join("client-index-operations-v1").exists());
    assert!(!second.join("client-index-operations-v1").exists());
    for (project, data, expected, secret) in [
        (&f.first, &first, b"retained first index".as_slice(), 11),
        (&f.second, &second, b"retained second index".as_slice(), 22),
    ] {
        let candidate = find_in(project, &f.projects).expect("selected existing store");
        let paths = candidate
            .identify(&[f.second.clone(), f.first.clone()])
            .expect("only exact original project");
        assert_eq!(paths.project(), project);
        assert_eq!(paths.data(), data);
        assert_eq!(paths.endpoint(), backend_runtime::derive_endpoint(data));
        assert_eq!(
            std::fs::read(data.join("retained-index-fixture")).expect("retained index"),
            expected
        );
        assert_eq!(
            std::fs::read(data.join("authority.secret")).expect("retained secret"),
            [secret; 32]
        );
        assert!(
            !data.join("client-index-operations-v1").exists(),
            "no new catalog or claim"
        );
    }
}

#[test]
fn empty_or_unrelated_owner_shelf_does_not_guess_a_project_or_use_the_gui_cwd() {
    let f = Fixture::new();
    let data = f.store(&f.first, 11, b"index");
    let candidate = find_in(&f.first, &f.projects).expect("selected existing store");
    assert!(candidate.identify(&[]).is_err());
    assert!(candidate.identify(std::slice::from_ref(&f.second)).is_err());
    assert_eq!(
        std::fs::read(data.join("retained-index-fixture")).expect("original"),
        b"index"
    );
}

#[test]
fn missing_default_store_is_not_created_or_indexed() {
    let f = Fixture::new();
    let kept = f.store(&f.first, 11, b"kept");
    let absent = backend_runtime::default_project_workspace_in(&f.second, &f.projects);
    assert!(find_in(&f.second, &f.projects).is_err());
    assert!(!absent.exists());
    let absent_parent = f.root.join("absent-platform-state/projects");
    assert!(find_in(&f.first, &absent_parent).is_err());
    assert!(!absent_parent.exists());
    assert_eq!(
        std::fs::read(kept.join("retained-index-fixture")).expect("retained"),
        b"kept"
    );
}

#[test]
fn changed_authority_or_replaced_store_refuses_the_old_candidate() {
    let f = Fixture::new();
    let data = f.store(&f.first, 11, b"retained");
    let candidate = find_in(&f.first, &f.projects).expect("selected store");
    std::fs::write(data.join("authority.secret"), [12; 32]).expect("same inode changed authority");
    assert!(candidate.verify().is_err());
    std::fs::write(data.join("authority.secret"), [11; 32]).expect("restore bytes");
    candidate.verify().expect("same credential generation");
    let kept = f.root.join("retained-original");
    std::fs::rename(&data, &kept).expect("retain old store");
    f.store(&f.first, 11, b"replacement");
    assert!(
        candidate.verify().is_err(),
        "same path/secret cannot lend a replacement the old identity"
    );
    assert_eq!(
        std::fs::read(kept.join("retained-index-fixture")).expect("original kept"),
        b"retained"
    );
}

#[test]
fn a_resolved_binding_cannot_be_redirected_to_another_project_or_endpoint() {
    let f = Fixture::new();
    let data = f.store(&f.first, 11, b"retained");
    let candidate = find_in(&f.first, &f.projects).expect("selected store");
    let selected = candidate
        .identify(std::slice::from_ref(&f.first))
        .expect("exact project");
    candidate.revalidate(&selected).expect("unchanged binding");
    let other_project = WorkspacePaths::discover(
        Some(f.second.clone()),
        Some(data.clone()),
        Some(backend_runtime::derive_endpoint(&data)),
    )
    .expect("paths only");
    assert!(candidate.revalidate(&other_project).is_err());
    let other_endpoint = WorkspacePaths::discover(
        Some(f.first.clone()),
        Some(data),
        Some(backend_runtime::derive_endpoint(
            &f.root.join("other-store"),
        )),
    )
    .expect("paths only");
    assert!(candidate.revalidate(&other_endpoint).is_err());
}

#[cfg(unix)]
#[test]
fn aliased_default_child_credential_or_state_parent_is_refused() {
    let f = Fixture::new();
    let data = f.store(&f.first, 11, b"retained");
    let alias = backend_runtime::default_project_workspace_in(&f.second, &f.projects);
    std::os::unix::fs::symlink(&data, &alias).expect("alias fixture");
    assert!(find_in(&f.second, &f.projects).is_err());
    std::fs::remove_file(data.join("authority.secret")).expect("remove fixture secret");
    std::os::unix::fs::symlink(f.root.join("missing-secret"), data.join("authority.secret"))
        .expect("credential alias");
    assert!(find_in(&f.first, &f.projects).is_err());
    std::fs::remove_file(data.join("authority.secret")).expect("remove credential alias");
    let cap = DirectoryCapability::open(&data).expect("original store");
    let mut file = cap
        .create_file_exclusive("authority.secret")
        .expect("restored private credential");
    file.write_all(&[11; 32]).expect("restore secret");
    drop(file);
    let root_alias = f.root.join("aliased-projects-root");
    std::os::unix::fs::symlink(&f.projects, &root_alias).expect("parent alias fixture");
    assert!(find_in(&f.first, &root_alias).is_err());
}

#[test]
fn credential_only_directory_is_not_admitted_as_an_existing_index() {
    let f = Fixture::new();
    let data = f.store(&f.first, 11, b"retained");
    std::fs::remove_file(data.join("OWNER.state")).expect("unstarted fixture");
    assert!(find_in(&f.first, &f.projects).is_err());
    assert_eq!(
        std::fs::read(data.join("authority.secret")).expect("preserved"),
        [11; 32]
    );
}

#[test]
fn selected_folder_requires_an_absolute_existing_directory_and_accepts_normalized_identity() {
    let f = Fixture::new();
    let data = f.store(&f.first, 11, b"retained");
    assert!(find_in(Path::new("relative-project"), &f.projects).is_err());
    assert!(find_in(&f.root.join("missing-project"), &f.projects).is_err());
    let file = f.root.join("a-file");
    std::fs::write(&file, b"not a directory").expect("file fixture");
    assert!(find_in(&file, &f.projects).is_err());
    assert_eq!(
        find_in(&f.first.join("."), &f.projects)
            .expect("normal spelling")
            .data(),
        data
    );
    #[cfg(unix)]
    {
        let alias = f.root.join("selected-project-alias");
        std::os::unix::fs::symlink(&f.first, &alias).expect("user-selected folder alias");
        assert_eq!(
            find_in(&alias, &f.projects)
                .expect("same canonical project")
                .data(),
            data
        );
    }
}

#[test]
fn nested_git_package_uses_parent_owner_and_requires_exact_selected_package() {
    let f = Fixture::new();
    backend_platform::durable::ensure_private_directory(&f.first.join(".git"))
        .expect("Git boundary marker");
    std::fs::write(f.first.join("Cargo.toml"), "[workspace]\nmembers = []\n")
        .expect("outer workspace marker");
    let package = f.first.join("crates/member");
    backend_platform::durable::ensure_private_directory(&package).expect("nested package");
    std::fs::write(package.join("Cargo.toml"), "[package]\nname = \"member\"\n")
        .expect("nested manifest marker");
    let owner_data = f.store(&f.first, 11, b"CLI owner selected from nested cwd");
    let explicit_data = f.store(&package, 22, b"separate explicit project selection");
    let candidate = find_in(&package, &f.projects).expect("shared default owner context");
    assert_eq!(candidate.project, f.first);
    assert_eq!(candidate.package, package);
    assert_eq!(candidate.data(), owner_data);
    assert_ne!(candidate.data(), explicit_data);
    assert!(
        candidate.identify(std::slice::from_ref(&f.first)).is_err(),
        "an indexed owner root does not prove the selected package is indexed"
    );
    let binding = candidate
        .identify(std::slice::from_ref(&package))
        .expect("authenticated exact selected package inside parent owner");
    assert_eq!(binding.project(), f.first);
    assert_eq!(binding.data(), owner_data);
    candidate
        .revalidate(&binding)
        .expect("same owner/package binding");
    assert_eq!(
        std::fs::read(explicit_data.join("retained-index-fixture"))
            .expect("unrelated explicit store unchanged"),
        b"separate explicit project selection"
    );
}

#[test]
fn nearest_non_git_manifest_owner_and_changed_boundary_preserve_package_isolation() {
    let f = Fixture::new();
    std::fs::write(f.first.join("Cargo.toml"), "[workspace]\nmembers = []\n")
        .expect("outer manifest");
    let member = f.first.join("crates/member");
    let package = member.join("src/domain");
    backend_platform::durable::ensure_private_directory(&package).expect("selected nested folder");
    std::fs::write(member.join("Cargo.toml"), "[package]\nname = \"member\"\n")
        .expect("nearest manifest");
    let data = f.store(&member, 11, b"nearest manifest owner index");
    let candidate = find_in(&package, &f.projects).expect("nearest manifest owns package");
    assert_eq!(candidate.project, member);
    assert_eq!(candidate.package, package);
    assert_eq!(candidate.data(), data);
    assert!(candidate.identify(std::slice::from_ref(&f.second)).is_err());
    let binding = candidate
        .identify(std::slice::from_ref(&package))
        .expect("exact package read");
    candidate
        .revalidate(&binding)
        .expect("unchanged nearest owner");
    backend_platform::durable::ensure_private_directory(&f.first.join(".git"))
        .expect("changed Git boundary");
    assert!(
        candidate.revalidate(&binding).is_err(),
        "new boundary cannot redirect old candidate"
    );
    let absent_owner = backend_runtime::default_project_workspace_in(&f.first, &f.projects);
    assert!(find_in(&package, &f.projects).is_err());
    assert!(
        !absent_owner.exists(),
        "a new owner context is never initialized by the picker"
    );
    assert_eq!(
        std::fs::read(data.join("retained-index-fixture")).expect("retained old owner"),
        b"nearest manifest owner index"
    );
}
