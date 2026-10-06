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

#[path = "compiler_fragment/nested.rs"]
mod nested;
pub use nested::{
    AtomFaultFacts, CanonicalDataFaultFacts, CompilerFragmentDataLane,
    CompilerFragmentDataResource, CompilerFragmentNestedFaultKind, EntityRecordFaultFacts,
    NestedCompilerFaultFacts, OccurrenceFaultFacts, SemanticDataFaultFacts,
    SignatureCarrierFaultFacts, TypeNodeFaultFacts, ValidationFaultFacts,
};

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
            CompilerFragmentFault::Build(
                error @ (BuildError::SignatureCarrierRoleCount { .. }
                | BuildError::SignatureCarrierRoleKind { .. }
                | BuildError::SignatureCarrierRoleOwnerKind { .. }
                | BuildError::SignatureCarrierBindingOwnerSet { .. }
                | BuildError::SignatureCarrierBindingSignature { .. }
                | BuildError::SignatureCarrierBindingCounts { .. }
                | BuildError::SignatureCarrierBindingEdgeRole { .. }
                | BuildError::SignatureCarrierBindingTargetCount { .. }
                | BuildError::SignatureCarrierBindingTargetKind { .. }
                | BuildError::SignatureCarrierBindingType { .. }
                | BuildError::SignatureCarrierBindingEdgeMismatch { .. }),
            ) => nested::signature_carrier(error)
                .expect("the matched build error has a signature-carrier projection"),
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
                nested::entity_record(ordinal.raw, *fault)
            }
            CompilerFragmentFault::Prepare(PrepareError::TypeNode { ordinal, fault }) => {
                nested::type_node(ordinal.raw, *fault)
            }
            CompilerFragmentFault::Prepare(PrepareError::SemanticData { cause }) => {
                nested::canonical_data(cause)
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
                nested::occurrence(*fault)
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
                nested::entity_record(
                    ordinal.raw,
                    backend_semantic::ir::EntityRecordFault::Type(*fault),
                )
            }
            CompilerFragmentFault::Validate(FragmentError::EntityRecord { ordinal, fault }) => {
                nested::entity_record(ordinal.raw, *fault)
            }
            CompilerFragmentFault::Validate(FragmentError::Atom { fault }) => nested::atom(*fault),
            CompilerFragmentFault::Validate(FragmentError::TypeNode { ordinal, fault }) => {
                nested::type_node(ordinal.raw, *fault)
            }
            CompilerFragmentFault::Validate(FragmentError::SemanticData { fault }) => {
                nested::semantic_data(fault)
            }
            CompilerFragmentFault::Validate(FragmentError::Occurrences { fault }) => {
                nested::occurrence(*fault)
            }
            CompilerFragmentFault::Validate(FragmentError::TruncatedHeader {
                required,
                actual,
            }) => nested::validation(ValidationFaultFacts::TruncatedHeader {
                required: usize_as_u64(*required),
                actual: usize_as_u64(*actual),
            }),
            CompilerFragmentFault::Validate(FragmentError::Magic { actual }) => {
                nested::validation(ValidationFaultFacts::Magic {
                    actual: u32::from_le_bytes(*actual),
                })
            }
            CompilerFragmentFault::Validate(FragmentError::Schema { actual }) => {
                nested::validation(ValidationFaultFacts::Schema { actual: *actual })
            }
            CompilerFragmentFault::Validate(FragmentError::DeclaredLength { declared, actual }) => {
                nested::validation(ValidationFaultFacts::DeclaredLength {
                    declared: usize_as_u64(*declared),
                    actual: usize_as_u64(*actual),
                })
            }
            CompilerFragmentFault::Validate(FragmentError::Extent { required, actual }) => {
                nested::validation(ValidationFaultFacts::Extent {
                    required: usize_as_u64(*required),
                    actual: usize_as_u64(*actual),
                })
            }
            CompilerFragmentFault::Validate(FragmentError::WireWidth { field, actual, .. }) => {
                let facts = match field {
                    backend_semantic::ir::WireField::DeclaredLength => {
                        ValidationFaultFacts::WireWidthDeclaredLength { actual: *actual }
                    }
                    backend_semantic::ir::WireField::SectionItemCount { ordinal } => {
                        ValidationFaultFacts::WireWidthSectionItemCount {
                            ordinal: *ordinal,
                            actual: *actual,
                        }
                    }
                    backend_semantic::ir::WireField::SectionOffset { ordinal } => {
                        ValidationFaultFacts::WireWidthSectionOffset {
                            ordinal: *ordinal,
                            actual: *actual,
                        }
                    }
                    backend_semantic::ir::WireField::SectionByteLength { ordinal } => {
                        ValidationFaultFacts::WireWidthSectionByteLength {
                            ordinal: *ordinal,
                            actual: *actual,
                        }
                    }
                    backend_semantic::ir::WireField::AtomStart { ordinal } => {
                        ValidationFaultFacts::WireWidthAtomStart {
                            ordinal: ordinal.raw,
                            actual: *actual,
                        }
                    }
                    backend_semantic::ir::WireField::AtomLength { ordinal } => {
                        ValidationFaultFacts::WireWidthAtomLength {
                            ordinal: ordinal.raw,
                            actual: *actual,
                        }
                    }
                };
                nested::validation(facts)
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

    /// Captures a bounded explanation derived only from the closed fault tag
    /// and scalar facts. The original typed cause, not this secondary text, is
    /// the diagnostic authority; arbitrary nested source/error strings are
    /// deliberately excluded from this display channel.
    #[must_use]
    pub fn detail(&self) -> BoundedCompilerFragmentDetail {
        let mut detail = BoundedCompilerFragmentDetail::new();
        if !detail.truncated {
            for character in self.kind().tag().chars() {
                let character = if character == '_' { ' ' } else { character };
                if write!(&mut detail, "{character}").is_err() {
                    detail.truncated = true;
                    return detail;
                }
            }
            let facts = serde_json::to_string(&self.facts())
                .unwrap_or_else(|_| "{\"kind\":\"unavailable\"}".to_owned());
            if write!(&mut detail, " failure; facts={facts}").is_err() {
                detail.truncated = true;
            }
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
        /// Concrete closed subcause; its exact variant determines required operands.
        fault: NestedCompilerFaultFacts,
    },
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
