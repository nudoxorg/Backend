//! Registry releases through the real owner (W-Acquire): a release resolved
//! from the local cargo cache is indexed as a root by the embedded owner the
//! desktop starts, and its page reads the release's real top items. The
//! expected names are read from the release's own source, never from memory.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::host::lease::DesktopHost;
use crate::model::pages::{PackageRef, PageValue};
use crate::runtime::actor::CancellationToken;
use crate::runtime::reads::{OutlineCache, PageReader as _, ReadContext, ReadRequest, SessionReader};
use backend_client::Session;
use backend_library::{CommandReply, SurfaceCommand, SurfaceReply};
use std::time::Instant;
use backend_runtime::WorkspacePaths;

fn scratch(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    // `/tmp`, not `temp_dir()`: under nix the latter makes the socket path
    // longer than `sockaddr_un` allows.
    PathBuf::from("/tmp").join(format!("nx-w-acquire-{tag}-{}-{nonce}", std::process::id()))
}

/// An owner on a fresh workspace, anchored at a one-file crate.
struct Owner {
    root: PathBuf,
    host: DesktopHost,
}

/// A fresh scratch root with a one-file anchor crate, and the owner's paths in it.
fn anchored(tag: &str) -> (PathBuf, WorkspacePaths) {
    let root = scratch(tag);
    let paths = anchored_at(&root);
    (root, paths)
}

/// The owner's paths under `root`, with its one-file anchor crate.
fn anchored_at(root: &Path) -> WorkspacePaths {
    crate::host::private_dir(root).expect("private scratch root");
    let anchor = root.join("anchor");
    std::fs::create_dir_all(anchor.join("src")).expect("anchor");
    std::fs::write(anchor.join("Cargo.toml"), b"[package]\nname = \"w-acquire-anchor\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").expect("manifest");
    std::fs::write(anchor.join("src/lib.rs"), b"pub fn one() -> u8 { 1 }\n").expect("source");
    WorkspacePaths::discover(Some(anchor), Some(root.join("data")), Some(root.with_extension("sock"))).expect("paths")
}

/// The package page's names at `endpoint`, as the app reads them.
fn names_at(endpoint: &Path, tree: &SourceTree) -> Vec<String> {
    let package = PackageRef::parse(tree.root.to_str().expect("UTF-8")).expect("package");
    let mut reader = SessionReader::connect(endpoint);
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let context = ReadContext { worker: 0, cancel: &cancel, outlines: &outlines };
    let Ok(PageValue::Package(dossier)) = reader.read(&ReadRequest::Package(package), &context) else { panic!("{}'s page did not read", tree.release) };
    let outline = dossier.outline.known().unwrap_or_else(|| panic!("{}'s outline: {:?}", tree.release, dossier.outline.gap()));
    assert!(outline.complete, "the whole outline was read");
    outline.walk().map(|node| node.decl.name.to_string()).collect()
}

impl Owner {
    fn start(tag: &str) -> Self {
        let (root, paths) = anchored(tag);
        let host = DesktopHost::start_with_paths(paths).expect("the owner starts");
        Self { root, host }
    }

    fn index(&self, tree: &SourceTree) {
        let mut session = Session::connect(self.host.endpoint()).expect("session");
        let indexed = session.index(tree.root.to_str().expect("UTF-8 tree"));
        assert!(indexed.is_ok(), "the owner refused {}: {indexed:?}", tree.release);
    }

