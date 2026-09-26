//! Product reply admission and encoded-size bounds.
//!
//! A reply names the command that produced it and stays inside the fixed row
//! bound. Size bounds count those same rows without building the wire body.

use crate::CommandId;

use super::{
    DeclarationRecord, DiffRecord, MAX_PRODUCT_ROWS, PackageReference, ProductAdmissionError,
    ProductText, ProjectRecord, RegistryMetadata, RegistryPackageRecord, ReleaseRecord,
    SemanticLinkDelta, SemanticLinkEvidence, SubscriptionRecord, SurfaceReply, TreeNodeRecord,
    TreeOpener, TreeSubject,
};

impl SurfaceReply {
    /// Returns the exact command identity required by this reply.
    #[must_use]
    pub const fn id(&self) -> CommandId {
        match self {
            Self::Advisory(_) => CommandId::Advisory,
            Self::Read(_) => CommandId::Read,
            Self::References { .. } => CommandId::References,
            Self::Diff(_) => CommandId::Diff,
            Self::Explored(_) => CommandId::Explore,
            Self::Package(_) => CommandId::Package,
            Self::ForgePackageAdded(_) => CommandId::ForgeAdd,
            Self::ForgePackageReferenced(_) => CommandId::ForgeReference,
            Self::Dependents(_) => CommandId::Dependents,
            Self::Dependencies(_) => CommandId::Dependencies,
            Self::Owner(_) => CommandId::Owner,
            Self::IndexSearch(_) => CommandId::IndexSearch,
            Self::PackageVersions(_) => CommandId::PackageVersions,
            Self::SemanticVersions(_) => CommandId::SemanticVersions,
            Self::SemanticVersionSelected(_) => CommandId::SelectSemanticVersion,
            Self::PackageProfile { .. } => CommandId::PackageProfile,
            Self::Subscribed(_) => CommandId::Subscribe,
            Self::Unsubscribed(_) => CommandId::Unsubscribe,
            Self::Subscriptions(_) => CommandId::Subscriptions,
            Self::Releases(_) => CommandId::Releases,
            Self::Projects(_) => CommandId::Projects,
            Self::ProjectCreated(_) => CommandId::ProjectCreate,
            Self::ProjectDeleted(_) => CommandId::ProjectDelete,
            Self::ProjectAdded(_) => CommandId::ProjectAdd,
            Self::ProjectRemoved(_) => CommandId::ProjectRemove,
            Self::ProjectSynced(_) => CommandId::ProjectSync,
            Self::Tree(_) => CommandId::Tree,
            Self::TreeOpened(_) => CommandId::TreeOpen,
            Self::TreeClosed(_) => CommandId::TreeClose,
        }
    }
    /// Verifies command identity and reply collection bounds.
    ///
    /// # Errors
    ///
    /// Returns [`ProductAdmissionError`] when the reply belongs to another command or exceeds the
    /// fixed product row bound.
    #[allow(
        clippy::match_same_arms,
        reason = "distinct typed reply collections share only their length admission rule"
    )]
    pub fn admit(&self, expected: CommandId) -> Result<(), ProductAdmissionError> {
        if self.id() != expected {
            return Err(ProductAdmissionError::CommandMismatch);
        }
        let count = match self {
            Self::Advisory(_) => 1,
            Self::Read(v) => v.len(),
            Self::References { references, .. } => references.len(),
            Self::Diff(rows) => rows.iter().try_fold(rows.len(), |count, row| {
                if !row.has_valid_identity_shape()
                    || row
                        .links
                        .iter()
                        .any(|link| !link.matches_declaration_sides(row.before, row.after))
                {
                    return Err(ProductAdmissionError::DiffShape);
                }
                count
                    .checked_add(row.links.len())
                    .ok_or(ProductAdmissionError::RowBound)
            })?,
            Self::Explored(v)
            | Self::Package(v)
            | Self::IndexSearch(v)
            | Self::PackageVersions(v)
            | Self::Dependents(RegistryMetadata::Recorded(v))
            | Self::Owner(RegistryMetadata::Recorded(v)) => v.len(),
            Self::Dependencies(crate::DependencyFacts::Known(v)) => v.len(),
            Self::SemanticVersions(records) => {
                let mut selected = 0_usize;
                for record in records {
                    record.admit()?;
                    selected = selected.saturating_add(usize::from(record.selected));
                }
                if selected > 1 {
                    return Err(ProductAdmissionError::SemanticVersionShape);
                }
                records.len()
            }
            Self::SemanticVersionSelected(record) => {
                record.admit()?;
                if !record.selected {
                    return Err(ProductAdmissionError::SemanticVersionShape);
                }
                1
            }
            Self::Subscriptions(v) => v.len(),
            Self::Releases(v) => v.len(),
            Self::Projects(v) => v.len(),
            Self::Tree(v) => v.len(),
            _ => 1,
        };
        if count > MAX_PRODUCT_ROWS {
            Err(ProductAdmissionError::RowBound)
        } else {
            match self {
                Self::Explored(rows)
                | Self::Package(rows)
                | Self::IndexSearch(rows)
                | Self::PackageVersions(rows) => {
                    for row in rows {
                        admit_registry_record(row)?;
                    }
                }
                Self::Dependents(RegistryMetadata::Recorded(rows))
                | Self::Owner(RegistryMetadata::Recorded(rows)) => {
                    for row in rows {
                        admit_registry_record(row)?;
                    }
                }
                Self::PackageProfile {
                    latest: Some(row), ..
                } => admit_registry_record(row)?,
                _ => {}
            }
            Ok(())
        }
    }

    pub(crate) fn encoded_size_bound(&self) -> usize {
        const ENVELOPE_BYTES: usize = 512;
        let payload = match self {
            Self::Read(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(declaration_record_bound(record))
            }),
            Self::Advisory(value) => fixed_record_bound()
                .saturating_add(serde_json::to_vec(value).map_or(0, |bytes| bytes.len())),
            Self::References { target, references } => references.iter().fold(
                fixed_record_bound().saturating_add(text_bound(target)),
                |bound, record| {
                    bound
                        .saturating_add(fixed_record_bound())
                        .saturating_add(text_bound(&record.site))
                        .saturating_add(semantic_link_evidence_bound(&record.evidence))
                },
            ),
            Self::Diff(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(diff_record_bound(record))
            }),
            Self::Explored(records)
            | Self::Package(records)
            | Self::IndexSearch(records)
            | Self::PackageVersions(records)
            | Self::Dependents(RegistryMetadata::Recorded(records))
            | Self::Owner(RegistryMetadata::Recorded(records)) => registry_records_bound(records),
            Self::ForgePackageAdded(record) | Self::ForgePackageReferenced(record) => {
                fixed_record_bound()
                    .saturating_add(serde_json::to_vec(record).map_or(0, |bytes| bytes.len()))
            }
            Self::SemanticVersions(records) => records.iter().fold(0_usize, |bound, record| {
                bound
                    .saturating_add(fixed_record_bound())
                    .saturating_add(package_reference_bound(&record.package))
                    .saturating_add(record.coordinate.as_str().len())
            }),
            Self::SemanticVersionSelected(record) => fixed_record_bound()
                .saturating_add(package_reference_bound(&record.package))
                .saturating_add(record.coordinate.as_str().len()),
            Self::Dependents(RegistryMetadata::NotRecorded(reason))
            | Self::Owner(RegistryMetadata::NotRecorded(reason)) => text_bound(reason),
            Self::Dependencies(crate::DependencyFacts::Known(records)) => {
                records.iter().fold(0_usize, |bound, record| {
                    bound.saturating_add(dependency_record_bound(record))
                })
            }
            Self::Dependencies(crate::DependencyFacts::Unknown(reason))
            | Self::Dependencies(crate::DependencyFacts::Unavailable(reason)) => text_bound(reason),
            Self::PackageProfile { latest, .. } => {
                latest.as_ref().map_or(64, registry_package_record_bound)
            }
            Self::Subscribed(record) => subscription_record_bound(record),
            Self::Unsubscribed(_) | Self::ProjectDeleted(_) | Self::TreeClosed(_) => 64,
            Self::Subscriptions(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(subscription_record_bound(record))
            }),
            Self::Releases(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(release_record_bound(record))
            }),
            Self::Projects(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(project_record_bound(record))
            }),
            Self::ProjectCreated(record)
            | Self::ProjectAdded(record)
            | Self::ProjectRemoved(record)
            | Self::ProjectSynced(record) => project_record_bound(record),
            Self::Tree(records) => records.iter().fold(0_usize, |bound, record| {
                bound.saturating_add(tree_node_record_bound(record))
            }),
            Self::TreeOpened(record) => tree_node_record_bound(record),
        };
        ENVELOPE_BYTES.saturating_add(payload)
    }
}

