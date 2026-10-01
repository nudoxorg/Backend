//! Declaration facts, end to end: a real `backend-locald` indexes two small
//! Rust packages written for this test, and every page is read through the
//! desktop's own runtime path, exactly as a board reads it.
//!
//! The two packages hold the same source. `facts_standalone` is its own
//! single-package workspace, so the owner compiles it and the compiler-backed
//! lane answers; `facts_unmanifested` has no Cargo manifest, so no compiler
//! authority can open it and the structural lane answers. (A member of a
//! small workspace is compiled like a standalone package, so it cannot stand
//! in for the structural lane.) Each assertion reads a fact's own text on the
//! page model (the deprecation note, the section body, the obligation of a
//! named method, a member's second paragraph), never a count, and each page
//! states which lane answered so neither lane can pass for the other.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines, missing_docs)]

use backend_client::{LocalSubscriptionTransport, Session};
use backend_desktop::core::{LocalProjectId, VersionedRoot};
use backend_desktop::model::AppSnapshot;
use backend_desktop::model::pages::{
    DocFragment, Member, Obligation, PageKey, SearchQuery, SectionKind, SymbolPage, SymbolRef,
};
use backend_desktop::runtime::reads::{ReadPool, SessionReader};
use backend_desktop::runtime::store::DataStore;
use backend_desktop::runtime::{DesktopRuntime, EngineActor, LocalEngineClient, UiEntityGraph};
use backend_library::{CommandReply, DeclarationKind, RowState};
use gpui::{Entity, TestAppContext};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const INDEX_DEADLINE: Duration = Duration::from_mins(15);
const READ_DEADLINE: Duration = Duration::from_mins(2);

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

    fn symbol(&self, cx: &mut TestAppContext, symbol: &SymbolRef) -> SymbolPage {
        self.ensure(cx, PageKey::Symbol(symbol.clone()));
        self.until(cx, symbol.as_str(), |store| {
            let resource = store.symbol(symbol);
            fault(&resource, symbol.as_str());
            resource.loaded_value().cloned()
        })
    }
}

fn fault<T>(resource: &backend_desktop::core::Resource<T>, what: &str) {
    if let backend_desktop::core::ResourceTerminal::Fault(error) = resource.terminal() {
        panic!("{what} failed: {:?} {}", error.code(), error.message());
    }
}


fn member<'a>(page: &'a SymbolPage, name: &str) -> &'a Member {
    page.members
        .known()
        .expect("members")
        .all()
        .find(|member| member.decl.name.as_ref() == name)
        .unwrap_or_else(|| panic!("{name} is not a member of {}", page.identity.name))
}

/// Reads every fact of the fixture source on one lane's pages.
fn assert_facts(plane: &Plane, cx: &mut TestAppContext, root: &Path, semantic: bool) {
    let within = utf8(root);
    let lane = if semantic { "compiler-backed" } else { "structural" };

    // Item-level deprecation, with the source's own since and note.
    let stale = plane.find(cx, "stale", DeclarationKind::Function, within);
    let stale_page = plane.symbol(cx, &stale);
    assert_eq!(stale_page.identity.semantic, semantic, "{lane} lane answered stale");
    let notice = stale_page
        .identity
        .facts
        .deprecated()
        .unwrap_or_else(|| panic!("{lane}: stale is not deprecated: {:?}", stale_page.identity.facts));
    assert_eq!(notice.since.as_deref(), Some("1.2.0"), "{lane}");
    assert_eq!(notice.note.as_deref(), Some("use `fresh` instead"), "{lane}");

    // Doc sections, read by the Rust convention.
    let sections = stale_page
        .sections
        .sections
        .iter()
        .map(|section| {
            (
                section.kind,
                section.title.to_string(),
                DocFragment::plain_text(&section.body),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sections,
        [
            (SectionKind::Errors, "Errors".to_owned(), "Fails when the name is empty.".to_owned()),
            (SectionKind::Panics, "Panics".to_owned(), "Never panics.".to_owned()),
        ],
        "{lane}"
    );
    assert_eq!(
        DocFragment::plain_text(&stale_page.sections.lead),
        "Makes a widget from a name.\n\nThe name is trimmed first.",
        "{lane}"
    );

    // A declaration the producer read and found current.
    let fresh = plane.find(cx, "fresh", DeclarationKind::Function, within);
    let fresh_page = plane.symbol(cx, &fresh);
    assert_eq!(fresh_page.identity.facts.deprecation.known(), Some(&None), "{lane}");

    // Required and provided trait members, and a member's whole documentation.
    let service = plane.find(cx, "Service", DeclarationKind::Trait, within);
    let service_page = plane.symbol(cx, &service);
    let execute = member(&service_page, "execute");
    assert_eq!(
        execute.decl.facts.obligation.known(),
        Some(&Some(Obligation::Required)),
        "{lane}: execute has no body"
    );
    let docs = DocFragment::plain_text(&execute.docs);
    assert!(
        docs.contains("A second paragraph: the ledger shows it only when a member keeps"),
        "{lane}: the member's second paragraph is gone: {docs:?}"
    );
    assert_eq!(
        member(&service_page, "describe").decl.facts.obligation.known(),
        Some(&Some(Obligation::Provided)),
        "{lane}: describe has a default body"
    );

    // A deprecated field, struck wherever its reference appears.
    let plain = plane.find(cx, "Plain", DeclarationKind::Struct, within);
    let plain_page = plane.symbol(cx, &plain);
    let label = member(&plain_page, "label");
    assert_eq!(
        label.decl.facts.deprecated().and_then(|notice| notice.note.as_deref()),
        Some("read `name` instead"),
        "{lane}"
    );
    assert_eq!(
        member(&plain_page, "name").decl.facts.deprecation.known(),
        Some(&None),
        "{lane}"
    );
}

#[gpui::test]
fn declaration_facts_reach_the_page_on_both_lanes(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .canonicalize()
        .expect("fixtures");
    let standalone = fixtures.join("facts_standalone");
    let structural = fixtures.join("facts_unmanifested");
    let (host, endpoint, state) = serve(&[&standalone, &structural]);

    let mut subscription = LocalSubscriptionTransport::connect(&endpoint).expect("subscription");
    let (_, revision) = subscription.bootstrap_root().expect("root");
    let snapshot = AppSnapshot::empty(VersionedRoot::from_revision(1, revision, 0));
    let project = LocalProjectId::from_path(&standalone).expect("project identity");
    let actor = EngineActor::start(LocalEngineClient::new(&endpoint, project), 32).expect("actor");
    let runtime = DesktopRuntime::new(snapshot, actor);
    let reader_endpoint = endpoint.clone();
    let pool = ReadPool::start(3, move |_| SessionReader::connect(&reader_endpoint)).expect("read pool");
    let graph = cx.update(|cx| UiEntityGraph::install_with_reads(cx, runtime, None, Some(pool)));
    let plane = Plane {
        store: graph.store.clone(),
    };

    assert_facts(&plane, cx, &standalone, true);
    assert_facts(&plane, cx, &structural, false);

    drop(graph);
    drop(subscription);
    drop(host);
    let _ = std::fs::remove_dir_all(state);
    let _ = std::fs::remove_file(endpoint);
}
