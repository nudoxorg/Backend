//! Resident rows for one semantic image.
//!
//! View publication reopens an image on every pass. Walking that image and
//! rendering its documents is the cost that stays after validation. This
//! residence keeps the projected rows while the image bytes, package, profile,
//! path, staleness, and structural sites are unchanged, then stamps the
//! current view basis on admission.

use super::super::IndexedProject;
use super::semantic::{SemanticRowSink, project_image_rows};
use backend_engine::Row;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

/// Images retained at once. One publication walks every image of the edited
/// package; the bound is large enough for that pass, and the least recently
/// used entry leaves first.
const MAX_RESIDENT_IMAGES: usize = 4096;
/// Declaration rows retained across those images.
const MAX_RESIDENT_ROWS: usize = 65_536;

/// How a repeated declaration identity is admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DuplicatePolicy {
    /// A second canonical declaration with this identity is a corrupt image.
    Error,
    /// A repeated external target is skipped. The first row stays.
    Skip,
}

/// One byte-budget step replayed when the cached rows are admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Charge {
    /// Decreases the remaining rebuild budget.
    Sub(usize),
    /// Increases the remaining rebuild budget.
    ///
    /// The stale note is appended and then credited, matching the historical
    /// projection accounting.
    Add(usize),
}

/// One cached row plus the admission policy that produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ProjectedRow {
    /// Row content. Its basis is replaced with the admitting view.
    pub(super) row: Row,
    /// Whether a duplicate of `row.id` is an error or a skip.
    pub(super) duplicate: DuplicatePolicy,
    /// Budget steps applied before the row is pushed.
    pub(super) charges: Vec<Charge>,
}

/// Rows projected from one image under one overlay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ProjectedImage {
    /// Declaration rows, then that declaration's external targets.
    pub(super) rows: Vec<ProjectedRow>,
}

/// Projected semantic rows reused across publications of the same image.
#[derive(Debug)]
pub(in crate::builtin) struct ImageRowResidence {
    image_limit: usize,
    row_limit: usize,
    entries: BTreeMap<[u8; 32], Arc<ProjectedImage>>,
    order: VecDeque<[u8; 32]>,
    hits: u64,
    misses: u64,
}

impl Default for ImageRowResidence {
    fn default() -> Self {
        Self::with_image_limit(MAX_RESIDENT_IMAGES)
    }
}

impl ImageRowResidence {
    fn with_image_limit(image_limit: usize) -> Self {
        Self::with_limits(image_limit, MAX_RESIDENT_ROWS)
    }

