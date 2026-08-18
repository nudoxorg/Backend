//! Red-first specification: **the local engine answers `Serve<Symbols>`**
//! (contract §2, task S3a — the last blocker for the GUI cutover).
//!
//! # Why this exists
//!
//! `Federated<S>` (landed, `heart/surface/federated.rs`) merges several
//! `Arc<dyn Serve<S>>` into one, so a caller cannot tell local from remote.
//! `RemoteClient` already gets `Serve<S>` free via a blanket impl. The local
//! engine does not implement it at all — it exposes
//! `EngineHandle::search(SearchQuery, Gen) -> (StreamHandle,
//! flume::Receiver<SearchEvent>)`, a completely different shape carrying a
//! completely different row type (`HitRow`). Until that gap closes there is
//! nothing to federate *with*, and lindsey keeps its two hand-written search
//! paths.
//!
//! # The local plane is the *rich* side — this inverts the usual assumption
//!
//! `HitRow` carries `sig_preview: Vec<SigToken>`, a fully rendered declaration,
//! because the engine has IR loaded. The index does not: every remote
//! `SymbolHit` has `signature: None` and (today) `reference: None`, and
//! `SymbolHit::from(Symbol)`'s doc comment says exactly why.
//!
//! So the local engine is what makes a federated row *navigable and readable*.
//! `Symbols::fuse` backfills both fields onto a remote winner, which is the
//! entire payoff of federating rather than choosing a plane. These tests pin
//! that the engine actually supplies them.
//!
//! # The one genuine blocker this closes
//!
//! `SymbolHit::package` is a `PackageId` = `UUIDv5(RegistryOrigin ‖ name ‖
//! **concrete version**)` (`heart/package/coordinates.rs:35-41`). The version
//! is part of the hash seed, not decoration — and it is **structurally absent**
//! at the hit-build site: `PackageMetadata` (`nudox-engine/src/lib.rs:388-398`)
//! has no version field, `PackageView` holds no back-reference to the
//! `VersionRegistry` that knows it, and `run_search` only ever sees
//! `Vec<Arc<PackageView>>`.
//!
//! The version is not *unknown*, only *discarded*: `IrSource` supplies it at
//! load time (the fixture below passes `Some("1.4.2")`, exactly as
//! `StaticSource` does in `tests/engine/multi_package_flows.rs:241`). The fix
//! is to retain it on `PackageView` rather than to thread a registry through
//! three call layers — a materialized package view that cannot say which
//! version it is has a hole in it regardless of this task.
//!
//! `a_local_hit_carries_the_package_id_the_index_would_mint` is the test that
//! matters: if the two planes disagree here, dedup silently never fires and
//! every synced package renders twice, with nothing erroring.
//!
//! # Requests the engine cannot honour must FAIL, not be ignored
//!
//! `Symbols::Request` is `heart::query::Query`, which carries fields the local
//! engine has no mechanism for: `at: Option<AsOf>` (point-in-time), and
//! `page.cursor` (resume token — `collect_name_hits` supports only a top-`N`
//! limit, with no offset concept at all).
//!
//! Silently ignoring either one returns a **confidently wrong answer**: current
//! data presented as historical, or page 1 presented as page 2. Both are worse
//! than no answer, and neither is detectable by the caller. So the adapter
//! rejects them with `Frame::Failed`. `routing`/`session` are hints and are
//! safe to drop; `rank`/`mode` select behaviour the engine applies
//! unconditionally, so ignoring them changes nothing observable.
//!
//! **Do not weaken these tests to make them pass.** In particular, do not make
//! the unsupported-request tests pass by inventing a partial cursor
//! implementation, and do not drop `Field`/`Param` hits to simplify the kind
//! mapping — see `member_kinds_are_mapped_not_dropped`.

use std::sync::Arc;

use heart::surface::{Frame, Gen as SurfaceGen, Serve, Symbols};
use heart::{Language, RegistryOrigin};