const fn fixed_record_bound() -> usize {
    512
}

fn text_bound(text: &ProductText) -> usize {
    text.as_str().len().saturating_add(32)
}

fn optional_text_bound(text: Option<&ProductText>) -> usize {
    text.map_or(16, text_bound)
}

fn package_reference_bound(package: &PackageReference) -> usize {
    package.as_str().len().saturating_add(64)
}

fn declaration_record_bound(record: &DeclarationRecord) -> usize {
    fixed_record_bound()
        .saturating_add(text_bound(&record.label))
        .saturating_add(optional_text_bound(record.signature.as_ref()))
}

fn diff_record_bound(record: &DiffRecord) -> usize {
    record.links.iter().fold(
        fixed_record_bound().saturating_add(text_bound(&record.label)),
        |bound, link| bound.saturating_add(semantic_link_delta_bound(link)),
    )
}

fn semantic_link_delta_bound(delta: &SemanticLinkDelta) -> usize {
    let evidence = match delta {
        SemanticLinkDelta::Added { evidence, .. } | SemanticLinkDelta::Removed { evidence, .. } => {
            semantic_link_evidence_bound(evidence)
        }
        SemanticLinkDelta::EvidenceChanged { before, after, .. } => {
            semantic_link_evidence_bound(before).saturating_add(semantic_link_evidence_bound(after))
        }
    };
    fixed_record_bound().saturating_add(evidence)
}