    fn with_limits(image_limit: usize, row_limit: usize) -> Self {
        Self {
            image_limit: image_limit.max(1),
            row_limit: row_limit.max(1),
            entries: BTreeMap::new(),
            order: VecDeque::new(),
            hits: 0,
            misses: 0,
        }
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn hits(&self) -> u64 {
        self.hits
    }

    fn misses(&self) -> u64 {
        self.misses
    }

    fn reset_counts(&mut self) {
        self.hits = 0;
        self.misses = 0;
    }

    fn touch(&mut self, key: [u8; 32]) {
        if let Some(position) = self.order.iter().position(|existing| *existing == key) {
            self.order.remove(position);
        }
        self.order.push_back(key);
    }

    fn evict(&mut self) {
        while self.entries.len() > self.image_limit || self.retained_rows() > self.row_limit {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    fn retained_rows(&self) -> usize {
        self.entries.values().map(|image| image.rows.len()).sum()
    }

    fn cached(&mut self, key: [u8; 32]) -> Option<Arc<ProjectedImage>> {
        let projected = self.entries.get(&key)?.clone();
        self.hits = self.hits.saturating_add(1);
        self.touch(key);
        Some(projected)
    }

    fn store(&mut self, key: [u8; 32], projected: Arc<ProjectedImage>) {
        self.entries.insert(key, projected);
        self.touch(key);
        self.evict();
        self.misses = self.misses.saturating_add(1);
    }
}

/// Admits one image's rows, projecting them only when the residence misses.
pub(super) fn append_resident_image_rows(
    image: &backend_semantic::ir::SemanticImageView<'_>,
    project: &IndexedProject,
    profile: backend_semantic::vocabulary::LanguageProfile,
    sink: &mut SemanticRowSink<'_>,
    residence: &mut ImageRowResidence,
) -> Result<(), super::super::BuiltinModelError> {
    let image_identity = *blake3::hash(image.as_ref()).as_bytes();
    let key = projection_key(image_identity, project, profile, sink);
    let projected = if let Some(cached) = residence.cached(key) {
        cached
    } else {
        let projected = Arc::new(project_image_rows(
            image,
            image_identity,
            project,
            profile,
            sink.initial.basis(),
            sink.stale,
            sink.path,
            sink.site_declarations,
        )?);
        residence.store(key, Arc::clone(&projected));
        projected
    };
    apply_projected_image(&projected, sink)
}

/// Stamps the current basis and replays symbol and byte admission.
pub(super) fn apply_projected_image(
    projected: &ProjectedImage,
    sink: &mut SemanticRowSink<'_>,
) -> Result<(), super::super::BuiltinModelError> {
    let basis = sink.initial.basis();
    for projected_row in &projected.rows {
        match projected_row.duplicate {
            DuplicatePolicy::Skip => {
                if !sink.symbols.insert(projected_row.row.id) {
                    continue;
                }
                if sink.rows.len() == sink.capacity {
                    return Err(row_bound());
                }
            }
            DuplicatePolicy::Error => {
                if sink.rows.len() == sink.capacity {
                    return Err(row_bound());
                }
                if !sink.symbols.insert(projected_row.row.id) {
                    return Err(super::super::BuiltinModelError(
                        "semantic publication contains a duplicate declaration identity".to_owned(),
                    ));
                }
            }
        }
        for charge in &projected_row.charges {
            apply_charge(sink, *charge)?;
        }
        let mut row = projected_row.row.clone();
        row.basis = basis;
        sink.rows.push(row);
    }
    Ok(())
}

fn apply_charge(
    sink: &mut SemanticRowSink<'_>,
    charge: Charge,
) -> Result<(), super::super::BuiltinModelError> {
    let updated = match charge {
        Charge::Sub(bytes) => sink.remaining_bytes.checked_sub(bytes),
        Charge::Add(bytes) => sink.remaining_bytes.checked_add(bytes),
    };
    *sink.remaining_bytes = updated.ok_or_else(|| {
        super::super::BuiltinModelError(
            "workspace semantic declarations exceed the rebuild byte bound".to_owned(),
        )
    })?;
    Ok(())
}

fn row_bound() -> super::super::BuiltinModelError {
    super::super::BuiltinModelError(
        "workspace semantic declarations exceed the rebuild row bound".to_owned(),
    )
}

fn projection_key(
    image_identity: [u8; 32],
    project: &IndexedProject,
    profile: backend_semantic::vocabulary::LanguageProfile,
    sink: &SemanticRowSink<'_>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic-image-rows.v1\0");
    hasher.update(&image_identity);
    hasher.update(&project.package.to_bytes());
    hasher.update(project.label.as_bytes());
    hasher.update(&[0]);
    let code: [u8; 2] = profile.into();
    hasher.update(&code);
    hasher.update(sink.path.as_bytes());
    hasher.update(&[0]);
    hasher.update(&[u8::from(sink.stale)]);
    for site in sink.site_declarations {
        hasher.update(site.name().as_bytes());
        hasher.update(&[0]);
        hasher.update(site.kind_name().as_bytes());
        hasher.update(&site.line().to_le_bytes());
        hasher.update(site.signature().as_bytes());
        hasher.update(&[0]);
        if let Some(text) = site.source_excerpt().text() {
            hasher.update(text.as_bytes());
        }
        hasher.update(&[0xff]);
    }
    *hasher.finalize().as_bytes()
}

/// Times a cold projection of one semantic image against the resident hit.
///
/// The image, package, and view basis are built before the timer. `cold` and
/// `stale_cold` include one reopen plus a projection miss. `warm` and
/// `stale_warm` include one reopen plus a hit. `resident` is the hit alone.
#[allow(clippy::expect_used, clippy::print_stdout)]
pub(super) fn measure_semantic_image_rows() {
    const SAMPLES: usize = 32;
    const WARMUPS: usize = 4;
    const PATH: &str = "src/worker.rs";
    let bytes = super::image_reopen::fixture_semantic_image(PATH).expect("fixture image");
    let opened = backend_semantic::ir::SemanticImageView::reopen(&bytes).expect("reopen");
    let project = fixture_project("fixture");
    let head = super::super::genesis().expect("genesis");
    let (initial, _) =
        super::super::initial_view_for_workspace(&head.snapshot()).expect("initial view");
    let mut proof = ImageRowResidence::default();
    let fresh = admit(&opened, &project, &initial, &mut proof, false);
    let mut stale_residence = ImageRowResidence::default();
    let stale = admit(&opened, &project, &initial, &mut stale_residence, true);
    fresh
        .iter()
        .any(|row| row.label.contains("Worker"))
        .then_some(())
        .expect("fixture image did not project Worker");
    (fresh.len() == stale.len())
        .then_some(())
        .expect("staleness changed the projected row set");
    fresh_has_no_note(&fresh);
    stale_has_note(&stale);
    drop(opened);

    let cold = sample(WARMUPS, SAMPLES, || {
        let mut residence = ImageRowResidence::default();
        let view = backend_semantic::ir::SemanticImageView::reopen(&bytes).expect("reopen");
        admit(&view, &project, &initial, &mut residence, false).len()
    });
    let mut warm_residence = ImageRowResidence::default();
    {
        let view = backend_semantic::ir::SemanticImageView::reopen(&bytes).expect("reopen");
        let _ = admit(&view, &project, &initial, &mut warm_residence, false);
    }
    for _ in 0..WARMUPS {
        let view = backend_semantic::ir::SemanticImageView::reopen(&bytes).expect("reopen");
        let _ = admit(&view, &project, &initial, &mut warm_residence, false);
    }
    warm_residence.reset_counts();
    let warm = sample(0, SAMPLES, || {
        let view = backend_semantic::ir::SemanticImageView::reopen(&bytes).expect("reopen");
        admit(&view, &project, &initial, &mut warm_residence, false).len()
    });
    let warm_hits = warm_residence.hits();
    (warm_hits == SAMPLES as u64)
        .then_some(())
        .expect("warm publication projected the image");
    let resident_view = backend_semantic::ir::SemanticImageView::reopen(&bytes).expect("reopen");
    for _ in 0..WARMUPS {
        let _ = admit(
            &resident_view,
            &project,
            &initial,
            &mut warm_residence,
            false,
        );
    }
    warm_residence.reset_counts();
    let resident = sample(0, SAMPLES, || {
        admit(
            &resident_view,
            &project,
            &initial,
            &mut warm_residence,
            false,
        )
        .len()
    });
    let resident_hits = warm_residence.hits();
    (resident_hits == SAMPLES as u64)
        .then_some(())
        .expect("resident admission projected the image");

    let stale_cold = sample(WARMUPS, SAMPLES, || {
        let mut residence = ImageRowResidence::default();
        let view = backend_semantic::ir::SemanticImageView::reopen(&bytes).expect("reopen");
        admit(&view, &project, &initial, &mut residence, true).len()
    });
    let mut stale_warm_residence = ImageRowResidence::default();
    {
        let view = backend_semantic::ir::SemanticImageView::reopen(&bytes).expect("reopen");
        let _ = admit(&view, &project, &initial, &mut stale_warm_residence, true);
    }
    for _ in 0..WARMUPS {
        let view = backend_semantic::ir::SemanticImageView::reopen(&bytes).expect("reopen");
        let _ = admit(&view, &project, &initial, &mut stale_warm_residence, true);
    }
    stale_warm_residence.reset_counts();
    let stale_warm = sample(0, SAMPLES, || {
        let view = backend_semantic::ir::SemanticImageView::reopen(&bytes).expect("reopen");
        admit(&view, &project, &initial, &mut stale_warm_residence, true).len()
    });
    (stale_warm_residence.hits() == SAMPLES as u64)
        .then_some(())
        .expect("stale overlay was projected on the warm path");

    let (cold_median, cold_p95) = percentiles(&cold);
    let (warm_median, warm_p95) = percentiles(&warm);
    let (resident_median, resident_p95) = percentiles(&resident);
    let (stale_cold_median, stale_cold_p95) = percentiles(&stale_cold);
    let (stale_warm_median, stale_warm_p95) = percentiles(&stale_warm);
    println!(
        "semantic_image_rows bytes={} rows={} cold_median_ns={cold_median} cold_p95_ns={cold_p95} warm_median_ns={warm_median} warm_p95_ns={warm_p95} resident_median_ns={resident_median} resident_p95_ns={resident_p95} stale_cold_median_ns={stale_cold_median} stale_cold_p95_ns={stale_cold_p95} stale_warm_median_ns={stale_warm_median} stale_warm_p95_ns={stale_warm_p95} warm_hits={warm_hits} resident_hits={resident_hits}",
        bytes.len(),
        fresh.len(),
    );
}

fn fixture_project(label: &str) -> IndexedProject {
    IndexedProject {
        package: backend_engine::package_key(label),
        label: label.to_owned(),
        files: Arc::<[[u8; 32]]>::from([]),
    }
}

fn admit(
    image: &backend_semantic::ir::SemanticImageView<'_>,
    project: &IndexedProject,
    initial: &backend_engine::ViewRoot,
    residence: &mut ImageRowResidence,
    stale: bool,
) -> Vec<Row> {
    let mut symbols = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    let mut remaining = usize::MAX;
    let mut sink = SemanticRowSink {
        initial,
        symbols: &mut symbols,
        rows: &mut rows,
        capacity: 64,
        remaining_bytes: &mut remaining,
        stale,
        path: "src/worker.rs",
        site_declarations: &[],
    };
    append_resident_image_rows(
        image,
        project,
        backend_semantic::vocabulary::LanguageProfile::Rust(
            backend_semantic::vocabulary::RustEdition::Rust2024,
        ),
        &mut sink,
        residence,
    )
    .expect("admit image rows");
    rows
}

fn fresh_has_no_note(rows: &[Row]) {
    let noted = rows.iter().any(|row| {
        row.document.iter().any(|fragment| {
            matches!(fragment, backend_engine::Fragment::Text(text) if text.contains("stale semantic image"))
        })
    });
    (!noted)
        .then_some(())
        .expect("a fresh row carried the stale note");
}

fn stale_has_note(rows: &[Row]) {
    let noted = rows.iter().all(|row| {
        row.document.iter().any(|fragment| {
            matches!(fragment, backend_engine::Fragment::Text(text) if text.contains("stale semantic image"))
        })
    });
    noted
        .then_some(())
        .expect("a stale row lost its typed note");
}

fn sample<T>(warmups: usize, samples: usize, mut body: impl FnMut() -> T) -> Vec<u128> {
    for _ in 0..warmups {
        let _ = body();
    }
    let mut samples_ns = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = std::time::Instant::now();
        let _ = body();
        samples_ns.push(started.elapsed().as_nanos());
    }
    samples_ns
}

#[allow(clippy::indexing_slicing)]
fn percentiles(samples: &[u128]) -> (u128, u128) {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let median = ordered[ordered.len() / 2];
    let p95 = ordered[ordered.len() * 95 / 100];
    (median, p95)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::super::super::{genesis, initial_view_for_workspace};
    use super::super::semantic::{SemanticRowSink, append_image_rows, project_image_rows};
    use super::{
        Charge, DuplicatePolicy, ImageRowResidence, ProjectedImage, ProjectedRow,
        append_resident_image_rows, apply_projected_image,
    };
    use backend_engine::{Row, RowId};
    use backend_semantic::ir::SemanticImageView;
    use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    const PATH: &str = "src/worker.rs";

    struct Fixture {
        bytes: Vec<u8>,
        initial: backend_engine::ViewRoot,
    }

    fn fixture() -> Fixture {
        let bytes =
            super::super::image_reopen::fixture_semantic_image(PATH).expect("fixture image");
        let head = genesis().expect("genesis");
        let (initial, _) = initial_view_for_workspace(&head.snapshot()).expect("initial view");
        Fixture { bytes, initial }
    }

    fn project(label: &str) -> super::super::super::IndexedProject {
        super::super::super::IndexedProject {
            package: backend_engine::package_key(label),
            label: label.to_owned(),
            files: Arc::<[[u8; 32]]>::from([]),
        }
    }

    #[derive(Debug)]
    struct Admitted {
        rows: Vec<Row>,
        remaining: usize,
    }

    fn admit(
        image: &SemanticImageView<'_>,
        project: &super::super::super::IndexedProject,
        initial: &backend_engine::ViewRoot,
        residence: &mut ImageRowResidence,
        stale: bool,
        capacity: usize,
        remaining: usize,
        symbols: &mut BTreeSet<RowId>,
        sites: &[&backend_compile::SourceDeclaration],
    ) -> Result<Admitted, super::super::super::BuiltinModelError> {
        let mut rows = Vec::new();
        let mut remaining = remaining;
        let mut sink = SemanticRowSink {
            initial,
            symbols,
            rows: &mut rows,
            capacity,
            remaining_bytes: &mut remaining,
            stale,
            path: PATH,
            site_declarations: sites,
        };
        append_resident_image_rows(
            image,
            project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            &mut sink,
            residence,
        )?;
        Ok(Admitted { rows, remaining })
    }

    fn admit_direct(
        image: &SemanticImageView<'_>,
        project: &super::super::super::IndexedProject,
        initial: &backend_engine::ViewRoot,
        stale: bool,
        capacity: usize,
        remaining: usize,
    ) -> Result<Admitted, super::super::super::BuiltinModelError> {
        let mut symbols = BTreeSet::new();
        let mut rows = Vec::new();
        let mut remaining = remaining;
        let mut sink = SemanticRowSink {
            initial,
            symbols: &mut symbols,
            rows: &mut rows,
            capacity,
            remaining_bytes: &mut remaining,
            stale,
            path: PATH,
            site_declarations: &[],
        };
        append_image_rows(
            image,
            project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            &mut sink,
        )?;
        Ok(Admitted { rows, remaining })
    }

    #[test]
    fn a_second_publication_hits_and_matches_the_direct_projection() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let project = project("fixture");
        let direct = admit_direct(&image, &project, &fixture.initial, false, 64, usize::MAX)
            .expect("direct");
        let mut residence = ImageRowResidence::default();
        let mut symbols = BTreeSet::new();
        let first = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("miss");
        let mut symbols = BTreeSet::new();
        let second = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("hit");
        assert_eq!(residence.misses(), 1);
        assert_eq!(residence.hits(), 1);
        assert_eq!(residence.len(), 1);
        assert_eq!(first.rows, direct.rows);
        assert_eq!(second.rows, direct.rows);
        assert_eq!(first.remaining, direct.remaining);
        assert_eq!(second.remaining, direct.remaining);
        assert!(second.rows.iter().any(|row| row.label.contains("Worker")));
        assert!(second.rows.iter().any(|row| row.label.contains("name")));
        assert!(second.rows.iter().any(|row| row.label.contains("Event")));
        assert!(second.rows.iter().any(|row| row.label.contains("Started")));
    }

    #[test]
    fn a_stale_overlay_does_not_reuse_fresh_rows() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let project = project("fixture");
        let mut residence = ImageRowResidence::default();
        let mut symbols = BTreeSet::new();
        let fresh = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("fresh");
        let mut symbols = BTreeSet::new();
        let stale = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            true,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("stale");
        let mut symbols = BTreeSet::new();
        let again = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("fresh hit");
        assert_eq!(residence.misses(), 2);
        assert_eq!(residence.hits(), 1);
        assert_eq!(residence.len(), 2);
        assert_eq!(again.rows, fresh.rows);
        assert!(stale.rows.iter().all(|row| {
            row.document.iter().any(|fragment| {
                matches!(fragment, backend_engine::Fragment::Text(text) if text.contains("stale semantic image"))
            })
        }));
        assert!(fresh.rows.iter().all(|row| {
            row.document.iter().all(|fragment| {
                !matches!(fragment, backend_engine::Fragment::Text(text) if text.contains("stale semantic image"))
            })
        }));
        assert_eq!(stale.rows.len(), fresh.rows.len());
    }

