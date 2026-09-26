//! Content freshness of one published semantic image against the current scan.
//!
//! Each image carries the source-fact identity of the bytes it was compiled
//! from. An in-place edit is stale even when the path set is unchanged.
//! Records that predate identity persistence keep the path-set comparison.

use super::super::super::BuiltinModelError;
use backend_semantic::ir::{SemanticCoreReader as _, SemanticImageView};
use std::collections::{BTreeMap, BTreeSet};

/// The relative source path and semantic content identity each compiled image
/// carries, read from the publication's activated images.
type CompiledSources =
    BTreeMap<String, backend_version::ContentId<backend_version::SourceFactDomain>>;

/// Typed freshness decisions for one (project, profile) publication target.
///
/// The comparison is content against content: each image carries the exact
/// `SourceFactDomain` identity its source bytes hash to, and every current
/// file record persists the same identity for its own bytes. An in-place edit
/// under an unchanged path set is therefore visible, which the older path-set
/// comparison could never prove. Where a current record predates identity
/// persistence the decision falls back to the coarse compiled-versus-current
/// path-set comparison, which stays the truth those records can support.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::builtin::view_build) struct FreshnessDecision {
    /// Each image's compiled path and its source content identity.
    pub(super) compiled: CompiledSources,
    /// The persisted identities behind this decision, present exactly when
    /// the comparison was decisive.
    identities:
        Option<BTreeMap<String, backend_version::ContentId<backend_version::SourceFactDomain>>>,
    /// Current paths of this profile whose content identity mismatches the
    /// image compiled from that path, or that no image compiled at all.
    pub(in crate::builtin::view_build) stale_paths: BTreeSet<String>,
    /// Whether every current file of the profile carries a persisted
    /// identity, making the content comparison decisive.
    pub(in crate::builtin::view_build) identity_decisive: bool,
    /// Whether the compiled path set already differs from the current scan.
    pub(in crate::builtin::view_build) path_sets_differ: bool,
}

impl FreshnessDecision {
    /// Returns whether the image compiled from `path` with `identity`
    /// predates the current file content.
    pub(in crate::builtin::view_build) fn image_stale(
        &self,
        path: &str,
        identity: backend_version::ContentId<backend_version::SourceFactDomain>,
    ) -> bool {
        match &self.identities {
            Some(identities) => identities
                .get(path)
                .is_none_or(|current| *current != identity),
            None => self.path_sets_differ,
        }
    }
}

/// Compares each compiled image's (path, content identity) against the
/// current scan.
///
/// Identity comparison decides freshness only when every current file of the
/// profile states its identity; otherwise the decision is the honest fallback
/// those records support: the compiled and current path sets must agree.
pub(in crate::builtin::view_build) fn freshness_decision(
    compiled: &CompiledSources,
    current_paths: &BTreeSet<String>,
    current_identities: Option<
        &BTreeMap<String, backend_version::ContentId<backend_version::SourceFactDomain>>,
    >,
) -> FreshnessDecision {
    let identity_decisive =
        current_identities.is_some_and(|identities| identities.len() == current_paths.len());
    let path_sets_differ = {
        let compiled_paths = compiled
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<&str>>();
        let current = current_paths
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        compiled_paths != current
    };
    let mut stale_paths = BTreeSet::new();
    if identity_decisive {
        let identities = current_identities.expect("decisive above");
        for path in current_paths {
            let stale = compiled.get(path).is_none_or(|identity| {
                identities
                    .get(path)
                    .is_none_or(|current| current != identity)
            });
            if stale {
                stale_paths.insert(path.clone());
            }
        }
    } else if path_sets_differ {
        stale_paths.clone_from(current_paths);
    }
    FreshnessDecision {
        compiled: compiled.clone(),
        identities: if identity_decisive {
            current_identities.cloned()
        } else {
            None
        },
        stale_paths,
        identity_decisive,
        path_sets_differ,
    }
}

/// Returns the relative source path and semantic content identity of every
/// activated image.
///
/// The publication binding already proves each image carries captured
/// provenance, so a missing path atom or identity is a broken invariant, not
/// a display gap.
pub(super) fn compiled_source_identities(
    activated: &super::super::super::ActivatedProductSemantics,
) -> Result<CompiledSources, BuiltinModelError> {
    let mut compiled = BTreeMap::new();
    for image in activated.images() {
        let view = SemanticImageView::reopen(image.as_ref()).map_err(|error| {
            BuiltinModelError(format!("reopen activated semantic image: {error}"))
        })?;
        let (path, identity) = compiled_source(&view)?;
        compiled.insert(path, identity);
    }
    Ok(compiled)
}

/// Returns one image's relative source path and its source content identity.
pub(in crate::builtin::view_build) fn compiled_source(
    image: &SemanticImageView<'_>,
) -> Result<
    (
        String,
        backend_version::ContentId<backend_version::SourceFactDomain>,
    ),
    BuiltinModelError,
> {
    let backend_semantic::ir::ImageProvenance::Captured { source, scope, .. } =
        image.image_facts().provenance
    else {
        return Err(BuiltinModelError(
            "semantic image lost its captured provenance before projection".to_owned(),
        ));
    };
    let path = image
        .atom(scope.path)
        .ok_or_else(|| BuiltinModelError("semantic image lost its source path".to_owned()))?;
    let path = std::str::from_utf8(path)
        .map(str::to_owned)
        .map_err(|_| BuiltinModelError("semantic source path is not UTF-8".to_owned()))?;
    Ok((path, source.identity))
}

/// Returns the exact relative source path one image was compiled from.
pub(super) fn compiled_source_path(
    image: &SemanticImageView<'_>,
) -> Result<String, BuiltinModelError> {
    compiled_source(image).map(|(path, _)| path)
}
