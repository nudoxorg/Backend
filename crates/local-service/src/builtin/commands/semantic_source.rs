//! Captured source ranges over the existing retained compiler/V3 history plane.
//!
//! Retention belongs to compiler publication and the existing native history
//! branches. Discovery and range reads create no tags, registry, or durable
//! query lease. A captured published tip must remain a checked ancestor of its
//! authenticated publication branch; a released/rewound root is Gone even for
//! cached bytes. Existing publication recovery may deliberately republish that
//! immutable tip; deleting a navigation ref does not cancel publication intent.
//! Compiler/IR roots and their lineage remain independently owned.
//!
//! Page roots and continuations freeze generation membership/count/keyset.
//! Mutable freshness/history statuses are observed per returned descriptor;
//! every source reference freezes its complete observation and canonical bytes.
//! Neither a later page nor publication recovery can rebind those bytes.

use super::super::semantic_authority::SemanticAuthority;
use super::super::{
    BuiltinAuthorityVerifier, BuiltinModel, BuiltinModelError, BuiltinSemanticRelation,
    BuiltinValidator,
};
use backend_engine::builtin::{ProductSemanticPublicationKey, ProductSemanticPublicationRecord};
use backend_library::{
    SemanticSourceDocument, SemanticSourceHistory, SemanticSourceReference, SemanticSourceRequest,
    VerifiedSemanticSourceReference,
};
use backend_replication::{
    FileSemanticRangeStore, HistoryCommitId, HistoryRefKind, HistoryRefName, SemanticTargetKey,
    TransportLimits,
};
use backend_semantic::ir::{JumboRopeLimits, SemanticTypedPlaneVerificationTierV2};

fn failure(error: impl std::fmt::Display) -> BuiltinModelError {
    BuiltinModelError(format!("semantic source admission: {error}"))
}

/// One immutable read plan under a complete owner-authenticated source guard.
/// It is only a pure byte cache: eviction changes cost, never source identity
/// or availability. There is no mutable snapshot alias or frontend dictionary.
pub(super) struct SourceReadPlan {
    reference: SemanticSourceReference,
    document: SemanticSourceDocument,
    retention: Option<backend_replication::HistoryRefAncestryProof>,
}

fn target(
    reference: &VerifiedSemanticSourceReference<'_>,
) -> Result<SemanticTargetKey, BuiltinModelError> {
    let raw = reference.reference();
    SemanticTargetKey::new(
        raw.metadata.package.as_str(),
        raw.metadata.coordinate.as_str(),
        raw.metadata.profile.profile().map_err(failure)?,
    )
    .map_err(failure)
}

fn publication_ref(
    reference: &VerifiedSemanticSourceReference<'_>,
) -> Result<Option<HistoryRefName>, BuiltinModelError> {
    let SemanticSourceHistory::Published { reference, .. } = &reference.reference().history else {
        return Ok(None);
    };
    HistoryRefName::new(reference.clone())
        .map(Some)
        .map_err(failure)
}

/// Captured source reads borrow the lifetime of the existing publication
/// history. A query never creates permanent tags or hidden per-caller roots.
/// Removing/rewinding that publication ref may make an old source Gone while
/// compiler/IR generation retention continues under its independent owners.
fn publication_retention(
    reference: &VerifiedSemanticSourceReference<'_>,
    authority: &SemanticAuthority,
    previous: Option<&backend_replication::HistoryRefAncestryProof>,
) -> Result<Option<backend_replication::HistoryRefAncestryProof>, SourceReadError> {
    let SemanticSourceHistory::Published { proof, .. } = &reference.reference().history else {
        return Ok(None);
    };
    let name = publication_ref(reference)?
        .ok_or_else(|| failure("published source has no native publication reference"))?;
    let target = target(reference)?;
    let history = FileSemanticRangeStore::open(authority.store(), TransportLimits::default())
        .map_err(failure)?;
    let current = history
        .history_ref(&target, HistoryRefKind::Branch, &name)
        .map_err(failure)?
        .ok_or(SourceReadError::Gone)?;
    let captured_tip = HistoryCommitId::from_bytes(proof.reference_tip);
    if let Some(previous) = previous
        && previous.ref_tip() == current.commit()
        && previous.ancestor() == captured_tip
    {
        return Ok(Some(previous.clone()));
    }
    history
        .history_ref_ancestry_if_retained(&target, HistoryRefKind::Branch, &name, captured_tip)
        .map_err(failure)?
        .map(Some)
        .ok_or(SourceReadError::Gone)
}

