//! Content truth, end to end: a real `backend-locald` indexes real Rust
//! crates from this repository, and every page is read through the desktop's
//! own runtime path — root actor → read pool → reply mapping → keyed store —
//! exactly as a board will read it. Assertions are on rendered content
//! (signature text, a doc sentence, member names, a relation, a byte span,
//! a version string), never on counts alone.
//!
//! Three projects, because the owner answers them in different lanes:
//!
//! * `crates/present` is compiled: the owner publishes a compiler image for
//!   it, so its pages are compiler-addressed and its source view carries the
//!   compiler's own declaration code. (Indexing it compiles the whole
//!   workspace's Cargo graph, so every package that graph names, for every
//!   platform, must be in the Cargo home the test runs against.)
//! * `present_text` is three of the same crate's modules copied beside no
//!   manifest. No compiler authority can open it, so the owner refuses its
//!   compile profile, settles the project as failed, and answers from the
//!   structural lane: signatures, docs, members and same-file or
//!   import-resolved calls tagged `SemanticConfidence::Syntactic`, with typed
//!   relations unknown and a source page that says no declaration code was
//!   captured. It reads the same real content, in the lane that is not
//!   environment dependent.
//! * `frontends/rust/fixtures/rich_project` is a standalone crate the owner
//!   compiles and serves with typed relations and source-verified
//!   (`SemanticConfidence::Compiler`) references.

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

/// The modules of `crates/present` the structural lane is read through. The
/// whole crate (about 38,000 lines) is more than the desktop's outline bound
/// of 8,000 declarations, past which an outline, and every members ledger or
/// name link built from it, is honestly partial; these three files are the
/// ones the assertions below read, and stay far inside it.
const TEXT_MODULES: &[&str] = &["drive.rs", "identity.rs", "page.rs"];

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

/// Indexes one project and checks the owner reported exactly what the test
/// relies on: a `structural_only` project (no manifest) is refused by its only
/// compile profile and nothing else; every other project indexes cleanly.
fn index(session: &mut Session, project: &Path, structural_only: bool) {
    use backend_client::ClientError;
    use backend_library::CommandFailure;
    match session.index(utf8(project)) {
        Ok(_) => assert!(!structural_only, "{} has no manifest, so its compile profile must be refused", project.display()),
        Err(ClientError::CommandFailed(CommandFailure::CompilerRefused { failure, .. })) if structural_only => {
            assert!(failure.relative_path().ends_with(".rs"), "{failure:?}");
        }
        Err(error) => panic!("index request for {}: {error:?}", project.display()),
    }
}

/// Copies the named Rust sources of `from` to `to`, leaving the manifest behind.
fn copy_rust_sources(from: &Path, to: &Path, names: &[&str]) {
    std::fs::create_dir_all(to).expect("copy directory");
    for name in names {
        std::fs::copy(from.join(name), to.join(name)).expect("copy source");
    }
}