// ---------------------------------------------------------------------------
// Fixture source
//
// `nudox_engine::test_support` is crate-private, so this mirrors the minimal
// `IrSource` pattern `tests/engine/multi_package_flows.rs:30-244` already
// establishes. Copy that file's helpers (`lineage`, `intro`, `build_package`,
// `StaticSource`) rather than inventing a second shape.
//
// The one thing that matters here and not there: the source supplies a
// **version** (`Some("1.4.2")`), which is what `PackageId` derivation needs.
// ---------------------------------------------------------------------------

mod fixture {
    //! `lineage`/`intro`/`StaticSource` are lifted verbatim from
    //! `tests/engine/multi_package_flows.rs:30-244` (that file's own doc
    //! comment explains why it has its own copy: `nudox_engine::test_support`
    //! is crate-private and unreachable from an integration test).
    //!
    //! Two adaptations beyond that file's minimal shape:
    //!
    //! * Every built package gets a synthetic root module (named after the
    //!   package) with every other entry parented under it, so
    //!   `PackageIndexes::path_of` — what `crate::surface`'s adapter uses for
    //!   `SymbolHit::path` — actually returns more than one segment. A
    //!   root-level entry with no parent has a single-segment moniker
    //!   identical to its own display name, which would make
    //!   `the_path_is_fully_qualified` fail for a reason that has nothing to
    //!   do with the adapter: real producers root every declaration under the
    //!   crate/module tree the same way (see `qualified_display_name`'s own
    //!   doc comment on why the moniker "already begins with the crate's root
    //!   module for most producers").
    //! * The source supplies a **version** (`Some(VERSION)`), which is what
    //!   `PackageId` derivation needs and what `multi_package_flows.rs` never
    //!   had to carry.

    use std::sync::Arc;

    use futures::stream::BoxStream;
    use heart::query::StableReference;
    use heart::surface::{Serve, Symbols};

    use nudox_ir::{
        apply::PristineIntroTable,
        change::{EcosystemId, IntroId, PackageLineageId, PackageName},
        entry::{Entry, Node, Symbol, Visibility},
        index::RawRef,
        kind::Kind,
        kinds::{Field, FieldKey, Module},
        view::IrView,
    };
    use nudox_engine::store::{
        package::{PackageView, Provenance},
        source::{Error, IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor},
    };
    use nudox_engine::{Engine, EngineConfig, EngineHandle, wire};

    // -----------------------------------------------------------------------
    // Minimal source helpers (verbatim from `multi_package_flows.rs`)
    // -----------------------------------------------------------------------

