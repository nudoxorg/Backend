//! Project selection and dependency-tree selection are separate typed routes.
//! Both pointer and Return preserve the existing index operation; a non-Cargo
//! or unread manifest can never be sent to the Cargo-only tree producer.

use crate::core::LocalProjectId;
use crate::model::browse::{BrowseKey, BrowseValue};
use crate::model::pages::{PageValue, ReadFailure};
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Route};
use crate::runtime::actor::{EngineClient, EngineDto, EngineFault, EngineRequest};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::shell::{fit_tests, tests};
use gpui::{Modifiers, TestAppContext};
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};

struct BrowseFixture {
    trees: Arc<Mutex<Vec<LocalProjectId>>>,
    project: LocalProjectId,
    capability: crate::model::project_browse::ProjectTreeCapability,
}

impl PageReader for BrowseFixture {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if matches!(request, ReadRequest::Orbit) {
            let PageValue::Orbit(mut model) = tests::Fixture.read(request, context)? else { panic!("fixture Orbit"); };
            model.indexed = crate::model::pages::Known::Known(Arc::from([crate::model::pages::IndexedPackage {
                package: crate::model::pages::PackageRef::parse(self.project.as_str()).expect("exact local project"),
                name: Arc::from("saved project"), readiness: crate::model::pages::Readiness::Ready, verified_registry_release: None,
            }]));
            return Ok(PageValue::Orbit(model));
        }
        if let ReadRequest::Package(package) = request
            && package.as_str() == self.project.as_str() {
            let mut dossier = tests::dossier();
            dossier.package = package.clone();
            dossier.project_tree = self.capability;
            if let crate::model::pages::Known::Known(record) = &mut dossier.record {
                record.package = package.clone(); record.name = Arc::from("Saved shelf project page");
                record.ecosystem = match self.capability {
                    crate::model::project_browse::ProjectTreeCapability::Cargo => crate::model::pages::Known::Known(backend_library::RegistryEcosystem::Cargo),
                    crate::model::project_browse::ProjectTreeCapability::Unestablished => crate::model::pages::Known::unknown(crate::model::pages::GapReason::NotRecorded, "FastAPI mixed project has no Cargo root evidence"),
                };
            }
            return Ok(PageValue::Package(dossier));
        }
        let ReadRequest::Browse(BrowseKey::Tree(project)) = request else { return tests::Fixture.read(request, context); };
        self.trees.lock().expect("Tree observations").push(project.clone());
        use backend_library::browse::{LockedInactiveCoverage, LockfileGraphCoverage, LockfileWorkspaceMembership, TreeInput, TreeSource, build_tree};
        let mut tree = build_tree(&TreeInput {
            root: project.service_coordinate().expect("exact local Tree address").to_owned(),
            source: TreeSource::Lockfile {
                reason: "saved Shelf project fixture".into(),
                coverage: LockfileGraphCoverage::Complete,
                workspace_membership: LockfileWorkspaceMembership::Unknown,
            },
            packages: Vec::new(), edges: Vec::new(), locked_inactive: 0,
            locked_inactive_coverage: LockedInactiveCoverage::Unavailable,
        }, &|_: &str, _: &str| panic!("empty fixture needs no advisory read"));
        let binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
            &project.path(), &tree.root,
        ).expect("exact display fixture request");
        tree.observation = Some(backend_library::browse::ProjectTreeObservationV1::DisplayOnly { binding });
        let mut model = crate::runtime::browse_reads::tree_model(&tree);
        model.prepared = Arc::new(facet::browse::library::Model {
            name: "Saved shelf project tree".into(),
            lede: "The exact project chosen in the Shelf".into(),
            lede_tip: None, note: None, alerts: vec![], facts: vec![], roles: vec![],
            inventory: vec![], inventory_index: std::collections::BTreeMap::new(), inventory_note: "".into(),
            twice_heading: None, twice: vec![],
        });
        Ok(PageValue::Browse(BrowseValue::Tree(Arc::new(model))))
    }
}

struct CountingEngine {
    indexes: Arc<AtomicUsize>,
}

impl EngineClient for CountingEngine {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        if matches!(request, EngineRequest::IndexProject { .. }) { self.indexes.fetch_add(1, Ordering::SeqCst); }
        tests::RootOnly.execute(request)
    }
}

