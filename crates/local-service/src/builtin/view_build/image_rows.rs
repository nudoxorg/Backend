//! Resident rows for one semantic image.
//!
//! View publication used to reopen an image on every pass. This residence
//! keeps two facts about those bytes. The path and source identity, plus the
//! publication keys that already admitted the image, so a later pass does not
//! validate again. And the projected rows, while the package, profile, path,
//! staleness, and structural sites are unchanged. Admission stamps the current
//! view basis onto the cached rows.

use super::super::IndexedProject;
use super::semantic::{SemanticRowSink, compiled_source, project_image_rows};
use backend_engine::Row;
use backend_version::{ContentId, SourceFactDomain};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
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

/// Path and source identity proved for one image, and the publication keys
/// that have admitted those bytes.
#[derive(Debug)]
struct ImageByteFacts {
    path: String,
    identity: ContentId<SourceFactDomain>,
    admissions: BTreeSet<[u8; 32]>,
}

/// Projected semantic rows reused across publications of the same image.
#[derive(Debug)]
pub(in crate::builtin) struct ImageRowResidence {
    image_limit: usize,
    row_limit: usize,
    entries: BTreeMap<[u8; 32], Arc<ProjectedImage>>,
    order: VecDeque<[u8; 32]>,
    facts: BTreeMap<[u8; 32], ImageByteFacts>,
    fact_order: VecDeque<[u8; 32]>,
    hits: u64,
    misses: u64,
    reopens: u64,
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
            facts: BTreeMap::new(),
            fact_order: VecDeque::new(),
            hits: 0,
            misses: 0,
            reopens: 0,
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

    fn reopens(&self) -> u64 {
        self.reopens
    }

    /// Counts one validation of image bytes.
    pub(super) fn note_reopen(&mut self) {
        self.reopens = self.reopens.saturating_add(1);
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

    fn touch_facts(&mut self, digest: [u8; 32]) {
        if let Some(position) = self
            .fact_order
            .iter()
            .position(|existing| *existing == digest)
        {
            self.fact_order.remove(position);
        }
        self.fact_order.push_back(digest);
    }

    fn evict_facts(&mut self) {
        while self.facts.len() > self.image_limit {
            let Some(oldest) = self.fact_order.pop_front() else {
                break;
            };
            self.facts.remove(&oldest);
        }
    }

    fn store_facts(
        &mut self,
        digest: [u8; 32],
        path: String,
        identity: ContentId<SourceFactDomain>,
    ) {
        if let Some(existing) = self.facts.get_mut(&digest) {
            existing.path = path;
            existing.identity = identity;
        } else {
            self.facts.insert(
                digest,
                ImageByteFacts {
                    path,
                    identity,
                    admissions: BTreeSet::new(),
                },
            );
        }
        self.touch_facts(digest);
        self.evict_facts();
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
    let image_identity = image_digest(image.as_ref());
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

fn image_digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Fingerprint of the publication key that `admit_image` checks.
///
/// The image bytes are not part of this token. A second key over the same
/// bytes misses until that key admits the image itself.
pub(super) fn publication_admission(
    profile: backend_semantic::vocabulary::LanguageProfile,
    ecosystem: &str,
    name: &str,
    coordinate: &str,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic-image-admission.v1\0");
    let code: [u8; 2] = profile.into();
    hasher.update(&code);
    hasher.update(ecosystem.as_bytes());
    hasher.update(&[0]);
    hasher.update(name.as_bytes());
    hasher.update(&[0]);
    hasher.update(coordinate.as_bytes());
    hasher.update(&[0]);
    *hasher.finalize().as_bytes()
}

/// One image opened for projection, or already admitted for this key.
pub(super) enum CompiledImage<'bytes> {
    /// Path and source identity retained from an earlier admission.
    Resident {
        /// Image bytes, used only when the row overlay misses.
        bytes: &'bytes [u8],
        /// Project-relative path the image was compiled from.
        path: String,
        /// Source content identity carried by the image.
        identity: ContentId<SourceFactDomain>,
        /// Blake3 of `bytes`, the row-cache image identity.
        digest: [u8; 32],
    },
    /// A view validated on this pass. Admission is not confirmed yet.
    Opened {
        /// Project-relative path the image was compiled from.
        path: String,
        /// Source content identity carried by the image.
        identity: ContentId<SourceFactDomain>,
        /// Validated view of `bytes`.
        view: backend_semantic::ir::SemanticImageView<'bytes>,
    },
}

/// Returns the retained path and identity when `admission` already passed.
///
/// A miss validates the bytes, records the path and identity, and leaves the
/// admission unconfirmed. Forgetting [`confirm_image_admission`] makes the
/// next call validate again.
pub(super) fn open_compiled_image<'bytes>(
    bytes: &'bytes [u8],
    admission: [u8; 32],
    residence: &mut ImageRowResidence,
) -> Result<CompiledImage<'bytes>, super::super::BuiltinModelError> {
    let digest = image_digest(bytes);
    if let Some(facts) = residence.facts.get(&digest)
        && facts.admissions.contains(&admission)
    {
        let path = facts.path.clone();
        let identity = facts.identity;
        residence.touch_facts(digest);
        return Ok(CompiledImage::Resident {
            bytes,
            path,
            identity,
            digest,
        });
    }
    let view = backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|error| {
        super::super::BuiltinModelError(format!("reopen activated semantic image: {error}"))
    })?;
    let (path, identity) = compiled_source(&view)?;
    residence.note_reopen();
    residence.store_facts(digest, path.clone(), identity);
    Ok(CompiledImage::Opened {
        path,
        identity,
        view,
    })
}

