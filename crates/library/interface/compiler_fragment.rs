//! Typed, bounded projection of compact-fragment construction failures.
//!
//! The concrete semantic/IR error stays available to in-process consumers.
//! Wire consumers receive its closed phase family, the operands we can safely
//! name as coordinates or capacity facts, and a bounded human explanation.

use serde::{Deserialize, Serialize};
use std::{fmt, fmt::Write as _};

use backend_semantic::ir::{
    BuildError, FragmentError, LayoutStep, PrepareError, SemanticSpace, WriteError,
};

use super::FragmentCause;

/// Concrete compact-fragment failure retained after the driver's scratch lease ends.
#[derive(Debug, Eq, PartialEq)]
pub enum CompilerFragmentFault {
    /// Owned semantic IR construction failed before compact-fragment preparation.
    Build(BuildError),
    /// Compact-fragment input validation or layout preparation failed.
    Prepare(PrepareError),
    /// Writing the prepared fragment into caller-owned bytes failed.
    Write(WriteError),
    /// Reopening the newly written fragment rejected its bytes.
    Validate(FragmentError),
}

/// Exact phase plus the original closed semantic/IR error.
#[derive(Debug, Eq, PartialEq)]
pub struct CompilerFragmentFailure {
    fault: CompilerFragmentFault,
}

impl CompilerFragmentFailure {
    /// Retains an owned IR build error as a preparation-phase compiler terminal.
    #[must_use]
    pub const fn build(fault: BuildError) -> Self {
        Self {
            fault: CompilerFragmentFault::Build(fault),
        }
    }

    /// Retains an exact compact-fragment preparation error.
    #[must_use]
    pub const fn prepare(fault: PrepareError) -> Self {
        Self {
            fault: CompilerFragmentFault::Prepare(fault),
        }
    }

    /// Retains an exact compact-fragment write error.
    #[must_use]
    pub const fn write(fault: WriteError) -> Self {
        Self {
            fault: CompilerFragmentFault::Write(fault),
        }
    }

    /// Retains an exact fresh-fragment validation error.
    #[must_use]
    pub const fn validate(fault: FragmentError) -> Self {
        Self {
            fault: CompilerFragmentFault::Validate(fault),
        }
    }

    /// Returns the existing protocol phase, derived from the retained error so an
    /// inconsistent phase/cause pair cannot be constructed.
    #[must_use]
    pub const fn phase(&self) -> FragmentCause {
        match &self.fault {
            CompilerFragmentFault::Build(_) | CompilerFragmentFault::Prepare(_) => {
                FragmentCause::Prepare
            }
            CompilerFragmentFault::Write(_) => FragmentCause::Write,
            CompilerFragmentFault::Validate(_) => FragmentCause::Validate,
        }
    }

    /// Returns the closed concrete semantic/IR error family and all of its original operands.
    #[must_use]
    pub const fn fault(&self) -> &CompilerFragmentFault {
        &self.fault
    }

    /// Returns the stable, specific error variant represented on the wire.
    ///
    /// The match is exhaustive across the semantic/IR error enums, so adding an
    /// IR variant requires an explicit diagnostic projection here.
    #[must_use]
    pub const fn kind(&self) -> CompilerFragmentFaultKind {
        match &self.fault {
            CompilerFragmentFault::Build(error) => CompilerFragmentFaultKind::Build(match error {
                BuildError::Capacity(_) => BuildFaultKind::Capacity,
                BuildError::InvalidTreeEntity { .. } => BuildFaultKind::InvalidTreeEntity,
                BuildError::Dangling { .. } => BuildFaultKind::Dangling,
                BuildError::RecursiveType { .. } => BuildFaultKind::RecursiveType,
                BuildError::CallableElement { .. } => BuildFaultKind::CallableElement,
                BuildError::MissingTypedVariadicParameter { .. } => {
                    BuildFaultKind::MissingTypedVariadicParameter
                }
                BuildError::EmptyQualifiedPath => BuildFaultKind::EmptyQualifiedPath,
                BuildError::TypeParameterRequirements { .. } => {
                    BuildFaultKind::TypeParameterRequirements
                }
                BuildError::EmptyCxxQualification => BuildFaultKind::EmptyCxxQualification,
                BuildError::IllegalCQualifierTarget { .. } => {
                    BuildFaultKind::IllegalCQualifierTarget
                }
                BuildError::IllegalCxxMemberPointerOwner { .. } => {
                    BuildFaultKind::IllegalCxxMemberPointerOwner
                }
                BuildError::InvalidDocumentationUtf8 { .. } => {
                    BuildFaultKind::InvalidDocumentationUtf8
                }
                BuildError::InvalidOccurrenceSpan { .. } => BuildFaultKind::InvalidOccurrenceSpan,
                BuildError::SignatureCarrierRoleCount { .. } => {
                    BuildFaultKind::SignatureCarrierRoleCount
                }
                BuildError::SignatureCarrierRoleKind { .. } => {
                    BuildFaultKind::SignatureCarrierRoleKind
                }
                BuildError::SignatureCarrierRoleOwnerKind { .. } => {
                    BuildFaultKind::SignatureCarrierRoleOwnerKind
                }
                BuildError::SignatureCarrierRoleAlreadyCaptured => {
                    BuildFaultKind::SignatureCarrierRoleAlreadyCaptured
                }
                BuildError::SignatureCarrierBindingsAlreadyCaptured => {
                    BuildFaultKind::SignatureCarrierBindingsAlreadyCaptured
                }
                BuildError::SignatureCarrierBindingOwnerSet { .. } => {
                    BuildFaultKind::SignatureCarrierBindingOwnerSet
                }
                BuildError::SignatureCarrierBindingSignature { .. } => {
                    BuildFaultKind::SignatureCarrierBindingSignature
                }
                BuildError::SignatureCarrierBindingCounts { .. } => {
                    BuildFaultKind::SignatureCarrierBindingCounts
                }
                BuildError::SignatureCarrierBindingEdgeRole { .. } => {
                    BuildFaultKind::SignatureCarrierBindingEdgeRole
                }
                BuildError::SignatureCarrierBindingTargetCount { .. } => {
                    BuildFaultKind::SignatureCarrierBindingTargetCount
                }
                BuildError::SignatureCarrierBindingTargetKind { .. } => {
                    BuildFaultKind::SignatureCarrierBindingTargetKind
                }
                BuildError::SignatureCarrierBindingType { .. } => {
                    BuildFaultKind::SignatureCarrierBindingType
                }
                BuildError::SignatureCarrierBindingEdgeMismatch { .. } => {
                    BuildFaultKind::SignatureCarrierBindingEdgeMismatch
                }
                BuildError::ParentCycle { .. } => BuildFaultKind::ParentCycle,
                BuildError::DeclarationKey { .. } => BuildFaultKind::DeclarationKey,
                BuildError::ScopedDeclarationPreimage { .. } => {
                    BuildFaultKind::ScopedDeclarationPreimage
                }
                BuildError::ForeignKeyPreimage { .. } => BuildFaultKind::ForeignKeyPreimage,
                BuildError::DuplicateDeclarationIdentity { .. } => {
                    BuildFaultKind::DuplicateDeclarationIdentity
                }
                BuildError::TreeVersionCount { .. } => BuildFaultKind::TreeVersionCount,
                BuildError::AuthorityRowCount { .. } => BuildFaultKind::AuthorityRowCount,
                BuildError::OccurrenceAuthorityRowCount { .. } => {
                    BuildFaultKind::OccurrenceAuthorityRowCount
                }
                BuildError::AuthorityFacts { .. } => BuildFaultKind::AuthorityFacts,
                BuildError::OccurrenceAuthorityFacts { .. } => {
                    BuildFaultKind::OccurrenceAuthorityFacts
                }
                BuildError::ImageProvenanceLineage { .. } => BuildFaultKind::ImageProvenanceLineage,
                BuildError::ImageProvenanceScope { .. } => BuildFaultKind::ImageProvenanceScope,
                BuildError::ImageProvenanceScopePreimage { .. } => {
                    BuildFaultKind::ImageProvenanceScopePreimage
                }
                BuildError::ImageProvenanceRecipe { .. } => BuildFaultKind::ImageProvenanceRecipe,
                BuildError::ImageProvenanceRebind { .. } => BuildFaultKind::ImageProvenanceRebind,
                BuildError::LanguageExtension { .. } => BuildFaultKind::LanguageExtension,
                BuildError::LanguageProfileMismatch { .. } => {
                    BuildFaultKind::LanguageProfileMismatch
                }
                BuildError::LanguageProfileRebind { .. } => BuildFaultKind::LanguageProfileRebind,
            }),
            CompilerFragmentFault::Prepare(error) => {
                CompilerFragmentFaultKind::Prepare(match error {
                    PrepareError::Count { .. } => PrepareFaultKind::Count,
                    PrepareError::AtomBytePoolOverflow { .. } => {
                        PrepareFaultKind::AtomBytePoolOverflow
                    }
                    PrepareError::LayoutOverflow { .. } => PrepareFaultKind::LayoutOverflow,
                    PrepareError::NativeCount { .. } => PrepareFaultKind::NativeCount,
                    PrepareError::OutputLength { .. } => PrepareFaultKind::OutputLength,
                    PrepareError::Entity { .. } => PrepareFaultKind::Entity,
                    PrepareError::TypeNode { .. } => PrepareFaultKind::TypeNode,
                    PrepareError::SemanticDataOverflow { .. } => {
                        PrepareFaultKind::SemanticDataOverflow
                    }
                    PrepareError::SemanticEntityRoots { .. } => {
                        PrepareFaultKind::SemanticEntityRoots
                    }
                    PrepareError::SemanticAtomLength { .. } => PrepareFaultKind::SemanticAtomLength,
                    PrepareError::SemanticData { .. } => PrepareFaultKind::SemanticData,
                    PrepareError::OccurrenceLane { .. } => PrepareFaultKind::OccurrenceLane,
                    PrepareError::TypeFacts { .. } => PrepareFaultKind::TypeFacts,
                    PrepareError::Documentation { .. } => PrepareFaultKind::Documentation,
                    PrepareError::ExtensionPools { .. } => PrepareFaultKind::ExtensionPools,
                    PrepareError::ExtensionPoolsMismatch => {
                        PrepareFaultKind::ExtensionPoolsMismatch
                    }
                })
            }
            CompilerFragmentFault::Write(error) => CompilerFragmentFaultKind::Write(match error {
                WriteError::OutputTooSmall { .. } => WriteFaultKind::OutputTooSmall,
                WriteError::ExtensionSection { .. } => WriteFaultKind::ExtensionSection,
                WriteError::AtomLength { .. } => WriteFaultKind::AtomLength,
                WriteError::AtomExtent { .. } => WriteFaultKind::AtomExtent,
                WriteError::SemanticAtomLength { .. } => WriteFaultKind::SemanticAtomLength,
            }),
            CompilerFragmentFault::Validate(error) => {
                CompilerFragmentFaultKind::Validate(match error {
                    FragmentError::ExtensionPoolPair { .. } => ValidateFaultKind::ExtensionPoolPair,
                    FragmentError::Documentation { .. } => ValidateFaultKind::Documentation,
                    FragmentError::ExtensionPools { .. } => ValidateFaultKind::ExtensionPools,
                    FragmentError::LanguageExtensions { .. } => {
                        ValidateFaultKind::LanguageExtensions
                    }
                    FragmentError::TruncatedHeader { .. } => ValidateFaultKind::TruncatedHeader,
                    FragmentError::Magic { .. } => ValidateFaultKind::Magic,
                    FragmentError::Schema { .. } => ValidateFaultKind::Schema,
                    FragmentError::DeclaredLength { .. } => ValidateFaultKind::DeclaredLength,
                    FragmentError::Extent { .. } => ValidateFaultKind::Extent,
                    FragmentError::WireWidth { .. } => ValidateFaultKind::WireWidth,
                    FragmentError::Directory { .. } => ValidateFaultKind::Directory,
                    FragmentError::MissingSection { .. } => ValidateFaultKind::MissingSection,
                    FragmentError::Entity { .. } => ValidateFaultKind::Entity,
                    FragmentError::EntityRecord { .. } => ValidateFaultKind::EntityRecord,
                    FragmentError::Atom { .. } => ValidateFaultKind::Atom,
                    FragmentError::TypeNode { .. } => ValidateFaultKind::TypeNode,
                    FragmentError::SourceIdentity { .. } => ValidateFaultKind::SourceIdentity,
                    FragmentError::RecipeFact { .. } => ValidateFaultKind::RecipeFact,
                    FragmentError::SemanticData { .. } => ValidateFaultKind::SemanticData,
                    FragmentError::Occurrences { .. } => ValidateFaultKind::Occurrences,
                    FragmentError::TypeFacts { .. } => ValidateFaultKind::TypeFacts,
                })
            }
        }
    }

