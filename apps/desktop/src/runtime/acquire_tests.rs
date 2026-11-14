//! Find's offers (W-Acquire): a crate the source can supply that the library
//! does not have is offered once, by its package URL, with where its source
//! is; a registry tree the owner already indexed is the library's, never
//! offered again.

#![allow(clippy::expect_used, clippy::panic)]

use crate::host::registry::{Composition, Origin, RegistrySource, SourceError, SourceTree};
use crate::model::pages::PackageRef;
use crate::model::release::{Availability, CrateName, Published, Release};
use crate::runtime::acquire::{self, Stage};
use crate::runtime::offload::Asker;
use backend_library::{Basis, Row, RowId, object_version, package_key, view_state_root};
use gpui::{TestAppContext, WeakEntity};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[test]
fn project_acquisition_never_offers_an_alternative_registry_from_the_crates_io_source() {
    use backend_library::browse::PackageOrigin;
    let release = release("toml", "1.1.6");
    let other = PackageOrigin::Registry {
        source: "registry+https://packages.example.test/index".to_owned(),
    };
    let crates_io = PackageOrigin::Registry {
        source: "registry+https://github.com/rust-lang/crates.io-index".to_owned(),
    };
    assert!(matches!(
        super::acquire::dependency_origin(&other, &release, &Shelf),
        super::acquire::Origin::Elsewhere(_)
    ));
    assert!(matches!(
        super::acquire::dependency_origin(&crates_io, &release, &Shelf),
        super::acquire::Origin::Registry(_)
    ));
}

/// A source with two crates: `anyhow` (unpacked, not indexed) and `toml`
/// (unpacked at `/cache/toml-0.8.23`, which the owner indexed).
struct Shelf;

fn release(name: &str, version: &str) -> Release {
    Release::new(name, version).expect("release")
}

impl RegistrySource for Shelf {
    fn releases(&self, _: &CrateName) -> Vec<Published> {
        Vec::new()
    }

    fn offline(&self, query: &str, _: usize) -> Vec<Release> {
        [
            release("anyhow", "1.0.104"),
            release("toml", "0.8.23"),
            release("anyhash", "0.1.0"),
        ]
        .into_iter()
        .filter(|release| release.name.as_str().contains(query))
        .collect()
    }

    fn availability(&self, release: &Release) -> Availability {
        if release.name.as_str() == "anyhash" {
            Availability::Download
        } else {
            Availability::Unpacked(PathBuf::from(format!("/cache/{}", release.stem())))
        }
    }

    fn release_of(&self, root: &Path) -> Option<Release> {
        (root == Path::new("/cache/toml-0.8.23")).then(|| release("toml", "0.8.23"))
    }

    fn resolve(&self, release: &Release) -> Result<SourceTree, SourceError> {
        match self.availability(release) {
            Availability::Unpacked(root) => Ok(SourceTree {
                release: release.clone(),
                root,
                origin: Origin::Cargo,
            }),
            Availability::Archive(_)
            | Availability::UnverifiedArchive(_)
            | Availability::Ambiguous { .. }
            | Availability::Download => Err(SourceError::NeedsDownload(release.clone())),
        }
    }
}

fn package_row(label: &str) -> Row {
    Row::new(
        RowId::Package(package_key(label)),
        Basis::new(view_state_root(&[]), object_version(b"fixture")),
        label,
    )
}

#[test]
fn a_crate_not_in_the_library_is_offered_once_with_where_its_source_is() {
    let indexed = package_row("/cache/toml-0.8.23");
    let found = super::browse_reads::find_packages("any", &[&indexed], &[], Some(&Shelf));
    let names = found
        .iter()
        .map(|package| (package.name.as_ref(), package.package.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            ("anyhash", "pkg:cargo/anyhash@0.1.0"),
            ("anyhow", "pkg:cargo/anyhow@1.0.104")
        ],
        "offers are keyed by their package URL"
    );
    let anyhow = found
        .iter()
        .find(|package| package.name.as_ref() == "anyhow")
        .expect("anyhow");
    let offer = anyhow.offer.as_ref().expect("anyhow is offered");
    assert_eq!(offer.release, release("anyhow", "1.0.104"));
    assert_eq!(
        offer.availability,
        Availability::Unpacked(PathBuf::from("/cache/anyhow-1.0.104"))
    );
    assert_eq!(offer.library, None, "not in the library yet");
    assert!(!anyhow.indexed);
    let anyhash = found
        .iter()
        .find(|package| package.name.as_ref() == "anyhash")
        .expect("anyhash");
    assert_eq!(
        anyhash.offer.as_ref().map(|offer| &offer.availability),
        Some(&Availability::Download),
        "a crate only the registry has says so"
    );
}