/// Read failures distinguish released retention from invalid selectors/data.
pub(super) enum SourceReadError {
    /// Current publication/generation roots no longer retain this exact source.
    Gone,
    /// The complete owner guard or native reconstruction failed admission.
    Admission(BuiltinModelError),
}
impl From<BuiltinModelError> for SourceReadError {
    fn from(error: BuiltinModelError) -> Self {
        Self::Admission(error)
    }
}
fn reconstruct(
    reference: &VerifiedSemanticSourceReference<'_>,
    authority: &SemanticAuthority,
) -> Result<SemanticSourceDocument, BuiltinModelError> {
    let raw = reference.reference();
    let SemanticSourceHistory::Published { proof, .. } = &raw.history else {
        return reference.reconstruct(Box::new([])).map_err(failure);
    };
    if proof.image_count != raw.metadata.artifacts
        || proof.target_package != raw.metadata.package.as_str()
        || proof.target_coordinate != raw.metadata.coordinate.as_str()
    {
        return Err(failure(
            "source proof locator differs from its exact compiler metadata",
        ));
    }
    let name = publication_ref(reference)?
        .ok_or_else(|| failure("published source has no publication ref"))?;
    let target = target(reference)?;
    let history = FileSemanticRangeStore::open(authority.store(), TransportLimits::default())
        .map_err(failure)?;
    let tip = HistoryCommitId::from_bytes(proof.reference_tip);
    let count = usize::try_from(proof.image_count).map_err(failure)?;
    let mut ids = Vec::new();
    ids.try_reserve_exact(count).map_err(failure)?;
    let mut current = tip;
    let mut first_parent = None;
    for ordinal in (0..count).rev() {
        let commit = history.history_commit(&target, current).map_err(failure)?;
        if commit.provenance() != &proof.package_identity
            || commit.parents().len() > 1
            || commit.generation_root().typed_v3_claim().is_none()
        {
            return Err(failure("mixed or unsupported source history chain"));
        }
        ids.push(current);
        first_parent = commit.parents().first().copied();
        if ordinal > 0 {
            current = first_parent.ok_or_else(|| failure("partial source history chain"))?;
        }
    }
    ids.reverse();
    let ancestries = history
        .history_ref_ancestry_proofs_for_captured_chain(
            &target,
            HistoryRefKind::Branch,
            &name,
            &ids,
            first_parent,
        )
        .map_err(failure)?;
    if ancestries.len() != count {
        return Err(failure("partial source ancestry proof"));
    }
    let mut images = Vec::new();
    images.try_reserve_exact(count).map_err(failure)?;
    for (ordinal, (id, ancestry)) in ids.iter().zip(&ancestries).enumerate() {
        if ancestry.ancestor() != *id {
            return Err(failure("source ancestry names another tip"));
        }
        let replay = history
            .replay_typed_v3_history(
                &target,
                HistoryRefKind::Branch,
                &name,
                *id,
                ancestry,
                SemanticTypedPlaneVerificationTierV2::Standard,
                JumboRopeLimits::default(),
            )
            .map_err(failure)?;
        let commit = replay.commit();
        let stamp = commit.selected_stamp();
        let captured = &proof.selection;
        if stamp.namespace() != &captured.namespace
            || stamp.profile().to_bytes() != captured.profile.to_bytes()
            || stamp.source_coordinate() != &captured.source_coordinate
            || stamp.selection_revision() != captured.selection_revision
            || stamp.selected_root() != &captured.selected_root
            || stamp.closure_id() != &captured.closure_id
            || stamp.catalog_root().as_bytes() != &captured.catalog_root
            || commit.provenance() != &proof.package_identity
            || usize::try_from(replay.image().artifact_ordinal()).ok() != Some(ordinal)
        {
            return Err(failure(
                "source image is outside the captured package proof",
            ));
        }
        images.push(backend_library::SemanticHistoryImagePublicationProof {
            image: backend_library::SemanticHistoryImageIdentity {
                artifact_ordinal: replay.image().artifact_ordinal(),
                semantic_generation: *replay.image().semantic_generation().as_bytes(),
                manifest_root: *replay.image().manifest_root().as_bytes(),
                image_identity: *replay.image_identity().as_ref(),
            },
            history_commit: *id.as_bytes(),
            parent_commits: commit
                .parents()
                .iter()
                .map(|id| *id.as_bytes())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        });
    }
    reference
        .reconstruct(images.into_boxed_slice())
        .map_err(failure)
}

