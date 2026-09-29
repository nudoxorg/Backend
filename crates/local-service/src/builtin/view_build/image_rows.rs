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
use hashlink::LruCache;
use std::collections::{BTreeSet, HashSet, VecDeque};

/// Images retained at once. One publication walks every image of the edited
/// package; the bound is large enough for that pass, and the least recently
/// used entry leaves first.
const MAX_RESIDENT_IMAGES: usize = 4096;
/// Declaration rows retained across those images.
const MAX_RESIDENT_ROWS: usize = 65_536;
/// Estimated owned projection storage retained across those images.
///
/// This counts row/vector backing, owned text payloads, and per-projection
/// object headers. Allocator slack and residence map/queue metadata are
/// bounded separately by the image and row limits and are not included.
const MAX_RESIDENT_PROJECTION_BYTES: usize = 128 * 1024 * 1024;
/// Successfully admitted publication keys remembered per image before the
/// oldest admission is forgotten and must be checked again.
const MAX_ADMISSIONS_PER_IMAGE: usize = 64;

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

/// One projection held in the residence cache.
#[derive(Debug)]
struct ResidentProjection {
    image: ProjectedImage,
    rows: usize,
    bytes: usize,
}

/// Path and source identity proved for one image, and the publication keys
/// that have admitted those bytes.
#[derive(Debug)]
struct ImageByteFacts {
    path: String,
    identity: ContentId<SourceFactDomain>,
    admissions: VecDeque<[u8; 32]>,
}

/// Projected semantic rows reused across publications of the same image.
#[derive(Debug)]
pub(in crate::builtin) struct ImageRowResidence {
    image_limit: usize,
    row_limit: usize,
    byte_limit: usize,
    resident_rows: usize,
    resident_bytes: usize,
    entries: LruCache<[u8; 32], ResidentProjection>,
    facts: LruCache<[u8; 32], ImageByteFacts>,
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
        Self::with_budgets(image_limit, row_limit, MAX_RESIDENT_PROJECTION_BYTES)
    }

    fn with_budgets(image_limit: usize, row_limit: usize, byte_limit: usize) -> Self {
        let image_limit = image_limit.max(1);
        Self {
            image_limit,
            row_limit: row_limit.max(1),
            byte_limit,
            resident_rows: 0,
            resident_bytes: 0,
            entries: LruCache::new(image_limit),
            facts: LruCache::new(image_limit),
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

    fn retained_rows(&self) -> usize {
        self.resident_rows
    }

    fn retained_bytes(&self) -> usize {
        self.resident_bytes
    }

    /// Counts one validation of image bytes.
    pub(super) fn note_reopen(&mut self) {
        self.reopens = self.reopens.saturating_add(1);
    }

    fn reset_counts(&mut self) {
        self.hits = 0;
        self.misses = 0;
    }

    fn evict_to_fit(&mut self, rows: usize, bytes: usize) {
        while self.entries.len() >= self.image_limit
            || self.retained_rows().saturating_add(rows) > self.row_limit
            || self.retained_bytes().saturating_add(bytes) > self.byte_limit
        {
            let Some((_, oldest)) = self.entries.remove_lru() else {
                break;
            };
            self.resident_rows = self.resident_rows.saturating_sub(oldest.rows);
            self.resident_bytes = self.resident_bytes.saturating_sub(oldest.bytes);
        }
    }

    /// Borrows a projection while its cache entry cannot be evicted.
    ///
    /// The exclusive residence borrow ensures callers finish replay before a
    /// later insertion can mutate the LRU, so cache hits need no shared owner.
    fn cached(&mut self, key: [u8; 32]) -> Option<&ProjectedImage> {
        let projected = self.entries.get(&key)?;
        self.hits = self.hits.saturating_add(1);
        Some(&projected.image)
    }

    fn store(
        &mut self,
        key: [u8; 32],
        projected: ProjectedImage,
    ) -> Result<&ProjectedImage, ProjectedImage> {
        self.misses = self.misses.saturating_add(1);
        let rows = projected.rows.len();
        let bytes = projected_image_retained_bytes(&projected);
        if rows > self.row_limit || bytes > self.byte_limit {
            return Err(projected);
        }

        if let Some(previous) = self.entries.remove(&key) {
            self.resident_rows = self.resident_rows.saturating_sub(previous.rows);
            self.resident_bytes = self.resident_bytes.saturating_sub(previous.bytes);
        }
        self.evict_to_fit(rows, bytes);
        if self.entries.len() >= self.image_limit
            || self.retained_rows().saturating_add(rows) > self.row_limit
            || self.retained_bytes().saturating_add(bytes) > self.byte_limit
        {
            return Err(projected);
        }

        self.resident_rows = self.resident_rows.saturating_add(rows);
        self.resident_bytes = self.resident_bytes.saturating_add(bytes);
        self.entries.insert(
            key,
            ResidentProjection {
                image: projected,
                rows,
                bytes,
            },
        );
        let resident = self
            .entries
            .get(&key)
            .expect("inserted projection remains resident");
        Ok(&resident.image)
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
                    admissions: VecDeque::new(),
                },
            );
        }
    }

    fn confirm_admission(&mut self, digest: [u8; 32], admission: [u8; 32]) {
        let Some(facts) = self.facts.get_mut(&digest) else {
            return;
        };
        if facts.admissions.contains(&admission) {
            return;
        }
        if facts.admissions.len() == MAX_ADMISSIONS_PER_IMAGE {
            facts.admissions.pop_front();
        }
        facts.admissions.push_back(admission);
    }
}

fn projected_image_retained_bytes(projected: &ProjectedImage) -> usize {
    let mut shared_text = HashSet::new();
    let mut bytes = std::mem::size_of::<ResidentProjection>().saturating_add(
        projected
            .rows
            .capacity()
            .saturating_mul(std::mem::size_of::<ProjectedRow>()),
    );
    for projected_row in &projected.rows {
        bytes = bytes
            .saturating_add(
                projected_row
                    .charges
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Charge>()),
            )
            .saturating_add(row_retained_bytes(&projected_row.row, &mut shared_text));
    }
    bytes
}