/// Confirms `admission` only after `admit_image` accepts the view.
///
/// A rejected image leaves the admission unset, so the next open validates
/// the bytes again.
pub(super) fn bind_opened_image(
    bytes: &[u8],
    admission: [u8; 32],
    view: &backend_semantic::ir::SemanticImageView<'_>,
    key: &backend_engine::builtin::ProductSemanticPublicationKey,
    residence: &mut ImageRowResidence,
) -> Result<(), super::super::BuiltinModelError> {
    key.admit_image(view).map_err(|error| {
        super::super::BuiltinModelError(format!(
            "bind semantic publication to product key: {error}"
        ))
    })?;
    confirm_image_admission(bytes, admission, residence);
    Ok(())
}

/// Records that `admit_image` accepted `bytes` for `admission`.
///
/// Calling this before `admit_image` returns `Ok` would skip that check on
/// the next publication. A missing fact is left missing: the next open
/// validates again.
pub(super) fn confirm_image_admission(
    bytes: &[u8],
    admission: [u8; 32],
    residence: &mut ImageRowResidence,
) {
    let digest = image_digest(bytes);
    if let Some(facts) = residence.facts.get_mut(&digest) {
        facts.admissions.insert(admission);
        residence.touch_facts(digest);
    }
}

/// Admits each image for `key`, reopening only when that key has not admitted it.
///
/// A resident admission returns without validating the bytes. A rejected key is
/// not recorded, so the next call validates again. An admission that already
/// succeeded stays in place.
pub(in crate::builtin) fn admit_activated_images(
    images: &[impl AsRef<[u8]>],
    key: &backend_engine::builtin::ProductSemanticPublicationKey,
    residence: &mut ImageRowResidence,
) -> Result<(), super::super::BuiltinModelError> {
    let lineage = key.lineage().map_err(|error| {
        super::super::BuiltinModelError(format!("semantic publication lineage: {error}"))
    })?;
    let admission = publication_admission(
        key.profile(),
        lineage.ecosystem,
        lineage.name,
        key.coordinate().as_str(),
    );
    for image in images {
        let bytes = image.as_ref();
        if let CompiledImage::Opened { view, .. } = open_compiled_image(bytes, admission, residence)?
        {
            bind_opened_image(bytes, admission, &view, key, residence)?;
        }
    }
    Ok(())
}

