//! Reported empty Rust scans: separate Git presence from root alias spelling.
use super::*;

struct PrivateProject(PathBuf);
impl PrivateProject {
    fn new(parent: &Path) -> Result<Self, String> {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_nanos();
        let root = parent.join(format!("nudox-rust-path-{}-{suffix}", std::process::id()));
        fs::create_dir(&root).map_err(|error| error.to_string())?;
        Ok(Self(root))
    }
}
impl Drop for PrivateProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn exercise_git_and_root_alias_axes(parent: &Path) -> Result<(), String> {
    let scratch = PrivateProject::new(parent)?;
    let physical = scratch.0.join("project");
    fs::create_dir(&physical).map_err(|error| error.to_string())?;
    fs::write(physical.join("Cargo.toml"),
        b"[package]\nname = \"path_count_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\npath = \"lib.rs\"\n")
        .map_err(|error| error.to_string())?;
    fs::write(physical.join("lib.rs"),
        b"/// Kept under direct, aliased, Git and non-Git roots.\npub fn source_path_probe() -> u64 { 7 }\n")
        .map_err(|error| error.to_string())?;
    let canonical = physical.canonicalize().map_err(|error| error.to_string())?;
    let alias_parent = scratch.0.join("root-alias");
    std::os::unix::fs::symlink(
        scratch
            .0
            .canonicalize()
            .map_err(|error| error.to_string())?,
        &alias_parent,
    )
    .map_err(|error| error.to_string())?;
    let alias = alias_parent.join("project");
    let key =
        backend_engine::package_key(canonical.to_str().ok_or("UTF-8 fixture root")?).to_bytes();
    let mut original_source = None;
    for with_git in [false, true] {
        if with_git {
            let result = std::process::Command::new("git")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "init",
                    "--quiet",
                    "--template=",
                ])
                .current_dir(&physical)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .map_err(|error| format!("required Git fixture: {error}"))?;
            assert!(
                result.status.success(),
                "real Git fixture: {:?}",
                result.stderr
            );
        }
        for root in [&canonical, &alias] {
            let scan = scan_project(
                root.to_str().ok_or("UTF-8 fixture alias")?,
                key,
                &BTreeMap::new(),
            )?;
            assert_eq!(scan.files.len(), 1, "Git={with_git}, root={root:?}");
            let fields = scan.files[0]
                .1
                .file_fields()
                .ok_or("expected Rust source row")?;
            assert_eq!(fields.path, "lib.rs");
            assert_eq!(fields.project, key);
            assert_eq!(fields.language, SourceLanguage::Rust);
            assert!(
                fields
                    .declarations
                    .iter()
                    .any(|item| item.name() == "source_path_probe")
            );
            assert!(
                !scan.source_facts.is_empty(),
                "real source facts must survive scan"
            );
            assert_eq!(scan.revision_fence.canonical_root(), canonical);
            assert!(compiler_revision_is_current(&scan.revision_fence)?);
            if let Some(expected) = original_source {
                assert_eq!(
                    scan.source_version, expected,
                    "same source/key through every axis"
                );
            } else {
                original_source = Some(scan.source_version);
            }
        }
    }
    Ok(())
}

#[test]
fn rust_source_count_and_declarations_survive_git_and_ancestor_alias_axes() -> Result<(), String> {
    exercise_git_and_root_alias_axes(&std::env::temp_dir())
}

#[cfg(target_os = "macos")]
#[test]
fn macos_tmp_alias_and_private_tmp_have_the_same_rust_source_frontier() -> Result<(), String> {
    let tmp = Path::new("/tmp");
    assert_eq!(
        tmp.canonicalize().map_err(|error| error.to_string())?,
        Path::new("/private/tmp")
    );
    exercise_git_and_root_alias_axes(tmp)
}