    /// Returns only the coordinate or capacity facts with stable names at this boundary.
    /// Other exact operands remain available through [`Self::fault`].
    #[must_use]
    pub fn facts(&self) -> CompilerFragmentFaultFacts {
        match &self.fault {
            CompilerFragmentFault::Build(BuildError::InvalidTreeEntity { raw, count }) => {
                CompilerFragmentFaultFacts::TreeEntity {
                    raw: *raw,
                    count: *count,
                }
            }
            CompilerFragmentFault::Build(BuildError::Dangling { space, raw }) => {
                CompilerFragmentFaultFacts::Dangling {
                    space: semantic_space(*space),
                    raw: *raw,
                }
            }
            CompilerFragmentFault::Build(BuildError::InvalidOccurrenceSpan {
                owner,
                start,
                end,
            }) => CompilerFragmentFaultFacts::OccurrenceSpan {
                owner: owner.raw,
                start: *start,
                end: *end,
            },
            CompilerFragmentFault::Build(BuildError::SignatureCarrierRoleCount {
                expected,
                observed,
            }) => nested_facts(
                CompilerFragmentNestedFaultKind::SignatureCarrierRoleCount,
                None,
                None,
                None,
                None,
                Some(*expected as u64),
                Some(*observed as u64),
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Build(BuildError::SignatureCarrierRoleKind { entity, kind }) => {
                nested_facts(
                    CompilerFragmentNestedFaultKind::SignatureCarrierRoleKind,
                    None,
                    Some(entity.raw),
                    None,
                    None,
                    None,
                    Some(u64::from(u16::from(*kind))),
                    None,
                    None,
                    None,
                    None,
                )
            }
            CompilerFragmentFault::Build(BuildError::SignatureCarrierRoleOwnerKind {
                owner,
                kind,
            }) => nested_facts(
                CompilerFragmentNestedFaultKind::SignatureCarrierRoleOwnerKind,
                None,
                Some(owner.raw),
                None,
                None,
                None,
                Some(u64::from(u16::from(*kind))),
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Build(BuildError::SignatureCarrierBindingOwnerSet {
                row,
                expected,
                observed,
            }) => nested_facts(
                CompilerFragmentNestedFaultKind::SignatureCarrierBindingOwnerSet,
                Some(usize_as_u64(*row)),
                None,
                expected.map(|entity| entity.raw),
                observed.map(|entity| entity.raw),
                None,
                None,
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Build(BuildError::SignatureCarrierBindingSignature {
                owner,
            }) => nested_facts(
                CompilerFragmentNestedFaultKind::SignatureCarrierBindingSignature,
                None,
                Some(owner.raw),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Build(BuildError::SignatureCarrierBindingCounts {
                owner,
                parameters,
                results,
            }) => nested_facts(
                CompilerFragmentNestedFaultKind::SignatureCarrierBindingCounts,
                None,
                Some(owner.raw),
                None,
                None,
                Some(u64::from(*parameters)),
                Some(u64::from(*results)),
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Build(BuildError::SignatureCarrierBindingEdgeRole {
                owner,
                position,
                expected,
                observed,
            }) => nested_facts(
                CompilerFragmentNestedFaultKind::SignatureCarrierBindingEdgeRole,
                Some(u64::from(*position)),
                Some(owner.raw),
                None,
                None,
                Some(u64::from(child_role(*expected) as u8)),
                Some(u64::from(child_role(*observed) as u8)),
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Build(BuildError::SignatureCarrierBindingTargetCount {
                expected,
                observed,
            }) => nested_facts(
                CompilerFragmentNestedFaultKind::SignatureCarrierBindingTargetCount,
                None,
                None,
                None,
                None,
                Some(usize_as_u64(*expected)),
                Some(usize_as_u64(*observed)),
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Build(BuildError::SignatureCarrierBindingTargetKind {
                owner,
                carrier,
                kind,
            }) => nested_facts(
                CompilerFragmentNestedFaultKind::SignatureCarrierBindingTargetKind,
                None,
                Some(owner.raw),
                Some(carrier.raw),
                None,
                None,
                Some(u64::from(u16::from(*kind))),
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Build(BuildError::SignatureCarrierBindingType {
                owner,
                carrier,
                role,
                position,
            }) => nested_facts(
                CompilerFragmentNestedFaultKind::SignatureCarrierBindingType,
                Some(u64::from(*position)),
                Some(owner.raw),
                Some(carrier.raw),
                None,
                None,
                Some(u64::from(signature_role(*role) as u8)),
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Build(BuildError::SignatureCarrierBindingEdgeMismatch {
                owner,
                position,
                product_target,
                type_target,
            }) => nested_facts(
                CompilerFragmentNestedFaultKind::SignatureCarrierBindingEdgeMismatch,
                Some(u64::from(*position)),
                Some(owner.raw),
                Some(*product_target),
                Some(*type_target),
                None,
                None,
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Prepare(PrepareError::Count { lane, actual, .. }) => {
                CompilerFragmentFaultFacts::Count {
                    lane: layout_step(*lane),
                    actual: *actual,
                    maximum: u32::MAX,
                }
            }
            CompilerFragmentFault::Prepare(PrepareError::AtomBytePoolOverflow { ordinal }) => {
                CompilerFragmentFaultFacts::RecordCoordinate {
                    lane: CompilerFragmentRecordLane::Atom,
                    ordinal: ordinal.raw,
                }
            }
            CompilerFragmentFault::Prepare(PrepareError::LayoutOverflow {
                step,
                entity_count,
                type_node_count,
            }) => CompilerFragmentFaultFacts::LayoutOverflow {
                step: layout_step(*step),
                entity_count: *entity_count,
                type_node_count: *type_node_count,
            },
            CompilerFragmentFault::Prepare(PrepareError::NativeCount { step, actual, .. }) => {
                CompilerFragmentFaultFacts::NativeCount {
                    step: layout_step(*step),
                    actual: *actual,
                    maximum: usize::MAX as u64,
                }
            }
            CompilerFragmentFault::Prepare(PrepareError::OutputLength { actual, .. }) => {
                CompilerFragmentFaultFacts::OutputLength {
                    actual: *actual,
                    maximum: u32::MAX,
                }
            }
            CompilerFragmentFault::Prepare(PrepareError::Entity { ordinal, fault }) => {
                entity_record_facts(ordinal.raw, *fault)
            }
            CompilerFragmentFault::Prepare(PrepareError::TypeNode { ordinal, fault }) => {
                type_node_facts(ordinal.raw, *fault)
            }
            CompilerFragmentFault::Prepare(PrepareError::SemanticData { cause }) => {
                canonical_data_facts(cause)
            }
            CompilerFragmentFault::Prepare(PrepareError::SemanticDataOverflow {
                atoms,
                products,
                children,
            }) => CompilerFragmentFaultFacts::SemanticDataOverflow {
                atoms: *atoms,
                products: *products,
                children: *children,
            },
            CompilerFragmentFault::Prepare(PrepareError::SemanticEntityRoots {
                roots,
                entities,
            }) => CompilerFragmentFaultFacts::SemanticEntityRoots {
                roots: *roots,
                entities: *entities,
            },
            CompilerFragmentFault::Prepare(PrepareError::SemanticAtomLength {
                ordinal,
                actual,
                ..
            }) => CompilerFragmentFaultFacts::AtomLength {
                ordinal: ordinal.raw,
                actual: *actual,
                maximum: u32::MAX,
            },
            CompilerFragmentFault::Prepare(PrepareError::OccurrenceLane { ordinal, fault }) => {
                occurrence_facts(*ordinal, *fault)
            }
            CompilerFragmentFault::Write(WriteError::OutputTooSmall {
                required,
                available,
            }) => CompilerFragmentFaultFacts::OutputTooSmall {
                required: *required,
                available: *available,
            },
            CompilerFragmentFault::Write(WriteError::AtomLength {
                ordinal, actual, ..
            })
            | CompilerFragmentFault::Write(WriteError::SemanticAtomLength {
                ordinal,
                actual,
                ..
            }) => CompilerFragmentFaultFacts::AtomLength {
                ordinal: ordinal.raw,
                actual: *actual,
                maximum: u32::MAX,
            },
            CompilerFragmentFault::Write(WriteError::AtomExtent { ordinal }) => {
                CompilerFragmentFaultFacts::AtomCoordinate {
                    ordinal: ordinal.raw,
                }
            }
            CompilerFragmentFault::Validate(FragmentError::Entity { ordinal, fault }) => {
                entity_record_facts(
                    ordinal.raw,
                    backend_semantic::ir::EntityRecordFault::Type(*fault),
                )
            }
            CompilerFragmentFault::Validate(FragmentError::EntityRecord { ordinal, fault }) => {
                entity_record_facts(ordinal.raw, *fault)
            }
            CompilerFragmentFault::Validate(FragmentError::Atom { fault }) => atom_facts(*fault),
            CompilerFragmentFault::Validate(FragmentError::TypeNode { ordinal, fault }) => {
                type_node_facts(ordinal.raw, *fault)
            }
            CompilerFragmentFault::Validate(FragmentError::SemanticData { fault }) => {
                semantic_data_facts(fault)
            }
            CompilerFragmentFault::Validate(FragmentError::Occurrences { fault }) => {
                occurrence_facts(0, *fault)
            }
            CompilerFragmentFault::Validate(FragmentError::TruncatedHeader {
                required,
                actual,
            }) => nested_facts(
                CompilerFragmentNestedFaultKind::ValidateTruncatedHeader,
                None,
                None,
                None,
                None,
                Some(usize_as_u64(*required)),
                Some(usize_as_u64(*actual)),
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Validate(FragmentError::Magic { actual }) => {
                let encoded = u32::from_le_bytes(*actual);
                nested_facts(
                    CompilerFragmentNestedFaultKind::ValidateMagic,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(u64::from(encoded)),
                    None,
                    None,
                    None,
                    None,
                )
            }
            CompilerFragmentFault::Validate(FragmentError::Schema { actual }) => nested_facts(
                CompilerFragmentNestedFaultKind::ValidateSchema,
                None,
                None,
                None,
                None,
                None,
                Some(u64::from(*actual)),
                None,
                None,
                None,
                None,
            ),
            CompilerFragmentFault::Validate(FragmentError::DeclaredLength { declared, actual }) => {
                nested_facts(
                    CompilerFragmentNestedFaultKind::ValidateDeclaredLength,
                    None,
                    None,
                    None,
                    None,
                    Some(usize_as_u64(*declared)),
                    Some(usize_as_u64(*actual)),
                    None,
                    None,
                    None,
                    None,
                )
            }
            CompilerFragmentFault::Validate(FragmentError::Extent { required, actual }) => {
                nested_facts(
                    CompilerFragmentNestedFaultKind::ValidateExtent,
                    None,
                    None,
                    None,
                    None,
                    Some(usize_as_u64(*required)),
                    Some(usize_as_u64(*actual)),
                    None,
                    None,
                    None,
                    None,
                )
            }
            CompilerFragmentFault::Validate(FragmentError::WireWidth { actual, .. }) => {
                nested_facts(
                    CompilerFragmentNestedFaultKind::ValidateWireWidth,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(u64::from(*actual)),
                    None,
                    None,
                    None,
                    None,
                )
            }
            CompilerFragmentFault::Build(
                BuildError::Capacity(_)
                | BuildError::RecursiveType { .. }
                | BuildError::CallableElement { .. }
                | BuildError::MissingTypedVariadicParameter { .. }
                | BuildError::EmptyQualifiedPath
                | BuildError::TypeParameterRequirements { .. }
                | BuildError::EmptyCxxQualification
                | BuildError::IllegalCQualifierTarget { .. }
                | BuildError::IllegalCxxMemberPointerOwner { .. }
                | BuildError::InvalidDocumentationUtf8 { .. }
                | BuildError::SignatureCarrierRoleAlreadyCaptured
                | BuildError::SignatureCarrierBindingsAlreadyCaptured
                | BuildError::ParentCycle { .. }
                | BuildError::DeclarationKey { .. }
                | BuildError::ScopedDeclarationPreimage { .. }
                | BuildError::ForeignKeyPreimage { .. }
                | BuildError::DuplicateDeclarationIdentity { .. }
                | BuildError::TreeVersionCount { .. }
                | BuildError::AuthorityRowCount { .. }
                | BuildError::OccurrenceAuthorityRowCount { .. }
                | BuildError::AuthorityFacts { .. }
                | BuildError::OccurrenceAuthorityFacts { .. }
                | BuildError::ImageProvenanceLineage { .. }
                | BuildError::ImageProvenanceScope { .. }
                | BuildError::ImageProvenanceScopePreimage { .. }
                | BuildError::ImageProvenanceRecipe { .. }
                | BuildError::ImageProvenanceRebind { .. }
                | BuildError::LanguageExtension { .. }
                | BuildError::LanguageProfileMismatch { .. }
                | BuildError::LanguageProfileRebind { .. },
            )
            | CompilerFragmentFault::Prepare(
                PrepareError::TypeFacts { .. }
                | PrepareError::Documentation { .. }
                | PrepareError::ExtensionPools { .. }
                | PrepareError::ExtensionPoolsMismatch,
            )
            | CompilerFragmentFault::Write(WriteError::ExtensionSection { .. })
            | CompilerFragmentFault::Validate(
                FragmentError::ExtensionPoolPair { .. }
                | FragmentError::Documentation { .. }
                | FragmentError::ExtensionPools { .. }
                | FragmentError::LanguageExtensions { .. }
                | FragmentError::Directory { .. }
                | FragmentError::MissingSection { .. }
                | FragmentError::SourceIdentity { .. }
                | FragmentError::RecipeFact { .. }
                | FragmentError::TypeFacts { .. },
            ) => CompilerFragmentFaultFacts::None,
        }
    }

    /// Captures the source error's human explanation into a fixed byte budget.
    /// The original typed cause, not this secondary text, is the diagnostic authority.
    #[must_use]
    pub fn detail(&self) -> BoundedCompilerFragmentDetail {
        let mut detail = BoundedCompilerFragmentDetail::new();
        let fault: &dyn fmt::Display = match &self.fault {
            CompilerFragmentFault::Build(fault) => fault,
            CompilerFragmentFault::Prepare(fault) => fault,
            CompilerFragmentFault::Write(fault) => fault,
            CompilerFragmentFault::Validate(fault) => fault,
        };
        if !detail.truncated && (write!(&mut detail, "{fault}").is_err() || detail.text.is_empty())
        {
            let _ = detail.write_str("fragment failure detail unavailable");
        }
        detail
    }
}

/// Bounded named operands for common relation, layout, and capacity failures.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompilerFragmentFaultFacts {
    /// No additional scalar operands are projected at this boundary.
    None,
    /// A tree-local entity coordinate was outside its entity table.
    TreeEntity {
        /// Rejected entity coordinate.
        raw: u32,
        /// Number of available entities.
        count: u32,
    },
    /// A semantic-pool reference was dangling.
    Dangling {
        /// Closed semantic pool containing the rejected coordinate.
        space: CompilerFragmentSemanticSpace,
        /// Rejected coordinate in that pool.
        raw: u32,
    },
    /// An occurrence span escaped the captured source extent of its owner.
    OccurrenceSpan {
        /// Entity whose source extent owns the occurrence.
        owner: u32,
        /// Inclusive occurrence start byte.
        start: u32,
        /// Exclusive occurrence end byte.
        end: u32,
    },
    /// A count could not fit the selected fragment lane's encoded width.
    Count {
        /// Lane whose count is being encoded.
        lane: CompilerFragmentLayoutStep,
        /// Original count before narrowing.
        actual: usize,
        /// Largest count representable in the fragment lane.
        maximum: u32,
    },
    /// A row coordinate retained for an error whose typed cause has more operands.
    RecordCoordinate {
        /// Fragment lane containing the rejected record.
        lane: CompilerFragmentRecordLane,
        /// Rejected record ordinal.
        ordinal: u32,
    },
    /// Layout arithmetic overflowed at one exact fragment step.
    LayoutOverflow {
        /// Layout step whose offset or extent overflowed.
        step: CompilerFragmentLayoutStep,
        /// Entity row count in this layout.
        entity_count: u32,
        /// Type-node row count in this layout.
        type_node_count: u32,
    },
    /// A valid wire count did not fit the platform's native address width.
    NativeCount {
        /// Lane whose count did not fit.
        step: CompilerFragmentLayoutStep,
        /// Rejected wire count.
        actual: u32,
        /// Largest count accepted by the current process.
        maximum: u64,
    },
    /// The required output extent did not fit the wire's byte-coordinate width.
    OutputLength {
        /// Required complete fragment byte length.
        actual: usize,
        /// Largest complete fragment size addressable by the wire format.
        maximum: u32,
    },
    /// Canonical semantic graph dimensions overflowed the payload layout.
    SemanticDataOverflow {
        /// Semantic atom count.
        atoms: u32,
        /// Semantic product count.
        products: u32,
        /// Semantic child count.
        children: u32,
    },
    /// Semantic graph root count differed from the fragment entity count.
    SemanticEntityRoots {
        /// Number of supplied semantic roots.
        roots: u32,
        /// Number of fragment entities requiring roots.
        entities: u32,
    },
    /// An atom byte length could not fit its wire field.
    AtomLength {
        /// Atom coordinate whose extent failed.
        ordinal: u32,
        /// Original atom length in bytes.
        actual: usize,
        /// Largest atom byte length accepted by the wire format.
        maximum: u32,
    },
    /// A caller output slice could not fit the exact prepared fragment.
    OutputTooSmall {
        /// Required output capacity in bytes.
        required: usize,
        /// Supplied output capacity in bytes.
        available: usize,
    },
    /// A prepared atom extent no longer fit its assigned byte region.
    AtomCoordinate {
        /// Atom coordinate whose extent changed.
        ordinal: u32,
    },
    /// Exact nested semantic, record, or signature-validation subcause.
    /// The closed `fault` tag determines each optional operand's meaning;
    /// raw names, identity bytes, and arbitrary user text are never retained.
    Nested {
        /// Exact closed nested reason tag.
        fault: CompilerFragmentNestedFaultKind,
        /// Closed semantic-data lane, when this reason is lane-specific.
        lane: Option<CompilerFragmentDataLane>,
        /// Closed canonicalization budget resource, when this is a budget fault.
        resource: Option<CompilerFragmentDataResource>,
        /// Row or sequence position when the nested reason has one.
        ordinal: Option<u64>,
        /// Owning entity coordinate when present.
        owner: Option<u32>,
        /// Rejected target coordinate when present.
        target: Option<u32>,
        /// Second coordinate or lane position when present.
        second_coordinate: Option<u64>,
        /// Expected count, role, or width when present.
        expected: Option<u64>,
        /// Observed count, role, or discriminant when present.
        actual: Option<u64>,
        /// Available row count when present.
        count: Option<u64>,
        /// Declared limit when present.
        limit: Option<u64>,
        /// Required capacity when present.
        required: Option<u64>,
        /// Supplied capacity when present.
        available: Option<u64>,
    },
}

/// Closed canonical semantic-data lane retained by nested admission faults.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerFragmentDataLane {
    Atoms,
    Products,
    Constructors,
    Lists,
    Children,
    AtomOrder,
    AtomMap,
    ProductOrder,
    ProductMap,
    Colors,
    NextColors,
    Hashes,
    NextHashes,
    Representatives,
    InternSlots,
}

/// Closed resource counter retained by a canonical semantic-data budget fault.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerFragmentDataResource {
    RefinementRounds,
    SortComparisons,
    HashEvaluations,
    InternProbes,
    Work,
}

/// Closed nested IR reason tag; the exact outer fault remains in `kind`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerFragmentNestedFaultKind {
    EntityTypeReference,
    EntityNameReference,
    EntityKindTag,
    EntityReservedBits,
    TypeNodeReservedBytes,
    TypeNodeTag,
    TypeNodePrimitive,
    TypeNodeEdge,
    AtomRange,
    AtomEmpty,
    CanonicalDataCount,
    CanonicalDataNativeCount,
    CanonicalDataNativeWork,
    CanonicalDataCanonicalCountOverflow,
    CanonicalDataCanonicalCountMismatch,
    CanonicalDataScratch,
    CanonicalDataOutputTooSmall,
    CanonicalDataProductHead,
    CanonicalDataProductList,
    CanonicalDataConstructorCount,
    CanonicalDataProductConstructor,
    CanonicalDataConstructorTag,
    CanonicalDataConstructorReservedPayload,
    CanonicalDataConstructorArityOverflow,
    CanonicalDataConstructorArity,
    CanonicalDataProductChildRole,
    CanonicalDataListExtent,
    CanonicalDataNativeExtent,
    CanonicalDataProductChild,
    CanonicalDataOutputLength,
    CanonicalDataCanonicalListExtent,
    CanonicalDataCanonicalAtom,
    CanonicalDataCanonicalProduct,
    CanonicalDataCanonicalList,
    CanonicalDataRefinementBound,
    CanonicalDataInternTableFull,
    CanonicalDataInternEntry,
    CanonicalDataResourceCounterOverflow,
    CanonicalDataBudgetAdmission,
    CanonicalDataBudgetExceeded,
    SemanticDataHeader,
    SemanticDataAtomLength,
    SemanticDataProductHead,
    SemanticDataProductList,
    SemanticDataConstructorCount,
    SemanticDataEntityRootCount,
    SemanticDataEntityRoot,
    SemanticDataConstructor,
    SemanticDataConstructorTag,
    SemanticDataConstructorReservedPayload,
    SemanticDataConstructorArityOverflow,
    SemanticDataConstructorArity,
    SemanticDataListExtent,
    SemanticDataChildRoleCode,
    SemanticDataChildRole,
    SemanticDataChildTag,
    SemanticDataLocalChild,
    SemanticDataLocalReserved,
    SemanticDataExternalAuthority,
    SemanticDataTrailing,
    OccurrenceOwner,
    OccurrenceLocalTarget,
    OccurrenceTargetTag,
    OccurrenceOriginTag,
    OccurrenceReferenceKind,
    OccurrenceConfidence,
    OccurrenceSpan,
    OccurrenceKindCell,
    OccurrenceEmptyPath,
    OccurrenceTruncated,
    OccurrenceTrailingBytes,
    OccurrenceLegacyStableTarget,
    OccurrenceAuthorityDomain,
    OccurrenceAuthorityWidth,
    TypeFactCountOverflow,
    TypeFactOwner,
    TypeFactRecord,
    TypeFactChildSpan,
    TypeFactForwardReference,
    TypeFactComputedTargetFromDeclared,
    TypeFactComputedForwardReference,
    TypeFactChildTargetOutOfRange,
    TypeFactNominalForward,
    TypeFactNominalOutOfRange,
    TypeFactAuthority,
    TypeFactTruncated,
    TypeFactTrailingBytes,
    TypeFactTag,
    TypeFactText,
    ValidateTruncatedHeader,
    ValidateMagic,
    ValidateSchema,
    ValidateDeclaredLength,
    ValidateExtent,
    ValidateWireWidth,
    DocumentationOwner,
    DocumentationFragmentTag,
    DocumentationLinkTag,
    DocumentationEmptyCell,
    DocumentationLinkTarget,
    DocumentationTruncated,
    DocumentationTrailingBytes,
    DocumentationPresence,
    ExtensionPoolEmptyName,
    ExtensionPoolTypeReference,
    ExtensionPoolBoundTypeReference,
    ExtensionPoolParameterBounds,
    ExtensionPoolEmptyLifetime,
    ExtensionPoolBoundPosition,
    ExtensionPoolFreePredicateSubject,
    ExtensionPoolFreePredicateBounds,
    ExtensionPoolFreePredicateList,
    ExtensionPoolParameterRequirements,
    ExtensionPoolParameterTag,
    ExtensionPoolParameterRange,
    SignatureCarrierRoleCount,
    SignatureCarrierRoleKind,
    SignatureCarrierRoleOwnerKind,
    SignatureCarrierBindingOwnerSet,
    SignatureCarrierBindingSignature,
    SignatureCarrierBindingCounts,
    SignatureCarrierBindingEdgeRole,
    SignatureCarrierBindingTargetCount,
    SignatureCarrierBindingTargetKind,
    SignatureCarrierBindingType,
    SignatureCarrierBindingEdgeMismatch,
}

/// Specific outer error variant retained by the stable compiler-fault projection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "family",
    content = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CompilerFragmentFaultKind {
    /// Owned semantic IR construction error.
    Build(BuildFaultKind),
    /// Fragment preparation error.
    Prepare(PrepareFaultKind),
    /// Fragment write error.
    Write(WriteFaultKind),
    /// Fresh-fragment validation error.
    Validate(ValidateFaultKind),
}

/// Named `BuildError` variant; adding an IR variant requires updating [`CompilerFragmentFailure::kind`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildFaultKind {
    Capacity,
    InvalidTreeEntity,
    Dangling,
    RecursiveType,
    CallableElement,
    MissingTypedVariadicParameter,
    EmptyQualifiedPath,
    TypeParameterRequirements,
    EmptyCxxQualification,
    IllegalCQualifierTarget,
    IllegalCxxMemberPointerOwner,
    InvalidDocumentationUtf8,
    InvalidOccurrenceSpan,
    SignatureCarrierRoleCount,
    SignatureCarrierRoleKind,
    SignatureCarrierRoleOwnerKind,
    SignatureCarrierRoleAlreadyCaptured,
    SignatureCarrierBindingsAlreadyCaptured,
    SignatureCarrierBindingOwnerSet,
    SignatureCarrierBindingSignature,
    SignatureCarrierBindingCounts,
    SignatureCarrierBindingEdgeRole,
    SignatureCarrierBindingTargetCount,
    SignatureCarrierBindingTargetKind,
    SignatureCarrierBindingType,
    SignatureCarrierBindingEdgeMismatch,
    ParentCycle,
    DeclarationKey,
    ScopedDeclarationPreimage,
    ForeignKeyPreimage,
    DuplicateDeclarationIdentity,
    TreeVersionCount,
    AuthorityRowCount,
    OccurrenceAuthorityRowCount,
    AuthorityFacts,
    OccurrenceAuthorityFacts,
    ImageProvenanceLineage,
    ImageProvenanceScope,
    ImageProvenanceScopePreimage,
    ImageProvenanceRecipe,
    ImageProvenanceRebind,
    LanguageExtension,
    LanguageProfileMismatch,
    LanguageProfileRebind,
}

/// Named `PrepareError` variant; nested typed source errors remain available through [`CompilerFragmentFailure::fault`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrepareFaultKind {
    Count,
    AtomBytePoolOverflow,
    LayoutOverflow,
    NativeCount,
    OutputLength,
    Entity,
    TypeNode,
    SemanticDataOverflow,
    SemanticEntityRoots,
    SemanticAtomLength,
    SemanticData,
    OccurrenceLane,
    TypeFacts,
    Documentation,
    ExtensionPools,
    ExtensionPoolsMismatch,
}

/// Named `WriteError` variant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteFaultKind {
    OutputTooSmall,
    ExtensionSection,
    AtomLength,
    AtomExtent,
    SemanticAtomLength,
}

/// Named `FragmentError` variant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidateFaultKind {
    ExtensionPoolPair,
    Documentation,
    ExtensionPools,
    LanguageExtensions,
    TruncatedHeader,
    Magic,
    Schema,
    DeclaredLength,
    Extent,
    WireWidth,
    Directory,
    MissingSection,
    Entity,
    EntityRecord,
    Atom,
    TypeNode,
    SourceIdentity,
    RecipeFact,
    SemanticData,
    Occurrences,
    TypeFacts,
}

/// Fragment row lane for a named terminal record coordinate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerFragmentRecordLane {
    /// Entity declaration rows.
    Entity,
    /// Type-node rows.
    TypeNode,
    /// Atom descriptors or their byte pool.
    Atom,
    /// Occurrence fact rows.
    Occurrence,
}

/// Wire-safe semantic pool coordinate family.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerFragmentSemanticSpace {
    Atom,
    Text,
    Type,
    Entity,
    External,
    Link,
    LinkOccurrence,
    TypeList,
    EntityList,
    AtomList,
    Docs,
    TupleElements,
    ObjectMembers,
    TemplateParts,
    TypeParameterBounds,
    TypeParameters,
    FreePredicates,
}

/// Wire-safe compact-fragment layout region.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerFragmentLayoutStep {
    Directory,
    EntityLane,
    TypeNodeLane,
    AtomRecordLane,
    AtomByteLane,
    SourceIdentityLane,
    RecipeFactLane,
    SemanticData,
    Occurrences,
    TypeFacts,
    Documentation,
    LanguageExtensions,
    ExtensionPools,
}