/// Applies a cached projection for `digest`.
///
/// `Ok(true)` stamped the resident rows. `Ok(false)` is a projection miss and
/// does not count as one: the caller reopens and projects. A capacity, byte,
/// or duplicate error is returned with the cached rows left in place.
pub(super) fn apply_resident_image(
    digest: [u8; 32],
    project: &IndexedProject,
    profile: backend_semantic::vocabulary::LanguageProfile,
    sink: &mut SemanticRowSink<'_>,
    residence: &mut ImageRowResidence,
) -> Result<bool, super::super::BuiltinModelError> {
    let key = projection_key(digest, project, profile, sink);
    let Some(projected) = residence.cached(key) else {
        return Ok(false);
    };
    apply_projected_image(&projected, sink)?;
    Ok(true)
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
    (proof.misses() == 1 && proof.len() == 1 && stale_residence.misses() == 1)
        .then_some(())
        .expect("proof projection was not a single miss");
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

/// Times thirty-two distinct images cold, then again after each one was admitted.
///
/// Images, the publication key, and the view basis are built before the timer.
/// `cold` validates and projects every image. `warm` reuses the admitted path
/// and the projected rows, so `warm_reopens` stays zero.
#[allow(clippy::expect_used, clippy::print_stdout)]
pub(super) fn measure_semantic_image_batch() {
    const IMAGES: usize = 32;
    const SAMPLES: usize = 32;
    const WARMUPS: usize = 4;
    let images = (0..IMAGES)
        .map(|index| {
            let path = format!("src/file{index}.rs");
            let salt = u8::try_from(index).expect("image index");
            super::image_reopen::fixture_semantic_image_salted(&path, salt).expect("fixture image")
        })
        .collect::<Vec<_>>();
    let project = fixture_project("fixture");
    let profile = backend_semantic::vocabulary::LanguageProfile::Rust(
        backend_semantic::vocabulary::RustEdition::Rust2024,
    );
    let package = backend_engine::PackageReference::parse("fixture").expect("package");
    let coordinate =
        backend_semantic::vocabulary::PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .expect("coordinate");
    let key =
        backend_engine::builtin::ProductSemanticPublicationKey::new(package, coordinate, profile)
            .expect("publication key");
    let head = super::super::genesis().expect("genesis");
    let (initial, _) =
        super::super::initial_view_for_workspace(&head.snapshot()).expect("initial view");
    let mut proof = ImageRowResidence::default();
    let proof_rows = publish_image_batch(&images, &key, &project, &initial, &mut proof);
    (proof_rows == IMAGES * 4 && proof.reopens() == IMAGES as u64)
        .then_some(())
        .expect("cold batch did not validate every image");
    let primed = publish_image_batch(&images, &key, &project, &initial, &mut proof);
    (primed == proof_rows && proof.reopens() == IMAGES as u64 && proof.hits() == IMAGES as u64)
        .then_some(())
        .expect("primed batch revalidated or missed a row cache");

    let cold = sample(WARMUPS, SAMPLES, || {
        let mut residence = ImageRowResidence::default();
        publish_image_batch(&images, &key, &project, &initial, &mut residence)
    });
    let mut warm_residence = ImageRowResidence::default();
    let _ = publish_image_batch(&images, &key, &project, &initial, &mut warm_residence);
    for _ in 0..WARMUPS {
        let _ = publish_image_batch(&images, &key, &project, &initial, &mut warm_residence);
    }
    let reopens_before = warm_residence.reopens();
    warm_residence.reset_counts();
    let warm = sample(0, SAMPLES, || {
        publish_image_batch(&images, &key, &project, &initial, &mut warm_residence)
    });
    let warm_reopens = warm_residence.reopens().saturating_sub(reopens_before);
    let warm_hits = warm_residence.hits();
    (warm_reopens == 0 && warm_hits == (IMAGES * SAMPLES) as u64)
        .then_some(())
        .expect("warm batch validated an image or missed the row cache");
    let (cold_median, cold_p95) = percentiles(&cold);
    let (warm_median, warm_p95) = percentiles(&warm);
    println!(
        "semantic_image_batch images={IMAGES} rows={proof_rows} cold_median_ns={cold_median} cold_p95_ns={cold_p95} warm_median_ns={warm_median} warm_p95_ns={warm_p95} warm_reopens={warm_reopens} warm_hits={warm_hits}"
    );
}

/// Times admitting a batch of images against reusing that admission.
///
/// Images and the publication key are built before either timer. `cold` validates
/// and admits every image. `warm` finds the admission and does not validate.
#[allow(clippy::expect_used, clippy::print_stdout)]
pub(super) fn measure_semantic_admission() {
    const IMAGES: usize = 32;
    const SAMPLES: usize = 32;
    const WARMUPS: usize = 4;
    let images = (0..IMAGES)
        .map(|index| {
            let path = format!("src/file{index}.rs");
            let salt = u8::try_from(index).expect("image index");
            super::image_reopen::fixture_semantic_image_salted(&path, salt).expect("fixture image")
        })
        .collect::<Vec<_>>();
    let profile = backend_semantic::vocabulary::LanguageProfile::Rust(
        backend_semantic::vocabulary::RustEdition::Rust2024,
    );
    let package = backend_engine::PackageReference::parse("fixture").expect("package");
    let coordinate =
        backend_semantic::vocabulary::PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
            .expect("coordinate");
    let key =
        backend_engine::builtin::ProductSemanticPublicationKey::new(package, coordinate, profile)
            .expect("publication key");
    let bytes: usize = images.iter().map(Vec::len).sum();
    for _ in 0..WARMUPS {
        let mut residence = ImageRowResidence::default();
        admit_activated_images(&images, &key, &mut residence).expect("cold warmup");
    }
    let mut cold = Vec::with_capacity(SAMPLES);
    let mut cold_reopens = 0_u64;
    for _ in 0..SAMPLES {
        let mut residence = ImageRowResidence::default();
        let started = std::time::Instant::now();
        admit_activated_images(&images, &key, &mut residence).expect("cold");
        cold.push(started.elapsed().as_nanos());
        cold_reopens += residence.reopens();
        std::hint::black_box(residence.reopens());
    }
    let mut warm_residence = ImageRowResidence::default();
    admit_activated_images(&images, &key, &mut warm_residence).expect("warm prime");
    for _ in 0..WARMUPS {
        admit_activated_images(&images, &key, &mut warm_residence).expect("warm warmup");
    }
    let reopens_before = warm_residence.reopens();
    let mut warm = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = std::time::Instant::now();
        admit_activated_images(&images, &key, &mut warm_residence).expect("warm");
        warm.push(started.elapsed().as_nanos());
        std::hint::black_box(warm_residence.reopens());
    }
    let warm_reopens = warm_residence.reopens().saturating_sub(reopens_before);
    (cold_reopens == (IMAGES * SAMPLES) as u64 && warm_reopens == 0)
        .then_some(())
        .expect("admission cache validated a warm image");
    let (cold_median, cold_p95) = percentiles(&cold);
    let (warm_median, warm_p95) = percentiles(&warm);
    println!(
        "semantic_admission images={IMAGES} bytes={bytes} cold_median_ns={cold_median} cold_p95_ns={cold_p95} warm_median_ns={warm_median} warm_p95_ns={warm_p95} cold_reopens={cold_reopens} warm_reopens={warm_reopens}"
    );
}