/// Checks retained native compiler identity before using a source guard's
/// root/proof locator. Captured metadata remains frozen; reconstruction never
/// substitutes a newer freshness observation or selected head.
pub(super) fn read(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    authority: &SemanticAuthority,
    request: &SemanticSourceRequest,
    cached: &mut Option<SourceReadPlan>,
) -> Result<backend_library::SemanticSourceChunk, SourceReadError> {
    let secret = *authority.product_statement_authority()?;
    let verified = request.reference.verify_owner(&secret).map_err(failure)?;
    let reference = verified.reference();
    let metadata = &reference.metadata;
    let key = ProductSemanticPublicationKey::new(
        metadata.package.clone(),
        metadata.coordinate.clone(),
        metadata.profile.profile().map_err(failure)?,
    )
    .map_err(failure)?
    .for_generation_bytes(metadata.generation.to_bytes())
    .map_err(failure)?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(failure)?;
    let Some(ProductSemanticPublicationRecord::Published { coverage, claim }) =
        relation.lookup(&key).map_err(failure)?
    else {
        return Err(SourceReadError::Gone);
    };
    let binding = claim.binding();
    let manifest = claim.manifest();
    if metadata.generation.to_bytes() != *binding.identity.as_ref()
        || metadata.complete
            != matches!(
                coverage,
                backend_engine::builtin::SemanticPublicationCoverage::Complete
            )
        || metadata.generation_root != *binding.generation.pinned_root.as_ref()
        || metadata.dependency_set != *binding.generation.dep_set.as_ref()
        || metadata.manifest != *manifest.identity.as_ref()
        || metadata.artifacts != manifest.fragment_count
        || metadata.semantic_bytes != manifest.byte_length
    {
        return Err(SourceReadError::Admission(failure(
            "captured source differs from its retained native generation",
        )));
    }
    let same = cached
        .as_ref()
        .is_some_and(|plan| plan.reference == *reference);
    let retention = publication_retention(
        &verified,
        authority,
        cached
            .as_ref()
            .filter(|_| same)
            .and_then(|plan| plan.retention.as_ref()),
    )?;
    if same {
        if let Some(plan) = cached.as_mut() {
            plan.retention = retention;
        }
    } else {
        let document = reconstruct(&verified, authority)?;
        *cached = Some(SourceReadPlan {
            reference: reference.clone(),
            document,
            retention,
        });
    }
    let plan = cached
        .as_ref()
        .ok_or_else(|| failure("source read plan is absent"))?;
    // The checked document factory borrows exact canonical bytes and creates
    // the bounded wire range directly; no producer-side decode is necessary.
    plan.document
        .chunk(&request.range)
        .map_err(|error| SourceReadError::Admission(failure(error)))
}