/// Specific compact-fragment terminal phase derived from the closed cause.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompilerFragmentFaultPhase {
    Prepare,
    Write,
    Validate,
}

const fn semantic_space(space: SemanticSpace) -> CompilerFragmentSemanticSpace {
    match space {
        SemanticSpace::Atom => CompilerFragmentSemanticSpace::Atom,
        SemanticSpace::Text => CompilerFragmentSemanticSpace::Text,
        SemanticSpace::Type => CompilerFragmentSemanticSpace::Type,
        SemanticSpace::Entity => CompilerFragmentSemanticSpace::Entity,
        SemanticSpace::External => CompilerFragmentSemanticSpace::External,
        SemanticSpace::Link => CompilerFragmentSemanticSpace::Link,
        SemanticSpace::LinkOccurrence => CompilerFragmentSemanticSpace::LinkOccurrence,
        SemanticSpace::TypeList => CompilerFragmentSemanticSpace::TypeList,
        SemanticSpace::EntityList => CompilerFragmentSemanticSpace::EntityList,
        SemanticSpace::AtomList => CompilerFragmentSemanticSpace::AtomList,
        SemanticSpace::Docs => CompilerFragmentSemanticSpace::Docs,
        SemanticSpace::TupleElements => CompilerFragmentSemanticSpace::TupleElements,
        SemanticSpace::ObjectMembers => CompilerFragmentSemanticSpace::ObjectMembers,
        SemanticSpace::TemplateParts => CompilerFragmentSemanticSpace::TemplateParts,
        SemanticSpace::TypeParameterBounds => CompilerFragmentSemanticSpace::TypeParameterBounds,
        SemanticSpace::TypeParameters => CompilerFragmentSemanticSpace::TypeParameters,
        SemanticSpace::FreePredicates => CompilerFragmentSemanticSpace::FreePredicates,
    }
}

