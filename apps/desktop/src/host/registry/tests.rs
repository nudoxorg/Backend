//! The cargo cache as a release source, against the real local cache: every
//! expectation is read from the cache's own files, never from memory.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use std::collections::BTreeMap;

fn scratch(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    std::env::temp_dir().join(format!("nx-w-acquire-{tag}-{}-{nonce}", std::process::id()))
}

fn home() -> PathBuf {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cargo")))
        .expect("a cargo home")
}

fn release(name: &str, version: &str) -> Release {
    Release::new(name, version).expect("release")
}

/// Every file under `root`, by relative path, with its bytes.
fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
            let path = entry.path();
            if entry.file_type().expect("type").is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(path.strip_prefix(root).expect("below root").to_path_buf(), std::fs::read(&path).expect("read"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A cargo home holding only `release`'s archive (copied from the real
/// cache), so the source must unpack it.
fn archive_only_home(release: &Release) -> PathBuf {
    let real = CargoCache::at(home(), scratch("unused")).archive(release).expect("the archive is in the local cargo cache");
    let fake = scratch("home");
    let index = real.parent().and_then(Path::file_name).expect("index dir");
    let dir = fake.join("registry").join("cache").join(index);
    std::fs::create_dir_all(&dir).expect("fake cache");
    std::fs::copy(&real, dir.join(real.file_name().expect("name"))).expect("copy archive");
    fake
}

#[test]
fn a_stem_reads_back_as_the_longest_version_suffix() {
    assert_eq!(Release::from_stem("md-5-0.10.6"), Some(release("md-5", "0.10.6")));
    assert_eq!(Release::from_stem("toml-1.1.6+spec-1.1.0"), Some(release("toml", "1.1.6+spec-1.1.0")));
    assert_eq!(Release::from_stem("serde_json-1.0.151"), Some(release("serde_json", "1.0.151")));
    assert_eq!(Release::from_stem("wasm-bindgen-0.2.100-beta.1"), Some(release("wasm-bindgen", "0.2.100-beta.1")));
    assert_eq!(Release::from_stem("anyhow"), None);
    assert_eq!(Release::from_stem("x-1.2"), None);
    assert!(CrateName::new("../escape").is_err());
    assert!(CrateName::new("9lives").is_err());
    assert!(Version::new("1.0.0/..").is_err());
    assert_eq!(release("toml", "1.1.6+spec-1.1.0").stem(), "toml-1.1.6+spec-1.1.0");
    assert_eq!(release("toml", "0.5.11").purl(), "pkg:cargo/toml@0.5.11");
    let built = release("toml", "1.1.6+spec-1.1.0");
    let package = crate::model::pages::PackageRef::parse(&built.purl()).expect("a release's package URL is an address");
    assert_eq!(Release::from_purl(package.as_str()), Some(built), "and reads back as the same release");
}

#[test]
fn toml_lists_every_published_release_and_knows_which_are_on_this_machine() {
    let source = CargoCache::at(home(), scratch("unpacked"));
    let toml = CrateName::new("toml").expect("name");
    let listed = source.releases(&toml);
    let published = cargo_home::releases("toml");
    assert!(published.len() > 20, "the registry index on this machine lists toml's releases");
    assert!(listed.len() >= published.len(), "every release the index lists is listed");
    let at = |version: &str| listed.iter().find(|entry| entry.release.version.as_str() == version).expect(version);
    for version in ["0.5.11", "0.8.23"] {
        let unpacked = home().join("registry/src");
        match &at(version).availability {
            Availability::Unpacked(tree) => assert!(tree.starts_with(&unpacked) && tree.ends_with(format!("toml-{version}")) && tree.join("Cargo.toml").is_file(), "{version}: {}", tree.display()),
            other => panic!("toml {version} is unpacked in the local cache, not {other:?}"),
        }
    }
    assert_eq!(at("0.1.0").availability, Availability::Download, "toml 0.1.0 is published and not on this machine");
    let expected = published.iter().find(|entry| entry.version == "0.5.11").and_then(|entry| entry.date.clone());
    assert_eq!(at("0.5.11").date.as_deref(), expected.as_deref(), "the date is the index's own");
    assert!(listed.windows(2).all(|pair| semver::cmp(pair[0].release.version.as_str(), pair[1].release.version.as_str()).is_le()), "oldest first");
}

#[test]
fn a_query_finds_crates_on_this_machine_at_their_newest_release() {
    let source = CargoCache::at(home(), scratch("unpacked"));
    let found = source.offline("anyhow", 8);
    let newest = std::fs::read_dir(home().join("registry/src"))
        .expect("src")
        .flatten()
        .flat_map(|index| std::fs::read_dir(index.path()).into_iter().flatten().flatten())
        .filter_map(|entry| Release::from_stem(entry.file_name().to_str()?))
        .filter(|release| release.name.as_str() == "anyhow")
        .max_by(|a, b| semver::cmp(a.version.as_str(), b.version.as_str()))
        .expect("anyhow is in the local cargo cache");
    assert_eq!(found.first(), Some(&newest), "the exact name leads, at its newest release: {found:?}");
    assert!(source.offline("", 8).is_empty(), "no query, no offers");
    assert!(source.offline("no-such-crate-anywhere", 8).is_empty());
}

#[test]
fn a_tree_is_known_as_its_release_and_a_release_as_its_tree() {
    let source = CargoCache::at(home(), scratch("unpacked"));
    let anyhow = release("anyhow", "1.0.104");
    let Availability::Unpacked(tree) = source.availability(&anyhow) else { panic!("anyhow 1.0.104 is unpacked on this machine") };
    assert_eq!(source.release_of(&tree), Some(anyhow.clone()));
    assert_eq!(source.release_of(Path::new(env!("CARGO_MANIFEST_DIR"))), None, "a project is not a release");
    let resolved = source.resolve(&anyhow).expect("resolve");
    assert_eq!(resolved.origin, Origin::Cargo);
    assert_eq!(resolved.root, tree.canonicalize().expect("canonical"));
    assert_eq!(source.resolve(&release("toml", "0.1.0")), Err(SourceError::NeedsDownload(release("toml", "0.1.0"))));
}

#[test]
fn an_archive_unpacks_to_exactly_the_files_cargo_unpacked() {
    let anyhow = release("anyhow", "1.0.104");
    let fake = archive_only_home(&anyhow);
    let unpacked = scratch("unpacked");
    let source = CargoCache::at(fake.clone(), unpacked.clone());
    assert!(matches!(source.releases(&anyhow.name).into_iter().find(|entry| entry.release == anyhow).map(|entry| entry.availability), Some(Availability::Archive(_))));
    let planned = source.own_tree(&anyhow);
    let tree = source.resolve(&anyhow).expect("unpack");
    assert_eq!(tree.root, planned.canonicalize().expect("planned is where it went"));
    assert!(matches!(tree.origin, Origin::Archive(_)));
    assert_eq!(source.release_of(&tree.root), Some(anyhow.clone()), "an unpacked tree is known as its release");
    let cargo = CargoCache::at(home(), scratch("unused")).cargo_tree(&anyhow).expect("cargo unpacked it too");
    let mut theirs = files(&cargo);
    theirs.remove(Path::new(".cargo-ok"));
    let ours = files(&tree.root);
    let differing = ours.keys().chain(theirs.keys()).filter(|path| ours.get(*path) != theirs.get(*path)).collect::<std::collections::BTreeSet<_>>();
    assert!(differing.is_empty(), "the unpacked tree differs from cargo's at {differing:?}");
    assert!(ours.len() > 10 && ours.contains_key(Path::new("src/lib.rs")), "a whole crate was unpacked: {} files", ours.len());
    let boundary = std::fs::read_to_string(tree.root.parent().expect("outer").join("Cargo.toml")).expect("boundary");
    assert!(boundary.contains("members = [\"anyhow-1.0.104\"]") && boundary.contains("resolver = \"2\""), "{boundary}");
    // A second resolve reads the tree that is there.
    assert_eq!(source.resolve(&anyhow).expect("again").root, tree.root);
    let _ = std::fs::remove_dir_all(&fake);
    let _ = std::fs::remove_dir_all(&unpacked);
}

#[test]
fn an_archive_that_is_not_the_published_one_is_refused() {
    let anyhow = release("anyhow", "1.0.104");
    let fake = archive_only_home(&anyhow);
    let archive = CargoCache::at(fake.clone(), scratch("unused")).archive(&anyhow).expect("copied");
    let unpacked = scratch("unpacked");
    let refused = archive::unpack(&archive, Some(&"0".repeat(64)), &anyhow, &unpacked);
    assert!(matches!(refused, Err(SourceError::Integrity { .. })), "{refused:?}");
    assert!(!unpacked.join(anyhow.stem()).exists(), "nothing is left behind");
    let _ = std::fs::remove_dir_all(&fake);
}

#[test]
fn the_boundary_uses_the_resolver_the_edition_implies() {
    assert!(archive::boundary("a-1.0.0", "[package]\nname = \"a\"\nedition = \"2021\"\n").contains("resolver = \"2\""));
    assert!(archive::boundary("a-1.0.0", "[package]\nedition = \"2018\"\n").contains("resolver = \"1\""));
    assert!(archive::boundary("a-1.0.0", "[package]\nedition = \"2024\"\n").contains("resolver = \"3\""));
    assert!(archive::boundary("a-1.0.0", "[package]\nedition = \"2021\"\nresolver = \"1\"\n").contains("resolver = \"1\""));
}
