//! Source sites proven from a selected native frontier and exact Clang name spans.
//!
//! This is independent of structural declaration correlation. It neither
//! creates structural facts nor permits missing native evidence to become a site.

use super::super::BuiltinModelError;
use backend_engine::application::DocumentationSession;
use backend_engine::builtin::VerifiedNativeSource;
use backend_semantic::ir::{
    DeclarationIdentity, ImageProvenance, ItemKind, SemanticCoreReader as _, SemanticImageView,
    SemanticReader as _,
};
use backend_semantic::vocabulary::{LanguageProfile, NativeTool, Stage};
use std::collections::{BTreeMap, BTreeSet};

// Covers per-candidate map/set entries and the final index. The fixed
// reservation also covers minimum B-tree node allocations for sparse maps.
const SITE_WORKING_BYTES: usize = 768;
const SITE_WORKING_BASE_BYTES: usize = 4096;

pub(super) struct NativeProjection<'source> {
    pub(super) source: &'source VerifiedNativeSource,
    pub(super) max_sites: usize,
    pub(super) max_working_bytes: usize,
}

pub(super) struct NativeSite<'source> {
    source: &'source VerifiedNativeSource,
    pub(super) line: u32,
    start: usize,
    end: usize,
}

impl NativeSite<'_> {
    pub(super) fn excerpt(&self) -> backend_compile::SourceExcerpt {
        // Construction checked UTF-8 boundaries against this immutable source.
        std::str::from_utf8(&self.source.bytes()[self.start..self.end])
            .map(backend_compile::SourceExcerpt::capture_bounded)
            .unwrap_or(backend_compile::SourceExcerpt::NotCaptured)
    }
}

pub(super) struct NativeSites<'source> {
    sites: BTreeMap<DeclarationIdentity, NativeSite<'source>>,
    pub(super) working_bytes: usize,
}

impl NativeSites<'_> {
    pub(super) fn get(&self, identity: &DeclarationIdentity) -> Option<&NativeSite<'_>> {
        self.sites.get(identity)
    }
}

