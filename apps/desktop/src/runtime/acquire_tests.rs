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
fn a_release_only_the_registry_has_fails_in_words_and_is_never_fetched(cx: &mut TestAppContext) {
    // The worker is a real thread that wakes the UI task, as the read pool's do.
    cx.executor().allow_parking();
    let wanted = release("anyhash", "0.1.0");
    let composition = Composition {
        endpoint: PathBuf::from("/nonexistent/owner.sock"),
        source: Arc::new(Shelf),
        authority: Arc::from("shelf"),
        refusals: None,
    };
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
    assert_eq!(
        settled(&wanted, cx),
        Some(Stage::Failed(Arc::from(
            "anyhash 0.1.0 is not on this machine; reading it needs a download"
        ))),
        "a release only the registry has is refused, not downloaded"
    );
}

#[gpui::test]
fn an_owner_that_does_not_answer_is_a_failure_with_its_words_and_can_be_tried_again(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let wanted = release("anyhow", "1.0.104");
    let compose = || {
        Some(Composition {
            endpoint: PathBuf::from("/nonexistent/owner.sock"),
            source: Arc::new(Shelf),
            authority: Arc::from("shelf"),
            refusals: None,
        })
    };
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
