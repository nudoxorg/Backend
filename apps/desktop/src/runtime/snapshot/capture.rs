//! Worker-only reduction of immutable read models into bounded display words.
use crate::core::{Resource, VersionedRoot};
use crate::runtime::snapshot::RetainedDisplay;
use crate::model::browse::{BrowseKey, BrowseValue, CargoReadmeState};
use crate::model::pages::{PageKey, PageStore, Known, CargoSourcePage, OrbitModel};
use crate::model::retained_display::{CaptureCoverage, DisplayAddress, DisplayBody, DisplayObservation, DisplayRow, DisplaySource, RetainedDisplayWire, MAX_DISPLAY_ROWS, MAX_DISPLAY_WORDS};
use crate::navigation::{BrowseRoute, OrbitRoute, Route};
use std::sync::Arc;

/// Capturing the Arcs is cheap on the UI thread; traversal is worker-only.
pub(crate) struct DisplayCapture { route: Route, root: VersionedRoot, value: CaptureValue }
enum CaptureValue { Browse(Arc<BrowseValue>), Source(Arc<CargoSourcePage>), World(Arc<OrbitModel>) }
impl DisplayCapture {
    pub(crate) fn select(pages: &PageStore, route: &Route, root: VersionedRoot) -> Option<Self> {
        if root.is_unserved() || DisplayAddress::for_route(route).is_none() { return None; }
        fn current<T>(pages: &PageStore, key: &PageKey, value: Resource<T>, root: VersionedRoot) -> Option<Arc<T>> {
            if pages.is_seeded(key) || pages.is_owner_read_revoked(key) || pages.inflight(key).is_some()
                || !value.is_loaded() || !value.value_root().is_some_and(|at| at.same_authority(root)) { return None; }
            value.loaded_arc().cloned()
        }
        let value = match route {
            Route::Orbit(OrbitRoute::Browse(browse)) => {
                let key = BrowseKey::from(browse);
                CaptureValue::Browse(current(pages, &PageKey::Browse(key.clone()), pages.browse(&key), root)?)
            }
            Route::CargoSource(source) => {
                let key = crate::model::pages::CargoSourceKey { context: source.browse.context()?.clone(),
                    package: crate::model::pages::PackageRef::parse(source.package.as_str()).ok()?, target: source.target.clone() };
                CaptureValue::Source(current(pages, &PageKey::CargoSource(key.clone()), pages.cargo_source(&key), root)?)
            }
            Route::Package(package) if package.cargo.is_some() => {
                let key = BrowseKey::CargoReadme(crate::model::browse::CargoReadmeKey {
                    context: package.cargo.clone()?, package: crate::model::pages::PackageRef::parse(package.package.as_str()).ok()? });
                CaptureValue::Browse(current(pages, &PageKey::Browse(key.clone()), pages.browse(&key), root)?)
            }
            Route::World => CaptureValue::World(current(pages, &PageKey::Orbit, pages.orbit(), root)?),
            Route::Symbol(_) | Route::Package(_) | Route::Orbit(_) => return None,
        };
        Some(Self { route: route.clone(), root, value })
    }

