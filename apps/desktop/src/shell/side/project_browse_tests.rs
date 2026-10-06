//! Choosing a saved project from a Library-scoped Shelf opens its exact Tree
//! even while a different declaration remains in the Reader. Pointer and
//! Return use the same row action and never start another index operation.

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
}

impl PageReader for BrowseFixture {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let ReadRequest::Browse(BrowseKey::Tree(project)) = request else { return tests::Fixture.read(request, context); };
        self.trees.lock().expect("Tree observations").push(project.clone());
        use backend_library::browse::{LockedInactiveCoverage, LockfileGraphCoverage, LockfileWorkspaceMembership, TreeInput, TreeSource, build_tree};
        let tree = build_tree(&TreeInput {
            root: project.service_coordinate().expect("exact local Tree address").to_owned(),
            source: TreeSource::Lockfile {
                reason: "saved Shelf project fixture".into(),
                coverage: LockfileGraphCoverage::Complete,
                workspace_membership: LockfileWorkspaceMembership::Unknown,
            },
            packages: Vec::new(), edges: Vec::new(), locked_inactive: 0,
            locked_inactive_coverage: LockedInactiveCoverage::Unavailable,
        }, &|_: &str, _: &str| panic!("empty fixture needs no advisory read"));
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

#[gpui::test]
fn already_active_saved_project_opens_its_tree_by_native_click_and_return_without_indexing(cx: &mut TestAppContext) {
    for keyboard in [false, true] {
        let trees = Arc::new(Mutex::new(Vec::new()));
        let indexes = Arc::new(AtomicUsize::new(0));
        let tree_reads = Arc::clone(&trees);
        let mut rig = tests::rig_with_engine(cx, Some(tests::page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, move |_| BrowseFixture { trees: Arc::clone(&tree_reads) }).expect("fixture reads"),
            CountingEngine { indexes: Arc::clone(&indexes) });
        let project = LocalProjectId::new("/fixture/shelf-browse-project").expect("saved local project");
        rig.go(Intent::AddProject { project: project.clone() });
        rig.go(Intent::ActivateProject(project.clone()));
        // Adding a new project starts its initial index. Browsing this saved
        // row must preserve that operation, rather than pretending setup did
        // not submit it.
        let initial_indexes = indexes.load(Ordering::SeqCst);
        assert_eq!(initial_indexes, 1, "fixture admission starts exactly one initial index");
        let before = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().workspace().projects[0].clone());
        let library = tests::native_bounds(&mut rig, "Button", "Library", true).expect("native Shelf way out");
        rig.cx.simulate_click(library.center(), Modifiers::none());
        rig.settle();
        assert_eq!(rig.route(), tests::page_route("RelationLabel"), "scoping the Shelf into Library leaves the unrelated Reader visible");
        assert!(fit_tests::painted(&mut rig).texts.iter().any(|text| text.content == "RelationLabel"));
        let row_id = format!("project-{}", project.as_str());
        let row = tests::native_bounds_id(&mut rig, &row_id, "Button", "shelf-browse-project", true)
            .expect("real already-active saved project row");
        if keyboard {
            // Native Shelf selection, then its ordinary Return command. The
            // key walk uses the real mounted order, including Find above it.
            let stops = rig.shell.read_with(rig.cx, |shell, _| shell.shelf_entity())
                .read_with(rig.cx, |shelf, _| shelf.targets.native_keys().len());
            for _ in 0..stops + 1 {
                if rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)).1.as_deref() == Some(row_id.as_str()) { break; }
                rig.keys("down");
            }
            assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx)).1.as_deref(), Some(row_id.as_str()),
                "keyboard selection reaches the actual saved project row");
            rig.keys("enter");
        } else {
            rig.cx.simulate_click(row.center(), Modifiers::none());
            rig.settle();
        }
        assert_eq!(rig.route(), Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(project.clone()))),
            "choosing an already-active project must actually open its exact typed Tree");
        let painted = fit_tests::painted(&mut rig);
        assert!(painted.texts.iter().any(|text| text.content == "Saved shelf project tree"),
            "the requested project Tree must really replace the unrelated Reader");
        assert!(trees.lock().expect("observed reads").iter().any(|read| read == &project),
            "the exact typed local read scope reaches the read pool");
        let after = rig.graph.store.read_with(rig.cx, |store, _| {
            assert_eq!(store.snapshot().workspace().active.as_ref(), Some(&project));
            store.snapshot().workspace().projects[0].clone()
        });
        assert_eq!((after.phase, after.request, after.operation), (before.phase, before.request, before.operation),
            "browsing preserves the saved index lifecycle and operation claim");
        assert_eq!(indexes.load(Ordering::SeqCst), initial_indexes,
            "native project browsing never submits another IndexProject");
    }
}