/// Starts an embedded owner on a private workspace and indexes `projects`,
/// returning once every project row has settled and the row count is stable.
/// The projects in `structural_only` are the ones without a manifest.
fn serve(projects: &[&Path], structural_only: &[&Path]) -> (backend_desktop::DesktopHost, PathBuf, PathBuf) {
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
        index(&mut session, project, structural_only.contains(project));
    }
    let started = Instant::now();
    let mut last_rows = 0;
    let mut stable = 0;
    let mut states = Vec::new();
    loop {
        if let Ok(CommandReply::Packages(snapshot)) = session.packages().map(|reply| reply.reply) {
            states = snapshot.root.rows().iter().map(|row| format!("{} {:?}", row.label, row.state)).collect();
        }
        let ready = match session.packages().map(|reply| reply.reply) {
            Ok(CommandReply::Packages(snapshot)) => projects.iter().all(|project| {
                // A project without a manifest settles as failed: the compile
                // refusal is recorded and the structural lane answers.
                let settled = if structural_only.contains(project) { RowState::Failed } else { RowState::Ready };
                snapshot.root.rows().iter().any(|row| row.label == utf8(project) && row.state == settled)
            }),
            _ => false,
        };
        let health = session.health();
        let rows = health.as_ref().map_or(0, |health| health.row_count());
        if let Err(error) = &health {
            states.push(format!("health: {error:?}"));
        }
        stable = if ready && rows > 0 && rows == last_rows { stable + 1 } else { 0 };
        last_rows = rows;
        if stable >= 3 {
            break;
        }
        assert!(started.elapsed() < INDEX_DEADLINE, "indexing never settled: {states:?}, {last_rows} rows");
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
            // Published in stages like a symbol page: wait for the outline.
            resource.loaded_value().filter(|dossier| {
                dossier.outline.gap().is_none_or(|gap| gap.reason != backend_desktop::model::pages::GapReason::Stale)
            }).cloned()
        })
    }

    fn symbol(&self, cx: &mut TestAppContext, symbol: &SymbolRef) -> SymbolPage {
        self.ensure(cx, PageKey::Symbol(symbol.clone()));
        self.until(cx, symbol.as_str(), |store| {
            let resource = store.symbol(symbol);
            fault(&resource, symbol.as_str());
            // A symbol page is published in stages (the document first, then
            // its neighbourhood, references and outline, each reading as a
            // `Stale` gap until it lands); only the finished page is the one a
            // reader is left with.
            resource.loaded_value().filter(|page| {
                [page.members.gap(), page.references.gap(), page.rose.up.gap()]
                    .into_iter()
                    .flatten()
                    .all(|gap| gap.reason != backend_desktop::model::pages::GapReason::Stale)
            }).cloned()
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

/// The `///` doc comment written above the line starting with `head`, read
/// from the source itself (attributes between them are skipped).
fn doc_comment_above(file: &Path, head: &str) -> String {
    let source = std::fs::read_to_string(file).expect("source file");
    let lines = source.lines().collect::<Vec<_>>();
    let at = lines.iter().position(|line| line.starts_with(head)).unwrap_or_else(|| panic!("{head} in {}", file.display()));
    let mut doc = Vec::new();
    for line in lines[..at].iter().rev() {
        let line = line.trim_start();
        if line.starts_with("#[") {
            continue;
        }
        let Some(text) = line.strip_prefix("///") else { break };
        doc.push(text.strip_prefix(' ').unwrap_or(text));
    }
    doc.reverse();
    doc.join("\n")
}

fn names<'a>(members: impl Iterator<Item = &'a backend_desktop::model::pages::Member>) -> Vec<String> {
    members.map(|member| member.decl.name.to_string()).collect()
}

/// A running owner with the desktop's runtime path over it: the same startup
/// as the native window (bootstrap the root, start the engine actor on the
/// index/admin lane and the read pool on its own lane).
struct World {
    host: backend_desktop::DesktopHost,
    endpoint: PathBuf,
    state: PathBuf,
    subscription: LocalSubscriptionTransport,
    graph: backend_desktop::runtime::UiEntityGraph,
    plane: Plane,
}

fn boot(cx: &mut TestAppContext, projects: &[&Path], structural_only: &[&Path]) -> World {
    let (host, endpoint, state) = serve(projects, structural_only);
    let mut subscription = LocalSubscriptionTransport::connect(&endpoint).expect("subscription");
    let (view, revision) = subscription.bootstrap_root().expect("root");
    assert_eq!(view.root(), revision.root());
    let snapshot = AppSnapshot::empty(VersionedRoot::from_revision(1, revision, 0));
    let project = LocalProjectId::from_path(projects[0]).expect("project identity");
    let actor = EngineActor::start(LocalEngineClient::new(&endpoint, project), 32).expect("actor");
    let runtime = DesktopRuntime::new(snapshot, actor);
    let reader_endpoint = endpoint.clone();
    let pool = ReadPool::start(3, move |_| SessionReader::connect(&reader_endpoint)).expect("read pool");
    let graph = cx.update(|cx| UiEntityGraph::install_with_reads(cx, runtime, None, Some(pool)));
    let plane = Plane {
        store: graph.store.clone(),
    };
    World { host, endpoint, state, subscription, graph, plane }
}

impl World {
    /// Leaves a plain-text view of every model the store holds, and cleans up.
    fn finish(self, cx: &mut TestAppContext) {
        let text = self
            .plane
            .store
            .read_with(cx, |store, _| backend_desktop::runtime::debug_page::render_text(store));
        let out = std::env::var("NUDOX_DEBUG_PAGE_OUT").map_or_else(
            |_| std::env::temp_dir().join("nudox-debug-page.txt"),
            PathBuf::from,
        );
        std::fs::write(&out, text).expect("debug page dump");
        let Self { host, endpoint, state, subscription, graph, plane: _ } = self;
        drop(graph);
        drop(subscription);
        drop(host);
        let _ = std::fs::remove_dir_all(state);
        let _ = std::fs::remove_file(endpoint);
    }
}

#[gpui::test]
fn every_board_reads_real_content_through_the_desktop_runtime(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let repo = repo();
    let present = repo.join("crates/present");
    let rich = repo.join("frontends/rust/fixtures/rich_project");
    // The same crate's sources with no manifest beside them: the structural lane.
    let text = PathBuf::from("/tmp").join(format!("nx-ct-text-{}", std::process::id())).join("present_text");
    copy_rust_sources(&present, &text, TEXT_MODULES);
    // The owner names a project by its canonical path (`/private/tmp` on macOS).
    let text = text.canonicalize().expect("canonical structural copy");
    let world = boot(cx, &[&text, &rich], &[&text]);
    let plane = &world.plane;

    // Orbit and Health load because the root published its Home route.
    let orbit = plane.until(cx, "orbit", |store| store.orbit().loaded_value().cloned());
    let indexed = orbit.indexed.known().expect("indexed packages");
    for (name, root) in [("present_text", &text), ("rich_project", &rich)] {
        let row = indexed
            .iter()
            .find(|package| package.package.as_str() == utf8(root))
            .unwrap_or_else(|| panic!("{name} is not on the shelf"));
        assert_eq!(row.name.as_ref(), name);
        // The compiled project is ready; the manifest-less copy is not, and
        // does not pass for ready.
        let ready = row.readiness == backend_desktop::model::pages::Readiness::Ready;
        assert_eq!(ready, root != &text, "{name}: {:?}", row.readiness);
    }
    let health = plane.until(cx, "health", |store| store.health().loaded_value().cloned());
    assert!(health.rows > 300, "the owner committed {} rows", health.rows);
    assert!(health.ingest.files_indexed >= 4, "{:?}", health.ingest);

    // ── Search: ranked rows with a reason each.
    let search = plane.search(cx, "Engine");
    let engine_row = search
        .rows
        .iter()
        .find(|row| {
            row.decl.name.as_ref() == "Engine"
                && row.decl.kind == Some(DeclarationKind::Trait)
                && row.decl.coordinate.as_str().contains(utf8(&text))
        })
        .expect("the Engine trait is a search result in the structural copy");
    assert_eq!(engine_row.reason, MatchReason::ExactName);
    assert_eq!(
        engine_row.snippet.as_deref(),
        Some("Whatever a surface talks to in order to read an admitted reply.")
    );
    assert!(
        engine_row.score.known().is_some(),
        "a ranked row carries its score: {:?}",
        engine_row.score.gap()
    );

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
    let members = trait_page
        .members
        .known()
        .unwrap_or_else(|| panic!("trait members: {:?} (identity semantic: {})", trait_page.members.gap(), trait_page.identity.semantic));
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
    // relations are unknown, and say why. References still answer, from the
    // structural lane, because same-file and import-resolved call text
    // needs no compiler image to read.
    assert_eq!(
        trait_page.rose.up.gap().map(|gap| gap.reason),
        Some(GapReason::NoSemanticPublication)
    );
    // References are not served from the structural lane: with no compiler
    // publication the owner says, in words, that cross-file reference joins
    // are not certified exhaustive and why, instead of listing call text it
    // cannot certify. (The compiled lane's exact, `Compiler`-tagged spans are
    // read below and for `crates/present` itself in the next test.)
    let intent_symbol = plane.find(cx, "accepted_view", DeclarationKind::Function, utf8(&text));
    let intent_page = plane.symbol(cx, &intent_symbol);
    for (name, page) in [("Engine", &trait_page), ("accepted_view", &intent_page)] {
        let gap = page.references.gap().unwrap_or_else(|| panic!("{name}: references are a gap: {:?}", page.references.known()));
        assert_eq!(gap.reason, GapReason::Unknown, "{name}: {gap:?}");
        assert!(gap.detail.contains("not certified exhaustive"), "{name}: {gap:?}");
        assert!(gap.detail.contains("has no selected semantic publication"), "{name}: {gap:?}");
    }
    let down = trait_page.rose.down.known().expect("containment");
    assert!(down.iter().any(|relation| relation.decl.name.as_ref() == "probe"));

    // ── The package dossier of the structural copy (a local project with no manifest).
    // The mosaic of the structural copy: every module of the crate's files.
    let text_dossier = plane.dossier(cx, &text);
    let tree = text_dossier
        .outline
        .known()
        .unwrap_or_else(|| panic!("mosaic tree: {:?}", text_dossier.outline.gap()));
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
    assert!(tree.count() > 100, "{} declarations", tree.count());

    // ── A struct page, reached through the mosaic: members ledger,
    // receivers, a by-name signature link.
    let page_struct = in_tree(tree, "Page", DeclarationKind::Struct, Some("page.rs"));
    let struct_page = plane.symbol(cx, &page_struct);
    assert_eq!(
        struct_page.signature.known().map(|signature| signature.text.to_string()),
        Some("pub struct Page".to_owned())
    );
    assert_eq!(
        DocFragment::plain_text(&struct_page.docs),
        doc_comment_above(&present.join("page.rs"), "pub struct Page {"),
        "the page's prose is the declaration's own doc comment"
    );
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
    assert!(identity_link.target.as_str().starts_with(&format!("{}::identity.rs:", utf8(&text))));
    assert_eq!(identity_link.provenance, Provenance::ByName);

    // ── The structural lane's source page: no compiler owns the declaration, so
    // the page says no declaration code was captured, and never presents the
    // file's bytes as if they were the indexed declaration.
    plane.ensure(cx, PageKey::Source(engine.clone()));
    let structural_source = plane.until(cx, "structural source", |store| {
        let resource = store.source(&engine);
        fault(&resource, "structural source");
        resource.loaded_value().cloned()
    });
    assert_eq!(structural_source.text.gap().map(|gap| gap.reason), Some(GapReason::NotCaptured));

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
    // The edge is a compiler fact whether the graph states it directly or the
    // desktop derives it from the impl block's type references.
    assert!(
        matches!(
            implementor.provenance,
            Provenance::Semantic(backend_library::SemanticConfidence::Compiler)
                | Provenance::Derived {
                    via: Derivation::ImplBlock,
                    confidence: Some(backend_library::SemanticConfidence::Compiler)
                }
        ),
        "{:?}",
        implementor.provenance
    );
    let sites = marker_page.references.known().expect("references");
    let site = sites
        .iter()
        .find(|site| site.site.name.as_ref() == "Boxed")
        .expect("the impl block uses Marker");
    // The compiler states the impl block's use of `Marker` as the implementation it is.
    assert_eq!(site.relation, SemanticLinkKind::Implements);
    assert_eq!(site.confidence, backend_library::SemanticConfidence::Compiler);
    let span = site.span.known().expect("reference span");
    assert_eq!(span.file.as_ref(), "src/lib.rs");
    let bytes = std::fs::read(rich.join(span.file.as_ref())).expect("fixture source");
    assert_eq!(&bytes[span.bytes.range()], b"Marker", "the span covers the use exactly");

    // The compiled crate's source page: the whole file, the declaration's own
    // line, and an in-crate name linking to its declaration.
    plane.ensure(cx, PageKey::Source(marker.clone()));
    let marker_source = plane.until(cx, "marker source", |store| {
        let resource = store.source(&marker);
        fault(&resource, "marker source");
        resource.loaded_value().cloned()
    });
    let marker_text = marker_source.text.known().unwrap_or_else(|| panic!("marker source: {:?}", marker_source.text.gap()));
    assert_eq!(marker_text.origin, SourceOrigin::LocalFile);
    let rich_file = std::fs::read_to_string(rich.join("src/lib.rs")).expect("rich lib.rs");
    assert_eq!(marker_text.text(), rich_file.as_str());
    let marker_lines = marker_source.declaration.known().expect("marker lines");
    let marker_line = rich_file.lines().nth(marker_lines.first as usize - 1);
    assert_eq!((marker_lines.first, marker_lines.last), (marker_lines.first, marker_lines.first));
    assert_eq!(marker_line, Some("pub trait Marker {}"));
    // `compute` builds a `Boxed` in its body: the in-crate name links to its
    // declaration, inside the region the compiler's extent verified.
    let compute = plane.find(cx, "compute", DeclarationKind::Function, "rich_project");
    plane.ensure(cx, PageKey::Source(compute.clone()));
    let compute_source = plane.until(cx, "compute source", |store| {
        let resource = store.source(&compute);
        fault(&resource, "compute source");
        resource.loaded_value().cloned()
    });
    let compute_text = compute_source.text.known().unwrap_or_else(|| panic!("compute source: {:?}", compute_source.text.gap()));
    let compute_names = compute_source.identifiers.known().expect("identifiers");
    assert!(
        compute_names.iter().any(|span| {
            compute_text.text().get(span.span.range()) == Some("Boxed") && span.link.target.as_str().ends_with("::Boxed")
        }),
        "an in-crate name links to its declaration: {:?}",
        compute_names
            .iter()
            .map(|span| (compute_text.text().get(span.span.range()).map(str::to_owned), span.link.target.to_string()))
            .collect::<Vec<_>>()
    );

    let boxed = plane.find(cx, "Boxed", DeclarationKind::Struct, "rich_project");
    let boxed_page = plane.symbol(cx, &boxed);
    let up = boxed_page.rose.up.known().expect("typed up");
    let is_marker = up
        .iter()
        .find(|relation| relation.decl.name.as_ref() == "Marker")
        .unwrap_or_else(|| {
            panic!(
                "Boxed is Marker: up = {:?}, relations gap = {:?}, implemented_by = {:?}",
                up.iter().map(|relation| (relation.decl.name.to_string(), relation.kind)).collect::<Vec<_>>(),
                boxed_page.rose.relations_gap,
                (
                    boxed_page.rose.left.known().map(|list| list.iter().map(|relation| (relation.decl.name.to_string(), relation.kind)).collect::<Vec<_>>()),
                    boxed_page.rose.right.known().map(|list| list.iter().map(|relation| (relation.decl.name.to_string(), relation.kind)).collect::<Vec<_>>()),
                )
            )
        });
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

    world.finish(cx);
}

/// The compiled crate (`crates/present` itself): its manifest facts, and its
/// source page, which carries the compiler's own declaration code.
#[gpui::test]
fn a_compiled_crates_dossier_and_source_page_read_through_the_desktop_runtime(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let repo = repo();
    let present = repo.join("crates/present");
    let world = boot(cx, &[&present], &[]);
    let plane = &world.plane;

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
    // ── The compiled crate's source page: the compiler's own declaration
    // code, the whole local file verified against it.
    let compiled_engine = plane.find(cx, "Engine", DeclarationKind::Trait, utf8(&present));
    let compiled_page = plane.symbol(cx, &compiled_engine);
    assert!(compiled_page.identity.semantic, "compiler-addressed coordinate");
    assert_eq!(compiled_page.identity.path.as_deref(), Some("drive.rs"));
    plane.ensure(cx, PageKey::Source(compiled_engine.clone()));
    let source = plane.until(cx, "source", |store| {
        let resource = store.source(&compiled_engine);
        fault(&resource, "source");
        resource.loaded_value().cloned()
    });
    let source_text = source.text.known().unwrap_or_else(|| panic!("source text: {:?}", source.text.gap()));
    assert_eq!(source_text.origin, SourceOrigin::LocalFile);
    let on_disk = std::fs::read_to_string(present.join("drive.rs")).expect("drive.rs");
    assert_eq!(source_text.text(), on_disk.as_str());
    let declaration = source.declaration.known().expect("declaration lines");
    let declared = on_disk
        .lines()
        .skip(declaration.first as usize - 1)
        .take((declaration.last - declaration.first + 1) as usize)
        .collect::<Vec<_>>();
    assert!(declared.contains(&"pub trait Engine {"), "the named lines hold the trait: {declared:?}");
    // `Engine` is longer than one source page, so the page holds its head and
    // says the excerpt is not complete; a complete excerpt ends at the brace.
    if source_text.complete {
        assert_eq!(declared.last(), Some(&"}"), "a complete excerpt ends at the trait's closing brace");
    }
    assert!(
        declared.first().is_some_and(|line| line.starts_with("///") || line.starts_with("pub trait Engine")),
        "the named lines start at the trait's documentation or its head: {declared:?}"
    );
    // A short declaration's named lines run from its head to its closing brace.
    let intent = plane.find(cx, "accepted_view", DeclarationKind::Function, utf8(&present));
    plane.ensure(cx, PageKey::Source(intent.clone()));
    let intent_source = plane.until(cx, "accepted_view source", |store| {
        let resource = store.source(&intent);
        fault(&resource, "accepted_view source");
        resource.loaded_value().cloned()
    });
    let intent_text = intent_source.text.known().unwrap_or_else(|| panic!("accepted_view source: {:?}", intent_source.text.gap()));
    let intent_lines = intent_source.declaration.known().expect("accepted_view lines");
    let intent_code = intent_text
        .text()
        .lines()
        .skip(intent_lines.first as usize - 1)
        .take((intent_lines.last - intent_lines.first + 1) as usize)
        .collect::<Vec<_>>();
    assert!(intent_code.iter().any(|line| line.contains("fn accepted_view")), "{intent_code:?}");
    assert_eq!(intent_code.last(), Some(&"}"), "{intent_code:?}");

    // Its one use is the call in `remove`, with the compiler's exact span: the
    // bytes of `drive.rs` at that span are the callee's name.
    let intent_page = plane.symbol(cx, &intent);
    let sites = intent_page.references.known().unwrap_or_else(|| panic!("references: {:?}", intent_page.references.gap()));
    let site = sites
        .iter()
        .find(|site| site.site.name.as_ref() == "remove")
        .unwrap_or_else(|| panic!("the call in remove is a use of accepted_view: {sites:?}"));
    assert_eq!(site.relation, SemanticLinkKind::Calls);
    assert_eq!(site.confidence, backend_library::SemanticConfidence::Compiler);
    let span = site.span.known().expect("call span");
    assert_eq!(span.file.as_ref(), "drive.rs");
    assert_eq!(&on_disk.as_bytes()[span.bytes.range()], b"accepted_view", "the span covers the call exactly");

    world.finish(cx);
}