/// Estimates storage owned by one projected row without cloning it.
fn row_retained_bytes(row: &Row, shared_text: &mut HashSet<usize>) -> usize {
    use backend_library::Fragment;

    let mut bytes = row.label.capacity();
    if let Some(preimage) = row.identity_preimage() {
        bytes = bytes.saturating_add(preimage.as_str().len());
    }
    bytes = bytes.saturating_add(
        row.document
            .len()
            .saturating_mul(std::mem::size_of::<Fragment>()),
    );
    for fragment in row.document.iter() {
        bytes = bytes.saturating_add(match fragment {
            Fragment::Text(text) | Fragment::Code(text) => text.capacity(),
            Fragment::Link { label, .. } => label.capacity(),
            Fragment::Break => 0,
        });
    }
    if let Some(signature) = row.signature.as_ref() {
        bytes = bytes.saturating_add(signature.capacity());
    }
    bytes = bytes.saturating_add(row.facts.text_bytes());
    if let Some(path) = row.source.file_path() {
        bytes = bytes.saturating_add(shared_text_retained_bytes(path, shared_text));
    }
    if let backend_compile::SourceExcerpt::Captured { text, .. } = &row.excerpt {
        bytes = bytes.saturating_add(shared_text_retained_bytes(text, shared_text));
    }
    bytes
}

/// Counts an `Arc<str>` allocation once when the same allocation is shared by
/// multiple cached rows. Its two-word refcount header is included.
fn shared_text_retained_bytes(text: &str, seen: &mut HashSet<usize>) -> usize {
    if seen.insert(text.as_ptr().addr()) {
        text.len().saturating_add(2 * std::mem::size_of::<usize>())
    } else {
        0
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
    if let Some(cached) = residence.cached(key) {
        return apply_projected_image(cached, sink);
    }
    let projected = project_image_rows(
        image,
        image_identity,
        project,
        profile,
        sink.initial.basis(),
        sink.stale,
        sink.path,
        sink.site_declarations,
    )?;
    match residence.store(key, projected) {
        Ok(cached) => apply_projected_image(cached, sink),
        Err(bypassed) => apply_projected_image(&bypassed, sink),
    }
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
        hash_source_excerpt(&mut hasher, site.source_excerpt());
        hash_declaration_facts(&mut hasher, site.facts());
        hasher.update(&[0xff]);
    }
    *hasher.finalize().as_bytes()
}

fn hash_text(hasher: &mut blake3::Hasher, text: &str) {
    // Every input in these source-site fields is bounded well below u32::MAX.
    let byte_len = u32::try_from(text.len()).unwrap_or(u32::MAX);
    hasher.update(&byte_len.to_le_bytes());
    hasher.update(text.as_bytes());
}

fn hash_optional_text(hasher: &mut blake3::Hasher, text: Option<&str>) {
    match text {
        Some(text) => {
            hasher.update(&[1]);
            hash_text(hasher, text);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

/// Hashes the complete explicit source-text state, including whether captured
/// text is complete or truncated.
fn hash_source_excerpt(hasher: &mut blake3::Hasher, excerpt: &backend_compile::SourceExcerpt) {
    use backend_compile::{SourceExcerpt, SourceExcerptExtent};

    match excerpt {
        SourceExcerpt::Captured { text, extent } => {
            hasher.update(&[
                0,
                match extent {
                    SourceExcerptExtent::Complete => 0,
                    SourceExcerptExtent::Truncated => 1,
                },
            ]);
            hash_text(hasher, text);
        }
        SourceExcerpt::NotCaptured => {
            hasher.update(&[1]);
        }
        SourceExcerpt::NotHydrated => {
            hasher.update(&[2]);
        }
        SourceExcerpt::Unconfigured => {
            hasher.update(&[3]);
        }
    }
}

/// Hashes every declaration fact with explicit tags and length-prefixed text.
fn hash_declaration_facts(hasher: &mut blake3::Hasher, facts: &backend_compile::DeclarationFacts) {
    use backend_compile::Fact;

    match &facts.deprecation {
        Fact::Unobserved => {
            hasher.update(&[0]);
        }
        Fact::Absent => {
            hasher.update(&[1]);
        }
        Fact::Present(notice) => {
            hasher.update(&[2]);
            hash_optional_text(hasher, notice.since());
            hash_optional_text(hasher, notice.note());
        }
    }
    match &facts.obligation {
        Fact::Unobserved => {
            hasher.update(&[0]);
        }
        Fact::Absent => {
            hasher.update(&[1]);
        }
        Fact::Present(obligation) => {
            hasher.update(&[2, obligation.wire_tag()]);
        }
    }
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
        /// Snapshot that owns `bytes`, when this open came from one.
        snapshot: Option<&'bytes backend_library::interface::SemanticImageSnapshot>,
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
    if let Some(resident) = resident_compiled_image(bytes, digest, admission, residence, None) {
        return Ok(resident);
    }
    let view = backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|error| {
        super::super::BuiltinModelError(format!("reopen activated semantic image: {error}"))
    })?;
    remember_opened_image(digest, view, residence, true)
}

/// Like [`open_compiled_image`], but a miss stores the proof on `image`.
pub(super) fn open_compiled_snapshot<'bytes>(
    image: &'bytes backend_library::interface::SemanticImageSnapshot,
    admission: [u8; 32],
    residence: &mut ImageRowResidence,
) -> Result<CompiledImage<'bytes>, super::super::BuiltinModelError> {
    let bytes = image.as_ref();
    let digest = image.content_digest();
    if let Some(resident) =
        resident_compiled_image(bytes, digest, admission, residence, Some(image))
    {
        return Ok(resident);
    }
    let before = backend_semantic::ir::semantic_image_validations();
    let view = image.reopen().map_err(|error| {
        super::super::BuiltinModelError(format!("reopen activated semantic image: {error}"))
    })?;
    let validated = backend_semantic::ir::semantic_image_validations() != before;
    remember_opened_image(digest, view, residence, validated)
}