    /// The package page's names, as the app reads them.
    fn names(&self, tree: &SourceTree) -> Vec<String> {
        names_at(self.host.endpoint(), tree)
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The names `lib.rs` declares at its top level (`pub struct|enum|trait|type|fn NAME`).
fn top_items(tree: &Path) -> Vec<String> {
    let source = std::fs::read_to_string(tree.join("src/lib.rs")).expect("lib.rs");
    let mut names = source
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("pub ")?;
            let rest = ["struct ", "enum ", "trait ", "type ", "fn "].iter().find_map(|kind| rest.strip_prefix(kind))?;
            let name = rest.split(|c: char| !(c.is_alphanumeric() || c == '_')).next()?;
            (!name.is_empty()).then(|| name.to_owned())
        })
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn anyhow() -> Release {
    Release::new("anyhow", "1.0.104").expect("release")
}

#[test]
fn a_release_from_the_cargo_cache_is_indexed_by_the_owner_and_its_page_reads_its_real_top_items() {
    let owner = Owner::start("cache");
    let source = CargoCache::from_env(owner.root.join("unpacked")).expect("a cargo home");
    let tree = source.resolve(&anyhow()).expect("anyhow 1.0.104 is in the local cargo cache");
    assert_eq!(tree.origin, Origin::Cargo);
    let expected = top_items(&tree.root);
    assert!(expected.len() >= 4, "anyhow's lib.rs declares its API at the top: {expected:?}");
    owner.index(&tree);
    let names = owner.names(&tree);
    let missing = expected.iter().filter(|name| !names.contains(name)).collect::<Vec<_>>();
    assert!(missing.is_empty(), "the page lacks {missing:?} of {expected:?}; it read {} names", names.len());
}

#[test]
fn a_release_only_its_archive_holds_is_unpacked_and_indexed_the_same_way() {
    let owner = Owner::start("archive");
    let release = anyhow();
    let real = CargoCache::from_env(owner.root.join("unused")).expect("home");
    let archive = real.archive(&release).expect("anyhow's archive is in the local cargo cache");
    // A cargo home holding only the archive: the source must unpack it.
    let home = owner.root.join("home");
    let index = archive.parent().and_then(Path::file_name).expect("index dir");
    std::fs::create_dir_all(home.join("registry/cache").join(index)).expect("cache dir");
    std::fs::copy(&archive, home.join("registry/cache").join(index).join(archive.file_name().expect("name"))).expect("copy");
    let source = CargoCache::at(home, owner.root.join("data/registry-sources"));
    let tree = source.resolve(&release).expect("unpack");
    assert!(matches!(tree.origin, Origin::Archive(_)), "{:?}", tree.origin);
    let expected = top_items(&tree.root);
    owner.index(&tree);
    let names = owner.names(&tree);
    let missing = expected.iter().filter(|name| !names.contains(name)).collect::<Vec<_>>();
    assert!(missing.is_empty(), "the page of the unpacked archive lacks {missing:?} of {expected:?}");
}

/// `frontends/rust/fixtures/toml_pin`: a project that pins toml 0.8.23.
fn toml_pin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../frontends/rust/fixtures/toml_pin").canonicalize().expect("the toml_pin fixture")
}

/// The registry releases a `Cargo.lock` pins (every `[[package]]` with a
/// registry `source`), read from the file itself.
fn lock_set(lockfile: &Path) -> std::collections::BTreeSet<(String, String)> {
    let text = std::fs::read_to_string(lockfile).expect("Cargo.lock");
    text.split("[[package]]")
        .skip(1)
        .filter_map(|entry| {
            let field = |key: &str| {
                entry.lines().find_map(|line| line.strip_prefix(key).map(|rest| rest.trim().trim_start_matches('=').trim().trim_matches('"').to_owned()))
            };
            let source = field("source ")?;
            if !source.starts_with("registry+") {
                return None;
            }
            Some((field("name ")?, field("version ")?))
        })
        .collect()
}

#[test]
fn a_projects_packages_are_the_releases_its_lockfile_pins_each_found_on_this_machine() {
    use crate::runtime::acquire::{Origin as Came, dependencies};
    let owner = Owner::start("lockset");
    let source = CargoCache::from_env(owner.root.join("unpacked")).expect("a cargo home");
    let project = toml_pin();
    let mut session = Session::connect(owner.host.endpoint()).expect("session");
    let found = dependencies(&mut session, &source, &project).expect("the owner reads the project's packages");
    let pinned = lock_set(&project.join("Cargo.lock"));
    assert!(pinned.len() >= 10, "toml_pin pins toml and its tree: {pinned:?}");
    let built = built_here(&project);
    let read = found.iter().map(|dependency| (dependency.release.name.to_string(), dependency.release.version.to_string())).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(found.len(), read.len(), "each release once: {found:?}");
    let unpinned = read.difference(&pinned).collect::<Vec<_>>();
    assert!(unpinned.is_empty(), "every package is a release Cargo.lock pins; these are not: {unpinned:?}");
    let missing = built.difference(&read).collect::<Vec<_>>();
    assert!(missing.is_empty(), "every release a build here compiles is read; missing {missing:?} of {built:?}");
    assert_eq!(found[0].release, Release::new("toml", "0.8.23").expect("release"), "the direct dependency comes first");
    assert!(found[0].direct && found[1..].iter().all(|dependency| !dependency.direct), "toml is the one direct dependency");
    for dependency in &found {
        let Came::Registry(Availability::Unpacked(tree)) = &dependency.origin else {
            panic!("{} is not found unpacked in the cargo cache: {:?}", dependency.release, dependency.origin)
        };
        assert!(tree.ends_with(dependency.release.stem()), "{} resolves to its own tree, not {}", dependency.release, tree.display());
    }
}