    fn lineage(ecosystem: &str, name: &str) -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new(ecosystem), PackageName::new(name))
    }

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    struct StaticSource {
        items: Vec<(PackageLineageId, Option<String>, Arc<PackageView>)>,
    }

    impl StaticSource {
        fn new(items: Vec<(PackageLineageId, Option<String>, Arc<PackageView>)>) -> Self {
            Self { items }
        }
    }

    impl IrSource for StaticSource {
        fn describe(&self) -> SourceDescriptor {
            SourceDescriptor {
                label: "static-serve-symbols".to_owned(),
                package_count_hint: Some(self.items.len() as u32),
            }
        }

        fn load(&self, _req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, Error>> {
            use futures::StreamExt as _;
            let events: Vec<Result<LoadEvent, Error>> = self
                .items
                .iter()
                .flat_map(|(lid, version, pkg)| {
                    [
                        Ok(LoadEvent::Discovered {
                            lineage: lid.clone(),
                            hint: PackageHint {
                                display_name: lid.name.as_str().to_owned(),
                                ecosystem: lid.ecosystem.as_str().to_owned(),
                                version: version.clone(),
                            },
                        }),
                        Ok(LoadEvent::Ready {
                            package: Arc::clone(pkg),
                        }),
                    ]
                })
                .collect();
            futures::stream::iter(events).boxed()
        }
    }

    // -----------------------------------------------------------------------
    // Entry / package builders
    // -----------------------------------------------------------------------

    fn bare_symbol(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    fn module_entry(name: &str) -> Entry {
        Entry::new(
            bare_symbol(name),
            Node::build(None::<RawRef>, []),
            Kind::Module(Module),
        )
    }

    /// A `Field` entry — for `member_kinds_are_mapped_not_dropped`, which
    /// needs a kind the adapter has no 1:1 `SymbolKind` target for.
    fn field_entry(name: &str) -> Entry {
        let field = Field {
            key: FieldKey::Named,
            ty: None,
            attributes: Vec::new().into_boxed_slice(),
        };
        Entry::new(bare_symbol(name), Node::build(None::<RawRef>, []), Kind::Field(field))
    }

    /// A distinct intro for the synthetic root module every built package
    /// gets — see this module's own doc comment.
    fn root_intro() -> IntroId {
        IntroId::from_raw([0xFF; 32])
    }

    /// A wide-range intro for `engine_serve_with_many`, which needs more
    /// distinct ids than `intro`'s `u8` domain covers.
    fn intro_wide(n: u32) -> IntroId {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&n.to_le_bytes());
        IntroId::from_raw(bytes)
    }

    /// Build a package view: a synthetic root module named after the
    /// package, plus every one of `entries` parented under it.
    fn build_package_view(
        lid: &PackageLineageId,
        entries: Vec<(IntroId, Entry)>,
        version: Option<String>,
    ) -> Arc<PackageView> {
        let mut table = PristineIntroTable::new();
        let root = root_intro();
        table.insert_live(root, module_entry(lid.name.as_str()), None);
        for (id, entry) in entries {
            table.insert_live(id, entry, Some(root));
        }
        let view = IrView::with_package(lid.clone(), table);
        Arc::new(PackageView::build(view, Provenance::TrustedLocal).with_version(version))
    }

    fn build_package(
        lid: &PackageLineageId,
        entries: &[(u8, &str)],
        version: Option<String>,
    ) -> Arc<PackageView> {
        build_package_view(
            lid,
            entries
                .iter()
                .map(|(n, name)| (intro(*n), module_entry(name)))
                .collect(),
            version,
        )
    }

    /// Wait until `n` `PackageLoadEvent`s have arrived, or 2s pass.
    async fn wait_for_packages(engine: &EngineHandle, n: usize) {
        let rx = engine.packages();
        let deadline = std::time::Duration::from_secs(2);
        let mut seen = 0usize;
        let _ = tokio::time::timeout(deadline, async {
            while seen < n {
                match rx.recv_async().await {
                    Ok(_) => seen += 1,
                    Err(_) => break,
                }
            }
        })
        .await;
    }

    // -----------------------------------------------------------------------
    // Public fixture surface
    // -----------------------------------------------------------------------

    /// A `Serve<Symbols>` over a single package exporting `Router`.
    pub async fn engine_serve(pkg: &str, version: &str) -> Arc<dyn Serve<Symbols>> {
        let (serve, _engine) = engine_serve_with_handle(pkg, version).await;
        serve
    }

    /// Same as [`engine_serve`], also returning the underlying `EngineHandle`
    /// so a test can verify a reference resolves to a real entry.
    pub async fn engine_serve_with_handle(pkg: &str, version: &str) -> (Arc<dyn Serve<Symbols>>, EngineHandle) {
        let lid = lineage("cargo", pkg);
        let package = build_package(&lid, &[(1, "Router")], Some(version.to_owned()));
        let source = StaticSource::new(vec![(lid, Some(version.to_owned()), package)]);
        let engine = Engine::start(EngineConfig::default(), source);
        wait_for_packages(&engine, 1).await;
        let serve: Arc<dyn Serve<Symbols>> = Arc::new(engine.clone());
        (serve, engine)
    }

    /// A `Serve<Symbols>` over a package exporting `Router` (a module) and
    /// `field_one` (a `Field` — a member kind with no 1:1 `SymbolKind`
    /// target).
    pub async fn engine_serve_with_members(pkg: &str, version: &str) -> Arc<dyn Serve<Symbols>> {
        let lid = lineage("cargo", pkg);
        let entries = vec![
            (intro(1), module_entry("Router")),
            (intro(2), field_entry("field_one")),
        ];
        let package = build_package_view(&lid, entries, Some(version.to_owned()));
        let source = StaticSource::new(vec![(lid, Some(version.to_owned()), package)]);
        let engine = Engine::start(EngineConfig::default(), source);
        wait_for_packages(&engine, 1).await;
        Arc::new(engine)
    }

    /// A `Serve<Symbols>` over a package exporting `count` distinctly-named
    /// `Router*` modules, all matching a prefix search for `"Router"` — for
    /// the page-limit and cancellation tests, which need enough in-flight
    /// work to observe.
    pub async fn engine_serve_with_many(pkg: &str, version: &str, count: usize) -> Arc<dyn Serve<Symbols>> {
        let lid = lineage("cargo", pkg);
        let entries: Vec<(IntroId, Entry)> = (0..count as u32)
            .map(|i| (intro_wide(i), module_entry(&format!("Router{i}"))))
            .collect();
        let package = build_package_view(&lid, entries, Some(version.to_owned()));
        let source = StaticSource::new(vec![(lid, Some(version.to_owned()), package)]);
        let engine = Engine::start(EngineConfig::default(), source);
        wait_for_packages(&engine, 1).await;
        Arc::new(engine)
    }

    /// Whether `reference` names a real, resolvable entry in `engine`'s
    /// corpus — drives `EngineHandle::open_symbol` (already public,
    /// `lib.rs`'s documented surface) and checks whether the first event is
    /// a real `Head` rather than `Failed(SymbolNotFound)`.
    pub async fn resolves(engine: &EngineHandle, reference: &StableReference) -> bool {
        let Some(bytes) = parse_hex(reference.intro_hex()) else {
            return false;
        };
        let package = PackageLineageId::new(
            EcosystemId::new(reference.ecosystem().to_owned()),
            PackageName::new(reference.package().to_owned()),
        );
        let intro = IntroId::from_raw(bytes);
        let key = wire::SymbolKey::new(package, intro);
        let (_handle, rx) = engine.open_symbol(key, wire::Gen(1));
        matches!(rx.recv_async().await, Ok(wire::DocEvent::Head(_)))
    }

    fn parse_hex(s: &str) -> Option<[u8; 32]> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
        }
        Some(out)
    }

    /// Whether the engine's own `crate::surface` adapter currently considers
    /// any local search live — see `nudox_engine::surface::
    /// active_symbol_searches`'s own doc comment.
    pub fn has_in_flight_search() -> bool {
        nudox_engine::surface::active_symbol_searches() > 0
    }
}

