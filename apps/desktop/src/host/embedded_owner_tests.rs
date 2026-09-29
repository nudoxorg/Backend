//! W-Index: what the embedded owner does on the machine and the state it is
//! given.
//!
//! After the 2026-09-29 index merge the desktop's fixture index was empty in the
//! development environment, for two reasons that lived in the host, not in the
//! window:
//!
//! - a workspace an earlier build wrote was refused whole ("source file is
//!   outside its project's canonical frontier"), because the source-file key
//!   layout changed under it; the host now sets it aside and the owner starts
//!   clean;
//! - the owner's Rust adapter was absent, so every Rust root was refused as
//!   `Unavailable { language: Rust, stage: LowerIr }`, because the merge made
//!   Cargo and Cargo's home mandatory next to `NUDOX_RUSTC` and the
//!   development shell only exports the latter; the host now supplies them
//!   ([`super::toolchain`]).

#![allow(clippy::expect_used, clippy::panic)]

use super::lease::{DesktopHost, HostMode};
use backend_client::Session;
use backend_library::{CommandReply, RowState};
use backend_runtime::WorkspacePaths;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn scratch(tag: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // `/tmp`, not `temp_dir()`: under nix the latter makes the socket path
    // longer than `sockaddr_un` allows.
    PathBuf::from("/tmp").join(format!("nx-w-index-{tag}-{}-{nonce}", std::process::id()))
}