fn usize_as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[allow(clippy::too_many_arguments)]
fn nested_facts(
    fault: CompilerFragmentNestedFaultKind,
    ordinal: Option<u64>,
    owner: Option<u32>,
    target: Option<u32>,
    second_coordinate: Option<u64>,
    expected: Option<u64>,
    actual: Option<u64>,
    count: Option<u64>,
    limit: Option<u64>,
    required: Option<u64>,
    available: Option<u64>,
) -> CompilerFragmentFaultFacts {
    CompilerFragmentFaultFacts::Nested {
        fault,
        lane: None,
        resource: None,
        ordinal,
        owner,
        target,
        second_coordinate,
        expected,
        actual,
        count,
        limit,
        required,
        available,
    }
}

fn nested_canonical_data_facts(
    fault: CompilerFragmentNestedFaultKind,
    operands: [Option<u64>; 10],
    lane: Option<CompilerFragmentDataLane>,
    resource: Option<CompilerFragmentDataResource>,
) -> CompilerFragmentFaultFacts {
    let [
        ordinal,
        owner,
        target,
        second_coordinate,
        expected,
        actual,
        count,
        limit,
        required,
        available,
    ] = operands;
    let mut facts = nested_facts(
        fault,
        ordinal,
        owner.map(|value| u32::try_from(value).unwrap_or(u32::MAX)),
        target.map(|value| u32::try_from(value).unwrap_or(u32::MAX)),
        second_coordinate,
        expected,
        actual,
        count,
        limit,
        required,
        available,
    );
    if let CompilerFragmentFaultFacts::Nested {
        lane: nested_lane,
        resource: nested_resource,
        ..
    } = &mut facts
    {
        *nested_lane = lane;
        *nested_resource = resource;
    }
    facts
}