/// Descriptor discovery captures expensive image history only for native page
/// members. Exact totals come from the immutable package-prefix key space;
/// neither a product-row truncation nor metadata-query count changes identity.
pub(super) fn versions(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    authority: &mut SemanticAuthority,
    package: &backend_library::PackageReference,
    limit: u16,
    cursor: Option<&backend_library::SemanticVersionsCursor>,
) -> Result<backend_library::SemanticVersionsPage, BuiltinModelError> {
    use backend_engine::builtin::SemanticPublicationSelection;
    use backend_library::{
        SemanticLanguageProfile, SemanticVersionDescriptor, SemanticVersionsCursor,
        SemanticVersionsPage,
    };
    if limit == 0 || limit > backend_library::QueryLimit::MAX {
        return Err(failure("semantic version page limit must be 1..200"));
    }
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let source_root = super::super::view_publish::source_root(&snapshot)?;
    let semantic_root = super::super::view_publish::semantic_root(&snapshot)?;
    let secret = *authority.product_statement_authority()?;
    let admitted_cursor = cursor
        .map(|cursor| cursor.verify_owner(&secret))
        .transpose()
        .map_err(failure)?;
    let continuation = admitted_cursor.as_ref().map(|cursor| cursor.claims());
    if continuation.is_some_and(|binding| {
        binding.package != *package
            || binding.source_relation_root != source_root
            || binding.semantic_relation_root != semantic_root
            || binding.limit != limit
    }) {
        return Err(failure(
            "semantic version cursor belongs to another request or native snapshot",
        ));
    }
    let resume_key = continuation
        .as_ref()
        .map(|binding| {
            ProductSemanticPublicationKey::new(
                package.clone(),
                binding.coordinate.clone(),
                binding.profile.profile().map_err(failure)?,
            )
            .map_err(failure)?
            .for_generation_bytes(binding.generation.to_bytes())
            .map_err(failure)
        })
        .transpose()?;
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(failure)?;
    if let Some(key) = &resume_key {
        let record = relation
            .lookup(key)
            .map_err(failure)?
            .ok_or_else(|| failure("semantic version cursor key is absent from its native root"))?;
        key.admit_record(&record).map_err(failure)?;
        if continuation
            .as_ref()
            .is_none_or(|binding| binding.offset == 0 || binding.offset >= binding.total)
        {
            return Err(failure(
                "semantic version cursor is outside its complete native extent",
            ));
        }
    }
    // The first page counts only cheap immutable keys. Subsequent pages seek
    // exclusively after the authenticated last key, reuse the root-bound total,
    // and never repeat the full history count or capture predecessor proofs.
    let mut from = resume_key
        .is_none()
        .then(|| ProductSemanticPublicationKey::package_lower_bound(package.clone()));
    let mut after = resume_key;
    let mut total = continuation.as_ref().map_or(0, |binding| binding.total);
    let mut members = Vec::new();
    let mut unavailable = None;
    loop {
        let page = if let Some(from) = from.take() {
            relation.page_from(&from, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        } else {
            relation.page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
        }
        .map_err(failure)?;
        let mut finished = false;
        for (key, record) in page.entries() {
            if key.package() != package {
                finished = true;
                break;
            }
            key.admit_record(record).map_err(failure)?;
            if key.is_selected() {
                match record {
                    ProductSemanticPublicationRecord::Published { claim, .. }
                        if continuation.is_none() =>
                    {
                        let generation_key = key
                            .clone()
                            .for_generation_bytes(*claim.binding().identity.as_ref())
                            .map_err(failure)?;
                        let generation = relation.lookup(&generation_key).map_err(failure)?
                            .ok_or_else(|| failure("selected semantic generation is absent from immutable native history"))?;
                        generation_key.admit_record(&generation).map_err(failure)?;
                    }
                    ProductSemanticPublicationRecord::Unavailable(reason) => {
                        unavailable.get_or_insert(*reason);
                    }
                    _ => {}
                }
                continue;
            }
            if !matches!(key.selection(), SemanticPublicationSelection::Generation(_)) {
                continue;
            }
            let ProductSemanticPublicationRecord::Published { coverage, claim } = record else {
                return Err(failure("immutable semantic generation is not published"));
            };
            if continuation.is_none() {
                total = total
                    .checked_add(1)
                    .ok_or_else(|| failure("semantic version count overflow"))?;
            }
            if members.len() < usize::from(limit) {
                members.push((key.clone(), *coverage, *claim));
            }
            if continuation.is_some() && members.len() == usize::from(limit) {
                finished = true;
                break;
            }
        }
        if finished {
            break;
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    if let Some(binding) = &continuation {
        let expected = (binding.total - binding.offset).min(u64::from(limit));
        if members.len() as u64 != expected {
            return Err(failure(
                "semantic version native range differs from its authenticated extent",
            ));
        }
    }
    if total == 0
        && let Some(reason) = unavailable
    {
        return Err(failure(format!(
            "semantic publication unavailable: {reason}"
        )));
    }
    let frontier = super::index::selected_project_source_frontier(&snapshot, package)?;
    let mut descriptors = Vec::new();
    let mut last = None;
    for (key, coverage, claim) in members {
        let selected_key = ProductSemanticPublicationKey::new(
            package.clone(),
            key.coordinate().clone(),
            key.profile(),
        )
        .map_err(failure)?;
        let selected = matches!(relation.lookup(&selected_key).map_err(failure)?,
            Some(ProductSemanticPublicationRecord::Published { claim: selected, .. })
                if selected.binding().identity == claim.binding().identity);
        let freshness_key =
            super::super::semantic_authority::SelectedSemanticPublicationKey::new(&selected_key)
                .map_err(failure)?;
        let freshness = authority.freshness(&snapshot, freshness_key, claim)?;
        let mut record =
            super::index::semantic_version_record(&key, coverage, claim, selected, freshness);
        if selected {
            record.selected_source_frontier = frontier.clone();
            record.history_status = authority.native_history_status(&selected_key, claim)?;
        }
        let reference =
            SemanticSourceReference::capture(&record, source_root, semantic_root, &secret)
                .map_err(failure)?;
        let verified = reference.verify_owner(&secret).map_err(failure)?;
        let descriptor = match publication_retention(&verified, authority, None) {
            Ok(_) => SemanticVersionDescriptor::Available { reference },
            Err(SourceReadError::Gone) => SemanticVersionDescriptor::Unavailable {
                metadata: reference.metadata,
                history: reference.history,
                reason: "captured source is no longer retained by its publication root".to_owned(),
            },
            Err(SourceReadError::Admission(error)) => return Err(error),
        };
        last = Some((
            key.coordinate().clone(),
            SemanticLanguageProfile::new(key.profile()),
            record.generation,
        ));
        descriptors.push(descriptor);
    }
    let offset = continuation.as_ref().map_or(0, |binding| binding.offset);
    let end = offset
        .checked_add(descriptors.len() as u64)
        .ok_or_else(|| failure("semantic version page extent overflow"))?;
    let more = end < total;
    let next_cursor = if more {
        let (coordinate, profile, generation) =
            last.ok_or_else(|| failure("nonterminal semantic version page is empty"))?;
        let binding = backend_library::SemanticVersionsCursorClaims {
            package: package.clone(),
            source_relation_root: source_root,
            semantic_relation_root: semantic_root,
            coordinate,
            profile,
            generation,
            limit,
            total,
            offset: end,
        };
        Some(SemanticVersionsCursor::issue(binding, &secret).map_err(failure)?)
    } else {
        None
    };
    let page = SemanticVersionsPage {
        package: package.clone(),
        source_relation_root: source_root,
        semantic_relation_root: semantic_root,
        total,
        offset,
        entries: descriptors.into_boxed_slice(),
        next_cursor,
        more,
        complete: !more,
    };
    page.admit_request(package, limit, cursor)
        .map_err(failure)?;
    Ok(page)
}