#[allow(clippy::expect_used)]
fn publish_image_batch(
    images: &[Vec<u8>],
    key: &backend_engine::builtin::ProductSemanticPublicationKey,
    project: &IndexedProject,
    initial: &backend_engine::ViewRoot,
    residence: &mut ImageRowResidence,
) -> usize {
    let lineage = key.lineage().expect("publication lineage");
    let admission = publication_admission(
        key.profile(),
        lineage.ecosystem,
        lineage.name,
        key.coordinate().as_str(),
    );
    let mut held = Vec::with_capacity(images.len());
    let mut pending = images;
    while let Some((bytes, rest)) = pending.split_first() {
        match open_compiled_image(bytes, admission, residence).expect("open image") {
            CompiledImage::Opened { view, .. } => {
                bind_opened_image(bytes, admission, &view, key, residence).expect("admit image");
                held.push(BatchImage::Opened(view));
            }
            CompiledImage::Resident { digest, path, .. } => {
                held.push(BatchImage::Resident { digest, path });
            }
        }
        pending = rest;
    }
    let mut symbols = BTreeSet::new();
    let mut rows = Vec::new();
    let mut remaining = usize::MAX;
    let profile = key.profile();
    for image in &held {
        match image {
            BatchImage::Opened(view) => {
                let path = compiled_source(view).expect("compiled source").0;
                let mut sink = SemanticRowSink {
                    initial,
                    symbols: &mut symbols,
                    rows: &mut rows,
                    capacity: 4_096,
                    remaining_bytes: &mut remaining,
                    stale: false,
                    path: &path,
                    site_declarations: &[],
                };
                append_resident_image_rows(view, project, profile, &mut sink, residence)
                    .expect("project image");
            }
            BatchImage::Resident { digest, path } => {
                let mut sink = SemanticRowSink {
                    initial,
                    symbols: &mut symbols,
                    rows: &mut rows,
                    capacity: 4_096,
                    remaining_bytes: &mut remaining,
                    stale: false,
                    path: path.as_str(),
                    site_declarations: &[],
                };
                let applied = apply_resident_image(*digest, project, profile, &mut sink, residence)
                    .expect("apply image");
                applied
                    .then_some(())
                    .expect("resident image missed its rows");
            }
        }
    }
    rows.len()
}