    pub(crate) fn prepare(&self) -> Option<RetainedDisplay> {
        // The page slot is not itself proof that the payload answered that address.
        match (&self.route, &self.value) {
            (Route::CargoSource(route), CaptureValue::Source(page))
                if page.package.as_str() == route.package.as_str() && page.target == route.target
                    && route.browse.context().is_some_and(|context| context.request_binding() == page.request_binding) => {},
            (Route::Package(route), CaptureValue::Browse(value))
                if matches!(value.as_ref(), BrowseValue::CargoReadme(page)
                    if page.package.as_str() == route.package.as_str()
                        && route.cargo.as_ref().is_some_and(|context| context.request_binding() == page.request_binding)) => {},
            (Route::Orbit(OrbitRoute::Browse(BrowseRoute::Tree(_))), CaptureValue::Browse(value))
                if matches!(value.as_ref(), BrowseValue::Tree(_)) => {},
            (Route::Orbit(OrbitRoute::Browse(BrowseRoute::FindHome)), CaptureValue::Browse(value))
                if matches!(value.as_ref(), BrowseValue::Find(page) if page.prepared.query.is_empty()) => {},
            (Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query))), CaptureValue::Browse(value))
                if matches!(value.as_ref(), BrowseValue::Find(page) if page.prepared.query.as_ref() == &*query.text
                    && match &page.answers { Known::Known(answers) => answers.query == query.text, Known::Unknown(_) => true }) => {},
            (Route::Orbit(OrbitRoute::Browse(BrowseRoute::Compare(selection))), CaptureValue::Browse(value))
                if matches!(value.as_ref(), BrowseValue::Compare(page) if page.packages.iter().map(|package| &package.package)
                    .eq(selection.packages().iter())) => {},
            (Route::World, CaptureValue::World(_)) => {},
            _ => return None,
        }
        let address = DisplayAddress::for_route(&self.route)?;
        let observation = DisplayObservation::at(self.root)?;
        let coverage = CaptureCoverage::Complete;
        if matches!(&self.value, CaptureValue::Source(page) if page.source.text().len() > crate::model::retained_display::MAX_DISPLAY_SOURCE) { return None; }
        let (source, body) = match &self.value {
            CaptureValue::Source(page) => (DisplaySource::Cargo { binding: page.request_binding,
                source_revision: page.source_revision, content_digest: page.content_digest,
                readme_origin: match &page.target { crate::navigation::CargoSourceTarget::PackageFile(_) => None,
                    crate::navigation::CargoSourceTarget::ReadmeLink(link) => Some(link.origin().clone()) } },
                DisplayBody::Source { path: Arc::from(page.target.path().as_str()), text: Arc::from(page.source.text()), first_line: page.source.first_line() }),
            CaptureValue::Browse(value) => match value.as_ref() {
                BrowseValue::CargoReadme(readme) => {
                    let CargoReadmeState::Read(document) = &readme.state else { return None; };
                    (DisplaySource::Cargo { binding: readme.request_binding, source_revision: readme.source_revision,
                        content_digest: document.origin.content_digest, readme_origin: Some(document.origin.clone()) },
                        DisplayBody::Markdown { source: document.source.clone() })
                }
                BrowseValue::Tree(tree) => {
                    let binding = tree.request_binding?;
                    let mut rows = Rows::new(&tree.reading.name)?;
                    rows.push("", &tree.reading.lede)?;
                    for note in [&tree.reading.locked_inactive_note, &tree.reading.source_note, &tree.reading.twice_line] {
                        if let Some(note) = note { rows.push("", note)?; }
                    }
                    rows.push("Advisory coverage", &tree.reading.health)?;
                    for alert in tree.reading.alerts.iter() { rows.push(&alert.title, &alert.why)?; }
                    for role in tree.reading.roles.iter() {
                        rows.push(role.label, role.serving.as_deref().unwrap_or(""))?;
                        for row in role.rows.iter() {
                            rows.push(&row.name, &row.evidence)?;
                            if let Some(text) = &row.at_rest { rows.push("", text)?; }
                            if let Some(text) = &row.description { rows.push("", text)?; }
                            for version in row.versions.iter() { rows.push("Version", version)?; }
                        }
                        if let Some(text) = &role.brings { rows.push("", text)?; }
                    }
                    rows.push("Inventory coverage", &tree.reading.inventory_note)?;
                    for row in tree.reading.inventory.iter() { rows.push(&row.name, &row.version)?; }
                    for duplicate in tree.reading.twice.iter() {
                        rows.push(&duplicate.name, &duplicate.verdict)?;
                        for path in duplicate.paths.iter() { rows.push("", path)?; }
                    }
                    (DisplaySource::Tree { binding }, rows.finish())
                }
                BrowseValue::Find(find) => {
                    let model = &find.prepared;
                    // Never save a staged page as complete or a continuation.
                    if model.loading { return None; }
                    let mut rows = Rows::new(&model.query)?;
                    for note in &model.coverage { rows.push("Coverage", note)?; }
                    if model.more_answers { rows.push("Coverage", "More answers were available; no continuation is retained.")?; }
                    for candidate in &model.candidates {
                        rows.push(&candidate.name, candidate.version.as_deref().unwrap_or(""))?;
                        if let Some(summary) = &candidate.summary { rows.push("", summary)?; }
                        for (label, detail) in &candidate.facts { rows.push(label, detail)?; }
                        for answer in &candidate.answers { rows.answer(answer)?; }
                    }
                    for answer in &model.loose { rows.answer(answer)?; }
                    (DisplaySource::Indexed, rows.finish())
                }
                BrowseValue::Compare(compare) => {
                    let mut rows = Rows::new("Comparison")?;
                    for candidate in &compare.prepared.candidates {
                        rows.push(&candidate.name, &candidate.origin)?;
                        if let Some(version) = &candidate.version { rows.push("Version", version)?; }
                        if let Some(description) = &candidate.description { rows.push("", description)?; }
                        for (label, detail) in &candidate.facts { rows.push(label, detail)?; }
                        if let Some(note) = &candidate.coverage { rows.push("Coverage", note)?; }
                        match &candidate.operations {
                            Some(operations) => for operation in operations { rows.answer(&operation.answer)?; },
                            None => rows.push("Declarations", "The outline was unread.")?,
                        }
                        if !candidate.complete { rows.push("Coverage", "This outline was incomplete.")?; }
                    }
                    (DisplaySource::Indexed, rows.finish())
                }
                BrowseValue::CargoSourceInventory(_) => return None,
            },
            CaptureValue::World(orbit) => {
                let mut rows = Rows::new("World")?;
                match &orbit.tree {
                    Known::Known(nodes) => for node in nodes.iter() { rows.push(&node.title, "")?; },
                    Known::Unknown(gap) => rows.push("Shared tree unavailable", &gap.detail)?,
                }
                match &orbit.projects {
                    Known::Known(projects) => for project in projects.iter() { rows.push(&project.name, "")?; },
                    Known::Unknown(gap) => rows.push("Projects unavailable", &gap.detail)?,
                }
                (DisplaySource::Indexed, rows.finish())
            }
        };
        // Complete refers only to this closed display projection. It grants
        // no complete source inventory, search universe, graph or API proof.
        let projection = RetainedDisplayWire { address, observation, coverage, source, body };
        RetainedDisplay::admit(projection, &self.route)
    }
}
struct Rows { title: Arc<str>, rows: Vec<DisplayRow>, bytes: usize }
impl Rows {
    fn new(title: &str) -> Option<Self> { (title.len() <= MAX_DISPLAY_WORDS).then(|| Self { title: Arc::from(title), rows: Vec::new(), bytes: title.len() }) }
    fn push(&mut self, label: &str, detail: &str) -> Option<()> {
        let bytes = self.bytes.checked_add(label.len())?.checked_add(detail.len())?;
        if self.rows.len() == MAX_DISPLAY_ROWS || bytes > MAX_DISPLAY_WORDS
            || label.len().saturating_add(detail.len()) > crate::model::retained_display::MAX_DISPLAY_ROW_WORDS { return None; }
        self.rows.push(DisplayRow { label: Arc::from(label), detail: Arc::from(detail) }); self.bytes = bytes; Some(())
    }
    fn answer(&mut self, answer: &facet::browse::find::Answer) -> Option<()> {
        self.push(&answer.name, &answer.reason)?;
        if let Some(context) = &answer.context { self.push("", context)?; }
        if let Some(summary) = &answer.summary { self.push("", summary)?; }
        if let Some(signature) = &answer.signature { self.push("", signature)?; }
        Some(())
    }
    fn finish(self) -> DisplayBody { DisplayBody::Reading { title: self.title, rows: self.rows } }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::model::pages::{PageValue, Landing};