const PKG: &str = "acme";
const VERSION: &str = "1.4.2";

/// Drain an `Answer<Symbols>` to completion, returning the converged rows.
///
/// **Upsert, not append** — see `Frame::Item`'s doc comment. A repeat key is a
/// supersede, not a new row; appending here would model a broken consumer.
async fn drain(
    answer: heart::surface::Answer<Symbols>,
) -> (Vec<heart::Scored<heart::surface::SymbolHit>>, Option<Frame<Symbols>>) {
    use heart::surface::Surface as _;
    let mut rows: Vec<heart::Scored<heart::surface::SymbolHit>> = Vec::new();
    let mut terminal = None;
    while let Some(frame) = answer.recv().await {
        match frame {
            Frame::Item(located) => {
                let key = Symbols::key(&located.value);
                match rows.iter_mut().find(|r| Symbols::key(r) == key) {
                    Some(existing) => *existing = located.value,
                    None => rows.push(located.value),
                }
            }
            other if other.is_terminal() => terminal = Some(other),
            _ => {}
        }
    }
    (rows, terminal)
}

/// The plain "find everything named X" request.
fn query(text: &str) -> heart::query::Query {
    heart::query::Query {
        target: heart::query::Target::Symbols,
        text: text.to_owned(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// 1. It is a `Serve<Symbols>`
// ---------------------------------------------------------------------------

/// THE structural test. If the engine cannot be held as `Arc<dyn
/// Serve<Symbols>>` there is nothing for `Federated` to merge and the GUI
/// cutover cannot happen.
#[tokio::test]
async fn the_engine_is_a_serve_symbols() {
    let serve: Arc<dyn Serve<Symbols>> = fixture::engine_serve(PKG, VERSION).await;
    let answer = serve.serve(query("Router"), SurfaceGen(1));
    let (rows, terminal) = drain(answer).await;

    assert!(!rows.is_empty(), "the fixture package exports `Router`");
    assert!(
        matches!(terminal, Some(Frame::End(_))),
        "the answer must end with a terminal frame, got {terminal:?}"
    );
}

/// `serve` must return immediately — lindsey calls it from the foreground
/// thread on every keystroke and cannot await a constructor there.
#[tokio::test]
async fn serve_returns_before_the_search_completes() {
    let serve = fixture::engine_serve(PKG, VERSION).await;

    let start = std::time::Instant::now();
    let answer = serve.serve(query("Router"), SurfaceGen(1));
    let elapsed = start.elapsed();

    assert!(
        elapsed < std::time::Duration::from_millis(100),
        "serve blocked for {elapsed:?}; it must return immediately with frames \
         arriving afterward"
    );
    drain(answer).await;
}

// ---------------------------------------------------------------------------
// 2. Identity — the cross-plane pin
// ---------------------------------------------------------------------------

/// THE test. The engine must mint the *same* `PackageId` the index would for
/// the same package, or `Symbols::key` disagrees across planes, dedup never
/// fires, and every synced package renders twice with nothing erroring.
///
/// This currently cannot pass: the version is discarded at load and
/// `PackageView` cannot report it (see this file's module doc).
#[tokio::test]
async fn a_local_hit_carries_the_package_id_the_index_would_mint() {
    let serve = fixture::engine_serve(PKG, VERSION).await;
    let (rows, _) = drain(serve.serve(query("Router"), SurfaceGen(1))).await;

    // What the index derives from full coordinates.
    let expected = heart::package::Coordinates {
        origin: RegistryOrigin::default_for(Language::Rust),
        name: heart::package::PackageName::from_canonical(Language::Rust, PKG, PKG),
        version: heart::PackageVersion::Cargo(
            semver::Version::parse(VERSION).expect("valid semver"),
        ),
    }
    .id();

    let hit = rows.first().expect("at least one hit");
    assert_eq!(
        hit.value.package, expected,
        "the engine minted a different PackageId than the index would — dedup \
         across planes will silently never fire"
    );
}

/// The navigable identity. `reference` is what makes a federated row openable;
/// its absence is what makes remote-only rows (correctly) not openable yet.
#[tokio::test]
async fn a_local_hit_carries_a_navigable_reference() {
    let serve = fixture::engine_serve(PKG, VERSION).await;
    let (rows, _) = drain(serve.serve(query("Router"), SurfaceGen(1))).await;

    let hit = rows.first().expect("at least one hit");
    let reference = hit
        .value
        .reference
        .as_ref()
        .expect("the local plane has IR, so it can always supply a reference");

    assert_eq!(reference.ecosystem(), "cargo");
    assert_eq!(reference.package(), PKG);
    assert_eq!(
        reference.intro_hex().len(),
        64,
        "an IntroId is a 32-byte blake3 digest in lowercase hex, got {:?}",
        reference.intro_hex()
    );
    assert!(
        reference
            .intro_hex()
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
}

/// The reference must be the *real* `IntroId`, not a synthesized one. This is
/// the defect `gui/src/stores/search_model.rs:607` ships today: a fabricated
/// key cannot match a sealed entry, so `resolve_symbol` returns
/// `SymbolNotFound` and clicking the row fails.
#[tokio::test]
async fn the_reference_resolves_to_a_real_entry() {
    let (serve, engine) = fixture::engine_serve_with_handle(PKG, VERSION).await;
    let (rows, _) = drain(serve.serve(query("Router"), SurfaceGen(1))).await;

    let reference = rows
        .first()
        .expect("at least one hit")
        .value
        .reference
        .clone()
        .expect("a local hit has a reference");

    assert!(
        fixture::resolves(&engine, &reference).await,
        "the reference does not name a live entry — it is synthesized, not real"
    );
}

/// The fully-qualified path, not a display string. `Symbols::key`'s other half
/// is this field, so a display string here (which collapses segments and
/// conditionally prefixes the package name) would break dedup for exactly the
/// symbols most likely to appear in both planes.
#[tokio::test]
async fn the_path_is_fully_qualified() {
    let serve = fixture::engine_serve(PKG, VERSION).await;
    let (rows, _) = drain(serve.serve(query("Router"), SurfaceGen(1))).await;

    let hit = rows.first().expect("at least one hit");
    assert!(
        hit.value.path.contains("Router"),
        "path {:?} does not name the symbol",
        hit.value.path
    );
    assert!(
        hit.value.path.len() > hit.value.display_name.len(),
        "path {:?} is no more qualified than the display name {:?} — this looks \
         like a leaf, not a fully-qualified path",
        hit.value.path,
        hit.value.display_name
    );
}

// ---------------------------------------------------------------------------
// 3. Fidelity — the local plane's contribution to a federated row
// ---------------------------------------------------------------------------

/// The engine has IR, so it can render a declaration. This is the field
/// `Symbols::fuse` backfills onto a bare remote winner, and the reason
/// federating beats picking a plane.
#[tokio::test]
async fn a_local_hit_carries_a_rendered_signature() {
    let serve = fixture::engine_serve(PKG, VERSION).await;
    let (rows, _) = drain(serve.serve(query("Router"), SurfaceGen(1))).await;

    let hit = rows.first().expect("at least one hit");
    let signature = hit
        .value
        .signature
        .as_ref()
        .expect("the local plane has IR, so it can render a signature");
    assert!(
        !signature.tokens().is_empty(),
        "an empty token list is not a rendered signature"
    );
}

/// `KindDiscriminant` has 13 variants; `SymbolKind` has 8. The catalog-building
/// precedent at `index/server/coordination/compile_inprocess.rs` *drops*
/// `Field`/`Param` (`return None`) because a catalog does not want them.
///
/// Local search deliberately **does** return them — `hits.rs`'s
/// `is_member_kind`/`kind_weight` score them down rather than exclude them. So
/// reusing that precedent here would silently narrow what local search returns
/// today: a regression disguised as a type conversion. They map to
/// `SymbolKind::Other`, which is what that variant is for.
#[tokio::test]
async fn member_kinds_are_mapped_not_dropped() {
    let serve = fixture::engine_serve_with_members(PKG, VERSION).await;
    let (rows, _) = drain(serve.serve(query("field_one"), SurfaceGen(1))).await;

    assert!(
        !rows.is_empty(),
        "a field hit vanished in the kind conversion; local search returns \
         member kinds and the adapter must not silently drop them"
    );
    assert_eq!(
        rows[0].value.kind,
        heart::SymbolKind::Other,
        "a member kind with no 1:1 target belongs in `Other`"
    );
}

// ---------------------------------------------------------------------------
// 4. Requests the engine cannot honour
// ---------------------------------------------------------------------------

/// A point-in-time request answered with *current* data is a confidently wrong
/// answer the caller cannot detect. The engine has no `AsOf` mechanism at all,
/// so it must refuse rather than lie.
#[tokio::test]
async fn a_point_in_time_request_fails_rather_than_answering_with_current_data() {
    let serve = fixture::engine_serve(PKG, VERSION).await;

    let request = heart::query::Query {
        at: Some(heart::query::AsOf::Time(1_700_000_000_000)),
        ..query("Router")
    };
    let (rows, terminal) = drain(serve.serve(request, SurfaceGen(1))).await;

    assert!(
        matches!(terminal, Some(Frame::Failed(_))),
        "an unsupported point-in-time query must fail loudly, got {terminal:?}"
    );
    assert!(
        rows.is_empty(),
        "a failed answer must not also deliver rows that look authoritative"
    );
}

/// Same argument for pagination. `collect_name_hits` has a top-`N` limit and no
/// offset concept, so honouring a cursor by ignoring it returns page 1 labelled
/// as page 2 — silently, forever.
#[tokio::test]
async fn a_cursor_request_fails_rather_than_silently_returning_page_one() {
    let serve = fixture::engine_serve(PKG, VERSION).await;

    let request = heart::query::Query {
        page: heart::query::PageSpecification {
            limit: 10,
            cursor: Some("opaque-resume-token".to_owned()),
        },
        ..query("Router")
    };
    let (_, terminal) = drain(serve.serve(request, SurfaceGen(1))).await;

    assert!(
        matches!(terminal, Some(Frame::Failed(_))),
        "an unsupported cursor must fail loudly, got {terminal:?}"
    );
}

/// A request for the wrong surface must be rejected. `Symbols::Request` is the
/// shared `heart::query::Query`, so `target` is still a runtime-checkable
/// mismatch — the one the `Surface` split could not remove because the request
/// type is deliberately shared across all three search surfaces.
#[tokio::test]
async fn a_non_symbols_target_is_rejected() {
    let serve = fixture::engine_serve(PKG, VERSION).await;

    let request = heart::query::Query {
        target: heart::query::Target::Packages,
        ..query("Router")
    };
    let (_, terminal) = drain(serve.serve(request, SurfaceGen(1))).await;

    assert!(matches!(terminal, Some(Frame::Failed(_))));
}

/// `limit` must be honoured — it is the one page field the engine *does*
/// support, and dropping it would flood a GUI that asked for ten rows.
#[tokio::test]
async fn the_page_limit_is_honoured() {
    let serve = fixture::engine_serve_with_many(PKG, VERSION, 50).await;

    let request = heart::query::Query {
        page: heart::query::PageSpecification {
            limit: 5,
            cursor: None,
        },
        ..query("Router")
    };
    let (rows, _) = drain(serve.serve(request, SurfaceGen(1))).await;

    assert!(
        rows.len() <= 5,
        "asked for 5 rows, got {} — the limit was dropped in conversion",
        rows.len()
    );
}

// ---------------------------------------------------------------------------
// 5. Cancellation
// ---------------------------------------------------------------------------

/// Search-as-you-type drops the previous `Answer` on every keystroke. That must
/// stop the underlying engine search, not merely close the channel — otherwise
/// a fast typist accumulates one abandoned in-flight query per character.
#[tokio::test]
async fn dropping_the_answer_cancels_the_underlying_search() {
    let serve = fixture::engine_serve_with_many(PKG, VERSION, 500).await;

    let answer = serve.serve(query("Router"), SurfaceGen(1));
    drop(answer);

    // The engine's own `StreamHandle` must have fired. `fixture::in_flight`
    // reports whether the engine still considers a search live.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !fixture::has_in_flight_search(),
        "dropping the Answer left the engine search running"
    );
}