fn entity_record_facts(
    ordinal: u32,
    fault: backend_semantic::ir::EntityRecordFault,
) -> CompilerFragmentFaultFacts {
    use backend_semantic::ir::EntityRecordFault as E;
    match fault {
        E::Type(fault) => nested_facts(
            CompilerFragmentNestedFaultKind::EntityTypeReference,
            Some(u64::from(ordinal)),
            None,
            Some(fault.target.raw),
            None,
            None,
            None,
            None,
            Some(u64::from(fault.node_count)),
            None,
            None,
        ),
        E::Name(fault) => nested_facts(
            CompilerFragmentNestedFaultKind::EntityNameReference,
            Some(u64::from(ordinal)),
            None,
            Some(fault.target.raw),
            None,
            None,
            None,
            None,
            Some(u64::from(fault.atom_count)),
            None,
            None,
        ),
        E::Kind { actual } => nested_facts(
            CompilerFragmentNestedFaultKind::EntityKindTag,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            Some(u64::from(actual)),
            None,
            None,
            None,
            None,
        ),
        E::Reserved { actual } => nested_facts(
            CompilerFragmentNestedFaultKind::EntityReservedBits,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            Some(u64::from(actual)),
            None,
            None,
            None,
            None,
        ),
    }
}

fn type_node_facts(
    ordinal: u32,
    fault: backend_semantic::ir::TypeNodeFault,
) -> CompilerFragmentFaultFacts {
    use backend_semantic::ir::TypeNodeFault as T;
    match fault {
        T::Reserved { actual } => nested_facts(
            CompilerFragmentNestedFaultKind::TypeNodeReservedBytes,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            Some(u64::from_le_bytes([
                actual[0], actual[1], actual[2], 0, 0, 0, 0, 0,
            ])),
            None,
            None,
            None,
            None,
        ),
        T::Tag { actual } => nested_facts(
            CompilerFragmentNestedFaultKind::TypeNodeTag,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            Some(u64::from(actual)),
            None,
            None,
            None,
            None,
        ),
        T::Primitive { actual } => nested_facts(
            CompilerFragmentNestedFaultKind::TypeNodePrimitive,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            Some(u64::from(actual)),
            None,
            None,
            None,
            None,
        ),
        T::Edge { target, node_count } => nested_facts(
            CompilerFragmentNestedFaultKind::TypeNodeEdge,
            Some(u64::from(ordinal)),
            None,
            Some(target.raw),
            None,
            None,
            None,
            None,
            Some(u64::from(node_count)),
            None,
            None,
        ),
    }
}