/// Produces no sites when image provenance differs from the selected source.
/// Shared/ambiguous name spans suppress all claimants, rather than choosing one.
/// The caller reserves `working_bytes` while projecting rows and refunds it
/// after this bounded temporary index and source buffer have been dropped.
pub(super) fn prove_native_sites<'source>(
    image: &SemanticImageView<'_>,
    project: [u8; 32],
    source: &'source VerifiedNativeSource,
    max_sites: usize,
    max_working_bytes: usize,
) -> Result<NativeSites<'source>, BuiltinModelError> {
    let source_bytes = source.bytes().len();
    if source_bytes > max_working_bytes {
        return Err(BuiltinModelError(
            "native site source exceeds projection working budget".to_owned(),
        ));
    }
    let empty = || NativeSites {
        sites: BTreeMap::new(),
        working_bytes: source_bytes,
    };
    if source.project() != project
        || !matches!(
            source.profile(),
            LanguageProfile::C(_) | LanguageProfile::Cxx(_)
        )
    {
        return Ok(empty());
    }
    let ImageProvenance::Captured {
        source: captured,
        recipe,
        scope,
        ..
    } = image.image_facts().provenance
    else {
        return Ok(empty());
    };
    if captured.identity != source.source_identity()
        || usize::try_from(captured.byte_len).ok() != Some(source.bytes().len())
        || recipe.profile != source.profile()
        || recipe.tool != NativeTool::Clang
        || recipe.stage != Stage::LowerIr
        || image.atom(scope.path) != Some(source.relative_path().as_bytes())
    {
        return Ok(empty());
    }
    let text = std::str::from_utf8(source.bytes())
        .map_err(|_| BuiltinModelError("verified native source lost UTF-8 authority".to_owned()))?;
    let mut spans =
        BTreeMap::<(u32, u32), Option<(DeclarationIdentity, NativeSite<'source>)>>::new();
    let mut identities = BTreeSet::new();
    let mut candidates = 0usize;
    let mut duplicates = BTreeSet::new();
    let mut working_bytes = source_bytes;
    let session = DocumentationSession::new(image);
    for row in session.canonical_entities() {
        let row =
            row.map_err(|error| BuiltinModelError(format!("native site declaration: {error}")))?;
        if matches!(
            row.entity.kind,
            ItemKind::Implementation | ItemKind::Parameter
        ) {
            continue;
        }
        let Some(name) = row.name.named_bytes() else {
            continue;
        };
        let Some(extent) = row.entity.source else {
            continue;
        };
        let Some(name_span) = image
            .clang_extension(row.entity.id)
            .and_then(|facts| facts.declared_name)
        else {
            continue;
        };
        if name_span.file() != extent.file()
            || image.atom(name_span.file()) != Some(source.relative_path().as_bytes())
            || name_span.start() < extent.start()
            || name_span.start() >= name_span.end()
            || name_span.end() > extent.end()
        {
            continue;
        }
        let (Ok(start), Ok(end), Ok(name_start), Ok(name_end)) = (
            usize::try_from(extent.start()),
            usize::try_from(extent.end()),
            usize::try_from(name_span.start()),
            usize::try_from(name_span.end()),
        ) else {
            continue;
        };
        if source.bytes().get(name_start..name_end) != Some(name)
            || text.get(start..end).is_none()
            || text.get(name_start..name_end).is_none()
        {
            continue;
        }
        candidates = candidates
            .checked_add(1)
            .filter(|count| *count <= max_sites)
            .ok_or_else(|| {
                BuiltinModelError("native site count exceeds projection capacity".to_owned())
            })?;
        if candidates == 1 {
            working_bytes = working_bytes
                .checked_add(SITE_WORKING_BASE_BYTES)
                .filter(|bytes| *bytes <= max_working_bytes)
                .ok_or_else(|| {
                    BuiltinModelError(
                        "native site maps exceed projection working budget".to_owned(),
                    )
                })?;
        }
        working_bytes = working_bytes
            .checked_add(SITE_WORKING_BYTES)
            .filter(|bytes| *bytes <= max_working_bytes)
            .ok_or_else(|| {
                BuiltinModelError("native sites exceed projection working budget".to_owned())
            })?;
        let identity = row.entity.version.identity();
        if !identities.insert(identity) {
            duplicates.insert(identity);
        }
        let key = (name_span.start(), name_span.end());
        if let Some(slot) = spans.get_mut(&key) {
            *slot = None;
            continue;
        }
        spans.insert(
            key,
            Some((
                identity,
                NativeSite {
                    source,
                    line: 0,
                    start,
                    end,
                },
            )),
        );
    }
    // Distinct but overlapping name spans are ambiguous too: matching a
    // suffix inside another declaration's token is not independent evidence.
    // Sorted intervals identify connected overlap groups in one pass.
    let mut overlaps = BTreeSet::new();
    let mut group_start = None;
    let mut group_end = 0;
    for &(start, end) in spans.keys() {
        if start < group_end {
            overlaps.insert((start, end));
            if let Some(first) = group_start {
                overlaps.insert(first);
            }
            group_end = group_end.max(end);
        } else {
            group_start = Some((start, end));
            group_end = end;
        }
    }
    // Sorted byte positions permit a single source scan, including when the
    // semantic entity iterator uses a different canonical declaration order.
    let mut cursor = 0usize;
    let mut line = 1u32;
    let mut sites = BTreeMap::new();
    for ((name_start, name_end), claim) in spans {
        if overlaps.contains(&(name_start, name_end)) {
            continue;
        }
        let Some((identity, mut site)) = claim else {
            continue;
        };
        if duplicates.contains(&identity) {
            continue;
        }
        let position = name_start as usize;
        let newlines = text.as_bytes()[cursor..position]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count();
        line = u32::try_from(newlines)
            .ok()
            .and_then(|count| line.checked_add(count))
            .ok_or_else(|| {
                BuiltinModelError("native source line overflows product location".to_owned())
            })?;
        cursor = position;
        site.line = line;
        sites.insert(identity, site);
    }
    Ok(NativeSites {
        sites,
        working_bytes,
    })
}