#[test]
fn a_release_the_owner_indexed_is_the_librarys_and_not_offered_again() {
    let indexed = package_row("/cache/toml-0.8.23");
    let found = super::browse_reads::find_packages("toml", &[&indexed], &[], Some(&Shelf));
    assert_eq!(
        found.len(),
        1,
        "one row for toml 0.8.23, not an indexed row and an offer: {found:?}"
    );
    let toml = &found[0];
    assert!(toml.indexed);
    assert_eq!(
        toml.package,
        PackageRef::parse("/cache/toml-0.8.23").expect("root")
    );
    assert_eq!(
        toml.name.as_ref(),
        "toml",
        "named as its crate, not its directory"
    );
    let offer = toml
        .offer
        .as_ref()
        .expect("the indexed tree is known as its release");
    assert_eq!(offer.release, release("toml", "0.8.23"));
    assert_eq!(
        offer.library.as_ref(),
        Some(&toml.package),
        "its page is the indexed root"
    );
}

/// `Shelf`, with anyhow's earlier releases listed by its registry index.
struct Versions;

impl RegistrySource for Versions {
    fn releases(&self, name: &CrateName) -> Vec<Published> {
        if name.as_str() != "anyhow" { return Vec::new(); }
        ["1.0.98", "1.0.104"].into_iter().map(|version| {
            let release = release("anyhow", version);
            Published {
                availability: self.availability(&release),
                release,
                date: crate::model::release::RegistryFact::Missing,
                yanked: crate::model::release::RegistryFact::Known(false),
            }
        }).collect()
    }
    fn offline(&self, query: &str, limit: usize) -> Vec<Release> { Shelf.offline(query, limit) }
    fn availability(&self, release: &Release) -> Availability {
        if release.version.as_str() == "1.0.98" {
            Availability::Archive(PathBuf::from("/cache/anyhow-1.0.98.crate"))
        } else {
            Shelf.availability(release)
        }
    }
    fn release_of(&self, root: &Path) -> Option<Release> { Shelf.release_of(root) }
    fn resolve(&self, release: &Release) -> Result<SourceTree, SourceError> { Shelf.resolve(release) }
}

/// GUI-08: Find offers an exact older release when it is asked for by
/// `name@version`, ahead of the newest one; an unlisted version offers none.
#[test]
fn name_at_version_offers_that_exact_release_first() {
    let found = super::browse_reads::find_packages("anyhow@1.0.98", &[], &[], Some(&Versions));
    let offers = found.iter()
        .map(|package| (package.package.as_str(), package.offer.as_ref().map(|offer| offer.release.clone())))
        .collect::<Vec<_>>();
    assert_eq!(offers.first(), Some(&("pkg:cargo/anyhow@1.0.98", Some(release("anyhow", "1.0.98")))),
        "the asked release leads: {offers:?}");
    assert_eq!(found[0].offer.as_ref().map(|offer| &offer.availability),
        Some(&Availability::Archive(PathBuf::from("/cache/anyhow-1.0.98.crate"))), "with where its source is");
    assert!(found.iter().all(|package| package.name.as_ref() == "anyhow"), "only its name matches: {offers:?}");
    let unlisted = super::browse_reads::find_packages("anyhow@9.9.9", &[], &[], Some(&Versions));
    assert!(unlisted.iter().all(|package| package.offer.as_ref().is_none_or(|offer| offer.release.version.as_str() != "9.9.9")),
        "an unlisted version is never invented: {unlisted:?}");
    let plain = super::browse_reads::find_packages("anyhow", &[], &[], Some(&Versions));
    assert_eq!(plain.iter().filter_map(|package| package.offer.as_ref()).map(|offer| offer.release.clone()).collect::<Vec<_>>(),
        [release("anyhow", "1.0.104")], "a bare name still offers the newest here");
}

#[test]
fn without_a_source_nothing_is_offered() {
    let found = super::browse_reads::find_packages("anyhow", &[], &[], None);
    assert!(found.is_empty());
}