/// The registry releases a build of `project` on this machine compiles, as
/// Cargo itself says (`cargo tree --offline --locked`, normal and build
/// edges): the oracle is Cargo, not the owner's reading of it. The lockfile
/// also pins releases no build here compiles (serde pins `serde_derive`
/// behind `cfg(any())`), which are not the project's packages.
fn built_here(project: &Path) -> std::collections::BTreeSet<(String, String)> {
    let rustc = std::env::var_os("NUDOX_RUSTC").expect("the dev shell exports NUDOX_RUSTC (the owner compiles with it)");
    let cargo = Path::new(&rustc).with_file_name("cargo");
    let output = std::process::Command::new(cargo)
        .args(["tree", "--offline", "--locked", "--prefix", "none", "-e", "normal,build", "-f", "{p}"])
        .current_dir(project)
        .output()
        .expect("cargo tree runs");
    assert!(output.status.success(), "cargo tree: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.contains('('))
        .filter_map(|line| {
            let (name, version) = line.trim().split_once(" v")?;
            Some((name.to_owned(), version.trim().to_owned()))
        })
        .collect()
}

/// A project that builds with one registry release, `equivalent 1.0.2`,
/// pinned by its own lockfile (the entry is toml_pin's, checksum and all).
fn one_dependency_project(root: &Path) -> PathBuf {
    let project = root.join("one-dependency");
    std::fs::create_dir_all(project.join("src")).expect("project");
    std::fs::write(
        project.join("Cargo.toml"),
        b"[package]\nname = \"nx-one-dependency\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nequivalent = \"=1.0.2\"\n\n[workspace]\n",
    )
    .expect("manifest");
    std::fs::write(project.join("src/lib.rs"), b"pub use equivalent::Equivalent;\n").expect("source");
    let pinned = std::fs::read_to_string(toml_pin().join("Cargo.lock")).expect("toml_pin's lock");
    let equivalent = pinned.split("[[package]]").find(|entry| entry.contains("name = \"equivalent\"")).expect("equivalent is pinned");
    std::fs::write(
        project.join("Cargo.lock"),
        format!("version = 4\n\n[[package]]{}\n[[package]]\nname = \"nx-one-dependency\"\nversion = \"0.1.0\"\ndependencies = [\n \"equivalent\",\n]\n", equivalent.trim_end()),
    )
    .expect("lockfile");
    project.canonicalize().expect("canonical")
}

#[gpui::test]
fn a_projects_packages_are_indexed_by_the_owner_and_their_pages_read_their_real_top_items(cx: &mut gpui::TestAppContext) {
    use crate::runtime::acquire::{self, Origin as Came, ProjectPackages, Stage};
    use crate::runtime::offload::Asker;
    // The worker is a real thread that wakes the UI task.
    cx.executor().allow_parking();
    let owner = Owner::start("deps");
    let project = one_dependency_project(&owner.root);
    let source = CargoCache::from_env(owner.root.join("unpacked")).expect("a cargo home");
    let composition = Composition { endpoint: owner.host.endpoint().to_path_buf(), source: Arc::new(source.clone()), refusals: None };
    let id = crate::core::LocalProjectId::from_path(&project).expect("project id");
    cx.update(|cx| acquire::add_dependencies_with(id.clone(), Some(composition), gpui::WeakEntity::new_invalid(), cx));
    let started = Instant::now();
    let packages = loop {
        cx.run_until_parked();
        let packages = cx.update(|cx| acquire::project_packages(&id, Asker::Everyone, cx));
        let settled = match &packages {
            Some(ProjectPackages::Read(found)) => found.iter().all(|(_, stage)| !stage.as_ref().is_some_and(Stage::working)),
            Some(ProjectPackages::Refused(_)) => true,
            Some(ProjectPackages::Reading) | None => false,
        };
        if settled {
            break packages;
        }
        assert!(started.elapsed() < std::time::Duration::from_secs(600), "the project's packages never settled: {packages:?}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let Some(ProjectPackages::Read(found)) = packages else { panic!("the owner did not read the project's packages: {packages:?}") };
    let wanted = Release::new("equivalent", "1.0.2").expect("release");
    let tree = source.resolve(&wanted).expect("equivalent 1.0.2 is in the local cargo cache");
    let [(dependency, Some(Stage::Added(page)))] = found.as_slice() else { panic!("one package, added: {found:?}") };
    assert_eq!(dependency.release, wanted);
    assert!(dependency.direct);
    assert!(matches!(dependency.origin, Came::Registry(Availability::Unpacked(_))), "{:?}", dependency.origin);
    assert_eq!(page.as_str(), tree.root.to_str().expect("UTF-8"), "its page is the tree the owner indexed");
    let mut session = Session::connect(owner.host.endpoint()).expect("session");
    let CommandReply::Packages(snapshot) = session.packages().expect("packages").reply else { panic!("packages") };
    assert!(
        snapshot.root.rows().iter().any(|row| row.label == page.as_str() && row.state == backend_library::RowState::Ready),
        "the owner lists {wanted} ready: {:?}",
        snapshot.root.rows().iter().map(|row| (row.label.clone(), row.state)).collect::<Vec<_>>()
    );
    let expected = top_items(&tree.root);
    assert!(!expected.is_empty(), "equivalent's lib.rs declares its API at the top");
    let names = owner.names(&tree);
    let missing = expected.iter().filter(|name| !names.contains(name)).collect::<Vec<_>>();
    assert!(missing.is_empty(), "the page lacks {missing:?} of {expected:?}; it read {names:?}");
}

fn cached(release: &str) -> PathBuf {
    crate::model::source_facts::registry::source_dirs()
        .into_iter()
        .map(|dir| dir.join(release))
        .find(|dir| dir.is_dir())
        .unwrap_or_else(|| panic!("{release} is not in the local cargo cache"))
        .canonicalize()
        .expect("canonical")
}

/// The files of a crate's `src/` that declare a public type or function at
/// their top level, each with one such name, read from the source itself.
fn public_items_by_file(tree: &Path) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut stack = vec![tree.join("src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("src").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else { continue };
            let item = source.lines().find_map(|line| {
                let rest = line.strip_prefix("pub ")?;
                let rest = ["struct ", "enum ", "trait "].iter().find_map(|kind| rest.strip_prefix(kind))?;
                let name = rest.split(|c: char| !(c.is_alphanumeric() || c == '_')).next()?;
                (!name.is_empty()).then(|| name.to_owned())
            });
            if let Some(item) = item {
                let relative = path.strip_prefix(tree).expect("inside the tree").to_string_lossy().into_owned();
                found.push((relative, item));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn a_multi_module_crates_names_carry_the_file_they_are_declared_in() {
    let owner = Owner::start("modules");
    let source = CargoCache::from_env(owner.root.join("unpacked")).expect("a cargo home");
    let tree = source.resolve(&Release::new("toml", "0.8.23").expect("release")).expect("toml 0.8.23 is in the local cargo cache");
    let expected = public_items_by_file(&tree.root);
    assert!(expected.len() >= 4, "toml 0.8.23 declares its types across several files: {expected:?}");
    owner.index(&tree);
    let package = PackageRef::parse(tree.root.to_str().expect("UTF-8")).expect("package");
    let mut reader = SessionReader::connect(owner.host.endpoint());
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let context = ReadContext { worker: 0, cancel: &cancel, outlines: &outlines };
    let Ok(PageValue::Package(dossier)) = reader.read(&ReadRequest::Package(package), &context) else { panic!("toml's page did not read") };
    let outline = dossier.outline.known().expect("toml's outline");
    let placed = outline.walk().filter_map(|node| Some((node.decl.path.as_deref()?.to_owned(), node.decl.name.to_string()))).collect::<std::collections::BTreeSet<_>>();
    let unplaced = expected.iter().filter(|item| !placed.contains(*item)).collect::<Vec<_>>();
    assert!(
        unplaced.is_empty(),
        "each public type is placed in the file (the module) that declares it; these are not: {unplaced:?}; placed: {:?}",
        placed.iter().filter(|(_, name)| expected.iter().any(|(_, wanted)| wanted == name)).collect::<Vec<_>>()
    );
    let files = placed.iter().map(|(path, _)| path.as_str()).collect::<std::collections::BTreeSet<_>>();
    assert!(files.len() >= 4, "toml's names come from its modules, not one: {files:?}");
}

#[test]
fn an_owner_reopens_its_workspace_after_indexing_a_crate_whose_docs_link_to_std() {
    // toml_write 0.1.2 documents `[`f32::NAN`] / [`f64::NAN`]`: links to
    // declarations outside the crate, read from its own source.
    let source = CargoCache::from_env(scratch("unused")).expect("a cargo home");
    let tree = source.resolve(&Release::new("toml_write", "0.1.2").expect("release")).expect("toml_write 0.1.2 is in the local cargo cache");
    let write = std::fs::read_to_string(tree.root.join("src/write.rs")).expect("write.rs");
    assert!(write.contains("[`f64::NAN`]"), "the crate's docs link to std");
    let (root, paths) = anchored("reopen");
    let first = DesktopHost::start_with_paths(paths.clone()).expect("the owner starts");
    let indexed = Session::connect(first.endpoint()).expect("session").index(tree.root.to_str().expect("UTF-8"));
    assert!(indexed.is_ok(), "the owner refused toml_write: {indexed:?}");
    let expected = names_at(first.endpoint(), &tree);
    drop(first);
    // Before: `compiled locald profile failed: view row: snapshot row
    // document: missing producer key commitment`, and a relaunch had no index.
    let reopened = DesktopHost::start_with_paths(paths).expect("the owner reopens the workspace it wrote");
    assert_eq!(names_at(reopened.endpoint(), &tree), expected, "the reopened owner serves the same page");
    drop(reopened);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_page_is_read_from_the_last_publication_while_the_owner_compiles_another_package() {
    let owner = Owner::start("concurrent");
    let source = CargoCache::from_env(owner.root.join("unpacked")).expect("a cargo home");
    let small = source.resolve(&Release::new("equivalent", "1.0.2").expect("release")).expect("equivalent 1.0.2 is in the local cargo cache");
    owner.index(&small);
    let expected = owner.names(&small);
    let big = source.resolve(&Release::new("toml_edit", "0.22.27").expect("release")).expect("toml_edit 0.22.27 is in the local cargo cache");
    let (endpoint, big_root) = (owner.host.endpoint().to_path_buf(), big.root.clone());
    let compiling = std::thread::spawn(move || {
        let started = Instant::now();
        let indexed = Session::connect(&endpoint).and_then(|mut session| session.index(big_root.to_str().expect("UTF-8")));
        (indexed.map(|_| ()).map_err(|error| error.to_string()), started.elapsed())
    });
    // Its scan and source frontier are on the owner loop; its compile is not.
    std::thread::sleep(std::time::Duration::from_secs(4));
    assert!(!compiling.is_finished(), "toml_edit's compile is under way");
    let asked = Instant::now();
    let names = owner.names(&small);
    let listed = Session::connect(owner.host.endpoint()).expect("session").packages().is_ok();
    let answered = asked.elapsed();
    let still_compiling = !compiling.is_finished();
    let (indexed, total) = compiling.join().expect("the compile thread");
    eprintln!("CONCURRENT read answered in {answered:?}; toml_edit indexed in {total:?}: {indexed:?}");
    assert_eq!(names, expected, "the page is equivalent's, from the last publication");
    assert!(listed, "the packages list is read too");
    assert!(
        still_compiling,
        "the read was answered only once toml_edit's index had finished ({answered:?} after asking, the index took {total:?}): it waited for the compile"
    );
    assert!(answered < std::time::Duration::from_secs(5), "the read waited {answered:?}");
    assert!(indexed.is_ok(), "the deferred index still publishes: {indexed:?}");
    let mut session = Session::connect(owner.host.endpoint()).expect("session");
    let CommandReply::Packages(snapshot) = session.packages().expect("packages").reply else { panic!("packages") };
    assert!(
        snapshot.root.rows().iter().any(|row| row.label == big.root.to_str().expect("UTF-8") && row.state == backend_library::RowState::Ready),
        "toml_edit is published once its compile is done"
    );
    let names = owner.names(&big);
    assert!(names.iter().any(|name| name == "DocumentMut"), "toml_edit's page reads its real names: {} names", names.len());
}

/// The child half of [`an_owner_killed_mid_compile_reopens_at_its_last_publication_and_indexes_again`]:
/// an owner at `NX_CRASH_ROOT` publishes equivalent, starts compiling
/// toml_edit, says so, and waits to be killed.
#[test]
#[ignore = "run by an_owner_killed_mid_compile_reopens_at_its_last_publication_and_indexes_again, which kills it"]
fn crash_child() {
    let root = PathBuf::from(std::env::var_os("NX_CRASH_ROOT").expect("NX_CRASH_ROOT"));
    let host = DesktopHost::start_with_paths(anchored_at(&root)).expect("the owner starts");
    let source = CargoCache::from_env(root.join("unpacked")).expect("a cargo home");
    let small = source.resolve(&Release::new("equivalent", "1.0.2").expect("release")).expect("equivalent");
    Session::connect(host.endpoint()).expect("session").index(small.root.to_str().expect("UTF-8")).expect("equivalent is published");
    let big = source.resolve(&Release::new("toml_edit", "0.22.27").expect("release")).expect("toml_edit");
    let endpoint = host.endpoint().to_path_buf();
    std::thread::spawn(move || {
        let _ = Session::connect(&endpoint).and_then(|mut session| session.index(big.root.to_str().expect("UTF-8")));
    });
    std::thread::sleep(std::time::Duration::from_secs(4));
    println!("CRASH-CHILD COMPILING");
    std::thread::sleep(std::time::Duration::from_secs(600));
    drop(host);
}

#[test]
fn an_owner_killed_mid_compile_reopens_at_its_last_publication_and_indexes_again() {
    use std::io::BufRead as _;
    let root = scratch("crash");
    let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "host::registry::owner_tests::crash_child", "--ignored", "--nocapture", "--test-threads=1"])
        .env("NX_CRASH_ROOT", &root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the child runs");
    let stdout = child.stdout.take().expect("stdout");
    let mut lines = std::io::BufReader::new(stdout).lines();
    let compiling = lines.by_ref().map_while(Result::ok).any(|line| line.contains("CRASH-CHILD COMPILING"));
    assert!(compiling, "the child reached its compile");
    // kill -9: no destructor, no flush, mid-compile.
    let killed = std::process::Command::new("kill").args(["-9", &child.id().to_string()]).status().expect("kill");
    assert!(killed.success());
    let _ = child.wait();
    let host = DesktopHost::start_with_paths(anchored_at(&root)).expect("the owner reopens after a crash mid-compile");
    let source = CargoCache::from_env(root.join("unpacked")).expect("a cargo home");
    let small = source.resolve(&Release::new("equivalent", "1.0.2").expect("release")).expect("equivalent");
    let big = source.resolve(&Release::new("toml_edit", "0.22.27").expect("release")).expect("toml_edit");
    let listed = |host: &DesktopHost| {
        let CommandReply::Packages(snapshot) = Session::connect(host.endpoint()).expect("session").packages().expect("packages").reply else { panic!("packages") };
        snapshot.root.rows().iter().map(|row| (row.label.clone(), row.state)).collect::<Vec<_>>()
    };
    let before = listed(&host);
    assert!(
        before.iter().any(|(label, state)| label == small.root.to_str().expect("UTF-8") && *state == backend_library::RowState::Ready),
        "the last publication stands: {before:?}"
    );
    assert!(!names_at(host.endpoint(), &small).is_empty(), "equivalent's page reads");
    // The job the crash cut short runs again, to the end.
    Session::connect(host.endpoint()).expect("session").index(big.root.to_str().expect("UTF-8")).expect("toml_edit indexes after the crash");
    assert!(names_at(host.endpoint(), &big).iter().any(|name| name == "DocumentMut"), "toml_edit's page reads its real names");
    drop(host);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_package_the_compiler_could_not_finish_still_says_so_after_a_relaunch() {
    use crate::runtime::acquire::{Listed, Stage, index_release};
    let owner = Owner::start("thin");
    let source = CargoCache::from_env(owner.root.join("unpacked")).expect("a cargo home");
    let refusals = owner.root.join("data/registry-sources/refusals.json");
    let composition = Composition { endpoint: owner.host.endpoint().to_path_buf(), source: Arc::new(source), refusals: Some(refusals.clone()) };
    // serde_core 1.0.229: the compiler stops on a generic parameter it cannot
    // lower (src/de/mod.rs); the owner still lists it on its source's names.
    let release = Release::new("serde_core", "1.0.229").expect("release");
    let first = index_release(&composition, &release, Listed::Any, &|_| {});
    let Stage::Partial { page, words } = &first else { panic!("the compiler finishes serde_core now; pick another refused crate: {first:?}") };
    assert!(words.contains("src/de/mod.rs"), "the owner's words name the file: {words}");
    // A relaunch: the owner already lists it, so it is not compiled again,
    // and it is still the package the compiler could not finish.
    let stages = std::sync::Mutex::new(Vec::new());
    let again = index_release(&composition, &release, Listed::Any, &|stage| stages.lock().expect("stages").push(stage));
    assert_eq!(again, Stage::Partial { page: page.clone(), words: words.clone() }, "after a relaunch it is still thin, for the same reason");
    assert!(!stages.lock().expect("stages").contains(&Stage::Indexing), "and it was not compiled again");
    assert!(refusals.is_file(), "the words are kept beside the owner's workspace");
}

#[test]
fn refusal_words_are_kept_and_forgotten_by_release() {
    let root = scratch("refusals");
    let refusals = crate::runtime::acquire::Refusals::at(&root.join("refusals.json"));
    assert_eq!(refusals.words("/cache/a-1.0.0"), None);
    refusals.keep("/cache/a-1.0.0", "the compiler could not finish reading src/lib.rs");
    refusals.keep("/cache/b-2.0.0", "another reason");
    assert_eq!(refusals.words("/cache/a-1.0.0").as_deref(), Some("the compiler could not finish reading src/lib.rs"));
    refusals.forget("/cache/a-1.0.0");
    assert_eq!(refusals.words("/cache/a-1.0.0"), None, "a compile that finished later forgets the refusal");
    assert_eq!(refusals.words("/cache/b-2.0.0").as_deref(), Some("another reason"));
    let _ = std::fs::remove_dir_all(&root);
}

/// Which of toml_pin's packages leaves the owner unable to reopen its own
/// workspace (learning probe): each is indexed, then the owner is restarted.
#[test]
#[ignore = "probe: run by hand"]
fn probe_reopen_after_each_package() {
    let root = scratch("reopen");
    crate::host::private_dir(&root).expect("private scratch root");
    let anchor = root.join("anchor");
    std::fs::create_dir_all(anchor.join("src")).expect("anchor");
    std::fs::write(anchor.join("Cargo.toml"), b"[package]\nname = \"w-acquire-anchor\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").expect("manifest");
    std::fs::write(anchor.join("src/lib.rs"), b"pub fn one() -> u8 { 1 }\n").expect("source");
    let paths = WorkspacePaths::discover(Some(anchor), Some(root.join("data")), Some(root.with_extension("sock"))).expect("paths");
    let source = CargoCache::from_env(root.join("unpacked")).expect("home");
    let only = std::env::var("NX_PROBE_ONLY").ok();
    let mut host = DesktopHost::start_with_paths(paths.clone()).expect("the owner starts");
    let found = {
        let mut session = Session::connect(host.endpoint()).expect("session");
        crate::runtime::acquire::dependencies(&mut session, &source, &toml_pin()).expect("packages")
    };
    for dependency in found {
        if only.as_deref().is_some_and(|only| !only.split(',').any(|name| name == dependency.release.name.as_str())) {
            continue;
        }
        let tree = source.resolve(&dependency.release).expect("tree");
        let indexed = Session::connect(host.endpoint()).expect("session").index(tree.root.to_str().expect("utf8")).map(|_| ());
        eprintln!("PROBE index {}: {:?}", dependency.release, indexed.as_ref().map_err(|error| error.to_string()));
        let listed = Session::connect(host.endpoint()).expect("session").packages().map(|_| ());
        eprintln!("PROBE   packages read: {:?}", listed.map_err(|error| error.to_string()));
        drop(host);
        match DesktopHost::start_with_paths(paths.clone()) {
            Ok(again) => {
                eprintln!("PROBE   reopened after {}", dependency.release);
                host = again;
            }
            Err(error) => {
                eprintln!("PROBE   REFUSED after {}: {error}", dependency.release);
                return;
            }
        }
    }
    drop(host);
    let _ = std::fs::remove_dir_all(&root);
}

/// Where each declaration of one release's outline says it lives (learning
/// probe): `NX_PROBE_RELEASE=toml_datetime-0.6.11`.
#[test]
#[ignore = "probe: run by hand"]
fn probe_module_paths_on_a_real_owner() {
    let stem = std::env::var("NX_PROBE_RELEASE").unwrap_or_else(|_| "toml_datetime-0.6.11".to_owned());
    let owner = Owner::start("paths");
    let path = cached(&stem);
    let mut session = Session::connect(owner.host.endpoint()).expect("session");
    eprintln!("PROBE index {stem}: {:?}", session.index(path.to_str().expect("utf8")).map(|_| "ok"));
    let package = PackageRef::parse(path.to_str().expect("utf8")).expect("package");
    let mut reader = SessionReader::connect(owner.host.endpoint());
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let context = ReadContext { worker: 0, cancel: &cancel, outlines: &outlines };
    let Ok(PageValue::Package(dossier)) = reader.read(&ReadRequest::Package(package), &context) else { panic!("dossier") };
    let tree = dossier.outline.known().expect("outline");
    let (mut with, mut without) = (0, 0);
    for node in tree.walk() {
        if node.decl.path.is_some() { with += 1 } else { without += 1 }
        eprintln!("PROBE decl kind={:?} path={:?}:{:?} name={} coord={}", node.decl.kind, node.decl.path, node.decl.line, node.decl.name, node.decl.coordinate.as_str());
    }
    eprintln!("PROBE {stem}: {with} with a path, {without} without");
}

/// What the owner does with registry trees admitted as roots (learning probe).
#[test]
#[ignore = "probe: run by hand"]
fn probe_registry_roots_on_a_real_owner() {
    let owner = Owner::start("probe");
    let host = &owner.host;
    let mut session = Session::connect(host.endpoint()).expect("session");
    for release in ["anyhow-1.0.104", "toml-0.5.11", "toml-0.8.23"] {
        let path = cached(release);
        let started = Instant::now();
        let reply = session.index(path.to_str().expect("utf8"));
        eprintln!("PROBE index {release}: {:?} in {:?}", reply.as_ref().map(|_| "ok"), started.elapsed());
    }
    let CommandReply::Packages(snapshot) = session.packages().expect("packages").reply else { panic!("packages") };
    for row in snapshot.root.rows() {
        eprintln!("PROBE row id={:?} label={} state={:?} kind={:?}", row.id, row.label, row.state, row.kind);
    }
    let mut reader = SessionReader::connect(host.endpoint());
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let context = ReadContext { worker: 0, cancel: &cancel, outlines: &outlines };
    for release in ["anyhow-1.0.104", "toml-0.5.11"] {
        let package = PackageRef::parse(cached(release).to_str().expect("utf8")).expect("package");
        eprintln!("PROBE package {} local={} display={}", package.as_str(), package.is_local(), package.display_name());
        let Ok(PageValue::Package(dossier)) = reader.read(&ReadRequest::Package(package.clone()), &context) else { panic!("dossier") };
        eprintln!("PROBE record={:?}", dossier.record.known().map(|r| (r.name.clone(), r.version.clone())));
        eprintln!("PROBE versions={:?}", dossier.versions);
        match dossier.outline.known() {
            Some(tree) => {
                eprintln!("PROBE outline count={} complete={}", tree.count(), tree.complete);
                for node in tree.walk().take(12) {
                    eprintln!("PROBE   decl {} name={} kind={:?} path={:?}:{:?}", node.decl.coordinate.as_str(), node.decl.name, node.decl.kind, node.decl.path, node.decl.line);
                }
            }
            None => eprintln!("PROBE outline gap {:?}", dossier.outline.gap()),
        }
    }
    let from = backend_library::PackageReference::parse(cached("toml-0.8.23").to_string_lossy().into_owned()).expect("from");
    let to = backend_library::PackageReference::parse(cached("toml-0.5.11").to_string_lossy().into_owned()).expect("to");
    let started = Instant::now();
    match session.surface(SurfaceCommand::Diff { from: from.clone(), to: to.clone() }) {
        Ok(SurfaceReply::Diff(rows)) => {
            eprintln!("PROBE diff rows={} in {:?}", rows.len(), started.elapsed());
            for row in rows.iter().take(25) {
                eprintln!("PROBE   diff {} {:?} links={}", row.label.as_str(), row.change, row.links.len());
            }
        }
        other => eprintln!("PROBE diff other {other:?}"),
    }
    for (name, command) in [
        ("package", SurfaceCommand::Package { package: to.clone() }),
        ("versions", SurfaceCommand::PackageVersions { package: to.clone() }),
        ("profile", SurfaceCommand::PackageProfile { package: to.clone() }),
    ] {
        eprintln!("PROBE {name}: {:?}", session.surface(command).map(|reply| format!("{reply:?}").chars().take(400).collect::<String>()));
    }
    drop(session);
}