fn browse_saved_project(cx: &mut TestAppContext, keyboard: bool, choose_tree: bool, capability: crate::model::project_browse::ProjectTreeCapability) {
    let trees = Arc::new(Mutex::new(Vec::new()));
    let indexes = Arc::new(AtomicUsize::new(0));
    let tree_reads = Arc::clone(&trees);
    let project = LocalProjectId::new(if capability == crate::model::project_browse::ProjectTreeCapability::Cargo { "/fixture/shelf-browse-project" } else { "/fixture/fastapi-full-stack" }).expect("saved local project");
    let fixture_project = project.clone();
    let mut rig = tests::rig_with_engine(cx, Some(tests::page_route("RelationLabel")), 1440.0, 900.0,
        ReadPool::start(1, move |_| BrowseFixture { trees: Arc::clone(&tree_reads), project: fixture_project.clone(), capability }).expect("fixture reads"),
        CountingEngine { indexes: Arc::clone(&indexes) });
    rig.go(Intent::AddProject { project: project.clone() });
    rig.go(Intent::ActivateProject(project.clone()));
    // Adding a new project starts its initial index. Browsing this saved
    // row must preserve that operation, rather than pretending setup did
    // not submit it.
    if choose_tree {
        // Capability comes from the selected package worker, never a catalog
        // scan. Visit it once, then return to the unrelated retained Reader.
        let package = crate::model::pages::PackageRef::parse(project.as_str()).expect("exact package");
        rig.go(Intent::Navigate(crate::shell::kit::package_route(&package).expect("package route")));
        rig.go(Intent::Navigate(tests::page_route("RelationLabel")));
    }
    let initial_indexes = indexes.load(Ordering::SeqCst);
    assert_eq!(initial_indexes, 1, "fixture admission starts exactly one initial index");
    let before = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().workspace().projects[0].clone());
    let library = tests::native_bounds(&mut rig, "Button", "Library", true).expect("native Shelf way out");
    rig.cx.simulate_click(library.center(), Modifiers::none());
    rig.settle();
    tests::native_boundary_evidence(&mut rig, &format!("Library pointer handed off: keyboard={keyboard} tree={choose_tree} capability={capability:?}"));
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)).0, super::super::focus::Zone::Shelf,
        "the admitted Library pointer keeps keyboard ownership in its current row list");
    assert_eq!(rig.route(), tests::page_route("RelationLabel"), "scoping the Shelf into Library leaves the unrelated Reader visible");
    assert!(fit_tests::painted(&mut rig).texts.iter().any(|text| text.content == "RelationLabel"));
    let row_id = format!("{}-{}", if choose_tree { "project-tree" } else { "project" }, project.as_str());
    let label = if choose_tree { "shelf-browse-project dependency tree" } else if capability == crate::model::project_browse::ProjectTreeCapability::Cargo { "shelf-browse-project" } else { "fastapi-full-stack" };
    if capability == crate::model::project_browse::ProjectTreeCapability::Unestablished {
        assert!(tests::native_bounds_id(&mut rig, &format!("project-tree-{}", project.as_str()), "Button", "fastapi-full-stack dependency tree", true).is_none(), "unknown/non-Cargo roots expose no Cargo route");
    }
    let row = tests::native_bounds_id(&mut rig, &row_id, "Button", label, true);
    if row.is_none() {
        tests::native_boundary_evidence(&mut rig, &format!("project row missing: keyboard={keyboard} tree={choose_tree} capability={capability:?} id={row_id} label={label}"));
    }
    let row = row.expect("real already-active saved project row");
    if keyboard {
        // Native Shelf selection, then its ordinary Return command. The
        // key walk uses the real mounted order, including Find above it.
        let stops = rig.shell.read_with(rig.cx, |shell, _| shell.shelf_entity())
            .read_with(rig.cx, |shelf, _| shelf.targets.native_keys().len());
        for _ in 0..stops + 1 {
            if rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)).1.as_deref() == Some(row_id.as_str()) { break; }
            rig.keys("down");
        }
        tests::native_boundary_evidence(&mut rig, &format!("Library keyboard walk complete: tree={choose_tree} capability={capability:?} expected={row_id}"));
        assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)).1.as_deref(), Some(row_id.as_str()),
            "keyboard selection reaches the actual saved project row");
        rig.keys("enter");
    } else {
        rig.cx.simulate_click(row.center(), Modifiers::none());
        rig.settle();
    }
    let expected = if choose_tree { Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project.clone()))) }
        else { crate::shell::kit::package_route(&crate::model::pages::PackageRef::parse(project.as_str()).expect("project package")).expect("package route") };
    assert_eq!(rig.route(), expected, "selection opens the exact typed destination");
    let painted = fit_tests::painted(&mut rig);
    assert!(painted.texts.iter().any(|text| text.content == if choose_tree { "Saved shelf project tree" } else { "Saved shelf project page" }), "the selected current project really replaces the unrelated Reader");
    if choose_tree {
        assert!(trees.lock().expect("observed reads").iter().any(|read| read == &project), "the exact typed local read scope reaches the read pool");
    } else {
        assert!(trees.lock().expect("observed reads").is_empty(), "opening the project package never probes the Cargo tree");
    }
    let after = rig.graph.store.read_with(rig.cx, |store, _| {
        assert_eq!(store.snapshot().workspace().active.as_ref(), Some(&project));
        store.snapshot().workspace().projects[0].clone()
    });
    assert_eq!((after.phase, after.request, after.operation), (before.phase, before.request, before.operation),
        "browsing preserves the saved index lifecycle and operation claim");
    assert_eq!(indexes.load(Ordering::SeqCst), initial_indexes,
        "native project browsing never submits another IndexProject");
}

#[gpui::test]
fn already_active_saved_project_opens_its_tree_by_native_click_and_return_without_indexing(cx: &mut TestAppContext) {
    for keyboard in [false, true] { browse_saved_project(cx, keyboard, true, crate::model::project_browse::ProjectTreeCapability::Cargo); }
}

#[gpui::test]
fn native_project_selection_opens_package_without_cargo_route_or_new_index(cx: &mut TestAppContext) {
    for capability in [crate::model::project_browse::ProjectTreeCapability::Cargo, crate::model::project_browse::ProjectTreeCapability::Unestablished] {
        for keyboard in [false, true] { browse_saved_project(cx, keyboard, false, capability); }
    }
}