    #[test]
    fn capture_requires_a_completed_nonrevoked_read_at_the_exact_observation() {
        let root = crate::runtime::snapshot::tests::served("capture", 1);
        let other = crate::runtime::snapshot::tests::served("other-capture", 2);
        let model = OrbitModel { tree: Known::Known(Arc::from([])), projects: Known::Known(Arc::from([])),
            indexed: Known::Known(Arc::from([])), explore: Known::Known(Arc::from([])) };
        let mut pages = PageStore::default();
        pages.seed(crate::model::pages::SeedEntry::Orbit(Arc::new(model.clone())), VersionedRoot::unserved());
        assert!(DisplayCapture::select(&pages, &Route::World, root).is_none());
        let key = PageKey::Orbit;
        let generation = pages
            .begin(&key, root)
            .expect("page generation admission")
            .expect("fresh revalidation");
        assert!(
            DisplayCapture::select(&pages, &Route::World, root).is_none(),
            "in-flight is never complete"
        );
        assert_eq!(
            pages.land(&key, generation, Ok(PageValue::Orbit(model))),
            Landing::Unchanged
        );
        assert!(DisplayCapture::select(&pages, &Route::World, other).is_none());
        let capture = DisplayCapture::select(&pages, &Route::World, root).expect("current read");
        let display = capture.prepare().expect("closed display");
        assert!(!display.served());
        assert!(pages.revoke_owner_read(&key));
        assert!(DisplayCapture::select(&pages, &Route::World, root).is_none(), "owner loss immediately fences capture");
    }