fn atom_facts(fault: backend_semantic::ir::AtomFault) -> CompilerFragmentFaultFacts {
    use backend_semantic::ir::AtomFault as A;
    match fault {
        A::Range {
            ordinal,
            start,
            length,
            byte_count,
        } => nested_facts(
            CompilerFragmentNestedFaultKind::AtomRange,
            Some(u64::from(ordinal.raw)),
            None,
            Some(start),
            Some(u64::from(length)),
            None,
            None,
            None,
            Some(u64::from(byte_count)),
            None,
            None,
        ),
        A::Empty { ordinal } => nested_facts(
            CompilerFragmentNestedFaultKind::AtomEmpty,
            Some(u64::from(ordinal.raw)),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
    }
}

fn canonical_data_facts(
    fault: &backend_semantic::ir::CanonicalDataError,
) -> CompilerFragmentFaultFacts {
    use CompilerFragmentNestedFaultKind as K;
    use backend_semantic::ir::CanonicalDataError as C;
    let none = [None; 10];
    match fault {
        C::Count { lane, actual, .. } => nested_canonical_data_facts(
            K::CanonicalDataCount,
            [
                None,
                None,
                None,
                None,
                None,
                Some(usize_as_u64(*actual)),
                None,
                None,
                None,
                None,
            ],
            Some(data_count_lane(*lane)),
            None,
        ),
        C::NativeCount { lane, actual, .. } => nested_canonical_data_facts(
            K::CanonicalDataNativeCount,
            [
                None,
                None,
                None,
                None,
                None,
                Some(u64::from(*actual)),
                None,
                None,
                None,
                None,
            ],
            Some(data_count_lane(*lane)),
            None,
        ),
        C::NativeWork { actual, .. } => nested_canonical_data_facts(
            K::CanonicalDataNativeWork,
            [
                None,
                None,
                None,
                None,
                None,
                Some(usize_as_u64(*actual)),
                None,
                None,
                None,
                None,
            ],
            None,
            None,
        ),
        C::CanonicalCountOverflow { lane } => nested_canonical_data_facts(
            K::CanonicalDataCanonicalCountOverflow,
            none,
            Some(data_count_lane(*lane)),
            None,
        ),
        C::CanonicalCountMismatch {
            lane,
            expected,
            actual,
        } => nested_canonical_data_facts(
            K::CanonicalDataCanonicalCountMismatch,
            [
                None,
                None,
                None,
                None,
                Some(u64::from(*expected)),
                Some(u64::from(*actual)),
                None,
                None,
                None,
                None,
            ],
            Some(data_count_lane(*lane)),
            None,
        ),
        C::Scratch {
            lane,
            required,
            actual,
        } => nested_canonical_data_facts(
            K::CanonicalDataScratch,
            [
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(usize_as_u64(*required)),
                Some(usize_as_u64(*actual)),
            ],
            Some(data_scratch_lane(*lane)),
            None,
        ),
        C::OutputTooSmall {
            lane,
            required,
            available,
        } => nested_canonical_data_facts(
            K::CanonicalDataOutputTooSmall,
            [
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(usize_as_u64(*required)),
                Some(usize_as_u64(*available)),
            ],
            Some(data_output_lane(*lane)),
            None,
        ),
        C::ProductHead {
            product,
            target,
            atom_count,
        } => nested_canonical_data_facts(
            K::CanonicalDataProductHead,
            [
                Some(u64::from(product.raw)),
                None,
                Some(target.raw),
                None,
                None,
                None,
                None,
                Some(u64::from(*atom_count)),
                None,
                None,
            ],
            None,
            None,
        ),
        C::ProductList {
            product,
            target,
            list_count,
        } => nested_canonical_data_facts(
            K::CanonicalDataProductList,
            [
                Some(u64::from(product.raw)),
                None,
                Some(target.raw),
                None,
                None,
                None,
                None,
                Some(u64::from(*list_count)),
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::Lists),
            None,
        ),
        C::ConstructorCount {
            product_count,
            constructor_count,
        } => nested_canonical_data_facts(
            K::CanonicalDataConstructorCount,
            [
                None,
                None,
                None,
                None,
                Some(u64::from(*product_count)),
                Some(u64::from(*constructor_count)),
                None,
                None,
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::Constructors),
            None,
        ),
        C::ProductConstructor { product, fault } => {
            let (kind, expected, actual) = match fault {
                backend_semantic::ir::ProductConstructorFault::Tag { actual } => (
                    K::CanonicalDataConstructorTag,
                    None,
                    Some(u64::from(*actual)),
                ),
                backend_semantic::ir::ProductConstructorFault::ReservedPayload {
                    payload0,
                    payload1,
                    ..
                } => (
                    K::CanonicalDataConstructorReservedPayload,
                    Some(u64::from(*payload0)),
                    Some(u64::from(*payload1)),
                ),
                backend_semantic::ir::ProductConstructorFault::ArityOverflow {
                    payload0,
                    payload1,
                    ..
                } => (
                    K::CanonicalDataConstructorArityOverflow,
                    Some(u64::from(*payload0)),
                    Some(u64::from(*payload1)),
                ),
                backend_semantic::ir::ProductConstructorFault::Arity {
                    expected, actual, ..
                } => (
                    K::CanonicalDataConstructorArity,
                    Some(u64::from(*expected)),
                    Some(u64::from(*actual)),
                ),
            };
            nested_canonical_data_facts(
                kind,
                [
                    Some(u64::from(product.raw)),
                    None,
                    None,
                    None,
                    expected,
                    actual,
                    None,
                    None,
                    None,
                    None,
                ],
                Some(CompilerFragmentDataLane::Constructors),
                None,
            )
        }
        C::ProductChildRole {
            product,
            list,
            child_ordinal,
            expected,
            actual,
        } => nested_canonical_data_facts(
            K::CanonicalDataProductChildRole,
            [
                Some(u64::from(product.raw)),
                None,
                Some(list.raw),
                Some(usize_as_u64(*child_ordinal)),
                Some(u64::from(child_role(*expected))),
                Some(u64::from(child_role(*actual))),
                None,
                None,
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::Children),
            None,
        ),
        C::ListExtent {
            list,
            span,
            pool_length,
        } => nested_canonical_data_facts(
            K::CanonicalDataListExtent,
            [
                Some(u64::from(list.raw)),
                None,
                Some(span.start),
                None,
                None,
                None,
                Some(u64::from(span.length)),
                Some(usize_as_u64(*pool_length)),
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::Children),
            None,
        ),
        C::NativeExtent {
            list,
            span,
            pool_length,
            ..
        } => nested_canonical_data_facts(
            K::CanonicalDataNativeExtent,
            [
                Some(u64::from(list.raw)),
                None,
                Some(span.start),
                None,
                None,
                None,
                Some(u64::from(span.length)),
                Some(usize_as_u64(*pool_length)),
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::Children),
            None,
        ),
        C::ProductChild {
            product,
            list,
            child_ordinal,
            target,
            product_count,
        } => nested_canonical_data_facts(
            K::CanonicalDataProductChild,
            [
                Some(u64::from(product.raw)),
                Some(u64::from(list.raw)),
                Some(target.raw),
                Some(usize_as_u64(*child_ordinal)),
                None,
                None,
                None,
                Some(u64::from(*product_count)),
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::Children),
            None,
        ),
        C::OutputLength { lane, actual } => nested_canonical_data_facts(
            K::CanonicalDataOutputLength,
            [
                None,
                None,
                None,
                None,
                None,
                Some(usize_as_u64(*actual)),
                None,
                None,
                None,
                None,
            ],
            Some(data_output_lane(*lane)),
            None,
        ),
        C::CanonicalListExtent {
            ordinal,
            span,
            fault,
        } => {
            let backend_semantic::ir::PooledListError::OutOfBounds { pool_length, .. } = fault;
            nested_canonical_data_facts(
                K::CanonicalDataCanonicalListExtent,
                [
                    Some(u64::from(ordinal.raw)),
                    None,
                    Some(span.start),
                    None,
                    None,
                    None,
                    Some(u64::from(span.length)),
                    Some(usize_as_u64(*pool_length)),
                    None,
                    None,
                ],
                Some(CompilerFragmentDataLane::Children),
                None,
            )
        }
        C::CanonicalAtom { ordinal, count } => nested_canonical_data_facts(
            K::CanonicalDataCanonicalAtom,
            [
                Some(u64::from(ordinal.raw)),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(usize_as_u64(*count)),
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::Atoms),
            None,
        ),
        C::CanonicalProduct { ordinal, count } => nested_canonical_data_facts(
            K::CanonicalDataCanonicalProduct,
            [
                Some(u64::from(ordinal.raw)),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(usize_as_u64(*count)),
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::Products),
            None,
        ),
        C::CanonicalList { ordinal, count } => nested_canonical_data_facts(
            K::CanonicalDataCanonicalList,
            [
                Some(u64::from(ordinal.raw)),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(usize_as_u64(*count)),
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::Lists),
            None,
        ),
        C::RefinementBound {
            rounds,
            product_count,
        } => nested_canonical_data_facts(
            K::CanonicalDataRefinementBound,
            [
                None,
                None,
                None,
                None,
                None,
                Some(u64::from(*rounds)),
                Some(u64::from(*product_count)),
                None,
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::Products),
            None,
        ),
        C::InternTableFull { product, capacity } => nested_canonical_data_facts(
            K::CanonicalDataInternTableFull,
            [
                Some(u64::from(product.raw)),
                None,
                None,
                None,
                None,
                None,
                None,
                Some(usize_as_u64(*capacity)),
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::InternSlots),
            None,
        ),
        C::InternEntry { product, entry, .. } => nested_canonical_data_facts(
            K::CanonicalDataInternEntry,
            [
                Some(u64::from(product.raw)),
                None,
                None,
                None,
                None,
                Some(*entry),
                None,
                None,
                None,
                None,
            ],
            Some(CompilerFragmentDataLane::InternSlots),
            None,
        ),
        C::ResourceCounterOverflow { resource } => nested_canonical_data_facts(
            K::CanonicalDataResourceCounterOverflow,
            none,
            None,
            Some(data_resource(*resource)),
        ),
        C::BudgetAdmission {
            resource,
            required,
            limit,
        } => nested_canonical_data_facts(
            K::CanonicalDataBudgetAdmission,
            [
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(*limit),
                Some(*required),
                None,
            ],
            None,
            Some(data_resource(*resource)),
        ),
        C::BudgetExceeded {
            resource,
            observed,
            limit,
        } => nested_canonical_data_facts(
            K::CanonicalDataBudgetExceeded,
            [
                None,
                None,
                None,
                None,
                None,
                Some(*observed),
                None,
                Some(*limit),
                None,
                None,
            ],
            None,
            Some(data_resource(*resource)),
        ),
    }
}

fn semantic_data_facts(
    fault: &backend_semantic::ir::SemanticDataFault,
) -> CompilerFragmentFaultFacts {
    use CompilerFragmentNestedFaultKind as K;
    use backend_semantic::ir::SemanticDataFault as S;

    match fault {
        S::Header { required, actual } => nested_facts(
            K::SemanticDataHeader,
            None,
            None,
            None,
            None,
            Some(usize_as_u64(*required)),
            Some(usize_as_u64(*actual)),
            None,
            None,
            None,
            None,
        ),
        S::AtomLength {
            ordinal,
            length,
            available,
        } => nested_facts(
            K::SemanticDataAtomLength,
            Some(u64::from(*ordinal)),
            None,
            None,
            None,
            Some(u64::from(*length)),
            None,
            None,
            None,
            None,
            Some(usize_as_u64(*available)),
        ),
        S::ProductHead {
            product,
            target,
            atom_count,
        } => nested_facts(
            K::SemanticDataProductHead,
            Some(u64::from(*product)),
            None,
            Some(*target),
            None,
            None,
            None,
            None,
            Some(u64::from(*atom_count)),
            None,
            None,
        ),
        S::ProductList {
            product,
            target,
            list_count,
        } => nested_facts(
            K::SemanticDataProductList,
            Some(u64::from(*product)),
            None,
            Some(*target),
            None,
            None,
            None,
            None,
            Some(u64::from(*list_count)),
            None,
            None,
        ),
        S::ConstructorCount {
            product_count,
            constructor_count,
        } => nested_facts(
            K::SemanticDataConstructorCount,
            None,
            None,
            None,
            None,
            Some(u64::from(*product_count)),
            Some(u64::from(*constructor_count)),
            None,
            None,
            None,
            None,
        ),
        S::EntityRootCount { expected, actual } => nested_facts(
            K::SemanticDataEntityRootCount,
            None,
            None,
            None,
            None,
            Some(u64::from(*expected)),
            Some(u64::from(*actual)),
            None,
            None,
            None,
            None,
        ),
        S::EntityRoot {
            entity,
            target,
            product_count,
        } => nested_facts(
            K::SemanticDataEntityRoot,
            Some(u64::from(*entity)),
            None,
            Some(*target),
            None,
            None,
            None,
            None,
            Some(u64::from(*product_count)),
            None,
            None,
        ),
        S::Constructor { product, fault } => {
            let (kind, expected, actual) = match fault {
                backend_semantic::ir::ProductConstructorFault::Tag { actual } => (
                    K::SemanticDataConstructorTag,
                    None,
                    Some(u64::from(*actual)),
                ),
                backend_semantic::ir::ProductConstructorFault::ReservedPayload {
                    payload0,
                    payload1,
                    ..
                } => (
                    K::SemanticDataConstructorReservedPayload,
                    Some(u64::from(*payload0)),
                    Some(u64::from(*payload1)),
                ),
                backend_semantic::ir::ProductConstructorFault::ArityOverflow {
                    payload0,
                    payload1,
                    ..
                } => (
                    K::SemanticDataConstructorArityOverflow,
                    Some(u64::from(*payload0)),
                    Some(u64::from(*payload1)),
                ),
                backend_semantic::ir::ProductConstructorFault::Arity {
                    expected, actual, ..
                } => (
                    K::SemanticDataConstructorArity,
                    Some(u64::from(*expected)),
                    Some(u64::from(*actual)),
                ),
            };
            nested_facts(
                kind,
                Some(u64::from(product)),
                None,
                None,
                None,
                expected,
                actual,
                None,
                None,
                None,
                None,
            )
        }
        S::ListExtent {
            list,
            start,
            length,
            child_count,
        } => nested_facts(
            K::SemanticDataListExtent,
            Some(u64::from(*list)),
            None,
            Some(*start),
            None,
            None,
            None,
            Some(u64::from(*length)),
            Some(u64::from(*child_count)),
            None,
            None,
        ),
        S::ChildRoleCode { child, actual } => nested_facts(
            K::SemanticDataChildRoleCode,
            Some(u64::from(*child)),
            None,
            None,
            None,
            None,
            Some(u64::from(*actual)),
            None,
            None,
            None,
            None,
        ),
        S::ChildRole {
            child,
            expected,
            actual,
        } => nested_facts(
            K::SemanticDataChildRole,
            Some(u64::from(*child)),
            None,
            None,
            None,
            Some(u64::from(child_role(*expected))),
            Some(u64::from(child_role(*actual))),
            None,
            None,
            None,
            None,
        ),
        S::ChildTag { child, actual } => nested_facts(
            K::SemanticDataChildTag,
            Some(u64::from(*child)),
            None,
            None,
            None,
            None,
            Some(u64::from(*actual)),
            None,
            None,
            None,
            None,
        ),
        S::LocalChild {
            child,
            target,
            product_count,
        } => nested_facts(
            K::SemanticDataLocalChild,
            Some(u64::from(*child)),
            None,
            Some(*target),
            None,
            None,
            None,
            None,
            Some(u64::from(*product_count)),
            None,
            None,
        ),
        S::LocalReserved { child, .. } => nested_facts(
            K::SemanticDataLocalReserved,
            Some(u64::from(*child)),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        S::ExternalAuthority {
            child,
            expected,
            observed,
            ..
        } => nested_facts(
            K::SemanticDataExternalAuthority,
            Some(u64::from(*child)),
            None,
            None,
            None,
            Some(u64::from(*expected)),
            Some(u64::from(*observed)),
            None,
            None,
            None,
            None,
        ),
        S::Trailing { actual } => nested_facts(
            K::SemanticDataTrailing,
            None,
            None,
            None,
            None,
            None,
            Some(usize_as_u64(*actual)),
            None,
            None,
            None,
            None,
        ),
    }
}

fn occurrence_facts(
    _ordinal: u32,
    fault: backend_semantic::ir::OccurrenceViewFault,
) -> CompilerFragmentFaultFacts {
    use CompilerFragmentNestedFaultKind as K;
    use backend_semantic::ir::OccurrenceViewFault as O;

    match fault {
        O::Owner {
            ordinal,
            owner,
            entity_count,
        } => nested_facts(
            K::OccurrenceOwner,
            Some(u64::from(ordinal)),
            Some(owner),
            None,
            None,
            None,
            None,
            None,
            Some(u64::from(entity_count)),
            None,
            None,
        ),
        O::LocalTarget {
            ordinal,
            target,
            entity_count,
        } => nested_facts(
            K::OccurrenceLocalTarget,
            Some(u64::from(ordinal)),
            None,
            Some(target),
            None,
            None,
            None,
            None,
            Some(u64::from(entity_count)),
            None,
            None,
        ),
        O::TargetTag { ordinal, actual } => nested_facts(
            K::OccurrenceTargetTag,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            Some(u64::from(actual)),
            None,
            None,
            None,
            None,
        ),
        O::OriginTag { ordinal, actual } => nested_facts(
            K::OccurrenceOriginTag,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            Some(u64::from(actual)),
            None,
            None,
            None,
            None,
        ),
        O::ReferenceKind { ordinal, actual } => nested_facts(
            K::OccurrenceReferenceKind,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            Some(u64::from(actual)),
            None,
            None,
            None,
            None,
        ),
        O::Confidence { ordinal, actual } => nested_facts(
            K::OccurrenceConfidence,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            Some(u64::from(actual)),
            None,
            None,
            None,
            None,
        ),
        O::Span {
            ordinal,
            start,
            end,
        } => nested_facts(
            K::OccurrenceSpan,
            Some(u64::from(ordinal)),
            None,
            Some(start),
            Some(u64::from(end)),
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        O::KindCell { ordinal, actual } => nested_facts(
            K::OccurrenceKindCell,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            Some(u64::from(actual)),
            None,
            None,
            None,
            None,
        ),
        O::EmptyPath { ordinal } => nested_facts(
            K::OccurrenceEmptyPath,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ),
        O::Truncated { ordinal, needed } => nested_facts(
            K::OccurrenceTruncated,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            Some(usize_as_u64(needed)),
            None,
            None,
            None,
            None,
            None,
        ),
        O::TrailingBytes { declared } => nested_facts(
            K::OccurrenceTrailingBytes,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(u64::from(declared)),
            None,
            None,
            None,
        ),
        O::LegacyStableTarget { ordinal, schema } => nested_facts(
            K::OccurrenceLegacyStableTarget,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            Some(u64::from(schema)),
            None,
            None,
            None,
            None,
            None,
        ),
        O::AuthorityDomain {
            ordinal,
            expected,
            observed,
            ..
        } => nested_facts(
            K::OccurrenceAuthorityDomain,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            Some(u64::from(u8::from(expected))),
            Some(u64::from(observed)),
            None,
            None,
            None,
            None,
        ),
        O::AuthorityWidth { ordinal, actual } => nested_facts(
            K::OccurrenceAuthorityWidth,
            Some(u64::from(ordinal)),
            None,
            None,
            None,
            None,
            Some(usize_as_u64(actual)),
            None,
            None,
            None,
            None,
        ),
    }
}

const fn data_count_lane(lane: backend_semantic::ir::DataCountLane) -> CompilerFragmentDataLane {
    use backend_semantic::ir::DataCountLane as L;
    match lane {
        L::Atoms => CompilerFragmentDataLane::Atoms,
        L::Products => CompilerFragmentDataLane::Products,
        L::Constructors => CompilerFragmentDataLane::Constructors,
        L::Lists => CompilerFragmentDataLane::Lists,
        L::Children => CompilerFragmentDataLane::Children,
    }
}

const fn data_scratch_lane(
    lane: backend_semantic::ir::DataScratchLane,
) -> CompilerFragmentDataLane {
    use backend_semantic::ir::DataScratchLane as L;
    match lane {
        L::AtomOrder => CompilerFragmentDataLane::AtomOrder,
        L::AtomMap => CompilerFragmentDataLane::AtomMap,
        L::ProductOrder => CompilerFragmentDataLane::ProductOrder,
        L::ProductMap => CompilerFragmentDataLane::ProductMap,
        L::Colors => CompilerFragmentDataLane::Colors,
        L::NextColors => CompilerFragmentDataLane::NextColors,
        L::Hashes => CompilerFragmentDataLane::Hashes,
        L::NextHashes => CompilerFragmentDataLane::NextHashes,
        L::Representatives => CompilerFragmentDataLane::Representatives,
        L::InternSlots => CompilerFragmentDataLane::InternSlots,
    }
}

const fn data_output_lane(lane: backend_semantic::ir::DataOutputLane) -> CompilerFragmentDataLane {
    use backend_semantic::ir::DataOutputLane as L;
    match lane {
        L::Atoms => CompilerFragmentDataLane::Atoms,
        L::Products => CompilerFragmentDataLane::Products,
        L::Constructors => CompilerFragmentDataLane::Constructors,
        L::Lists => CompilerFragmentDataLane::Lists,
        L::Children => CompilerFragmentDataLane::Children,
    }
}

const fn data_resource(
    resource: backend_semantic::ir::DataResource,
) -> CompilerFragmentDataResource {
    use backend_semantic::ir::DataResource as R;
    match resource {
        R::RefinementRounds => CompilerFragmentDataResource::RefinementRounds,
        R::SortComparisons => CompilerFragmentDataResource::SortComparisons,
        R::HashEvaluations => CompilerFragmentDataResource::HashEvaluations,
        R::InternProbes => CompilerFragmentDataResource::InternProbes,
        R::Work => CompilerFragmentDataResource::Work,
    }
}

const fn child_role(role: backend_semantic::ir::ProductChildRole) -> u8 {
    use backend_semantic::ir::ProductChildRole as R;
    match role {
        R::FunctionParameter => 0,
        R::FunctionResult => 1,
        R::GenericArgument => 2,
        R::TupleElement => 3,
        R::ArrayElement => 4,
        R::UnionMember => 5,
        R::IntersectionMember => 6,
        R::ProductMember => 7,
    }
}

const fn signature_role(role: backend_semantic::ir::SignatureCarrierBindingRole) -> u8 {
    use backend_semantic::ir::SignatureCarrierBindingRole as R;
    match role {
        R::Parameter => 0,
        R::Result => 1,
    }
}

const fn layout_step(step: LayoutStep) -> CompilerFragmentLayoutStep {
    match step {
        LayoutStep::Directory => CompilerFragmentLayoutStep::Directory,
        LayoutStep::EntityLane => CompilerFragmentLayoutStep::EntityLane,
        LayoutStep::TypeNodeLane => CompilerFragmentLayoutStep::TypeNodeLane,
        LayoutStep::AtomRecordLane => CompilerFragmentLayoutStep::AtomRecordLane,
        LayoutStep::AtomByteLane => CompilerFragmentLayoutStep::AtomByteLane,
        LayoutStep::SourceIdentityLane => CompilerFragmentLayoutStep::SourceIdentityLane,
        LayoutStep::RecipeFactLane => CompilerFragmentLayoutStep::RecipeFactLane,
        LayoutStep::SemanticData => CompilerFragmentLayoutStep::SemanticData,
        LayoutStep::Occurrences => CompilerFragmentLayoutStep::Occurrences,
        LayoutStep::TypeFacts => CompilerFragmentLayoutStep::TypeFacts,
        LayoutStep::Documentation => CompilerFragmentLayoutStep::Documentation,
        LayoutStep::LanguageExtensions => CompilerFragmentLayoutStep::LanguageExtensions,
        LayoutStep::ExtensionPools => CompilerFragmentLayoutStep::ExtensionPools,
    }
}

impl CompilerFragmentFaultKind {
    /// Returns the phase implied by this closed cause family.
    #[must_use]
    pub const fn phase(self) -> CompilerFragmentFaultPhase {
        match self {
            Self::Build(_) | Self::Prepare(_) => CompilerFragmentFaultPhase::Prepare,
            Self::Write(_) => CompilerFragmentFaultPhase::Write,
            Self::Validate(_) => CompilerFragmentFaultPhase::Validate,
        }
    }

    /// Stable wire tag containing both the error family and exact outer variant.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Build(kind) => kind.tag(),
            Self::Prepare(kind) => kind.tag(),
            Self::Write(kind) => kind.tag(),
            Self::Validate(kind) => kind.tag(),
        }
    }
}

impl BuildFaultKind {
    /// Stable snake-case wire tag.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Capacity => "build_capacity",
            Self::InvalidTreeEntity => "build_invalid_tree_entity",
            Self::Dangling => "build_dangling",
            Self::RecursiveType => "build_recursive_type",
            Self::CallableElement => "build_callable_element",
            Self::MissingTypedVariadicParameter => "build_missing_typed_variadic_parameter",
            Self::EmptyQualifiedPath => "build_empty_qualified_path",
            Self::TypeParameterRequirements => "build_type_parameter_requirements",
            Self::EmptyCxxQualification => "build_empty_cxx_qualification",
            Self::IllegalCQualifierTarget => "build_illegal_c_qualifier_target",
            Self::IllegalCxxMemberPointerOwner => "build_illegal_cxx_member_pointer_owner",
            Self::InvalidDocumentationUtf8 => "build_invalid_documentation_utf8",
            Self::InvalidOccurrenceSpan => "build_invalid_occurrence_span",
            Self::SignatureCarrierRoleCount => "build_signature_carrier_role_count",
            Self::SignatureCarrierRoleKind => "build_signature_carrier_role_kind",
            Self::SignatureCarrierRoleOwnerKind => "build_signature_carrier_role_owner_kind",
            Self::SignatureCarrierRoleAlreadyCaptured => {
                "build_signature_carrier_role_already_captured"
            }
            Self::SignatureCarrierBindingsAlreadyCaptured => {
                "build_signature_carrier_bindings_already_captured"
            }
            Self::SignatureCarrierBindingOwnerSet => "build_signature_carrier_binding_owner_set",
            Self::SignatureCarrierBindingSignature => "build_signature_carrier_binding_signature",
            Self::SignatureCarrierBindingCounts => "build_signature_carrier_binding_counts",
            Self::SignatureCarrierBindingEdgeRole => "build_signature_carrier_binding_edge_role",
            Self::SignatureCarrierBindingTargetCount => {
                "build_signature_carrier_binding_target_count"
            }
            Self::SignatureCarrierBindingTargetKind => {
                "build_signature_carrier_binding_target_kind"
            }
            Self::SignatureCarrierBindingType => "build_signature_carrier_binding_type",
            Self::SignatureCarrierBindingEdgeMismatch => {
                "build_signature_carrier_binding_edge_mismatch"
            }
            Self::ParentCycle => "build_parent_cycle",
            Self::DeclarationKey => "build_declaration_key",
            Self::ScopedDeclarationPreimage => "build_scoped_declaration_preimage",
            Self::ForeignKeyPreimage => "build_foreign_key_preimage",
            Self::DuplicateDeclarationIdentity => "build_duplicate_declaration_identity",
            Self::TreeVersionCount => "build_tree_version_count",
            Self::AuthorityRowCount => "build_authority_row_count",
            Self::OccurrenceAuthorityRowCount => "build_occurrence_authority_row_count",
            Self::AuthorityFacts => "build_authority_facts",
            Self::OccurrenceAuthorityFacts => "build_occurrence_authority_facts",
            Self::ImageProvenanceLineage => "build_image_provenance_lineage",
            Self::ImageProvenanceScope => "build_image_provenance_scope",
            Self::ImageProvenanceScopePreimage => "build_image_provenance_scope_preimage",
            Self::ImageProvenanceRecipe => "build_image_provenance_recipe",
            Self::ImageProvenanceRebind => "build_image_provenance_rebind",
            Self::LanguageExtension => "build_language_extension",
            Self::LanguageProfileMismatch => "build_language_profile_mismatch",
            Self::LanguageProfileRebind => "build_language_profile_rebind",
        }
    }
}

