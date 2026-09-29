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

impl Owner {
    fn start(tag: &str) -> Self {
        let root = scratch(tag);
        crate::host::private_dir(&root).expect("private scratch root");
        let anchor = root.join("anchor");
        std::fs::create_dir_all(anchor.join("src")).expect("anchor");
        std::fs::write(anchor.join("Cargo.toml"), b"[package]\nname = \"w-acquire-anchor\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").expect("manifest");
        std::fs::write(anchor.join("src/lib.rs"), b"pub fn one() -> u8 { 1 }\n").expect("source");
        let paths = WorkspacePaths::discover(Some(anchor), Some(root.join("data")), Some(root.with_extension("sock"))).expect("paths");
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
        let package = PackageRef::parse(tree.root.to_str().expect("UTF-8")).expect("package");
        let mut reader = SessionReader::connect(self.host.endpoint());
        let cancel = CancellationToken::new();
        let outlines = OutlineCache::default();
        let context = ReadContext { worker: 0, cancel: &cancel, outlines: &outlines };
        let Ok(PageValue::Package(dossier)) = reader.read(&ReadRequest::Package(package), &context) else { panic!("{}'s page did not read", tree.release) };
        let outline = dossier.outline.known().unwrap_or_else(|| panic!("{}'s outline: {:?}", tree.release, dossier.outline.gap()));
        assert!(outline.complete, "the whole outline was read");
        outline.walk().map(|node| node.decl.name.to_string()).collect()
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

fn cached(release: &str) -> PathBuf {
    crate::model::source_facts::registry::source_dirs()
        .into_iter()
        .map(|dir| dir.join(release))
        .find(|dir| dir.is_dir())
        .unwrap_or_else(|| panic!("{release} is not in the local cargo cache"))
        .canonicalize()
        .expect("canonical")
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