    #[test]
    fn a_same_slot_cargo_payload_for_another_target_cannot_be_captured() {
        use crate::core::{LocalProjectId, PackageId};
        use crate::model::pages::{SourceText, SourceOrigin};
        use crate::navigation::{CargoSourceRoute, CargoSourcePath, CargoSourceTarget};
        let project = LocalProjectId::new("/workspace/capture-source").expect("project");
        let (_, package, context) = crate::runtime::store::cargo_context_tests::fixture(&project);
        let source = CargoSourceRoute::new(context.clone(), PackageId::new(package.as_str()).expect("package"),
            CargoSourcePath::new("src/lib.rs").expect("path"), None).expect("route");
        let text = "pub fn captured() {}\n";
        let mut page = CargoSourcePage { package, request_binding: context.request_binding(), target: source.target.clone(),
            source: SourceText::new(text.into(), 1, SourceOrigin::LocalFile, true).expect("text"),
            source_revision: [7; 32], content_digest: *blake3::hash(text.as_bytes()).as_bytes() };
        let root = crate::runtime::snapshot::tests::served("source-capture", 2);
        let route = Route::CargoSource(source);
        let exact = DisplayCapture { route: route.clone(), root, value: CaptureValue::Source(Arc::new(page.clone())) };
        assert!(exact.prepare().is_some());
        page.target = CargoSourceTarget::PackageFile(CargoSourcePath::new("src/other.rs").expect("other path"));
        let wrong = DisplayCapture { route, root, value: CaptureValue::Source(Arc::new(page)) };
        assert!(wrong.prepare().is_none(), "the page slot cannot rebind another file's bytes");
    }

    #[test]
    fn projection_rows_stop_before_exceeding_capture_budget() {
        let mut rows = Rows::new("title").expect("title");
        for _ in 0..MAX_DISPLAY_ROWS { assert!(rows.push("", "").is_some()); }
        assert!(rows.push("", "").is_none());
        let mut rows = Rows::new("title").expect("title");
        assert!(rows.push("", &"x".repeat(MAX_DISPLAY_WORDS)).is_none());
        assert!(rows.rows.is_empty(), "oversized input is rejected before copying it");
    }
}