impl PrepareFaultKind {
    /// Stable snake-case wire tag.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Count => "prepare_count",
            Self::AtomBytePoolOverflow => "prepare_atom_byte_pool_overflow",
            Self::LayoutOverflow => "prepare_layout_overflow",
            Self::NativeCount => "prepare_native_count",
            Self::OutputLength => "prepare_output_length",
            Self::Entity => "prepare_entity",
            Self::TypeNode => "prepare_type_node",
            Self::SemanticDataOverflow => "prepare_semantic_data_overflow",
            Self::SemanticEntityRoots => "prepare_semantic_entity_roots",
            Self::SemanticAtomLength => "prepare_semantic_atom_length",
            Self::SemanticData => "prepare_semantic_data",
            Self::OccurrenceLane => "prepare_occurrence_lane",
            Self::TypeFacts => "prepare_type_facts",
            Self::Documentation => "prepare_documentation",
            Self::ExtensionPools => "prepare_extension_pools",
            Self::ExtensionPoolsMismatch => "prepare_extension_pools_mismatch",
        }
    }
}

impl WriteFaultKind {
    /// Stable snake-case wire tag.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::OutputTooSmall => "write_output_too_small",
            Self::ExtensionSection => "write_extension_section",
            Self::AtomLength => "write_atom_length",
            Self::AtomExtent => "write_atom_extent",
            Self::SemanticAtomLength => "write_semantic_atom_length",
        }
    }
}

