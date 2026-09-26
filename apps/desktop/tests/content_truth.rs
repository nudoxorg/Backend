//! Content truth, end to end: a real `backend-locald` indexes real Rust
//! crates from this repository, and every page is read through the desktop's
//! own runtime path — root actor → read pool → reply mapping → keyed store —
//! exactly as a board will read it. Assertions are on rendered content
//! (signature text, a doc sentence, member names, a relation, a byte span,
//! a version string), never on counts alone.
//!
//! Two crates, because the builtin owner answers them differently:
//! `crates/present` is a workspace member, so the owner publishes its
//! structural projection (docs, signatures, members, source) but refuses a
//! compiler publication; `frontends/rust/fixtures/rich_project` is a
//! standalone crate, so the owner compiles it and serves typed relations and
//! source-verified references.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines, missing_docs)]

use backend_client::{LocalSubscriptionTransport, Session};
use backend_desktop::core::{LocalProjectId, VersionedRoot};
use backend_desktop::model::AppSnapshot;
use backend_desktop::model::pages::{
    Derivation, DocFragment, GapReason, MatchReason, OutlineTree, PackageDossier, PackageRef,
    PageKey, Provenance, Receiver, RecordSource, RelationKind, SearchQuery, SourceOrigin,
    SymbolPage, SymbolRef,
};
use backend_desktop::runtime::reads::{ReadPool, SessionReader};
use backend_desktop::runtime::store::DataStore;
use backend_desktop::runtime::{DesktopRuntime, EngineActor, LocalEngineClient, UiEntityGraph};
use backend_library::{CommandReply, DeclarationKind, RowState, SemanticLinkKind};
use gpui::{Entity, TestAppContext};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const INDEX_DEADLINE: Duration = Duration::from_mins(15);
const READ_DEADLINE: Duration = Duration::from_mins(2);

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("fixture paths are UTF-8")
}

/// Starts an embedded owner on a private workspace and indexes `projects`,
/// returning once every project row is ready and the row count is stable.
fn serve(projects: &[&Path]) -> (backend_desktop::DesktopHost, PathBuf, PathBuf) {
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            % 100_000
    );
    // `/tmp`, not `temp_dir()`: a long socket path exceeds `sockaddr_un`.
    let state = PathBuf::from("/tmp").join(format!("nx-ct-{nonce}"));
    let endpoint = PathBuf::from("/tmp").join(format!("nx-ct-{nonce}.sock"));
    std::fs::create_dir_all(state.join("data")).expect("workspace");
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(projects[0].to_path_buf()),
        Some(state.join("data")),
        Some(endpoint.clone()),
    )
    .expect("workspace paths");
    let host = backend_desktop::DesktopHost::start_with_paths(paths).expect("embedded owner");
    let mut session = Session::connect(&endpoint).expect("session");
    for project in projects {
        session.index(utf8(project)).expect("index request");
    }
    let started = Instant::now();
    let mut last_rows = 0;
    let mut stable = 0;
    loop {
        let ready = match session.packages().map(|reply| reply.reply) {
            Ok(CommandReply::Packages(snapshot)) => projects.iter().all(|project| {
                snapshot
                    .root
                    .rows()
                    .iter()
                    .any(|row| row.label == utf8(project) && row.state == RowState::Ready)
            }),
            _ => false,
        };
        let rows = session.health().map_or(0, |health| health.row_count());
        stable = if ready && rows > 0 && rows == last_rows { stable + 1 } else { 0 };
        last_rows = rows;
        if stable >= 3 {
            break;
        }
        assert!(started.elapsed() < INDEX_DEADLINE, "indexing never settled");
        std::thread::sleep(Duration::from_millis(300));
    }
    (host, endpoint, state)
}

struct Plane {
    store: Entity<DataStore>,
}

impl Plane {
    fn ensure(&self, cx: &mut TestAppContext, key: PageKey) {
        self.store.update(cx, |store, cx| {
            store.ensure(key, cx);
        });
    }

