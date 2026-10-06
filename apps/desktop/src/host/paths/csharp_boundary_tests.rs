//! C# launch markers are a shallow convenience heuristic. Explicit folder
//! admission remains independent and still creates a normal unsent local row.

use super::looks_like_project;
use crate::core::{LocalProjectId, ResourceIdentity, VersionedRoot};
use crate::model::{AppSnapshot, ProjectPhase};
use crate::navigation::{Effect, FolderPickerOutcome, Intent, reduce};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Directory(PathBuf);

impl Directory {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "nudox-csharp-boundary-{}-{nonce}",
            std::process::id()
        ));
        crate::host::private_dir(&path)?;
        Ok(Self(path.canonicalize()?))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn plain_csharp_projects_and_solutions_need_no_git_marker() -> TestResult {
    let directory = Directory::new()?;
    for name in ["Reader.csproj", "Reader.sln", "Reader.slnx"] {
        assert!(!looks_like_project(directory.path()));
        assert!(!directory.path().join(".git").exists());
        let marker = directory.path().join(name);
        fs::write(&marker, b"")?;
        assert!(looks_like_project(directory.path()), "direct {name}");
        fs::remove_file(marker)?;
    }
    Ok(())
}

#[test]
fn msbuild_suffixes_ignore_ascii_case_and_keep_native_names() -> TestResult {
    let directory = Directory::new()?;
    for name in ["讀者.CSPROJ", "Reader.SlN", "Reader.SLNX"] {
        let marker = directory.path().join(name);
        fs::write(&marker, b"")?;
        assert!(looks_like_project(directory.path()), "native {name}");
        fs::remove_file(marker)?;
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn msbuild_suffix_classification_preserves_non_utf8_native_basenames() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    for (bytes, admitted) in [
        (b"reader-\xff.CSPROJ".as_slice(), true),
        (b"reader-\xff.SlN".as_slice(), true),
        (b"reader-\xff.slnx".as_slice(), true),
        (b"reader-\xff.csproj.bak".as_slice(), false),
        (b"reader.\xff".as_slice(), false),
        (b".csproj".as_slice(), false),
    ] {
        assert_eq!(
            super::msbuild_marker_name(&OsString::from_vec(bytes.to_vec())),
            admitted
        );
    }
}

// Darwin filesystems reject malformed UTF-8 names at file creation. The
// platform-independent classifier above still covers its native path boundary.
#[cfg(target_os = "linux")]
#[test]
fn a_non_utf8_native_basename_can_have_an_msbuild_suffix() -> TestResult {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let directory = Directory::new()?;
    let marker = directory
        .path()
        .join(OsString::from_vec(b"reader-\xff.CSPROJ".to_vec()));
    fs::write(marker, b"")?;
    assert!(looks_like_project(directory.path()));
    Ok(())
}

#[test]
fn nested_files_directories_and_other_suffixes_do_not_mark_the_parent() -> TestResult {
    let directory = Directory::new()?;
    for name in ["Reader.csproj", "Reader.sln", "Reader.slnx", "package.json"] {
        fs::create_dir(directory.path().join(name))?;
    }
    let nested = directory.path().join("nested");
    fs::create_dir(&nested)?;
    fs::write(nested.join("Reader.csproj"), b"")?;
    assert!(looks_like_project(&nested));
    for name in [
        "Reader.csproj.bak",
        "Reader.sln.json",
        "Reader.cs",
        ".csproj",
    ] {
        fs::write(directory.path().join(name), b"")?;
    }
    assert!(!looks_like_project(directory.path()));
    assert!(!looks_like_project(&directory.path().join("absent")));
    assert!(!looks_like_project(&directory.path().join("Reader.cs")));
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_only_msbuild_markers_do_not_certify_a_boundary() -> TestResult {
    use std::os::unix::fs::symlink;

    let directory = Directory::new()?;
    let candidate = directory.path().join("candidate");
    fs::create_dir(&candidate)?;
    let target = directory.path().join("regular-target");
    fs::write(&target, b"")?;
    for name in ["Reader.csproj", "Reader.sln", "Reader.slnx"] {
        symlink(&target, candidate.join(name))?;
    }
    symlink(
        directory.path().join("absent"),
        candidate.join("Missing.csproj"),
    )?;
    assert!(!looks_like_project(&candidate));
    fs::write(candidate.join("Actual.csproj"), b"")?;
    assert!(looks_like_project(&candidate));
    Ok(())
}

#[test]
fn existing_launch_markers_are_preserved() -> TestResult {
    let directory = Directory::new()?;
    for name in [
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "CMakeLists.txt",
    ] {
        let marker = directory.path().join(name);
        fs::write(&marker, b"")?;
        assert!(looks_like_project(directory.path()), "existing {name}");
        fs::remove_file(marker)?;
    }
    fs::create_dir(directory.path().join(".git"))?;
    assert!(looks_like_project(directory.path()));
    Ok(())
}

#[test]
fn explicit_add_folder_remains_independent_of_launch_markers() -> TestResult {
    let directory = Directory::new()?;
    assert!(!looks_like_project(directory.path()));
    let project = LocalProjectId::from_path(directory.path())?;
    let snapshot = AppSnapshot::empty(VersionedRoot::unserved());
    let picked = reduce(
        &snapshot,
        Intent::FolderPickerResult {
            outcome: FolderPickerOutcome::Selected(vec![directory.path().to_path_buf()].into()),
        },
    );
    let workspace = picked.snapshot.workspace();
    assert_eq!(workspace.projects.len(), 1);
    assert_eq!(workspace.projects[0].id, project);
    assert_eq!(workspace.projects[0].phase, ProjectPhase::Indexing);
    assert_eq!(workspace.projects[0].request, None);
    assert_eq!(workspace.projects[0].operation, None);
    assert_eq!(workspace.path_error, None);
    assert_eq!(workspace.active.as_ref(), Some(&project));
    assert_eq!(
        picked.snapshot.shelf().selected,
        Some(ResourceIdentity::Local(project))
    );
    assert_eq!(picked.effects, [Effect::Persist]);
    assert!(!looks_like_project(directory.path()));
    Ok(())
}