impl ValidateFaultKind {
    /// Stable snake-case wire tag.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::ExtensionPoolPair => "validate_extension_pool_pair",
            Self::Documentation => "validate_documentation",
            Self::ExtensionPools => "validate_extension_pools",
            Self::LanguageExtensions => "validate_language_extensions",
            Self::TruncatedHeader => "validate_truncated_header",
            Self::Magic => "validate_magic",
            Self::Schema => "validate_schema",
            Self::DeclaredLength => "validate_declared_length",
            Self::Extent => "validate_extent",
            Self::WireWidth => "validate_wire_width",
            Self::Directory => "validate_directory",
            Self::MissingSection => "validate_missing_section",
            Self::Entity => "validate_entity",
            Self::EntityRecord => "validate_entity_record",
            Self::Atom => "validate_atom",
            Self::TypeNode => "validate_type_node",
            Self::SourceIdentity => "validate_source_identity",
            Self::RecipeFact => "validate_recipe_fact",
            Self::SemanticData => "validate_semantic_data",
            Self::Occurrences => "validate_occurrences",
            Self::TypeFacts => "validate_type_facts",
        }
    }
}

/// Closed family of the retained concrete fragment error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerFragmentFaultFamily {
    /// Semantic IR materialization failed.
    Build,
    /// Fragment preflight, canonicalization, or layout preparation failed.
    Prepare,
    /// Caller-owned output admission failed.
    Write,
    /// Freshly written compact bytes failed validation.
    Validate,
}

impl CompilerFragmentFailure {
    /// Returns the closed error family represented by the retained concrete error.
    #[must_use]
    pub const fn family(&self) -> CompilerFragmentFaultFamily {
        match &self.fault {
            CompilerFragmentFault::Build(_) => CompilerFragmentFaultFamily::Build,
            CompilerFragmentFault::Prepare(_) => CompilerFragmentFaultFamily::Prepare,
            CompilerFragmentFault::Write(_) => CompilerFragmentFaultFamily::Write,
            CompilerFragmentFault::Validate(_) => CompilerFragmentFaultFamily::Validate,
        }
    }
}

/// Maximum UTF-8 byte length of the secondary compiler fragment explanation.
pub const MAX_COMPILER_FRAGMENT_DETAIL_BYTES: usize = 384;

/// Human-readable secondary detail retained within a fixed UTF-8 byte budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedCompilerFragmentDetail {
    /// Sanitized UTF-8 explanation, at most [`MAX_COMPILER_FRAGMENT_DETAIL_BYTES`] bytes.
    pub text: String,
    /// Whether the source explanation exceeded the byte budget or allocation was unavailable.
    pub truncated: bool,
}

impl BoundedCompilerFragmentDetail {
    fn new() -> Self {
        let mut text = String::new();
        let reserved = text
            .try_reserve_exact(MAX_COMPILER_FRAGMENT_DETAIL_BYTES)
            .is_ok();
        Self {
            text,
            truncated: !reserved,
        }
    }
}

impl fmt::Write for BoundedCompilerFragmentDetail {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.truncated {
            return Ok(());
        }
        for character in value.chars() {
            let character = if character.is_control() {
                ' '
            } else {
                character
            };
            let width = character.len_utf8();
            if self.text.len().saturating_add(width) > MAX_COMPILER_FRAGMENT_DETAIL_BYTES {
                self.truncated = true;
                break;
            }
            self.text.push(character);
        }
        Ok(())
    }
}
