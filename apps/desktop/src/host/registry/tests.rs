//! The cargo cache as a release source, against the real local cache: every
//! expectation is read from the cache's own files, never from memory.

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use std::collections::BTreeMap;

fn scratch(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("nx-w-acquire-{tag}-{}-{nonce}", std::process::id()))
}

fn private_unpack(tag: &str) -> (PathBuf, PathBuf) {
    let parent = scratch(tag);
    crate::host::private_dir(&parent).expect("private temporary parent");
    (parent.clone(), parent.join("registry-sources"))
}

fn source() -> CargoCache {
    CargoCache::from_env(scratch("test-unpacked")).expect("an effective cargo cache")
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
                out.insert(
                    path.strip_prefix(root).expect("below root").to_path_buf(),
                    std::fs::read(&path).expect("read"),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn write_index_record(index: &Path, release: &Release, checksum: &str, yanked: bool, date: &str) {
    let record = serde_json::json!({
        "name": release.name.as_str(),
        "vers": release.version.as_str(),
        "cksum": checksum,
        "yanked": yanked,
        "pubtime": format!("{date}T00:00:00Z"),
    });
    let cached = index
        .join(".cache")
        .join(cache_relative(release.name.as_str()));
    std::fs::create_dir_all(cached.parent().expect("index record parent")).expect("fake index");
    let mut bytes = serde_json::to_vec(&record).expect("index record");
    bytes.push(0);
    std::fs::write(cached, bytes).expect("fake index record");
}

/// A cargo home holding only `release`'s archive (copied from the real
/// cache), so the source must unpack it.
fn archive_only_home(release: &Release) -> PathBuf {
    let real_source = CargoCache::from_env(scratch("unused")).expect("the effective cargo cache");
    let real = real_source
        .archive(release)
        .expect("the archive is in the local cargo cache");
    let index = real.parent().and_then(Path::file_name).expect("index dir");
    let checksum = real_source
        .checksum_in_index(&index.to_string_lossy(), release)
        .expect("the local index has the archive checksum");
    let fake = scratch("home");
    let dir = fake.join("registry").join("cache").join(index);
    std::fs::create_dir_all(&dir).expect("fake cache");
    std::fs::copy(&real, dir.join(real.file_name().expect("name"))).expect("copy archive");
    let index = real.parent().and_then(Path::file_name).expect("index dir");
    let record = serde_json::json!({
        "name": release.name.as_str(),
        "vers": release.version.as_str(),
        "cksum": checksum,
        "yanked": false,
        "pubtime": "2026-01-01T00:00:00Z",
    });
    let cached = fake
        .join("registry/index")
        .join(index)
        .join(".cache")
        .join(cache_relative(release.name.as_str()));
    std::fs::create_dir_all(cached.parent().expect("index record parent")).expect("fake index");
    let mut bytes = serde_json::to_vec(&record).expect("index record");
    bytes.push(0);
    std::fs::write(cached, bytes).expect("fake index record");
    fake
}

#[test]
fn a_stem_reads_back_as_the_longest_version_suffix() {
    assert_eq!(
        Release::from_stem("md-5-0.10.6"),
        Some(release("md-5", "0.10.6"))
    );
    assert_eq!(
        Release::from_stem("toml-1.1.6+spec-1.1.0"),
        Some(release("toml", "1.1.6+spec-1.1.0"))
    );
    assert_eq!(
        Release::from_stem("serde_json-1.0.151"),
        Some(release("serde_json", "1.0.151"))
    );
    assert_eq!(
        Release::from_stem("wasm-bindgen-0.2.100-beta.1"),
        Some(release("wasm-bindgen", "0.2.100-beta.1"))
    );
    assert_eq!(Release::from_stem("anyhow"), None);
    assert_eq!(Release::from_stem("x-1.2"), None);
    assert!(CrateName::new("../escape").is_err());
    assert!(CrateName::new("9lives").is_err());
    assert!(Version::new("1.0.0/..").is_err());
    assert_eq!(
        release("toml", "1.1.6+spec-1.1.0").stem(),
        "toml-1.1.6+spec-1.1.0"
    );
    assert_eq!(release("toml", "0.5.11").purl(), "pkg:cargo/toml@0.5.11");
    let built = release("toml", "1.1.6+spec-1.1.0");
    let package = crate::model::pages::PackageRef::parse(&built.purl())
        .expect("a release's package URL is an address");
    assert_eq!(
        Release::from_purl(package.as_str()),
        Some(built),
        "and reads back as the same release"
    );
}

#[test]
fn toml_lists_every_published_release_and_knows_which_are_on_this_machine() {
    let source = source();
    let toml = CrateName::new("toml").expect("name");
    let listed = source.releases(&toml);
    assert!(
        listed.len() > 20,
        "the registry index on this machine lists toml's releases"
    );
    let at = |version: &str| {
        listed
            .iter()
            .find(|entry| entry.release.version.as_str() == version)
            .expect(version)
    };
    for version in ["0.5.11", "0.8.23"] {
        let unpacked = source.source_dirs();
        match &at(version).availability {
            Availability::Unpacked(tree) => assert!(
                unpacked.iter().any(|dir| tree.starts_with(dir))
                    && tree.ends_with(format!("toml-{version}"))
                    && tree.join("Cargo.toml").is_file(),
                "{version}: {}",
                tree.display()
            ),
            other => panic!("toml {version} is unpacked in the local cache, not {other:?}"),
        }
    }
    assert_eq!(
        at("0.1.0").availability,
        Availability::Download,
        "toml 0.1.0 is published and not on this machine"
    );
    assert!(
        matches!(&at("0.5.11").date, RegistryFact::Known(_)),
        "the date is known from the effective index"
    );
    assert_eq!(
        &at("0.5.11").yanked,
        &RegistryFact::Known(false),
        "only an explicit false is unyanked"
    );
    assert!(
        listed.windows(2).all(|pair| semver::cmp(
            pair[0].release.version.as_str(),
            pair[1].release.version.as_str()
        )
        .is_le()),
        "oldest first"
    );
}

#[test]
fn a_query_finds_crates_on_this_machine_at_their_newest_release() {
    let source = source();
    let found = source.offline("anyhow", 8);
    let newest = source
        .source_dirs()
        .iter()
        .flat_map(|index| std::fs::read_dir(index).into_iter().flatten().flatten())
        .filter_map(|entry| Release::from_stem(entry.file_name().to_str()?))
        .filter(|release| release.name.as_str() == "anyhow")
        .max_by(|a, b| semver::cmp(a.version.as_str(), b.version.as_str()))
        .expect("anyhow is in the local cargo cache");
    assert_eq!(
        found.first(),
        Some(&newest),
        "the exact name leads, at its newest release: {found:?}"
    );
    assert!(source.offline("", 8).is_empty(), "no query, no offers");
    assert!(source.offline("no-such-crate-anywhere", 8).is_empty());
}

#[test]
fn a_tree_is_known_as_its_release_and_a_release_as_its_tree() {
    let source = source();
    let anyhow = release("anyhow", "1.0.104");
    let Availability::Unpacked(tree) = source.availability(&anyhow) else {
        panic!("anyhow 1.0.104 is unpacked on this machine")
    };
    assert_eq!(source.release_of(&tree), Some(anyhow.clone()));
    assert_eq!(
        source.release_of(Path::new(env!("CARGO_MANIFEST_DIR"))),
        None,
        "a project is not a release"
    );
    let resolved = source.resolve(&anyhow).expect("resolve");
    assert_eq!(resolved.origin, Origin::Cargo);
    assert_eq!(resolved.root, tree.canonicalize().expect("canonical"));
    assert_eq!(
        source.resolve(&release("toml", "0.1.0")),
        Err(SourceError::NeedsDownload(release("toml", "0.1.0")))
    );
}

#[test]
fn repeated_release_trees_are_not_chosen_by_directory_order() {
    let home = scratch("ambiguous-home");
    let release = release("tiny-crate", "1.2.3");
    for index in ["index.crates.io-a", "index.crates.io-b"] {
        let tree = home.join("registry/src").join(index).join(release.stem());
        std::fs::create_dir_all(&tree).expect("registry source tree");
        std::fs::write(
            tree.join("Cargo.toml"),
            b"[package]\nname = \"tiny-crate\"\nversion = \"1.2.3\"\n",
        )
        .expect("manifest");
    }

    let source = CargoCache::at(home.clone(), scratch("ambiguous-unpacked"));
    let error = source
        .resolve_unpacked(&release)
        .expect_err("two unrelated indexes are ambiguous");
    let SourceError::Ambiguous { paths, .. } = error else {
        panic!("expected ambiguity, got {error:?}")
    };
    assert_eq!(paths.len(), 2);
    assert!(matches!(
        source.availability(&release),
        Availability::Ambiguous { indexes } if indexes.len() == 2
    ));
    assert!(paths.iter().all(|path| path.join("Cargo.toml").is_file()));
    assert_eq!(
        source.authority_key().as_str(),
        CargoCache::at(home, scratch("elsewhere"))
            .authority_key()
            .as_str()
    );
    std::fs::remove_dir_all(source.home).expect("remove temporary registry");
}

#[test]
fn duplicate_index_metadata_is_ambiguous_before_a_download_is_considered() {
    let home = scratch("ambiguous-metadata-home");
    let release = release("tiny-crate", "1.2.3");
    for (index, checksum, yanked, date) in [
        ("index.crates.io-a", "a", false, "2026-01-02"),
        ("index.crates.io-b", "b", true, "2026-02-03"),
    ] {
        write_index_record(
            &home.join("registry/index").join(index),
            &release,
            &checksum.repeat(64),
            yanked,
            date,
        );
    }
    let source = CargoCache::at(home.clone(), scratch("ambiguous-metadata-unpacked"));
    assert!(matches!(
        source.availability(&release),
        Availability::Ambiguous { indexes } if indexes.len() == 2
    ));
    let published = source.releases(&release.name).remove(0);
    assert_eq!(published.date, RegistryFact::Ambiguous);
    assert_eq!(published.yanked, RegistryFact::Ambiguous);
    assert!(matches!(
        source.resolve(&release),
        Err(SourceError::Ambiguous { .. })
    ));
    std::fs::remove_dir_all(home).expect("remove temporary registry");
}

#[test]
fn an_explicit_source_root_selects_the_matching_registry_metadata() {
    let home = scratch("selected-home");
    let release = release("tiny-crate", "1.2.3");
    let selected = home.join("registry/src/index.crates.io-a");
    for index in ["index.crates.io-a", "index.crates.io-b"] {
        let tree = home.join("registry/src").join(index).join(release.stem());
        std::fs::create_dir_all(&tree).expect("registry source tree");
        std::fs::write(
            tree.join("Cargo.toml"),
            b"[package]\nname = \"tiny-crate\"\nversion = \"1.2.3\"\n",
        )
        .expect("manifest");
        let record = home.join("registry/index").join(index);
        write_index_record(
            &record,
            &release,
            if index.ends_with("-a") {
                "a".repeat(64).as_str()
            } else {
                "b".repeat(64).as_str()
            },
            index.ends_with("-b"),
            if index.ends_with("-a") {
                "2026-01-02"
            } else {
                "2026-02-03"
            },
        );
    }

    let source = CargoCache {
        home: home.clone(),
        source_root: Some(selected),
        unpacked: scratch("selected-unpacked"),
    };
    let resolved = source
        .resolve_unpacked(&release)
        .expect("the override selects one index");
    assert!(
        resolved
            .root
            .ends_with("index.crates.io-a/tiny-crate-1.2.3")
    );
    let published = source.releases(&release.name).remove(0);
    assert_eq!(published.date, RegistryFact::Known(Arc::from("2026-01-02")));
    assert_eq!(published.yanked, RegistryFact::Known(false));
    std::fs::remove_dir_all(home).expect("remove temporary registry");
}

#[test]
fn an_archive_unpacks_to_exactly_the_files_cargo_unpacked() {
    let anyhow = release("anyhow", "1.0.104");
    let fake = archive_only_home(&anyhow);
    let (private_root, unpacked) = private_unpack("unpacked");
    let source = CargoCache::at(fake.clone(), unpacked.clone());
    assert!(matches!(
        source
            .releases(&anyhow.name)
            .into_iter()
            .find(|entry| entry.release == anyhow)
            .map(|entry| entry.availability),
        Some(Availability::Archive(_))
    ));
    let checksum = source
        .unique_release_checksum(&anyhow)
        .expect("one effective archive checksum");
    let planned = source
        .own_tree(&anyhow, &checksum)
        .expect("valid checksum gives an app cache path");
    let tree = source.resolve(&anyhow).expect("unpack");
    assert_eq!(
        tree.root,
        planned.canonicalize().expect("planned is where it went")
    );
    assert!(matches!(tree.origin, Origin::Archive(_)));
    assert_eq!(
        source.release_of(&tree.root),
        Some(anyhow.clone()),
        "an unpacked tree is known as its release"
    );
    let cargo = source().cargo_tree(&anyhow).expect("cargo unpacked it too");
    let mut theirs = files(&cargo);
    theirs.remove(Path::new(".cargo-ok"));
    let ours = files(&tree.root);
    let differing = ours
        .keys()
        .chain(theirs.keys())
        .filter(|path| ours.get(*path) != theirs.get(*path))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        differing.is_empty(),
        "the unpacked tree differs from cargo's at {differing:?}"
    );
    assert!(
        ours.len() > 10 && ours.contains_key(Path::new("src/lib.rs")),
        "a whole crate was unpacked: {} files",
        ours.len()
    );
    let boundary = std::fs::read_to_string(tree.root.parent().expect("outer").join("Cargo.toml"))
        .expect("boundary");
    assert!(
        boundary.contains("members = [\"anyhow-1.0.104\"]")
            && boundary.contains("resolver = \"2\""),
        "{boundary}"
    );
    // A second resolve reads the tree that is there.
    assert_eq!(source.resolve(&anyhow).expect("again").root, tree.root);
    let archive = source.archive(&anyhow).expect("the cached archive");
    std::fs::remove_file(archive).expect("remove archive, keep verified app cache");
    assert!(matches!(
        source.availability(&anyhow),
        Availability::Unpacked(ref path) if path == &planned
    ));
    let different_authority = CargoCache {
        home: fake.clone(),
        source_root: Some(fake.join("registry/src/index.crates.io-other")),
        unpacked: unpacked.clone(),
    };
    assert_eq!(
        different_authority.availability(&anyhow),
        Availability::Download,
        "a different registry authority cannot reuse the app cache"
    );
    let _ = std::fs::remove_dir_all(&fake);
    let _ = std::fs::remove_dir_all(private_root);
}

#[test]
fn an_app_cache_is_rechecked_against_its_extracted_source_bytes() {
    let release = release("anyhow", "1.0.104");
    let fake = archive_only_home(&release);
    let (private_root, unpacked) = private_unpack("tampered-cache");
    let source = CargoCache::at(fake.clone(), unpacked);
    let archive = source.archive(&release).expect("archive");
    let tree = source.resolve(&release).expect("verified archive").root;
    std::fs::remove_file(archive).expect("remove original archive");
    std::fs::write(tree.join("src/lib.rs"), b"changed after verification")
        .expect("tamper with extracted source");
    assert_eq!(source.availability(&release), Availability::Download);
    assert!(matches!(
        source.resolve(&release),
        Err(SourceError::NeedsDownload(_))
    ));
    std::fs::remove_dir_all(fake).expect("remove temporary registry");
    std::fs::remove_dir_all(private_root).expect("remove private app cache");
}

#[test]
fn an_archive_that_is_not_the_published_one_is_refused() {
    let anyhow = release("anyhow", "1.0.104");
    let fake = archive_only_home(&anyhow);
    let archive = CargoCache::at(fake.clone(), scratch("unused"))
        .archive(&anyhow)
        .expect("copied");
    let unpacked = scratch("unpacked");
    let refused = archive::unpack(
        &archive,
        &"0".repeat(64),
        "test-authority",
        &anyhow,
        &unpacked,
    );
    assert!(
        matches!(refused, Err(SourceError::Integrity { .. })),
        "{refused:?}"
    );
    assert!(
        !unpacked.join("test-authority").join(anyhow.stem()).exists(),
        "nothing is left behind"
    );
    let _ = std::fs::remove_dir_all(&fake);
}

#[test]
fn an_archive_without_an_effective_index_checksum_is_not_read() {
    let release = release("anyhow", "1.0.104");
    let fake = archive_only_home(&release);
    std::fs::remove_dir_all(fake.join("registry/index")).expect("remove local index");
    let source = CargoCache::at(fake.clone(), scratch("unverified-unpacked"));
    assert!(matches!(
        source.availability(&release),
        Availability::UnverifiedArchive(_)
    ));
    assert_eq!(
        source.resolve(&release),
        Err(SourceError::UnverifiedArchive(release))
    );
    std::fs::remove_dir_all(fake).expect("remove temporary registry");
}

#[test]
fn the_boundary_uses_the_resolver_the_edition_implies() {
    assert!(
        archive::boundary("a-1.0.0", "[package]\nname = \"a\"\nedition = \"2021\"\n")
            .contains("resolver = \"2\"")
    );
    assert!(
        archive::boundary("a-1.0.0", "[package]\nedition = \"2018\"\n")
            .contains("resolver = \"1\"")
    );
    assert!(
        archive::boundary("a-1.0.0", "[package]\nedition = \"2024\"\n")
            .contains("resolver = \"3\"")
    );
    assert!(
        archive::boundary(
            "a-1.0.0",
            "[package]\nedition = \"2021\"\nresolver = \"1\"\n"
        )
        .contains("resolver = \"1\"")
    );
}