fn resident_compiled_image<'bytes>(
    bytes: &'bytes [u8],
    digest: [u8; 32],
    admission: [u8; 32],
    residence: &mut ImageRowResidence,
    snapshot: Option<&'bytes backend_library::interface::SemanticImageSnapshot>,
) -> Option<CompiledImage<'bytes>> {
    let facts = residence.facts.get(&digest)?;
    if !facts.admissions.contains(&admission) {
        return None;
    }
    let path = facts.path.clone();
    let identity = facts.identity;
    Some(CompiledImage::Resident {
        bytes,
        path,
        identity,
        digest,
        snapshot,
    })
}

/// Reopens an admitted image for a row-cache miss.
///
/// A snapshot reuses the structural proof stored beside its bytes. Raw bytes
/// validate, and only that validation counts as a reopen.
pub(super) fn reopen_resident_image<'bytes>(
    bytes: &'bytes [u8],
    snapshot: Option<&'bytes backend_library::interface::SemanticImageSnapshot>,
    residence: &mut ImageRowResidence,
) -> Result<backend_semantic::ir::SemanticImageView<'bytes>, super::super::BuiltinModelError> {
    if let Some(image) = snapshot {
        let before = backend_semantic::ir::semantic_image_validations();
        let view = image.reopen().map_err(|error| {
            super::super::BuiltinModelError(format!("reopen activated semantic image: {error}"))
        })?;
        if backend_semantic::ir::semantic_image_validations() != before {
            residence.note_reopen();
        }
        return Ok(view);
    }
    let view = backend_semantic::ir::SemanticImageView::reopen(bytes).map_err(|error| {
        super::super::BuiltinModelError(format!("reopen activated semantic image: {error}"))
    })?;
    residence.note_reopen();
    Ok(view)
}

fn remember_opened_image<'bytes>(
    digest: [u8; 32],
    view: backend_semantic::ir::SemanticImageView<'bytes>,
    residence: &mut ImageRowResidence,
    validated: bool,
) -> Result<CompiledImage<'bytes>, super::super::BuiltinModelError> {
    let (path, identity) = compiled_source(&view)?;
    if validated {
        residence.note_reopen();
    }
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
    confirm_digest(image_digest(bytes), admission, residence);
}

fn confirm_digest(digest: [u8; 32], admission: [u8; 32], residence: &mut ImageRowResidence) {
    residence.confirm_admission(digest, admission);
}

/// Admits each image for `key`, reopening only when that key has not admitted it.
///
/// A resident admission returns without validating the bytes. A rejected key is
/// not recorded, so the next call validates again. An admission that already
/// succeeded stays in place.
pub(in crate::builtin) fn admit_activated_images(
    images: &[backend_library::interface::SemanticImageSnapshot],
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
        if let CompiledImage::Opened { view, .. } =
            open_compiled_snapshot(image, admission, residence)?
        {
            key.admit_image(&view).map_err(|error| {
                super::super::BuiltinModelError(format!(
                    "bind semantic publication to product key: {error}"
                ))
            })?;
            confirm_digest(image.content_digest(), admission, residence);
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
    apply_projected_image(projected, sink)?;
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
    let snapshots = admitted_snapshots(&images);
    for _ in 0..WARMUPS {
        let cold_snapshots = admitted_snapshots(&images);
        let mut residence = ImageRowResidence::default();
        admit_activated_images(&cold_snapshots, &key, &mut residence).expect("cold warmup");
    }
    let mut cold = Vec::with_capacity(SAMPLES);
    let mut cold_setup = Vec::with_capacity(SAMPLES);
    let mut cold_reopens = 0_u64;
    for _ in 0..SAMPLES {
        let setup_started = std::time::Instant::now();
        let cold_snapshots = admitted_snapshots(&images);
        cold_setup.push(setup_started.elapsed().as_nanos());
        let mut residence = ImageRowResidence::default();
        let started = std::time::Instant::now();
        admit_activated_images(&cold_snapshots, &key, &mut residence).expect("cold");
        cold.push(started.elapsed().as_nanos());
        cold_reopens += residence.reopens();
        std::hint::black_box(residence.reopens());
    }
    let mut warm_residence = ImageRowResidence::default();
    admit_activated_images(&snapshots, &key, &mut warm_residence).expect("warm prime");
    for _ in 0..WARMUPS {
        admit_activated_images(&snapshots, &key, &mut warm_residence).expect("warm warmup");
    }
    let reopens_before = warm_residence.reopens();
    let mut warm = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = std::time::Instant::now();
        admit_activated_images(&snapshots, &key, &mut warm_residence).expect("warm");
        warm.push(started.elapsed().as_nanos());
        std::hint::black_box(warm_residence.reopens());
    }
    let warm_reopens = warm_residence.reopens().saturating_sub(reopens_before);
    (cold_reopens == (IMAGES * SAMPLES) as u64 && warm_reopens == 0)
        .then_some(())
        .expect("admission cache validated a warm image");
    let (cold_median, cold_p95) = percentiles(&cold);
    let (cold_setup_median, _) = percentiles(&cold_setup);
    let (warm_median, warm_p95) = percentiles(&warm);
    println!(
        "semantic_admission images={IMAGES} bytes={bytes} cold_median_ns={cold_median} cold_p95_ns={cold_p95} cold_setup_median_ns={cold_setup_median} warm_median_ns={warm_median} warm_p95_ns={warm_p95} cold_reopens={cold_reopens} warm_reopens={warm_reopens}"
    );
}

