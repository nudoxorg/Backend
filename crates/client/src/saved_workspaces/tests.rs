use super::*;
use std::error::Error;
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
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = backend_runtime::normalize_surface_path(std::env::temp_dir().canonicalize()?)
            .join(format!(
                "nudox-saved-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        backend_platform::durable::ensure_private_directory(&root)?;
        let projects = root.join("projects");
        backend_platform::durable::ensure_private_directory(&projects)?;
        let first = root.join("first-project");
        let second = root.join("second-project");
        for project in [&first, &second] {
            backend_platform::durable::ensure_private_directory(project)?;
        }
        Ok(Self {
            root,
            projects,
            first,
            second,
        })
    }
    fn store(&self, project: &Path, secret: u8, index: &[u8]) -> Result<PathBuf, Box<dyn Error>> {
        let data = backend_runtime::default_project_workspace_in(project, &self.projects);
        backend_platform::durable::ensure_private_directory(&data)?;
        let cap = DirectoryCapability::open(&data)?;
        for (name, bytes) in [
            ("authority.secret", vec![secret; 32]),
            ("OWNER.state", index.to_vec()),
            ("retained-index-fixture", index.to_vec()),
        ] {
            let mut file = cap.create_file_exclusive(name)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        Ok(data)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn existing_legacy_stores_without_terminal_claims_remain_isolated() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    let first = f.store(&f.first, 11, b"retained first index")?;
    let second = f.store(&f.second, 22, b"retained second index")?;
    assert!(!first.join("client-index-operations-v1").exists());
    assert!(!second.join("client-index-operations-v1").exists());
    for (project, data, expected, secret) in [
        (&f.first, &first, b"retained first index".as_slice(), 11),
        (&f.second, &second, b"retained second index".as_slice(), 22),
    ] {
        let candidate = find_in(project, &f.projects)?;
        let paths = candidate.identify(&[f.second.clone(), f.first.clone()])?;
        assert_eq!(paths.project(), project);
        assert_eq!(paths.data(), data);
        assert_eq!(paths.endpoint(), backend_runtime::derive_endpoint(data));
        assert_eq!(
            std::fs::read(data.join("retained-index-fixture"))?,
            expected
        );
        assert_eq!(std::fs::read(data.join("authority.secret"))?, [secret; 32]);
        assert!(
            !data.join("client-index-operations-v1").exists(),
            "no new catalog or claim"
        );
    }
    Ok(())
}

#[test]
fn empty_or_unrelated_owner_shelf_does_not_guess_a_project_or_use_the_gui_cwd()
-> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    let data = f.store(&f.first, 11, b"index")?;
    let candidate = find_in(&f.first, &f.projects)?;
    assert!(candidate.identify(&[]).is_err());
    assert!(candidate.identify(std::slice::from_ref(&f.second)).is_err());
    assert_eq!(
        std::fs::read(data.join("retained-index-fixture"))?,
        b"index"
    );
    Ok(())
}

#[test]
fn missing_default_store_is_not_created_or_indexed() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    let kept = f.store(&f.first, 11, b"kept")?;
    let absent = backend_runtime::default_project_workspace_in(&f.second, &f.projects);
    assert!(find_in(&f.second, &f.projects).is_err());
    assert!(!absent.exists());
    let absent_parent = f.root.join("absent-platform-state/projects");
    assert!(find_in(&f.first, &absent_parent).is_err());
    assert!(!absent_parent.exists());
    assert_eq!(std::fs::read(kept.join("retained-index-fixture"))?, b"kept");
    Ok(())
}

#[test]
fn changed_authority_or_replaced_store_refuses_the_old_candidate() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    let data = f.store(&f.first, 11, b"retained")?;
    let candidate = find_in(&f.first, &f.projects)?;
    std::fs::write(data.join("authority.secret"), [12; 32])?;
    assert!(candidate.verify().is_err());
    std::fs::write(data.join("authority.secret"), [11; 32])?;
    candidate.verify()?;
    let kept = f.root.join("retained-original");
    std::fs::rename(&data, &kept)?;
    f.store(&f.first, 11, b"replacement")?;
    assert!(
        candidate.verify().is_err(),
        "same path/secret cannot lend a replacement the old identity"
    );
    assert_eq!(
        std::fs::read(kept.join("retained-index-fixture"))?,
        b"retained"
    );
    Ok(())
}