    /// Runs the UI executor until `done` holds; worker threads land results
    /// through the store's wake task in between.
    fn until<T>(&self, cx: &mut TestAppContext, what: &str, read: impl Fn(&DataStore) -> Option<T>) -> T {
        let started = Instant::now();
        loop {
            cx.run_until_parked();
            if let Some(value) = self.store.read_with(cx, |store, _| read(store)) {
                return value;
            }
            assert!(started.elapsed() < READ_DEADLINE, "{what} never loaded");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn search(&self, cx: &mut TestAppContext, text: &str) -> backend_desktop::model::pages::SearchPage {
        let query = SearchQuery::new(text, 200).expect("query");
        self.ensure(cx, PageKey::Search(query.clone()));
        self.until(cx, text, |store| {
            let resource = store.search(&query);
            fault(&resource, text);
            resource.loaded_value().cloned()
        })
    }

    /// Searches `name`, picks the row of `kind` whose coordinate contains
    /// `within`, and returns its exact coordinate.
    fn find(&self, cx: &mut TestAppContext, name: &str, kind: DeclarationKind, within: &str) -> SymbolRef {
        let page = self.search(cx, name);
        page.rows
            .iter()
            .find(|row| {
                row.decl.name.as_ref() == name
                    && row.decl.kind == Some(kind)
                    && row.decl.coordinate.as_str().contains(within)
            })
            .map_or_else(
                || {
                    panic!(
                        "search {name:?} found no {kind:?} in {within}: {:?}",
                        page.rows
                            .iter()
                            .map(|row| (row.decl.name.to_string(), row.decl.kind))
                            .collect::<Vec<_>>()
                    )
                },
                |row| row.decl.coordinate.clone(),
            )
    }

    fn dossier(&self, cx: &mut TestAppContext, root: &Path) -> PackageDossier {
        let package = PackageRef::parse(utf8(root)).expect("package ref");
        self.ensure(cx, PageKey::Package(package.clone()));
        self.until(cx, "dossier", |store| {
            let resource = store.package(&package);
            fault(&resource, "dossier");
            resource.loaded_value().cloned()
        })
    }

    fn symbol(&self, cx: &mut TestAppContext, symbol: &SymbolRef) -> SymbolPage {
        self.ensure(cx, PageKey::Symbol(symbol.clone()));
        self.until(cx, symbol.as_str(), |store| {
            let resource = store.symbol(symbol);
            fault(&resource, symbol.as_str());
            resource.loaded_value().cloned()
        })
    }
}

/// Finds one declaration in a dossier's outline tree (the mosaic).
fn in_tree(tree: &OutlineTree, name: &str, kind: DeclarationKind, path: Option<&str>) -> SymbolRef {
    tree.walk()
        .find(|node| {
            node.decl.name.as_ref() == name
                && node.decl.kind == Some(kind)
                && path.is_none_or(|path| node.decl.path.as_deref() == Some(path))
        })
        .map_or_else(
            || panic!("{name} ({kind:?}) is not in the outline"),
            |node| node.decl.coordinate.clone(),
        )
}

fn fault<T>(resource: &backend_desktop::core::Resource<T>, what: &str) {
    if let backend_desktop::core::ResourceTerminal::Fault(error) = resource.terminal() {
        panic!("{what} failed: {:?} {}", error.code(), error.message());
    }
}

fn workspace_version(repo: &Path) -> String {
    let manifest = std::fs::read_to_string(repo.join("Cargo.toml")).expect("root manifest");
    let table: toml::Table = manifest.parse().expect("root manifest TOML");
    table["workspace"]["package"]["version"]
        .as_str()
        .expect("workspace version")
        .to_owned()
}

fn names<'a>(members: impl Iterator<Item = &'a backend_desktop::model::pages::Member>) -> Vec<String> {
    members.map(|member| member.decl.name.to_string()).collect()
}

#[gpui::test]
fn every_board_reads_real_content_through_the_desktop_runtime(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let repo = repo();
    let present = repo.join("crates/present");
    let rich = repo.join("frontends/rust/fixtures/rich_project");
    let (host, endpoint, state) = serve(&[&present, &rich]);

    // The same startup as the native window: bootstrap the root, start the
    // engine actor on the index/admin lane and the read pool on its own lane.
    let mut subscription = LocalSubscriptionTransport::connect(&endpoint).expect("subscription");
    let (view, revision) = subscription.bootstrap_root().expect("root");
    assert_eq!(view.root(), revision.root());
    let snapshot = AppSnapshot::empty(VersionedRoot::from_revision(1, revision, 0));
    let project = LocalProjectId::from_path(&present).expect("project identity");
    let actor = EngineActor::start(LocalEngineClient::new(&endpoint, project), 32).expect("actor");
    let runtime = DesktopRuntime::new(snapshot, actor);
    let reader_endpoint = endpoint.clone();
    let pool = ReadPool::start(3, move |_| SessionReader::connect(&reader_endpoint)).expect("read pool");
    let graph = cx.update(|cx| UiEntityGraph::install_with_reads(cx, runtime, None, Some(pool)));
    let plane = Plane {
        store: graph.store.clone(),
    };

    // Orbit and Health load because the root published its Home route.
    let orbit = plane.until(cx, "orbit", |store| store.orbit().loaded_value().cloned());
    let indexed = orbit.indexed.known().expect("indexed packages");
    for (name, root) in [("present", &present), ("rich_project", &rich)] {
        let row = indexed
            .iter()
            .find(|package| package.package.as_str() == utf8(root))
            .unwrap_or_else(|| panic!("{name} is not on the shelf"));
        assert_eq!(row.name.as_ref(), name);
        assert_eq!(row.readiness, backend_desktop::model::pages::Readiness::Ready);
    }
    let health = plane.until(cx, "health", |store| store.health().loaded_value().cloned());
    assert!(health.rows > 1_000, "the owner committed {} rows", health.rows);
    assert!(health.ingest.files_indexed >= 20, "{:?}", health.ingest);

    // ── Search: ranked rows with a reason each.
    let search = plane.search(cx, "Engine");
    let engine_row = search
        .rows
        .iter()
        .find(|row| row.decl.name.as_ref() == "Engine" && row.decl.kind == Some(DeclarationKind::Trait))
        .expect("the Engine trait is a search result");
    assert_eq!(engine_row.reason, MatchReason::ExactName);
    assert_eq!(
        engine_row.snippet.as_deref(),
        Some("Whatever a surface talks to in order to read an admitted reply.")
    );
    assert_eq!(engine_row.score.gap().map(|gap| gap.reason), Some(GapReason::NotServed));

    // ── A trait page (structural projection).
    let engine = engine_row.decl.coordinate.clone();
    let trait_page = plane.symbol(cx, &engine);
    assert_eq!(trait_page.identity.kind, Some(DeclarationKind::Trait));
    assert_eq!(trait_page.identity.path.as_deref(), Some("drive.rs"));
    assert_eq!(
        trait_page.signature.known().map(|signature| signature.text.to_string()),
        Some("pub trait Engine".to_owned())
    );
    let docs = DocFragment::plain_text(&trait_page.docs);
    assert!(
        docs.starts_with("Whatever a surface talks to in order to read an admitted reply."),
        "{docs}"
    );
    assert!(docs.contains("Nothing\nin this crate knows which it is holding."), "{docs}");
    let members = trait_page.members.known().expect("trait members");
    let methods = names(members.all());
    for method in ["revision", "health", "probe", "probe_page", "surface"] {
        assert!(methods.contains(&method.to_owned()), "{method} missing from {methods:?}");
    }
    let changes = members
        .does
        .iter()
        .find(|group| group.receiver == Receiver::Changes)
        .expect("&mut self methods");
    assert!(names(changes.members.iter()).contains(&"probe".to_owned()));
    let outline = trait_page.outline.known().expect("outline position");
    assert_eq!(
        outline.ancestors.iter().map(|decl| decl.name.to_string()).collect::<Vec<_>>(),
        ["drive.rs"]
    );
    // The workspace-member crate has no compiler publication: typed
    // relations and references are unknown, and say why.
    assert_eq!(
        trait_page.rose.up.gap().map(|gap| gap.reason),
        Some(GapReason::NoSemanticPublication)
    );
    let references = trait_page.references.gap().expect("references gap");
    assert_eq!(references.reason, GapReason::NoSemanticPublication);
    assert!(references.detail.contains("complete semantic publication"), "{}", references.detail);
    let down = trait_page.rose.down.known().expect("containment");
    assert!(down.iter().any(|relation| relation.decl.name.as_ref() == "probe"));

    // ── The package dossier (a local project: manifest facts, no registry claims).
    let dossier = plane.dossier(cx, &present);
    let record = dossier.record.known().expect("manifest record");
    assert_eq!(record.source, RecordSource::LocalManifest);
    assert_eq!(record.name.as_ref(), "backend-present");
    assert_eq!(
        record.version.known().map(ToString::to_string),
        Some(workspace_version(&repo)),
        "the dossier's version string is the crate's resolved version"
    );
    assert_eq!(record.downloads.gap().map(|gap| gap.reason), Some(GapReason::LocalProject));
    assert_eq!(dossier.versions.gap().map(|gap| gap.reason), Some(GapReason::LocalProject));
    let dependencies = dossier.dependencies.known().expect("manifest dependencies");
    assert!(dependencies.iter().any(|dependency| dependency.name.as_ref() == "backend-library"));
    let tree = dossier.outline.known().expect("mosaic tree");
    assert!(tree.complete, "every outline page was read");
    let drive = tree
        .roots
        .iter()
        .find(|node| node.decl.name.as_ref() == "drive.rs")
        .expect("drive.rs module");
    assert!(drive
        .children
        .iter()
        .any(|node| node.decl.name.as_ref() == "Engine" && node.decl.kind == Some(DeclarationKind::Trait)));
    assert!(tree.count() > 1_000, "{} declarations", tree.count());

    // ── A struct page, reached through the mosaic: members ledger,
    // receivers, a by-name signature link.
    let page_struct = in_tree(tree, "Page", DeclarationKind::Struct, Some("page.rs"));
    let struct_page = plane.symbol(cx, &page_struct);
    assert_eq!(
        struct_page.signature.known().map(|signature| signature.text.to_string()),
        Some("pub struct Page".to_owned())
    );
    assert_eq!(DocFragment::plain_text(&struct_page.docs), "One complete declaration page.");
    let members = struct_page.members.known().expect("struct members");
    let fields = names(members.made_of.iter());
    for field in ["identity", "prose", "members", "relations", "source", "notes"] {
        assert!(fields.contains(&field.to_owned()), "{field} missing from {fields:?}");
    }
    let receiver_of = |name: &str| {
        members
            .does
            .iter()
            .find(|group| group.members.iter().any(|member| member.decl.name.as_ref() == name))
            .map(|group| group.receiver)
    };
    assert_eq!(receiver_of("with_prose"), Some(Receiver::Consumes));
    assert_eq!(receiver_of("prose"), Some(Receiver::Reads));
    assert_eq!(receiver_of("new"), Some(Receiver::Makes));
    let constructor = members
        .all()
        .find(|member| member.decl.name.as_ref() == "new")
        .expect("Page::new");
    let signature = constructor.signature.known().expect("constructor signature");
    let identity_link = signature
        .links()
        .find(|token| signature.token_text(token) == "Identity")
        .and_then(|token| token.link.as_ref())
        .expect("Identity in Page::new links by name");
    assert!(identity_link.target.as_str().starts_with(&format!("{}::identity.rs:", utf8(&present))));
    assert_eq!(identity_link.provenance, Provenance::ByName);

    // ── The source view: the whole local file, verified against the excerpt.
    plane.ensure(cx, PageKey::Source(engine.clone()));
    let source = plane.until(cx, "source", |store| {
        let resource = store.source(&engine);
        fault(&resource, "source");
        resource.loaded_value().cloned()
    });
    let text = source.text.known().expect("source text");
    assert_eq!(text.origin, SourceOrigin::LocalFile);
    let declaration = source.declaration.known().expect("declaration lines");
    let first = text
        .line_span(declaration.first)
        .map(|span| &text.text[span.range()])
        .expect("declaration line");
    assert_eq!(first, "pub trait Engine {");
    let on_disk = std::fs::read_to_string(present.join("drive.rs")).expect("drive.rs");
    assert_eq!(text.text.as_ref(), on_disk.as_str());
    let identifiers = source.identifiers.known().expect("identifiers");
    assert!(
        identifiers.iter().any(|span| {
            &text.text[span.span.range()] == "Probe"
                && span.link.target.as_str().contains("drive.rs:")
                && span.link.target.as_str().ends_with("::Probe")
        }),
        "an in-crate identifier links to its declaration"
    );

    // ── Semantic crate: typed relations, derived impl, exact reference spans.
    let marker = plane.find(cx, "Marker", DeclarationKind::Trait, "rich_project");
    let marker_page = plane.symbol(cx, &marker);
    assert!(marker_page.identity.semantic, "compiler-addressed coordinate");
    assert_eq!(
        marker_page.signature.known().map(|signature| signature.text.to_string()),
        Some("pub trait Marker".to_owned())
    );
    let implementors = marker_page.rose.implemented_by.known().expect("implementors");
    let implementor = implementors
        .iter()
        .find(|relation| relation.decl.name.as_ref() == "Boxed")
        .expect("Boxed implements Marker");
    assert_eq!(implementor.kind, RelationKind::Semantic(SemanticLinkKind::Implements));
    assert!(matches!(
        implementor.provenance,
        Provenance::Derived {
            via: Derivation::ImplBlock,
            confidence: backend_library::SemanticConfidence::Compiler
        }
    ));
    let sites = marker_page.references.known().expect("references");
    let site = sites
        .iter()
        .find(|site| site.site.name.as_ref() == "Boxed")
        .expect("the impl block uses Marker");
    assert_eq!(site.relation, SemanticLinkKind::TypeReference);
    assert_eq!(site.confidence, backend_library::SemanticConfidence::Compiler);
    let span = site.span.known().expect("reference span");
    assert_eq!(span.file.as_ref(), "src/lib.rs");
    let bytes = std::fs::read(rich.join(span.file.as_ref())).expect("fixture source");
    assert_eq!(&bytes[span.bytes.range()], b"Marker", "the span covers the use exactly");

    let boxed = plane.find(cx, "Boxed", DeclarationKind::Struct, "rich_project");
    let boxed_page = plane.symbol(cx, &boxed);
    let up = boxed_page.rose.up.known().expect("typed up");
    let is_marker = up
        .iter()
        .find(|relation| relation.decl.name.as_ref() == "Marker")
        .expect("Boxed is Marker");
    assert_eq!(is_marker.kind, RelationKind::Semantic(SemanticLinkKind::Implements));
    assert!(is_marker.via.is_some(), "derived through the impl block");
    let fields = boxed_page.members.known().map(|members| names(members.made_of.iter()));
    assert_eq!(fields, Some(vec!["value".to_owned()]));
    assert_eq!(
        boxed_page.signature.gap().map(|gap| gap.reason),
        Some(GapReason::Encoded),
        "the struct's signature is served as an encoded type"
    );
    let rich_dossier = plane.dossier(cx, &rich);
    let rich_tree = rich_dossier.outline.known().expect("rich outline");
    let get = in_tree(rich_tree, "get", DeclarationKind::Function, None);
    let get_page = plane.symbol(cx, &get);
    let callers = get_page.rose.left.known().expect("callers");
    assert!(
        callers.iter().any(|relation| relation.decl.name.as_ref() == "compute"
            && relation.kind == RelationKind::Semantic(SemanticLinkKind::MethodCall)),
        "compute() calls get()"
    );
    let reads = get_page.rose.right.known().expect("outgoing");
    assert!(reads.iter().any(|relation| relation.decl.name.as_ref() == "value"
        && relation.kind == RelationKind::Semantic(SemanticLinkKind::Reads)));

    // Leave the lead a plain-text view of every model the store holds.
    let text = plane
        .store
        .read_with(cx, |store, _| backend_desktop::runtime::debug_page::render_text(store));
    let out = std::env::var("NUDOX_DEBUG_PAGE_OUT").map_or_else(
        |_| std::env::temp_dir().join("nudox-debug-page.txt"),
        PathBuf::from,
    );
    std::fs::write(&out, text).expect("debug page dump");

    drop(graph);
    drop(subscription);
    drop(host);
    let _ = std::fs::remove_dir_all(state);
    let _ = std::fs::remove_file(endpoint);
}