/// Times validating a batch of snapshots against reusing their structural proofs.
///
/// Fresh snapshots are prepared outside each `cold` timer; construction cost is
/// reported separately. `warm` rebuilds views from proofs stored on one batch.
#[allow(clippy::expect_used, clippy::print_stdout)]
pub(super) fn measure_semantic_image_proof() {
    const IMAGES: usize = 32;
    const SAMPLES: usize = 32;
    const WARMUPS: usize = 4;
    let image_bytes = (0..IMAGES)
        .map(|index| {
            let path = format!("src/file{index}.rs");
            let salt = u8::try_from(index).expect("image index");
            super::image_reopen::fixture_semantic_image_salted(&path, salt).expect("fixture image")
        })
        .collect::<Vec<_>>();
    let snapshots = admitted_snapshots(&image_bytes);
    let bytes: usize = snapshots.iter().map(|image| image.as_ref().len()).sum();
    for _ in 0..WARMUPS {
        let cold_snapshots = admitted_snapshots(&image_bytes);
        let _ = reopen_snapshots(&cold_snapshots);
    }
    let mut cold = Vec::with_capacity(SAMPLES);
    let mut cold_setup = Vec::with_capacity(SAMPLES);
    let mut cold_validations = 0_u64;
    for _ in 0..SAMPLES {
        let setup_started = std::time::Instant::now();
        let cold_snapshots = admitted_snapshots(&image_bytes);
        cold_setup.push(setup_started.elapsed().as_nanos());
        backend_semantic::ir::reset_semantic_image_validations();
        let started = std::time::Instant::now();
        reopen_snapshots(&cold_snapshots);
        cold.push(started.elapsed().as_nanos());
        cold_validations += backend_semantic::ir::semantic_image_validations();
    }
    reopen_snapshots(&snapshots);
    for _ in 0..WARMUPS {
        reopen_snapshots(&snapshots);
    }
    backend_semantic::ir::reset_semantic_image_validations();
    let mut warm = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = std::time::Instant::now();
        reopen_snapshots(&snapshots);
        warm.push(started.elapsed().as_nanos());
    }
    let warm_validations = backend_semantic::ir::semantic_image_validations();
    (cold_validations == (IMAGES * SAMPLES) as u64 && warm_validations == 0)
        .then_some(())
        .expect("warm proof skipped validation");
    let (cold_median, cold_p95) = percentiles(&cold);
    let (cold_setup_median, _) = percentiles(&cold_setup);
    let (warm_median, warm_p95) = percentiles(&warm);
    println!(
        "semantic_image_proof images={IMAGES} bytes={bytes} cold_median_ns={cold_median} cold_p95_ns={cold_p95} cold_setup_median_ns={cold_setup_median} warm_median_ns={warm_median} warm_p95_ns={warm_p95} cold_validations={cold_validations} warm_validations={warm_validations}"
    );
}

/// Times admission and a row-cache miss against the snapshot's stored digest and proof.
///
/// The publication key is built before any timer. Fresh snapshots are prepared
/// outside each `cold` timer, with their construction cost reported separately.
/// `warm` finds the admission. `miss` rebuilds the view through the overlay helper.
#[allow(clippy::expect_used, clippy::print_stdout)]
pub(super) fn measure_semantic_snapshot_residence() {
    const IMAGES: usize = 32;
    const SAMPLES: usize = 32;
    const WARMUPS: usize = 4;
    let image_bytes = (0..IMAGES)
        .map(|index| {
            let path = format!("src/file{index}.rs");
            let salt = u8::try_from(index).expect("image index");
            super::image_reopen::fixture_semantic_image_salted(&path, salt).expect("fixture image")
        })
        .collect::<Vec<_>>();
    let snapshots = admitted_snapshots(&image_bytes);
    for image in &snapshots {
        let hashed = *blake3::hash(image.as_ref()).as_bytes();
        (hashed == image.content_digest())
            .then_some(())
            .expect("snapshot digest drifted from its bytes");
    }
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
    let bytes: usize = snapshots.iter().map(|image| image.as_ref().len()).sum();
    for _ in 0..WARMUPS {
        let cold_snapshots = admitted_snapshots(&image_bytes);
        let mut residence = ImageRowResidence::default();
        admit_activated_images(&cold_snapshots, &key, &mut residence).expect("cold warmup");
    }
    let mut cold = Vec::with_capacity(SAMPLES);
    let mut cold_setup = Vec::with_capacity(SAMPLES);
    let mut cold_reopens = 0_u64;
    for _ in 0..SAMPLES {
        let setup_started = std::time::Instant::now();
        let cold_snapshots = admitted_snapshots(&image_bytes);
        cold_setup.push(setup_started.elapsed().as_nanos());
        let mut residence = ImageRowResidence::default();
        let started = std::time::Instant::now();
        admit_activated_images(&cold_snapshots, &key, &mut residence).expect("cold");
        cold.push(started.elapsed().as_nanos());
        cold_reopens += residence.reopens();
        std::hint::black_box(residence.reopens());
    }
    let mut warm_residence = ImageRowResidence::default();
    admit_activated_images(&snapshots, &key, &mut warm_residence).expect("warm prime");
    for _ in 0..WARMUPS {
        admit_activated_images(&snapshots, &key, &mut warm_residence).expect("warm warmup");
    }
    let reopens_before = warm_residence.reopens();
    let mut warm = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let started = std::time::Instant::now();
        admit_activated_images(&snapshots, &key, &mut warm_residence).expect("warm");
        warm.push(started.elapsed().as_nanos());
        std::hint::black_box(warm_residence.reopens());
    }
    let warm_reopens = warm_residence.reopens().saturating_sub(reopens_before);
    (cold_reopens == (IMAGES * SAMPLES) as u64 && warm_reopens == 0)
        .then_some(())
        .expect("snapshot residence hashed or validated a warm image");
    let mut miss_residence = ImageRowResidence::default();
    for _ in 0..WARMUPS {
        reopen_resident_batch(&snapshots, &mut miss_residence);
    }
    let mut miss = Vec::with_capacity(SAMPLES);
    let mut miss_validations = 0_u64;
    for _ in 0..SAMPLES {
        backend_semantic::ir::reset_semantic_image_validations();
        let started = std::time::Instant::now();
        reopen_resident_batch(&snapshots, &mut miss_residence);
        miss.push(started.elapsed().as_nanos());
        miss_validations += backend_semantic::ir::semantic_image_validations();
    }
    (miss_validations == 0)
        .then_some(())
        .expect("overlay miss validated a proved snapshot");
    let (cold_median, cold_p95) = percentiles(&cold);
    let (cold_setup_median, _) = percentiles(&cold_setup);
    let (warm_median, warm_p95) = percentiles(&warm);
    let (miss_median, miss_p95) = percentiles(&miss);
    println!(
        "semantic_snapshot_residence images={IMAGES} bytes={bytes} cold_median_ns={cold_median} cold_p95_ns={cold_p95} cold_setup_median_ns={cold_setup_median} warm_median_ns={warm_median} warm_p95_ns={warm_p95} miss_median_ns={miss_median} miss_p95_ns={miss_p95} cold_reopens={cold_reopens} warm_reopens={warm_reopens} miss_validations={miss_validations}"
    );
}