    #[test]
    fn a_different_package_misses_and_changes_the_symbol() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let mut residence = ImageRowResidence::default();
        let mut symbols = BTreeSet::new();
        let fixture_rows = admit(
            &image,
            &project("fixture"),
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("fixture");
        let mut symbols = BTreeSet::new();
        let other = admit(
            &image,
            &project("other"),
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("other");
        assert_eq!(residence.misses(), 2);
        assert_eq!(residence.hits(), 0);
        let fixture_id = fixture_rows.rows.first().expect("fixture row").id;
        let other_id = other.rows.first().expect("other row").id;
        assert_ne!(fixture_id, other_id);
        assert!(other.rows.iter().any(|row| row.label.contains("other")));
    }

    #[test]
    fn a_different_structural_site_misses() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let project = project("fixture");
        let kept = backend_compile::SourceDeclaration::at_path(
            PATH,
            "Worker",
            backend_engine::DeclarationKind::Struct,
            2,
            "struct Worker",
            "",
        )
        .expect("kept site");
        let edited = backend_compile::SourceDeclaration::at_path(
            PATH,
            "Worker",
            backend_engine::DeclarationKind::Struct,
            2,
            "struct Worker { renamed: u64 }",
            "",
        )
        .expect("edited site");
        let mut residence = ImageRowResidence::default();
        let mut symbols = BTreeSet::new();
        let _ = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[&kept],
        )
        .expect("kept");
        let mut symbols = BTreeSet::new();
        let _ = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[&edited],
        )
        .expect("edited");
        let mut symbols = BTreeSet::new();
        let _ = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[&kept],
        )
        .expect("kept hit");
        assert_eq!(residence.misses(), 2);
        assert_eq!(residence.hits(), 1);
        assert_eq!(residence.len(), 2);
    }

    #[test]
    fn admission_restamps_the_current_view_basis() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let project = project("fixture");
        let projected = project_image_rows(
            &image,
            *blake3::hash(image.as_ref()).as_bytes(),
            &project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            fixture.initial.basis(),
            false,
            PATH,
            &[],
        )
        .expect("project");
        let mut tweaked = projected;
        let row = tweaked.rows.first_mut().expect("row");
        row.row.basis.schema = row.row.basis.schema.wrapping_add(9);
        let corrupted = row.row.basis;
        let mut symbols = BTreeSet::new();
        let mut rows = Vec::new();
        let mut remaining = usize::MAX;
        let mut sink = SemanticRowSink {
            initial: &fixture.initial,
            symbols: &mut symbols,
            rows: &mut rows,
            capacity: 64,
            remaining_bytes: &mut remaining,
            stale: false,
            path: PATH,
            site_declarations: &[],
        };
        apply_projected_image(&tweaked, &mut sink).expect("apply");
        let admitted = rows.first().expect("admitted");
        assert_eq!(admitted.basis, fixture.initial.basis());
        assert_ne!(admitted.basis, corrupted);
    }

    #[test]
    fn a_tight_budget_fails_the_same_way_on_a_hit() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let project = project("fixture");
        for budget in [0_usize, 1, 8, 32, 128, 1024, usize::MAX] {
            let direct = admit_direct(&image, &project, &fixture.initial, false, 64, budget)
                .map_err(|error| error.to_string());
            let mut residence = ImageRowResidence::default();
            let mut symbols = BTreeSet::new();
            let miss = admit(
                &image,
                &project,
                &fixture.initial,
                &mut residence,
                false,
                64,
                budget,
                &mut symbols,
                &[],
            )
            .map_err(|error| error.to_string());
            let mut symbols = BTreeSet::new();
            let hit = admit(
                &image,
                &project,
                &fixture.initial,
                &mut residence,
                false,
                64,
                budget,
                &mut symbols,
                &[],
            )
            .map_err(|error| error.to_string());
            match (&direct, &miss, &hit) {
                (Ok(direct), Ok(miss), Ok(hit)) => {
                    assert_eq!(miss.rows, direct.rows, "budget {budget}");
                    assert_eq!(hit.rows, direct.rows, "budget {budget}");
                    assert_eq!(miss.remaining, direct.remaining, "budget {budget}");
                    assert_eq!(hit.remaining, direct.remaining, "budget {budget}");
                }
                (Err(direct), Err(miss), Err(hit)) => {
                    assert_eq!(miss.to_string(), direct.to_string(), "budget {budget}");
                    assert_eq!(hit.to_string(), direct.to_string(), "budget {budget}");
                }
                _ => panic!("budget {budget} diverged: {direct:?} {miss:?} {hit:?}"),
            }
        }
    }

    #[test]
    fn capacity_and_duplicate_failures_keep_the_projection() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let project = project("fixture");
        let mut residence = ImageRowResidence::default();
        let mut symbols = BTreeSet::new();
        let primed = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("prime");
        let mut symbols = BTreeSet::new();
        let capacity = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            1,
            usize::MAX,
            &mut symbols,
            &[],
        );
        assert!(
            capacity
                .expect_err("capacity")
                .to_string()
                .contains("row bound")
        );
        assert_eq!(
            symbols.len(),
            1,
            "the first row was admitted before the bound"
        );
        let stolen = primed.rows.first().expect("row").id;
        let mut symbols = BTreeSet::from([stolen]);
        let duplicate = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        );
        assert!(
            duplicate
                .expect_err("duplicate")
                .to_string()
                .contains("duplicate declaration identity")
        );
        assert_eq!(residence.len(), 1);
        assert_eq!(residence.misses(), 1);
        assert!(residence.hits() >= 2);
        let mut symbols = BTreeSet::new();
        let recovered = admit(
            &image,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("recovered hit");
        assert_eq!(recovered.rows, primed.rows);
    }

    #[test]
    fn the_least_recently_used_image_is_evicted() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let project = project("fixture");
        let mut residence = ImageRowResidence::with_image_limit(2);
        let paths = ["src/a.rs", "src/b.rs", "src/c.rs"];
        for path in paths {
            let mut symbols = BTreeSet::new();
            let mut rows = Vec::new();
            let mut remaining = usize::MAX;
            let mut sink = SemanticRowSink {
                initial: &fixture.initial,
                symbols: &mut symbols,
                rows: &mut rows,
                capacity: 64,
                remaining_bytes: &mut remaining,
                stale: false,
                path,
                site_declarations: &[],
            };
            append_resident_image_rows(
                &image,
                &project,
                LanguageProfile::Rust(RustEdition::Rust2024),
                &mut sink,
                &mut residence,
            )
            .expect("store");
        }
        assert_eq!(residence.len(), 2);
        assert_eq!(residence.misses(), 3);
        let mut symbols = BTreeSet::new();
        let mut rows = Vec::new();
        let mut remaining = usize::MAX;
        let mut sink = SemanticRowSink {
            initial: &fixture.initial,
            symbols: &mut symbols,
            rows: &mut rows,
            capacity: 64,
            remaining_bytes: &mut remaining,
            stale: false,
            path: "src/a.rs",
            site_declarations: &[],
        };
        append_resident_image_rows(
            &image,
            &project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            &mut sink,
            &mut residence,
        )
        .expect("evicted path");
        assert_eq!(residence.misses(), 4, "the oldest path was projected again");
        let mut symbols = BTreeSet::new();
        let mut rows = Vec::new();
        let mut remaining = usize::MAX;
        let mut sink = SemanticRowSink {
            initial: &fixture.initial,
            symbols: &mut symbols,
            rows: &mut rows,
            capacity: 64,
            remaining_bytes: &mut remaining,
            stale: false,
            path: "src/c.rs",
            site_declarations: &[],
        };
        append_resident_image_rows(
            &image,
            &project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            &mut sink,
            &mut residence,
        )
        .expect("recent path");
        assert_eq!(residence.hits(), 1, "the most recent path stayed resident");
    }

    #[test]
    fn an_image_past_the_row_budget_is_dropped() {
        let fixture = fixture();
        let mut residence = ImageRowResidence::with_limits(4, 2);
        let mut rows = Vec::new();
        for index in 0..4 {
            let label = format!("row-{index}");
            rows.push(ProjectedRow {
                row: Row::new(
                    RowId::Symbol(backend_engine::symbol_key(&label)),
                    fixture.initial.basis(),
                    label,
                ),
                duplicate: DuplicatePolicy::Error,
                charges: vec![Charge::Sub(1)],
            });
        }
        residence.store([9_u8; 32], Arc::new(ProjectedImage { rows }));
        assert_eq!(residence.len(), 0, "one image past the row budget leaves");
        assert_eq!(residence.misses(), 1);
        let label = "kept";
        residence.store(
            [8_u8; 32],
            Arc::new(ProjectedImage {
                rows: vec![ProjectedRow {
                    row: Row::new(
                        RowId::Symbol(backend_engine::symbol_key(label)),
                        fixture.initial.basis(),
                        label,
                    ),
                    duplicate: DuplicatePolicy::Error,
                    charges: vec![Charge::Sub(1)],
                }],
            }),
        );
        assert_eq!(residence.len(), 1, "an image inside the budget stays");
    }
}