/// Lets the worker thread and the drain task run until `release` is no
/// longer under way (the worker is a real thread; the executor is not).
fn settled(release: &Release, cx: &mut TestAppContext) -> Option<Stage> {
    for _ in 0..500 {
        cx.run_until_parked();
        let stage = cx.update(|cx| acquire::stage(release, Asker::Everyone, cx));
        if !stage.as_ref().is_some_and(Stage::working) {
            return stage;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("{release} never settled")
}

#[gpui::test]
fn a_registry_only_release_needs_the_owner_to_fetch_it(cx: &mut TestAppContext) {
    // The worker is a real thread that wakes the UI task, as the read pool's do.
    cx.executor().allow_parking();
    let wanted = release("anyhash", "0.1.0");
    let composition = Composition {
        endpoint: PathBuf::from("/nonexistent/owner.sock"),
        source: Arc::new(Shelf),
        authority: Arc::from("shelf"),
        generation: crate::host::registry::CompositionGeneration::default(),
        refusals: None,
    };
    let composition = cx.update(|cx| acquire::test_install_producer(composition, cx));
    cx.update(|cx| {
        acquire::add_with(
            wanted.clone(),
            Some(composition),
            WeakEntity::new_invalid(),
            cx,
        )
    });
    assert_eq!(
        cx.update(|cx| acquire::stage(&wanted, Asker::Everyone, cx)),
        Some(Stage::Queued),
        "it is taken at once"
    );
    let Some(Stage::Failed(words)) = settled(&wanted, cx) else {
        panic!("the unreachable owner must fail the request")
    };
    assert!(
        words.starts_with("the local index could not be reached: "),
        "only the owner can fetch the published release: {words}"
    );
}

#[gpui::test]
fn an_owner_that_does_not_answer_is_a_failure_with_its_words_and_can_be_tried_again(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let wanted = release("anyhow", "1.0.104");
    let composition = cx.update(|cx| acquire::test_install_producer(Composition {
            endpoint: PathBuf::from("/nonexistent/owner.sock"),
            source: Arc::new(Shelf),
            authority: Arc::from("shelf"),
            generation: crate::host::registry::CompositionGeneration::default(),
            refusals: None,
        }, cx));
    let compose = || Some(composition.clone());
    cx.update(|cx| acquire::add_with(wanted.clone(), compose(), WeakEntity::new_invalid(), cx));
    let Some(Stage::Failed(words)) = settled(&wanted, cx) else {
        panic!("an unreachable source is a failure")
    };
    assert!(
        words.starts_with("the index refused anyhow 1.0.104: "),
        "the failure names the release and carries the owner's words: {words}"
    );
    // Asking again after a failure tries again (and fails the same way).
    cx.update(|cx| acquire::add_with(wanted.clone(), compose(), WeakEntity::new_invalid(), cx));
    assert_eq!(
        cx.update(|cx| acquire::stage(&wanted, Asker::Everyone, cx)),
        Some(Stage::Queued)
    );
    assert!(matches!(settled(&wanted, cx), Some(Stage::Failed(_))));
}

/// A press on Add or Remove while no owner is serving the library (the owner
/// is starting, replaced, or its freshness is unconfirmed) is said, never
/// dropped: the release reports why nothing was sent, and the ordinary "Try
/// again" is offered.
#[gpui::test]
fn an_add_or_removal_pressed_while_no_owner_serves_says_why_it_was_not_sent(cx: &mut TestAppContext) {
    use crate::navigation::Intent;
    use crate::runtime::acquire::Removal;
    use crate::runtime::owner::OwnerGate;
    let wanted = release("anyhash", "0.1.0");
    let page = PackageRef::parse("/cache/anyhash-0.1.0").expect("page");
    let pool = crate::runtime::reads::ReadPool::start(1, |_| crate::shell::tests::Fixture).expect("pool");
    // The owner is still starting: no serving attachment, no mutation lease.
    let mut rig = crate::shell::tests::rig_with_engine_gate(
        cx, None, 1200.0, 800.0, pool, crate::shell::tests::RootOnly, Some(OwnerGate::starting()),
    );
    // A registry producer exists, so the only thing missing is the owner.
    let composition = rig.cx.update(|_, cx| acquire::test_install_producer(Composition {
        endpoint: PathBuf::from("/nonexistent/owner.sock"),
        source: Arc::new(Shelf),
        authority: Arc::from("shelf"),
        generation: crate::host::registry::CompositionGeneration::default(),
        refusals: None,
    }, cx));
    let _ = composition;
    assert_eq!(rig.cx.update(|_, cx| acquire::stage(&wanted, Asker::Everyone, cx)), None, "nothing was asked yet");

    rig.go(Intent::AddRelease(wanted.clone()));
    let Some(Stage::Failed(words)) = rig.cx.update(|_, cx| acquire::stage(&wanted, Asker::Everyone, cx)) else {
        panic!("an Add with no serving owner must say it was not sent")
    };
    assert!(words.contains("index is not open yet") && words.contains("Try again"), "{words}");

    rig.go(Intent::RemoveRelease { release: wanted.clone(), page });
    let Some(Removal::Failed(words)) = rig.cx.update(|_, cx| acquire::removal(&wanted, Asker::Everyone, cx)) else {
        panic!("a Remove with no serving owner must say it was not sent")
    };
    assert!(words.contains("index is not open yet"), "{words}");
}
