//! Source locations proven from a selected malformed-C# frontier and a
//! Roslyn image's exact full-declaration spans.
//!
//! This is deliberately separate from Clang's name-token reconciliation:
//! Roslyn already binds each declaration row to its exact source extent and
//! the lowerer preserves that extent on the corresponding semantic entity.

use super::super::BuiltinModelError;
use backend_engine::application::DocumentationSession;
use backend_engine::builtin::VerifiedNativeSource;
use backend_semantic::ir::{
    DeclarationIdentity, ImageProvenance, SemanticCoreReader as _, SemanticImageView,
    SemanticReader as _,
};
use backend_semantic::vocabulary::{LanguageProfile, NativeTool, Stage};
use std::collections::{BTreeMap, BTreeSet};

const SITE_WORKING_BYTES: usize = 768;
const SITE_WORKING_BASE_BYTES: usize = 4096;

pub(super) struct NativeCSharpProjection<'source> {
    pub(super) source: &'source VerifiedNativeSource,
    pub(super) max_sites: usize,
    pub(super) max_working_bytes: usize,
}

pub(super) struct NativeCSharpSite<'source> {
    source: &'source VerifiedNativeSource,
    pub(super) line: u32,
    start: usize,
    end: usize,
}

impl NativeCSharpSite<'_> {
    pub(super) fn excerpt(&self) -> backend_compile::SourceExcerpt {
        // The source and UTF-8 boundaries are verified when the site is built.
        std::str::from_utf8(&self.source.bytes()[self.start..self.end])
            .map(backend_compile::SourceExcerpt::capture_bounded)
            .unwrap_or(backend_compile::SourceExcerpt::NotCaptured)
    }
}

pub(super) struct NativeCSharpSites<'source> {
    sites: BTreeMap<DeclarationIdentity, NativeCSharpSite<'source>>,
    pub(super) working_bytes: usize,
}

impl NativeCSharpSites<'_> {
    pub(super) fn get(&self, identity: &DeclarationIdentity) -> Option<&NativeCSharpSite<'_>> {
        self.sites.get(identity)
    }
}

/// Binds semantic rows to source locations only when the image provenance,
/// selected C# candidate, root-bound bytes, source path, and authority tool all
/// agree. Duplicate identities or identical declaration extents are refused;
/// nested extents are expected and remain distinct Roslyn declarations.
pub(super) fn prove_native_csharp_sites<'source>(
    image: &SemanticImageView<'_>,
    project: [u8; 32],
    projection: NativeCSharpProjection<'source>,
) -> Result<NativeCSharpSites<'source>, BuiltinModelError> {
    let source = projection.source;
    let source_bytes = source.bytes().len();
    if source_bytes > projection.max_working_bytes {
        return Err(BuiltinModelError(
            "native C# source exceeds projection working budget".to_owned(),
        ));
    }
    let empty = || NativeCSharpSites {
        sites: BTreeMap::new(),
        working_bytes: source_bytes,
    };
    if source.project() != project || !matches!(source.profile(), LanguageProfile::CSharp(_)) {
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
        || usize::try_from(captured.byte_len).ok() != Some(source_bytes)
        || recipe.profile != source.profile()
        || recipe.tool != NativeTool::CSharpCompiler
        || recipe.stage != Stage::LowerIr
        || image.atom(scope.path) != Some(source.relative_path().as_bytes())
    {
        return Ok(empty());
    }
    let text = std::str::from_utf8(source.bytes()).map_err(|_| {
        BuiltinModelError("verified native C# source lost UTF-8 authority".to_owned())
    })?;
    let session = DocumentationSession::new(image);
    if !session.publication_ownership_available() {
        return Ok(empty());
    }
    let mut spans = BTreeMap::<(u32, u32), Option<DeclarationIdentity>>::new();
    let mut identities = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    let mut candidates = 0usize;
    let mut working_bytes = source_bytes;
    for row in session.canonical_entities() {
        let row =
            row.map_err(|error| BuiltinModelError(format!("native C# site declaration: {error}")))?;
        if !session.owns_declaration(&row.entity) {
            continue;
        }
        let Some(extent) = row.entity.source else {
            continue;
        };
        if image.atom(extent.file()) != Some(source.relative_path().as_bytes()) {
            continue;
        }
        let (Ok(start), Ok(end)) = (
            usize::try_from(extent.start()),
            usize::try_from(extent.end()),
        ) else {
            continue;
        };
        let Some(slice) = source.bytes().get(start..end) else {
            continue;
        };
        let Ok(declaration) = std::str::from_utf8(slice) else {
            continue;
        };
        if declaration.is_empty() || text.get(start..end).is_none() {
            continue;
        }
        candidates = candidates
            .checked_add(1)
            .filter(|count| *count <= projection.max_sites)
            .ok_or_else(|| {
                BuiltinModelError("native C# site count exceeds projection capacity".to_owned())
            })?;
        if candidates == 1 {
            working_bytes = working_bytes
                .checked_add(SITE_WORKING_BASE_BYTES)
                .filter(|bytes| *bytes <= projection.max_working_bytes)
                .ok_or_else(|| {
                    BuiltinModelError("native C# site map exceeds working budget".to_owned())
                })?;
        }
        working_bytes = working_bytes
            .checked_add(SITE_WORKING_BYTES)
            .filter(|bytes| *bytes <= projection.max_working_bytes)
            .ok_or_else(|| BuiltinModelError("native C# sites exceed working budget".to_owned()))?;
        let identity = row.entity.version.identity();
        if !identities.insert(identity) {
            duplicates.insert(identity);
        }
        match spans.entry((extent.start(), extent.end())) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Some(identity));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.insert(None);
            }
        }
    }

    let mut cursor = 0usize;
    let mut line = 1u32;
    let mut sites = BTreeMap::new();
    for ((start, end), claim) in spans {
        let Some(identity) = claim else {
            continue;
        };
        if duplicates.contains(&identity) {
            continue;
        }
        let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end)) else {
            continue;
        };
        if start < cursor {
            continue;
        }
        let newlines = source.bytes()[cursor..start]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count();
        line = u32::try_from(newlines)
            .ok()
            .and_then(|count| line.checked_add(count))
            .ok_or_else(|| {
                BuiltinModelError("native C# source line overflows product location".to_owned())
            })?;
        cursor = start;
        let Some(slice) = source.bytes().get(start..end) else {
            continue;
        };
        let declaration = std::str::from_utf8(slice).map_err(|_| {
            BuiltinModelError("native C# declaration boundary lost UTF-8 authority".to_owned())
        })?;
        sites.insert(
            identity,
            NativeCSharpSite {
                source,
                line,
                start,
                end,
            },
        );
    }
    Ok(NativeCSharpSites {
        sites,
        working_bytes,
    })
}