/// A small Rust crate, as the fixture roots are.
fn rust_project(root: &Path) -> PathBuf {
    // The owner refuses a state directory whose parent anyone else can enter.
    super::private_dir(root).expect("private scratch root");
    let project = root.join("project");
    std::fs::create_dir_all(project.join("src")).expect("project");
    std::fs::write(
        project.join("Cargo.toml"),
        b"[package]\nname = \"w-index-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("manifest");
    std::fs::write(
        project.join("src/lib.rs"),
        b"/// One.\npub fn one() -> u8 {\n    1\n}\n",
    )
    .expect("source");
    project
}

fn workspace(root: &Path, project: &Path) -> WorkspacePaths {
    WorkspacePaths::discover(
        Some(project.to_path_buf()),
        Some(root.join("data")),
        Some(root.with_extension("sock")),
    )
    .expect("paths")
}

/// What the owner lists, by project label.
fn listed(session: &mut Session) -> Vec<(String, RowState)> {
    let CommandReply::Packages(snapshot) = session.packages().expect("packages").reply else {
        panic!("the packages command answered with something else")
    };
    snapshot
        .root
        .rows()
        .iter()
        .map(|row| (row.label.clone(), row.state))
        .collect()
}

#[test]
fn an_index_another_build_wrote_is_set_aside_and_the_owner_starts_clean() {
    let root = scratch("state");
    let project = rust_project(&root);
    let paths = workspace(&root, &project);
    paths.initialize().expect("initialize the workspace");
    backend_local_service::builtin::write_state_from_another_build(
        paths.data(),
        paths.authority_secret(),
    )
    .expect("write the workspace an earlier build left");
    assert!(
        paths.data().join("workspace.journal").is_file(),
        "the old workspace is on disk"
    );

    // Before: the owner this host used to start refused this directory whole
    // (`refusal_is_typed_and_names_the_layout` in the local service pins its words).
    let host = DesktopHost::start_with_paths(paths.clone())
        .expect("a workspace an earlier build wrote is replaced, not refused");

    assert_eq!(
        host.mode(),
        HostMode::Embedded,
        "this window owns the fresh workspace"
    );
    let set_aside = host
        .state_set_aside()
        .expect("the host says where the old state went")
        .to_path_buf();
    assert!(
        set_aside.starts_with(paths.data()),
        "it stays inside the workspace: {}",
        set_aside.display()
    );
    assert!(
        set_aside
            .join("workspace.journal")
            .metadata()
            .is_ok_and(|journal| journal.len() > 0),
        "the old workspace's journal is kept whole in {}",
        set_aside.display()
    );
    assert!(
        paths.authority_secret().is_file(),
        "the credential its clients hold stays where it was"
    );

    let mut session = Session::connect(host.endpoint()).expect("session");
    assert_eq!(
        listed(&mut session),
        Vec::new(),
        "the fresh workspace lists nothing: the project the old one held is indexed again when asked for"
    );
    drop(session);

    // A second start on the same directory finds a workspace this build wrote:
    // nothing more is set aside.
    drop(host);
    let again =
        DesktopHost::start_with_paths(paths.clone()).expect("restart on the fresh workspace");
    assert!(
        again.state_set_aside().is_none(),
        "nothing is set aside twice"
    );
    assert_eq!(
        std::fs::read_dir(paths.data())
            .expect("workspace")
            .flatten()
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("from-another-build"))
            .count(),
        1,
        "one set-aside directory, from the first start"
    );
    drop(again);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_embedded_owner_compiles_rust_and_never_answers_unavailable() {
    assert!(
        std::env::var_os("NUDOX_RUSTC").is_some(),
        "run inside the development shell: the owner finds its Rust compiler through NUDOX_RUSTC, and this host supplies the rest"
    );
    let root = scratch("rust");
    let project = rust_project(&root);
    let paths = workspace(&root, &project);
    let host = DesktopHost::start_with_paths(paths).expect("the owner starts");
    let mut session = Session::connect(host.endpoint()).expect("session");

    let indexed = session.index(project.to_str().expect("UTF-8 project path"));

    // Before: `Err(… package semantic compilation failed for src/lib.rs: Unavailable { language: Rust, stage: LowerIr })`
    // on every Rust root, whatever its source.
    assert!(
        indexed.is_ok(),
        "the owner refused a plain Rust crate: {indexed:?}"
    );
    let canonical = project.canonicalize().expect("canonical project");
    assert_eq!(
        listed(&mut session),
        vec![(canonical.to_string_lossy().into_owned(), RowState::Ready)],
        "the crate is listed, indexed and ready"
    );
    drop(session);
    drop(host);
    let _ = std::fs::remove_dir_all(&root);
}

/// A C# project. The owner has no C# authority unless `NUDOX_ROSLYN_HELPER`
/// names one, and the development shell does not, so its compile is refused
/// (`Unavailable { language: CSharp, stage: LowerIr }`) after its source has
/// been scanned and its frontier committed.
fn refused_project(root: &Path) -> PathBuf {
    let project = root.join("project");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::write(
        project.join("P.csproj"),
        b"<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>\n",
    )
    .expect("project file");
    std::fs::write(
        project.join("Program.cs"),
        b"public class P {\n    public static int Ok() {\n        return 1;\n    }\n}\n",
    )
    .expect("source");
    project
}

#[test]
fn a_refused_project_is_listed_in_the_same_boot_that_names_why() {
    assert!(
        std::env::var_os("NUDOX_ROSLYN_HELPER").is_none(),
        "this test needs an owner with no C# authority: unset NUDOX_ROSLYN_HELPER"
    );
    let root = scratch("refused");
    super::private_dir(&root).expect("private scratch root");
    let project = refused_project(&root);
    let host = DesktopHost::start_with_paths(workspace(&root, &project)).expect("the owner starts");
    let mut session = Session::connect(host.endpoint()).expect("session");

    let refusal = session
        .index(project.to_str().expect("UTF-8 project path"))
        .expect_err("the compile is refused: the owner has no C# authority")
        .to_string();

    assert!(
        refusal.contains("local semantic compilation failed") && refusal.contains("CSharp"),
        "the refusal is the owner's named one: {refusal}"
    );
    // Before: `[]`. The source frontier was committed and the view was not
    // published, so the project stayed out of the Library until the next
    // command or restart.
    let canonical = project.canonicalize().expect("canonical project");
    assert_eq!(
        listed(&mut session),
        vec![(canonical.to_string_lossy().into_owned(), RowState::Ready)],
        "the refused project is listed at once, on its structural rows"
    );
    let rows = session.health().expect("health").row_count();
    assert!(
        rows > 1,
        "the package row and its declarations are served: {rows} rows"
    );
    drop(session);
    drop(host);
    let _ = std::fs::remove_dir_all(&root);
}
