//! Hand-built in-memory packages for engine unit tests.
//!
//! # Why not fixtures
//!
//! `nudox_store::source::fixtures::FixtureSource` exists and is the right tool
//! for testing the *stream* (does `open_symbol` emit `Head` first?). It is the
//! wrong tool for testing the *version plane*, because it produces exactly one
//! generation of each of its two packages and there is no way to ask it for a
//! second one that differs in a controlled way.
//!
//! The timeline classifier's whole job is to notice a one-field difference
//! between two generations. Testing it needs packages where the author decides,
//! per version, exactly which field moved — a rename with the signature held
//! constant, a signature change with the name held constant, a doc edit with
//! everything else identical. That is what this module builds: no producer, no
//! rust-analyzer, no filesystem, just `PristineIntroTable::insert_live` and a
//! `PackageView` around it.

use std::sync::Arc;

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    entry::{Deprecation, Entry, Node, Symbol, Visibility},
    index::RawRef,
    kind::Kind,
    kinds::{Module, Reexport},
    view::IrView,
};
use nudox_store::package::{PackageView, Provenance};

/// A `cargo:` lineage for `name`.
pub(crate) fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new(name))
}

/// A deterministic `IntroId` from a single byte.
///
/// `IntroId` is opaque and only ever compared, so tests do not need a real
/// domain-separated digest — they need two generations to agree on the same
/// bytes, which is exactly what "the same `n`" gives.
pub(crate) fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

/// One entry to place in a hand-built package.
///
/// Every field defaults to something inert so a test only has to name the axis
/// it is exercising. `Kind` is deliberately restricted to the two unit-struct
/// kinds — `Module` renders as `mod X`, `Reexport` as `pub use X` — because
/// that is the cheapest way to make `chunk::signature::tokens` produce two
/// different token streams for the same name, which is what a signature-change
/// test needs.
#[derive(Clone)]
pub(crate) struct EntrySpec {
    intro: u8,
    name: String,
    docs: String,
    visibility: Visibility,
    deprecation: Option<Deprecation>,
    reexport: bool,
    source: std::path::PathBuf,
    span: std::ops::Range<usize>,
}

/// An inert entry: public module named `name`, no docs, not deprecated.
pub(crate) fn entry(intro: u8, name: &str) -> EntrySpec {
    EntrySpec {
        intro,
        name: name.to_owned(),
        docs: String::new(),
        visibility: Visibility::Public,
        deprecation: None,
        reexport: false,
        source: std::path::PathBuf::from("src/lib.rs"),
        span: 0..0,
    }
}

impl EntrySpec {
    /// Give the entry a doc comment.
    pub(crate) fn docs(mut self, docs: &str) -> Self {
        self.docs = docs.to_owned();
        self
    }

    /// Rename the entry while leaving its `IntroId` alone — a rename in the
    /// K18 sense, which is the only kind the IR models.
    pub(crate) fn named(mut self, name: &str) -> Self {
        self.name = name.to_owned();
        self
    }

    /// Change the entry's visibility.
    pub(crate) fn visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self
    }

    /// Mark the entry deprecated with `note`.
    pub(crate) fn deprecated(mut self, note: &str) -> Self {
        self.deprecation = Some(Deprecation {
            note: Some(note.to_owned()),
            since: None,
        });
        self
    }

    /// Render as `pub use X` instead of `mod X` — i.e. change the signature
    /// without touching the name.
    pub(crate) fn reexport(mut self) -> Self {
        self.reexport = true;
        self
    }

    /// Move the declaration to a different file and byte range.
    ///
    /// Used to prove that the classifier reports `Unchanged` for a declaration
    /// that only moved. See `crate::timeline` for why that is not automatic.
    pub(crate) fn moved_to(mut self, file: &str, span: std::ops::Range<usize>) -> Self {
        self.source = std::path::PathBuf::from(file);
        self.span = span;
        self
    }

    fn build(self) -> (IntroId, Entry) {
        let sym = Symbol {
            name: self.name,
            visibility: self.visibility,
            documentation: self.docs,
            source: self.source,
            span: self.span,
            aliases: Box::new([]),
            deprecation: self.deprecation,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        let kind = if self.reexport {
            Kind::Reexport(Reexport)
        } else {
            Kind::Module(Module)
        };
        (
            intro(self.intro),
            Entry::new(sym, Node::build(None::<RawRef>, []), kind),
        )
    }
}