#[allow(clippy::expect_used)]
fn reopen_resident_batch(
    images: &[backend_library::interface::SemanticImageSnapshot],
    residence: &mut ImageRowResidence,
) {
    for image in images {
        let view = reopen_resident_image(image.as_ref(), Some(image), residence).expect("overlay");
        std::hint::black_box(view.as_ref().len());
    }
}

#[allow(clippy::expect_used)]
fn reopen_snapshots(images: &[backend_library::interface::SemanticImageSnapshot]) {
    for image in images {
        let view = image.reopen().expect("reopen snapshot");
        std::hint::black_box(view.as_ref().len());
    }
}

#[allow(clippy::expect_used)]
fn admitted_snapshot(bytes: &[u8]) -> backend_library::interface::SemanticImageSnapshot {
    let authority = backend_library::interface::SemanticImageAuthority {
        identity: backend_version::ArtifactId::<
            backend_version::IrSemanticImageEncoding,
            backend_version::IrSemanticImageDomain,
        >::from_encoded_bytes(bytes),
        byte_len: u32::try_from(bytes.len()).expect("snapshot length"),
    };
    backend_library::interface::SemanticImageSnapshot::try_from_reopened(authority, bytes)
        .expect("snapshot")
}

#[allow(clippy::expect_used)]
fn admitted_snapshots(
    images: &[Vec<u8>],
) -> Vec<backend_library::interface::SemanticImageSnapshot> {
    images
        .iter()
        .map(|image| admitted_snapshot(image))
        .collect()
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
        files: std::sync::Arc::<[[u8; 32]]>::from([]),
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
        open_compiled_snapshot, publication_admission, publish_image_batch, reopen_resident_image,
    };
    use backend_engine::{Row, RowId};
    use backend_semantic::ir::SemanticImageView;
    use backend_semantic::vocabulary::{LanguageProfile, RustEdition};
    use backend_version::{ContentId, SourceFactDomain};
    use std::collections::BTreeSet;
    use std::sync::{Arc, Barrier};

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

    fn source_aware_fixture_bytes() -> Vec<u8> {
        use backend_semantic::ir::{
            BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts,
            EntityVersion, FactAvailability, IrBuilder, ItemKind, ParentageAuthority,
            SourceIdentity, SourceSpan, TreeItemInput, VariantFingerprint, Visibility,
            encode_full_semantic_image, full_semantic_image_len,
        };
        use backend_semantic::vocabulary::{
            CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
        };

        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"source-aware-row"),
            byte_len: 13,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<backend_version::ToolchainDomain>::from_canonical_bytes(
                b"source-aware-toolchain",
            ),
        );
        let coordinate =
            PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned()).expect("fixture coordinate");
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(source, recipe, &coordinate, PATH)
            .expect("fixture provenance");
        let source_file = builder
            .intern_atom(PATH.as_bytes())
            .expect("source file atom");
        let versions = [EntityVersion {
            family: DeclarationFamilyId::from_raw([1; 16]),
            variant: VariantFingerprint::from_raw([2; 16]),
            core_payload: CorePayloadHash::from_raw([3; 16]),
        }];
        let items = [TreeItemInput {
            name: b"Worker",
            kind: ItemKind::Record,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                source: FactAvailability::Captured,
                source_file: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: SourceSpan::new(source_file, 0, 13),
            extension: None,
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &[],
            })
            .expect("fixture tree");
        let ir = builder.finish().expect("fixture image");
        let mut bytes = vec![0; full_semantic_image_len(&ir).expect("image length")];
        encode_full_semantic_image(&ir, &mut bytes).expect("encode image");
        bytes
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

    fn direct_rows_with_sites(
        image: &SemanticImageView<'_>,
        project: &super::super::super::IndexedProject,
        initial: &backend_engine::ViewRoot,
        sites: &[&backend_compile::SourceDeclaration],
    ) -> Vec<Row> {
        let projected = super::project_image_rows(
            image,
            super::image_digest(image.as_ref()),
            project,
            LanguageProfile::Rust(RustEdition::Rust2024),
            initial.basis(),
            false,
            PATH,
            sites,
        )
        .expect("direct projection");
        let mut symbols = BTreeSet::new();
        let mut rows = Vec::new();
        let mut remaining = usize::MAX;
        let mut sink = SemanticRowSink {
            initial,
            symbols: &mut symbols,
            rows: &mut rows,
            capacity: 64,
            remaining_bytes: &mut remaining,
            stale: false,
            path: PATH,
            site_declarations: sites,
        };
        super::apply_projected_image(&projected, &mut sink).expect("apply direct projection");
        rows
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
    fn structural_site_facts_change_the_cached_rows_and_match_direct_projection() {
        use backend_compile::{DeclarationFacts, Fact, Obligation};

        let fixture = fixture();
        let bytes = source_aware_fixture_bytes();
        let image = SemanticImageView::reopen(&bytes).expect("reopen source-aware image");
        let project = project("fixture");
        let excerpt = backend_compile::SourceExcerpt::captured(
            "struct Worker",
            backend_compile::SourceExcerptExtent::Complete,
        )
        .expect("excerpt");
        let unobserved = backend_compile::SourceDeclaration::at_path(
            PATH,
            "Worker",
            backend_engine::DeclarationKind::Struct,
            2,
            "struct Worker",
            "",
        )
        .expect("unobserved site")
        .with_facts(DeclarationFacts::UNOBSERVED)
        .with_source_excerpt(excerpt.clone());
        let observed = backend_compile::SourceDeclaration::at_path(
            PATH,
            "Worker",
            backend_engine::DeclarationKind::Struct,
            2,
            "struct Worker",
            "",
        )
        .expect("observed site")
        .with_facts(DeclarationFacts {
            deprecation: Fact::Unobserved,
            obligation: Fact::Present(Obligation::Required),
        })
        .with_source_excerpt(excerpt);
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
            &[&unobserved],
        )
        .expect("unobserved projection");
        let first_worker = first
            .rows
            .iter()
            .find(|row| row.kind == Some(backend_engine::DeclarationKind::Struct))
            .expect("worker row");
        assert_eq!(first_worker.facts, DeclarationFacts::UNOBSERVED);

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
            &[&observed],
        )
        .expect("observed projection");
        assert_eq!(
            second.rows,
            direct_rows_with_sites(&image, &project, &fixture.initial, &[&observed])
        );
        let second_worker = second
            .rows
            .iter()
            .find(|row| row.kind == Some(backend_engine::DeclarationKind::Struct))
            .expect("worker row");
        assert_eq!(
            second_worker.facts.obligation,
            Fact::Present(Obligation::Required)
        );
        assert_eq!(residence.misses(), 2);
        assert_eq!(residence.hits(), 0);
    }

    #[test]
    fn structural_site_excerpt_state_and_extent_are_part_of_the_cache_key() {
        use backend_compile::{DeclarationFacts, SourceExcerpt, SourceExcerptExtent};

        let fixture = fixture();
        let bytes = source_aware_fixture_bytes();
        let image = SemanticImageView::reopen(&bytes).expect("reopen source-aware image");
        let project = project("fixture");
        let excerpts = [
            SourceExcerpt::captured("struct Worker", SourceExcerptExtent::Complete)
                .expect("complete excerpt"),
            SourceExcerpt::captured("struct Worker", SourceExcerptExtent::Truncated)
                .expect("truncated excerpt"),
            SourceExcerpt::NotCaptured,
            SourceExcerpt::NotHydrated,
            SourceExcerpt::Unconfigured,
        ];
        let mut residence = ImageRowResidence::default();

        for excerpt in &excerpts {
            let site = backend_compile::SourceDeclaration::at_path(
                PATH,
                "Worker",
                backend_engine::DeclarationKind::Struct,
                2,
                "struct Worker",
                "",
            )
            .expect("site")
            .with_facts(DeclarationFacts::UNOBSERVED)
            .with_source_excerpt(excerpt.clone());
            let mut symbols = BTreeSet::new();
            let admitted = admit(
                &image,
                &project,
                &fixture.initial,
                &mut residence,
                false,
                64,
                usize::MAX,
                &mut symbols,
                &[&site],
            )
            .expect("admit excerpt");
            assert_eq!(
                admitted.rows,
                direct_rows_with_sites(&image, &project, &fixture.initial, &[&site])
            );
            let worker = admitted
                .rows
                .iter()
                .find(|row| row.kind == Some(backend_engine::DeclarationKind::Struct))
                .expect("worker row");
            assert_eq!(&worker.excerpt, excerpt);
        }

        assert_eq!(residence.misses(), excerpts.len() as u64);
        assert_eq!(residence.hits(), 0);
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
        assert!(
            residence
                .store([9_u8; 32], ProjectedImage { rows })
                .is_err()
        );
        assert_eq!(residence.len(), 0, "one image past the row budget leaves");
        assert_eq!(residence.misses(), 1);
        let label = "kept";
        let _ = residence
            .store(
                [8_u8; 32],
                ProjectedImage {
                    rows: vec![ProjectedRow {
                        row: Row::new(
                            RowId::Symbol(backend_engine::symbol_key(label)),
                            fixture.initial.basis(),
                            label,
                        ),
                        duplicate: DuplicatePolicy::Error,
                        charges: vec![Charge::Sub(1)],
                    }],
                },
            )
            .expect("image inside the row budget");
        assert_eq!(residence.len(), 1, "an image inside the budget stays");
    }

    fn one_projected_row(label: &str, basis: backend_engine::Basis) -> ProjectedImage {
        ProjectedImage {
            rows: vec![ProjectedRow {
                row: Row::new(
                    RowId::Symbol(backend_engine::symbol_key(label)),
                    basis,
                    label,
                )
                .with_signature(format!("signature for {label}")),
                duplicate: DuplicatePolicy::Error,
                charges: vec![Charge::Sub(1)],
            }],
        }
    }

    #[test]
    fn a_projection_hit_promotes_it_before_lru_eviction() {
        let fixture = fixture();
        let oldest_key = [0_u8; 32];
        let touched_key = [1_u8; 32];
        let newest_key = [2_u8; 32];
        let mut residence = ImageRowResidence::with_budgets(2, 16, 1_000_000);

        let _ = residence
            .store(
                oldest_key,
                one_projected_row("oldest", fixture.initial.basis()),
            )
            .expect("oldest projection fits");
        let _ = residence
            .store(
                touched_key,
                one_projected_row("touched", fixture.initial.basis()),
            )
            .expect("touched projection fits");
        let _ = residence
            .cached(oldest_key)
            .expect("cache hit promotes the oldest entry");
        let _ = residence
            .store(
                newest_key,
                one_projected_row("newest", fixture.initial.basis()),
            )
            .expect("newest projection fits after LRU eviction");

        assert!(residence.entries.contains_key(&oldest_key));
        assert!(!residence.entries.contains_key(&touched_key));
        assert!(residence.entries.contains_key(&newest_key));
    }

    #[test]
    fn the_weighted_budget_evicts_old_projections_before_admitting_new_rows() {
        let fixture = fixture();
        let first_key = [1_u8; 32];
        let second_key = [2_u8; 32];
        let first = one_projected_row("first", fixture.initial.basis());
        let second = one_projected_row("a longer second label", fixture.initial.basis());
        let first_bytes = super::projected_image_retained_bytes(&first);
        let second_bytes = super::projected_image_retained_bytes(&second);
        let byte_limit = first_bytes.max(second_bytes);
        let mut residence = ImageRowResidence::with_budgets(4, 16, byte_limit);

        let _ = residence
            .store(first_key, first)
            .expect("first projection fits");
        let _ = residence
            .store(second_key, second)
            .expect("second projection fits after eviction");

        assert_eq!(residence.len(), 1);
        assert!(!residence.entries.contains_key(&first_key));
        assert!(residence.entries.contains_key(&second_key));
        assert_eq!(residence.retained_rows(), 1);
        assert_eq!(residence.retained_bytes(), second_bytes);
    }

    #[test]
    fn an_oversized_projection_bypasses_the_cache_and_keeps_small_rows() {
        let fixture = fixture();
        let small_key = [3_u8; 32];
        let oversized_key = [4_u8; 32];
        let small = one_projected_row("small", fixture.initial.basis());
        let byte_limit = super::projected_image_retained_bytes(&small);
        let oversized = one_projected_row(&"large".repeat(1024), fixture.initial.basis());
        assert!(super::projected_image_retained_bytes(&oversized) > byte_limit);
        let mut residence = ImageRowResidence::with_budgets(4, 16, byte_limit);

        let _ = residence
            .store(small_key, small)
            .expect("small projection fits");
        let bypassed = residence
            .store(oversized_key, oversized)
            .expect_err("oversized projection must bypass retention");

        assert_eq!(bypassed.rows.len(), 1);
        assert_eq!(residence.len(), 1);
        assert!(residence.entries.contains_key(&small_key));
        assert!(!residence.entries.contains_key(&oversized_key));
        assert_eq!(residence.retained_bytes(), byte_limit);
        assert_eq!(residence.misses(), 2);
    }

    #[test]
    fn borrowed_projection_replays_exactly_before_a_later_eviction() {
        let fixture = fixture();
        let first_key = [5_u8; 32];
        let second_key = [6_u8; 32];
        let mut first = one_projected_row("first", fixture.initial.basis());
        first.rows.push(
            one_projected_row("second", fixture.initial.basis())
                .rows
                .remove(0),
        );
        let expected = first.clone();
        let second = one_projected_row("replacement", fixture.initial.basis());
        let first_bytes = super::projected_image_retained_bytes(&first);
        let second_bytes = super::projected_image_retained_bytes(&second);
        let byte_limit = first_bytes.max(second_bytes);
        let mut residence = ImageRowResidence::with_budgets(4, 16, byte_limit);
        let _ = residence.store(first_key, first).expect("first projection");

        let replayed = {
            let cached = residence.cached(first_key).expect("borrowed cache hit");
            replay_projected_image(cached, &fixture.initial)
        };
        let direct = replay_projected_image(&expected, &fixture.initial);
        assert_eq!(
            replayed.0, direct.0,
            "cached replay preserves row order and content"
        );
        assert_eq!(replayed.1, direct.1, "cached replay preserves byte charges");

        let _ = residence
            .store(second_key, second)
            .expect("the finished borrow permits a later LRU mutation");
        assert!(residence.entries.contains_key(&second_key));
        assert!(!residence.entries.contains_key(&first_key));
        assert_eq!(residence.retained_rows(), 1);
        assert_eq!(residence.retained_bytes(), second_bytes);
    }

    fn replay_projected_image(
        projected: &ProjectedImage,
        initial: &backend_engine::ViewRoot,
    ) -> (Vec<Row>, usize) {
        let mut symbols = BTreeSet::new();
        let mut rows = Vec::new();
        let mut remaining_bytes = 100;
        let mut sink = SemanticRowSink {
            initial,
            symbols: &mut symbols,
            rows: &mut rows,
            capacity: 8,
            remaining_bytes: &mut remaining_bytes,
            stale: false,
            path: PATH,
            site_declarations: &[],
        };
        apply_projected_image(projected, &mut sink).expect("replay projected rows");
        (rows, remaining_bytes)
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
    fn old_admissions_are_forgotten_after_the_per_image_limit() {
        let fixture = fixture();
        let digest = super::image_digest(&fixture.bytes);
        let admissions = (0..=super::MAX_ADMISSIONS_PER_IMAGE)
            .map(|version| admission(&format!("pkg:cargo/fixture@{version}.0.0")))
            .collect::<Vec<_>>();
        let oldest = *admissions.first().expect("oldest admission");
        let newest = *admissions.last().expect("newest admission");
        let mut residence = ImageRowResidence::with_image_limit(1);

        let opened = open_compiled_image(&fixture.bytes, oldest, &mut residence).expect("open");
        assert!(matches!(opened, CompiledImage::Opened { .. }));
        confirm_image_admission(&fixture.bytes, oldest, &mut residence);
        for admission in admissions.iter().skip(1) {
            confirm_image_admission(&fixture.bytes, *admission, &mut residence);
        }

        let facts = residence.facts.get(&digest).expect("retained image facts");
        assert_eq!(facts.admissions.len(), super::MAX_ADMISSIONS_PER_IMAGE);
        assert!(!facts.admissions.contains(&oldest));
        assert!(facts.admissions.contains(&newest));

        let before = residence.reopens();
        let forgotten =
            open_compiled_image(&fixture.bytes, oldest, &mut residence).expect("recheck");
        assert!(matches!(forgotten, CompiledImage::Opened { .. }));
        assert_eq!(residence.reopens(), before + 1);
        confirm_image_admission(&fixture.bytes, oldest, &mut residence);
        let retained = open_compiled_image(&fixture.bytes, newest, &mut residence).expect("recent");
        assert!(matches!(retained, CompiledImage::Resident { .. }));
        assert_eq!(residence.reopens(), before + 1);
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
        admit_activated_images(
            &[super::admitted_snapshot(&fixture.bytes)],
            &key,
            &mut residence,
        )
        .expect("cold");
        admit_activated_images(
            &[super::admitted_snapshot(&fixture.bytes)],
            &key,
            &mut residence,
        )
        .expect("warm");
        assert_eq!(residence.reopens(), 1);
    }

    #[test]
    fn a_rejected_activation_keeps_the_admitted_image() {
        let fixture = fixture();
        let key = fixture_key("pkg:cargo/fixture@1.0.0");
        let foreign = fixture_key("pkg:cargo/other@1.0.0");
        let mut residence = ImageRowResidence::default();
        admit_activated_images(
            &[super::admitted_snapshot(&fixture.bytes)],
            &key,
            &mut residence,
        )
        .expect("admit");
        let rejected = admit_activated_images(
            &[super::admitted_snapshot(&fixture.bytes)],
            &foreign,
            &mut residence,
        );
        assert!(
            rejected
                .expect_err("foreign package")
                .to_string()
                .contains("bind semantic publication")
        );
        assert_eq!(residence.reopens(), 2);
        admit_activated_images(
            &[super::admitted_snapshot(&fixture.bytes)],
            &key,
            &mut residence,
        )
        .expect("kept");
        assert_eq!(residence.reopens(), 2);
        let rejected_again = admit_activated_images(
            &[super::admitted_snapshot(&fixture.bytes)],
            &foreign,
            &mut residence,
        );
        assert!(rejected_again.is_err(), "a rejected key was remembered");
        assert_eq!(residence.reopens(), 3);
    }

    #[test]
    fn a_second_snapshot_reopen_does_not_validate() {
        let fixture = fixture();
        let image = super::admitted_snapshot(&fixture.bytes);
        backend_semantic::ir::reset_semantic_image_validations();
        let first = image.reopen().expect("cold");
        let first_names = declaration_identities(&first);
        assert_eq!(backend_semantic::ir::semantic_image_validations(), 1);
        let second = image.reopen().expect("warm");
        assert_eq!(backend_semantic::ir::semantic_image_validations(), 1);
        assert_eq!(declaration_identities(&second), first_names);
    }

    #[test]
    fn concurrent_snapshot_misses_validate_once_and_return_the_same_image() {
        const READERS: usize = 8;
        let fixture = fixture();
        let image = super::admitted_snapshot(&fixture.bytes);
        let barrier = Arc::new(Barrier::new(READERS));
        let observed = std::thread::scope(|scope| {
            let image = &image;
            let readers = (0..READERS)
                .map(|_| {
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        backend_semantic::ir::reset_semantic_image_validations();
                        barrier.wait();
                        let view = image.reopen().expect("parallel reopen");
                        (
                            declaration_identities(&view),
                            view.as_ref().to_vec(),
                            backend_semantic::ir::semantic_image_validations(),
                        )
                    })
                })
                .collect::<Vec<_>>();
            readers
                .into_iter()
                .map(|reader| reader.join().expect("reader thread"))
                .collect::<Vec<_>>()
        });

        let (expected_names, expected_bytes, _) = &observed[0];
        assert!(
            observed
                .iter()
                .all(|(names, bytes, _)| { names == expected_names && bytes == expected_bytes })
        );
        assert_eq!(
            observed
                .iter()
                .map(|(_, _, validations)| *validations)
                .sum::<u64>(),
            1
        );
    }

    #[test]
    fn a_rejected_snapshot_is_not_remembered() {
        let bytes = b"not-a-semantic-image".to_vec();
        let image = super::admitted_snapshot(&bytes);
        backend_semantic::ir::reset_semantic_image_validations();
        let first_error = match image.reopen() {
            Err(error) => error.to_string(),
            Ok(_) => panic!("corrupt bytes were admitted"),
        };
        assert_eq!(backend_semantic::ir::semantic_image_validations(), 1);
        let second_error = match image.reopen() {
            Err(error) => error.to_string(),
            Ok(_) => panic!("a failed proof was remembered"),
        };
        assert_eq!(second_error, first_error, "grammar error changed on retry");
        assert_eq!(backend_semantic::ir::semantic_image_validations(), 2);
    }

    #[test]
    fn a_second_snapshot_of_the_same_bytes_validates_again() {
        let fixture = fixture();
        let first = super::admitted_snapshot(&fixture.bytes);
        let second = super::admitted_snapshot(&fixture.bytes);
        backend_semantic::ir::reset_semantic_image_validations();
        let left = first.reopen().expect("first");
        let right = second.reopen().expect("second");
        assert_eq!(backend_semantic::ir::semantic_image_validations(), 2);
        assert_eq!(
            declaration_identities(&left),
            declaration_identities(&right)
        );
    }

    #[test]
    fn a_snapshot_digest_matches_its_bytes_after_authority_changes() {
        let fixture = fixture();
        let mut image = super::admitted_snapshot(&fixture.bytes);
        let digest = *blake3::hash(fixture.bytes.as_slice()).as_bytes();
        assert_eq!(image.content_digest(), digest);
        image.authority.byte_len = 0;
        assert_eq!(image.content_digest(), digest);
        image.authority.byte_len = u32::try_from(fixture.bytes.len()).expect("length");
        let view = image.reopen().expect("reopen");
        assert_eq!(view.as_ref(), fixture.bytes.as_slice());
    }

    #[test]
    fn a_stale_overlay_reuses_the_snapshot_proof() {
        let fixture = fixture();
        let snapshot = super::admitted_snapshot(&fixture.bytes);
        let project = project("fixture");
        let admission = admission("pkg:cargo/fixture@1.0.0");
        let mut residence = ImageRowResidence::default();
        let opened = open_compiled_snapshot(&snapshot, admission, &mut residence).expect("open");
        let CompiledImage::Opened { view, .. } = &opened else {
            panic!("first snapshot open was resident");
        };
        confirm_image_admission(&fixture.bytes, admission, &mut residence);
        let mut symbols = BTreeSet::new();
        let fresh = admit(
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
        .expect("fresh project");
        drop(opened);
        let resident = open_compiled_snapshot(&snapshot, admission, &mut residence).expect("hit");
        let CompiledImage::Resident {
            bytes,
            digest,
            snapshot: Some(image),
            ..
        } = resident
        else {
            panic!("confirmed snapshot was validated again");
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
        assert!(!applied, "stale overlay reused fresh rows");
        let reopens = residence.reopens();
        backend_semantic::ir::reset_semantic_image_validations();
        let proved = reopen_resident_image(bytes, Some(image), &mut residence).expect("proof");
        assert_eq!(backend_semantic::ir::semantic_image_validations(), 0);
        assert_eq!(residence.reopens(), reopens);
        let mut symbols = BTreeSet::new();
        let stale = admit(
            &proved,
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
        assert_eq!(stale.rows.len(), fresh.rows.len());
        backend_semantic::ir::reset_semantic_image_validations();
        let _ = reopen_resident_image(snapshot.as_ref(), Some(&snapshot), &mut residence)
            .expect("retained proof");
        assert_eq!(backend_semantic::ir::semantic_image_validations(), 0);
        assert_eq!(residence.reopens(), reopens);
    }

    fn declaration_identities(
        image: &SemanticImageView<'_>,
    ) -> Vec<backend_semantic::ir::DeclarationIdentity> {
        let session = backend_engine::application::DocumentationSession::new(image);
        session
            .canonical_entities()
            .map(|entity| entity.expect("entity").entity.version.identity())
            .collect()
    }
}