fn semantic_link_evidence_bound(evidence: &SemanticLinkEvidence) -> usize {
    evidence.source.as_ref().map_or(128, |source| {
        fixed_record_bound().saturating_add(text_bound(&source.file))
    })
}

fn registry_records_bound(records: &[RegistryPackageRecord]) -> usize {
    records.iter().fold(0_usize, |bound, record| {
        bound.saturating_add(registry_package_record_bound(record))
    })
}

fn registry_package_record_bound(record: &RegistryPackageRecord) -> usize {
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.coordinate))
        .saturating_add(text_bound(&record.name))
        .saturating_add(text_bound(&record.version))
        .saturating_add(serde_json::to_vec(&record.native_metadata).map_or(0, |bytes| bytes.len()))
        .saturating_add(serde_json::to_vec(&record.forge_sources).map_or(0, |bytes| bytes.len()))
        .saturating_add(serde_json::to_vec(&record.advisory).map_or(0, |bytes| bytes.len()))
}

fn admit_registry_record(record: &RegistryPackageRecord) -> Result<(), ProductAdmissionError> {
    let native_identity_matches = record
        .native_metadata
        .identity()
        .is_ok_and(|identity| identity == record.native_metadata_version);
    if record.native_metadata.admit().is_err() || !native_identity_matches {
        return Err(ProductAdmissionError::NativeMetadata);
    }
    if record.forge_sources.len() > crate::MAX_REGISTRY_FORGE_ASSOCIATIONS
        || record
            .forge_sources
            .iter()
            .any(|association| association.admit_for_registry(&record.coordinate).is_err())
    {
        return Err(ProductAdmissionError::ForgeAssociation);
    }
    Ok(())
}

fn dependency_record_bound(record: &crate::PackageDependencyRecord) -> usize {
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.source))
        .saturating_add(text_bound(&record.target.name))
        .saturating_add(text_bound(&record.target.requirement))
        .saturating_add(
            record
                .target
                .resolved
                .as_ref()
                .map_or(0, package_reference_bound),
        )
}

fn subscription_record_bound(record: &SubscriptionRecord) -> usize {
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.package))
        .saturating_add(optional_text_bound(record.seen.as_ref()))
}

fn release_record_bound(record: &ReleaseRecord) -> usize {
    fixed_record_bound()
        .saturating_add(package_reference_bound(&record.package))
        .saturating_add(text_bound(&record.version))
}

fn project_record_bound(record: &ProjectRecord) -> usize {
    record.members.iter().fold(
        fixed_record_bound()
            .saturating_add(record.name.as_str().len())
            .saturating_add(optional_text_bound(record.lockfile.as_ref())),
        |bound, package| bound.saturating_add(package_reference_bound(package)),
    )
}

fn tree_node_record_bound(record: &TreeNodeRecord) -> usize {
    fixed_record_bound()
        .saturating_add(tree_subject_bound(&record.subject))
        .saturating_add(text_bound(&record.title))
        .saturating_add(match &record.opener {
            TreeOpener::Mcp(client) => text_bound(client),
            TreeOpener::Desktop | TreeOpener::Cli => 32,
        })
}

fn tree_subject_bound(subject: &TreeSubject) -> usize {
    match subject {
        TreeSubject::Package(package) => package_reference_bound(package),
        TreeSubject::Declaration(text) | TreeSubject::Search(text) | TreeSubject::Owner(text) => {
            text_bound(text)
        }
        TreeSubject::Explore(query) => optional_text_bound(query.as_ref()),
    }
}