/// Seal `entries` into a `PackageView` for `lineage`.
pub(crate) fn package(lineage: &PackageLineageId, entries: Vec<EntrySpec>) -> Arc<PackageView> {
    let mut table = PristineIntroTable::new();
    for spec in entries {
        let (id, e) = spec.build();
        table.insert_live(id, e, None);
    }
    let view = IrView::with_package(lineage.clone(), table);
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

/// Shorthand for a package of inert entries named by `(intro, name)` pairs.
pub(crate) fn package_with(lineage: &PackageLineageId, entries: &[(u8, &str)]) -> Arc<PackageView> {
    package(
        lineage,
        entries.iter().map(|(n, name)| entry(*n, name)).collect(),
    )
}

// ---------------------------------------------------------------------------
// StaticSource
// ---------------------------------------------------------------------------

/// An [`IrSource`] that replays a fixed list of already-built generations.
///
/// `FixtureSource` produces one generation per package and `ProducerSource`
/// needs a real toolchain and minutes of wall clock. Neither can express "here
/// are three versions of one crate that differ in exactly this way", which is
/// the input every test of the version plane needs.
///
/// Emits `Discovered` before `Ready` for each generation, as the [`IrSource`]
/// contract requires and as the engine's version-recovery depends on.
pub(crate) struct StaticSource {
    generations: Vec<(PackageLineageId, Option<String>, Arc<PackageView>)>,
}

impl StaticSource {
    /// Replay `(version, package)` pairs for `lineage`, in the given order.
    pub(crate) fn versions(
        lineage: &PackageLineageId,
        generations: Vec<(&str, Arc<PackageView>)>,
    ) -> Self {
        Self {
            generations: generations
                .into_iter()
                .map(|(v, p)| (lineage.clone(), Some(v.to_owned()), p))
                .collect(),
        }
    }
}

impl nudox_store::source::IrSource for StaticSource {
    fn describe(&self) -> nudox_store::source::SourceDescriptor {
        nudox_store::source::SourceDescriptor {
            label: "static".to_owned(),
            package_count_hint: Some(self.generations.len() as u32),
        }
    }

    fn load(
        &self,
        _req: nudox_store::source::LoadRequest,
    ) -> futures::stream::BoxStream<
        'static,
        Result<nudox_store::source::LoadEvent, nudox_store::source::SourceError>,
    > {
        use futures::StreamExt as _;
        use nudox_store::source::{LoadEvent, PackageHint};

        let events: Vec<_> = self
            .generations
            .iter()
            .flat_map(|(lineage, version, package)| {
                [
                    Ok(LoadEvent::Discovered {
                        lineage: lineage.clone(),
                        hint: PackageHint {
                            display_name: lineage.name.as_str().to_owned(),
                            ecosystem: lineage.ecosystem.as_str().to_owned(),
                            version: version.clone(),
                        },
                    }),
                    Ok(LoadEvent::Ready {
                        package: Arc::clone(package),
                    }),
                ]
            })
            .collect();

        futures::stream::iter(events).boxed()
    }
}

/// Poll until `cond` holds, or panic after ~500 ms.
///
/// Corpus seeding is spawned on the engine runtime and there is no completion
/// signal to await, so tests that need a loaded engine poll for the state they
/// depend on rather than sleeping a fixed amount and hoping.
pub(crate) async fn wait_until(mut cond: impl FnMut() -> bool, what: &str) {
    for _ in 0..50 {
        if cond() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Start an engine over `generations` of `lineage` and wait until it is settled.
///
/// "Settled" means two things, and both are needed because the seeding task
/// updates two structures in sequence:
///
/// 1. Every generation is in the version registry.
/// 2. The corpus holds the generation the registry calls current.
///
/// Waiting only on (1) is a race: `VersionRegistry::record` runs *before* the
/// `Corpus::insert` it authorises, so a test that checks the corpus straight
/// after seeing the registry fill up can observe the previous generation still
/// resident.
pub(crate) async fn start_and_settle(
    lineage: &PackageLineageId,
    generations: Vec<(&str, Arc<PackageView>)>,
) -> crate::runtime::EngineHandle {
    let expected = generations.len();
    let engine = crate::runtime::Engine::start(
        crate::runtime::EngineConfig::default(),
        StaticSource::versions(lineage, generations),
    );

    wait_until(
        || engine.versions(lineage).len() == expected,
        "all generations to be recorded",
    )
    .await;

    for _ in 0..50 {
        let want = engine.versions(lineage).current().map(|v| v.symbol_count);
        let have = engine
            .corpus()
            .package(lineage)
            .await
            .map(|p| p.view().table().len() as u64);
        if have.is_some() && have == want {
            return engine;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for the corpus to hold the current generation");
}