#[test]
fn a_resolved_binding_cannot_be_redirected_to_another_project_or_endpoint()
-> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    let data = f.store(&f.first, 11, b"retained")?;
    let candidate = find_in(&f.first, &f.projects)?;
    let selected = candidate.identify(std::slice::from_ref(&f.first))?;
    candidate.revalidate(&selected)?;
    let other_project = WorkspacePaths::discover(
        Some(f.second.clone()),
        Some(data.clone()),
        Some(backend_runtime::derive_endpoint(&data)),
    )?;
    assert!(candidate.revalidate(&other_project).is_err());
    let other_endpoint = WorkspacePaths::discover(
        Some(f.first.clone()),
        Some(data),
        Some(backend_runtime::derive_endpoint(
            &f.root.join("other-store"),
        )),
    )?;
    assert!(candidate.revalidate(&other_endpoint).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn aliased_default_child_credential_or_state_parent_is_refused() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    let data = f.store(&f.first, 11, b"retained")?;
    let alias = backend_runtime::default_project_workspace_in(&f.second, &f.projects);
    std::os::unix::fs::symlink(&data, &alias)?;
    assert!(find_in(&f.second, &f.projects).is_err());
    std::fs::remove_file(data.join("authority.secret"))?;
    std::os::unix::fs::symlink(f.root.join("missing-secret"), data.join("authority.secret"))?;
    assert!(find_in(&f.first, &f.projects).is_err());
    std::fs::remove_file(data.join("authority.secret"))?;
    let cap = DirectoryCapability::open(&data)?;
    let mut file = cap.create_file_exclusive("authority.secret")?;
    file.write_all(&[11; 32])?;
    drop(file);
    let root_alias = f.root.join("aliased-projects-root");
    std::os::unix::fs::symlink(&f.projects, &root_alias)?;
    assert!(find_in(&f.first, &root_alias).is_err());
    Ok(())
}

#[test]
fn credential_only_directory_is_not_admitted_as_an_existing_index() -> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    let data = f.store(&f.first, 11, b"retained")?;
    std::fs::remove_file(data.join("OWNER.state"))?;
    assert!(find_in(&f.first, &f.projects).is_err());
    assert_eq!(std::fs::read(data.join("authority.secret"))?, [11; 32]);
    Ok(())
}

#[test]
fn selected_folder_requires_an_absolute_existing_directory_and_accepts_normalized_identity()
-> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    let data = f.store(&f.first, 11, b"retained")?;
    assert!(find_in(Path::new("relative-project"), &f.projects).is_err());
    assert!(find_in(&f.root.join("missing-project"), &f.projects).is_err());
    let file = f.root.join("a-file");
    std::fs::write(&file, b"not a directory")?;
    assert!(find_in(&file, &f.projects).is_err());
    assert_eq!(find_in(&f.first.join("."), &f.projects)?.data(), data);
    #[cfg(unix)]
    {
        let alias = f.root.join("selected-project-alias");
        std::os::unix::fs::symlink(&f.first, &alias)?;
        assert_eq!(find_in(&alias, &f.projects)?.data(), data);
    }
    Ok(())
}

#[test]
fn nested_git_package_uses_parent_owner_and_requires_exact_selected_package()
-> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    backend_platform::durable::ensure_private_directory(&f.first.join(".git"))?;
    std::fs::write(f.first.join("Cargo.toml"), "[workspace]\nmembers = []\n")?;
    let package = f.first.join("crates/member");
    backend_platform::durable::ensure_private_directory(&f.first.join("crates"))?;
    backend_platform::durable::ensure_private_directory(&package)?;
    std::fs::write(package.join("Cargo.toml"), "[package]\nname = \"member\"\n")?;
    let owner_data = f.store(&f.first, 11, b"CLI owner selected from nested cwd")?;
    let explicit_data = f.store(&package, 22, b"separate explicit project selection")?;
    let candidate = find_in(&package, &f.projects)?;
    assert_eq!(candidate.project, f.first);
    assert_eq!(candidate.package, package);
    assert_eq!(candidate.data(), owner_data);
    assert_ne!(candidate.data(), explicit_data);
    assert!(
        candidate.identify(std::slice::from_ref(&f.first)).is_err(),
        "an indexed owner root does not prove the selected package is indexed"
    );
    let binding = candidate.identify(std::slice::from_ref(&package))?;
    assert_eq!(binding.project(), f.first);
    assert_eq!(binding.data(), owner_data);
    candidate.revalidate(&binding)?;
    assert_eq!(
        std::fs::read(explicit_data.join("retained-index-fixture"))?,
        b"separate explicit project selection"
    );
    Ok(())
}

#[test]
fn nearest_non_git_manifest_owner_and_changed_boundary_preserve_package_isolation()
-> Result<(), Box<dyn Error>> {
    let f = Fixture::new()?;
    std::fs::write(f.first.join("Cargo.toml"), "[workspace]\nmembers = []\n")?;
    let member = f.first.join("crates/member");
    let package = member.join("src/domain");
    backend_platform::durable::ensure_private_directory(&f.first.join("crates"))?;
    backend_platform::durable::ensure_private_directory(&member)?;
    backend_platform::durable::ensure_private_directory(&member.join("src"))?;
    backend_platform::durable::ensure_private_directory(&package)?;
    std::fs::write(member.join("Cargo.toml"), "[package]\nname = \"member\"\n")?;
    let data = f.store(&member, 11, b"nearest manifest owner index")?;
    let candidate = find_in(&package, &f.projects)?;
    assert_eq!(candidate.project, member);
    assert_eq!(candidate.package, package);
    assert_eq!(candidate.data(), data);
    assert!(candidate.identify(std::slice::from_ref(&f.second)).is_err());
    let binding = candidate.identify(std::slice::from_ref(&package))?;
    candidate.revalidate(&binding)?;
    backend_platform::durable::ensure_private_directory(&f.first.join(".git"))?;
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
        std::fs::read(data.join("retained-index-fixture"))?,
        b"nearest manifest owner index"
    );
    Ok(())
}