enum BatchImage<'bytes> {
    Opened(backend_semantic::ir::SemanticImageView<'bytes>),
    Resident { digest: [u8; 32], path: String },
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
        Charge, CompiledImage, DuplicatePolicy, ImageRowResidence, ProjectedImage, ProjectedRow,
        admit_activated_images, append_resident_image_rows, apply_projected_image,
        apply_resident_image, bind_opened_image, confirm_image_admission, open_compiled_image,
        publication_admission, publish_image_batch,
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

    fn admission(coordinate: &str) -> [u8; 32] {
        publication_admission(
            LanguageProfile::Rust(RustEdition::Rust2024),
            "cargo",
            "fixture",
            coordinate,
        )
    }

    #[test]
    fn a_confirmed_image_does_not_reopen() {
        let fixture = fixture();
        let admission = admission("pkg:cargo/fixture@1.0.0");
        let mut residence = ImageRowResidence::default();
        let first = open_compiled_image(&fixture.bytes, admission, &mut residence).expect("open");
        let CompiledImage::Opened { path, .. } = first else {
            panic!("the first open validated nothing");
        };
        assert_eq!(path, PATH);
        assert_eq!(residence.reopens(), 1);
        confirm_image_admission(&fixture.bytes, admission, &mut residence);
        let second = open_compiled_image(&fixture.bytes, admission, &mut residence).expect("hit");
        let CompiledImage::Resident {
            path: resident_path,
            identity,
            ..
        } = second
        else {
            panic!("a confirmed image was validated again");
        };
        assert_eq!(resident_path, PATH);
        assert_eq!(residence.reopens(), 1);
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let (direct_path, direct_identity) =
            super::super::semantic::compiled_source(&image).expect("source");
        assert_eq!(resident_path, direct_path);
        assert_eq!(identity, direct_identity);
    }

    #[test]
    fn an_unconfirmed_image_reopens() {
        let fixture = fixture();
        let admission = admission("pkg:cargo/fixture@1.0.0");
        let mut residence = ImageRowResidence::default();
        let first = open_compiled_image(&fixture.bytes, admission, &mut residence).expect("open");
        assert!(matches!(first, CompiledImage::Opened { .. }));
        let second = open_compiled_image(&fixture.bytes, admission, &mut residence).expect("again");
        assert!(matches!(second, CompiledImage::Opened { .. }));
        assert_eq!(residence.reopens(), 2);
    }

    #[test]
    fn a_different_publication_key_reopens() {
        let fixture = fixture();
        let first_key = admission("pkg:cargo/fixture@1.0.0");
        let other_key = admission("pkg:cargo/fixture@2.0.0");
        assert_ne!(first_key, other_key);
        let mut residence = ImageRowResidence::default();
        let _ = open_compiled_image(&fixture.bytes, first_key, &mut residence).expect("open");
        confirm_image_admission(&fixture.bytes, first_key, &mut residence);
        let second = open_compiled_image(&fixture.bytes, other_key, &mut residence).expect("other");
        assert!(matches!(second, CompiledImage::Opened { .. }));
        assert_eq!(residence.reopens(), 2);
        confirm_image_admission(&fixture.bytes, other_key, &mut residence);
        let again = open_compiled_image(&fixture.bytes, first_key, &mut residence).expect("first");
        assert!(matches!(again, CompiledImage::Resident { .. }));
        assert_eq!(residence.reopens(), 2);
    }

    #[test]
    fn a_resident_apply_matches_the_direct_projection() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let project = project("fixture");
        let direct = admit_direct(&image, &project, &fixture.initial, false, 64, usize::MAX)
            .expect("direct");
        let admission = admission("pkg:cargo/fixture@1.0.0");
        let mut residence = ImageRowResidence::default();
        let opened = open_compiled_image(&fixture.bytes, admission, &mut residence).expect("open");
        let CompiledImage::Opened { view, .. } = &opened else {
            panic!("first open was resident");
        };
        confirm_image_admission(&fixture.bytes, admission, &mut residence);
        let mut symbols = BTreeSet::new();
        let projected = admit(
            view,
            &project,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("project");
        drop(opened);
        let resident = open_compiled_image(&fixture.bytes, admission, &mut residence).expect("hit");
        let CompiledImage::Resident { digest, .. } = resident else {
            panic!("confirmed image reopened");
        };
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
        let applied = apply_resident_image(
            digest,
            &project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            &mut sink,
            &mut residence,
        )
        .expect("apply");
        assert!(applied);
        assert_eq!(rows, direct.rows);
        assert_eq!(remaining, direct.remaining);
        assert_eq!(projected.rows, direct.rows);
        assert_eq!(residence.reopens(), 1);
    }

    #[test]
    fn a_stale_overlay_misses_without_dropping_the_fresh_rows() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let project = project("fixture");
        let admission = admission("pkg:cargo/fixture@1.0.0");
        let mut residence = ImageRowResidence::default();
        let _ = open_compiled_image(&fixture.bytes, admission, &mut residence).expect("open");
        confirm_image_admission(&fixture.bytes, admission, &mut residence);
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
        .expect("fresh project");
        let digest = *blake3::hash(fixture.bytes.as_slice()).as_bytes();
        let mut symbols = BTreeSet::new();
        let mut rows = Vec::new();
        let mut remaining = usize::MAX;
        let mut sink = SemanticRowSink {
            initial: &fixture.initial,
            symbols: &mut symbols,
            rows: &mut rows,
            capacity: 64,
            remaining_bytes: &mut remaining,
            stale: true,
            path: PATH,
            site_declarations: &[],
        };
        let applied = apply_resident_image(
            digest,
            &project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            &mut sink,
            &mut residence,
        )
        .expect("stale miss");
        assert!(!applied);
        assert!(rows.is_empty());
        assert_eq!(residence.misses(), 1);
        assert_eq!(residence.hits(), 0);
        assert_eq!(residence.reopens(), 1);
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
        .expect("stale project");
        assert!(stale.rows.iter().all(|row| {
            row.document.iter().any(|fragment| {
                matches!(fragment, backend_engine::Fragment::Text(text) if text.contains("stale semantic image"))
            })
        }));
        assert_eq!(residence.misses(), 2);
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
        let fresh_hit = apply_resident_image(
            digest,
            &project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            &mut sink,
            &mut residence,
        )
        .expect("fresh hit");
        assert!(fresh_hit);
        assert_eq!(rows, fresh.rows);
        assert!(fresh.rows.iter().all(|row| {
            row.document.iter().all(|fragment| {
                !matches!(fragment, backend_engine::Fragment::Text(text) if text.contains("stale semantic image"))
            })
        }));
        assert_eq!(residence.reopens(), 1);
    }

    #[test]
    fn a_capacity_failure_on_a_resident_apply_keeps_the_image() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let project = project("fixture");
        let admission = admission("pkg:cargo/fixture@1.0.0");
        let mut residence = ImageRowResidence::default();
        let _ = open_compiled_image(&fixture.bytes, admission, &mut residence).expect("open");
        confirm_image_admission(&fixture.bytes, admission, &mut residence);
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
        let digest = *blake3::hash(fixture.bytes.as_slice()).as_bytes();
        let mut symbols = BTreeSet::new();
        let mut rows = Vec::new();
        let mut remaining = usize::MAX;
        let mut sink = SemanticRowSink {
            initial: &fixture.initial,
            symbols: &mut symbols,
            rows: &mut rows,
            capacity: 1,
            remaining_bytes: &mut remaining,
            stale: false,
            path: PATH,
            site_declarations: &[],
        };
        let failed = apply_resident_image(
            digest,
            &project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            &mut sink,
            &mut residence,
        );
        assert!(
            failed
                .expect_err("capacity")
                .to_string()
                .contains("row bound")
        );
        assert_eq!(residence.reopens(), 1);
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
        let recovered = apply_resident_image(
            digest,
            &project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            &mut sink,
            &mut residence,
        )
        .expect("recovered");
        assert!(recovered);
        assert_eq!(rows, primed.rows);
        assert_eq!(residence.reopens(), 1);
    }

    #[test]
    fn the_least_recently_used_image_facts_reopen() {
        let older =
            super::super::image_reopen::fixture_semantic_image("src/older.rs").expect("older");
        let newer =
            super::super::image_reopen::fixture_semantic_image("src/newer.rs").expect("newer");
        assert_ne!(older, newer);
        let token = admission("pkg:cargo/fixture@1.0.0");
        let mut residence = ImageRowResidence::with_image_limit(1);
        let _ = open_compiled_image(&older, token, &mut residence).expect("older");
        confirm_image_admission(&older, token, &mut residence);
        let _ = open_compiled_image(&newer, token, &mut residence).expect("newer");
        confirm_image_admission(&newer, token, &mut residence);
        let again = open_compiled_image(&older, token, &mut residence).expect("evicted");
        assert!(
            matches!(again, CompiledImage::Opened { .. }),
            "the older image's facts stayed past the limit"
        );
        assert_eq!(residence.reopens(), 3);
        confirm_image_admission(&older, token, &mut residence);
        let kept = open_compiled_image(&older, token, &mut residence).expect("kept");
        assert!(matches!(kept, CompiledImage::Resident { .. }));
        let evicted_newer = open_compiled_image(&newer, token, &mut residence).expect("newer gone");
        assert!(matches!(evicted_newer, CompiledImage::Opened { .. }));
    }

    fn fixture_key(coordinate: &str) -> backend_engine::builtin::ProductSemanticPublicationKey {
        let package = backend_engine::PackageReference::parse("fixture").expect("package");
        let coordinate = backend_semantic::vocabulary::PackageUrl::parse(coordinate.to_owned())
            .expect("coordinate");
        backend_engine::builtin::ProductSemanticPublicationKey::new(
            package,
            coordinate,
            LanguageProfile::Rust(RustEdition::Rust2024),
        )
        .expect("publication key")
    }

    #[test]
    fn a_rejected_publication_key_is_not_remembered() {
        let fixture = fixture();
        let token = admission("pkg:cargo/fixture@1.0.0");
        let mut residence = ImageRowResidence::default();
        let opened = open_compiled_image(&fixture.bytes, token, &mut residence).expect("open");
        let CompiledImage::Opened { view, .. } = &opened else {
            panic!("the first open was already resident");
        };
        let foreign = fixture_key("pkg:cargo/other@1.0.0");
        let rejected = bind_opened_image(&fixture.bytes, token, view, &foreign, &mut residence);
        assert!(
            rejected
                .expect_err("foreign package")
                .to_string()
                .contains("bind semantic publication")
        );
        drop(opened);
        let again = open_compiled_image(&fixture.bytes, token, &mut residence).expect("again");
        assert!(matches!(again, CompiledImage::Opened { .. }));
        assert_eq!(residence.reopens(), 2);
    }

    #[test]
    fn evicting_image_facts_keeps_projected_rows() {
        let fixture = fixture();
        let image = SemanticImageView::reopen(&fixture.bytes).expect("reopen");
        let indexed = project("fixture");
        let mut residence = ImageRowResidence::with_image_limit(1);
        let mut symbols = BTreeSet::new();
        let primed = admit(
            &image,
            &indexed,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("project");
        let token = admission("pkg:cargo/fixture@1.0.0");
        let older =
            super::super::image_reopen::fixture_semantic_image("src/older.rs").expect("older");
        let newer =
            super::super::image_reopen::fixture_semantic_image("src/newer.rs").expect("newer");
        let _ = open_compiled_image(&older, token, &mut residence).expect("older");
        confirm_image_admission(&older, token, &mut residence);
        let _ = open_compiled_image(&newer, token, &mut residence).expect("newer");
        confirm_image_admission(&newer, token, &mut residence);
        let evicted = open_compiled_image(&older, token, &mut residence).expect("facts evicted");
        assert!(matches!(evicted, CompiledImage::Opened { .. }));
        let mut symbols = BTreeSet::new();
        let hit = admit(
            &image,
            &indexed,
            &fixture.initial,
            &mut residence,
            false,
            64,
            usize::MAX,
            &mut symbols,
            &[],
        )
        .expect("projection hit");
        assert_eq!(hit.rows, primed.rows);
        assert_eq!(
            residence.misses(),
            1,
            "fact eviction dropped the projection"
        );
        assert!(residence.hits() >= 1);
    }

    #[test]
    fn two_salted_images_share_one_symbol_set() {
        let fixture = fixture();
        let first = super::super::image_reopen::fixture_semantic_image_salted("src/a.rs", 0)
            .expect("salt 0");
        let second = super::super::image_reopen::fixture_semantic_image_salted("src/b.rs", 1)
            .expect("salt 1");
        assert_ne!(first, second);
        let images = vec![first, second];
        let key = fixture_key("pkg:cargo/fixture@1.0.0");
        let indexed = project("fixture");
        let mut residence = ImageRowResidence::default();
        let rows = publish_image_batch(&images, &key, &indexed, &fixture.initial, &mut residence);
        assert_eq!(rows, 8);
        assert_eq!(residence.reopens(), 2);
        let again = publish_image_batch(&images, &key, &indexed, &fixture.initial, &mut residence);
        assert_eq!(again, 8);
        assert_eq!(residence.reopens(), 2);
        assert_eq!(residence.hits(), 2);
    }

    #[test]
    fn admitting_an_image_twice_validates_once() {
        let fixture = fixture();
        let key = fixture_key("pkg:cargo/fixture@1.0.0");
        let mut residence = ImageRowResidence::default();
        admit_activated_images(&[fixture.bytes.clone()], &key, &mut residence).expect("cold");
        admit_activated_images(&[fixture.bytes.clone()], &key, &mut residence).expect("warm");
        assert_eq!(residence.reopens(), 1);
    }

    #[test]
    fn a_rejected_activation_keeps_the_admitted_image() {
        let fixture = fixture();
        let key = fixture_key("pkg:cargo/fixture@1.0.0");
        let foreign = fixture_key("pkg:cargo/other@1.0.0");
        let mut residence = ImageRowResidence::default();
        admit_activated_images(&[fixture.bytes.clone()], &key, &mut residence).expect("admit");
        let rejected = admit_activated_images(&[fixture.bytes.clone()], &foreign, &mut residence);
        assert!(
            rejected
                .expect_err("foreign package")
                .to_string()
                .contains("bind semantic publication")
        );
        assert_eq!(residence.reopens(), 2);
        admit_activated_images(&[fixture.bytes.clone()], &key, &mut residence).expect("kept");
        assert_eq!(residence.reopens(), 2);
        let rejected_again =
            admit_activated_images(&[fixture.bytes.clone()], &foreign, &mut residence);
        assert!(rejected_again.is_err(), "a rejected key was remembered");
        assert_eq!(residence.reopens(), 3);
    }
}
