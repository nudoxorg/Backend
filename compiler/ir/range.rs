//! Defines range behavior for `compiler-ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the range invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::{num::TryFromIntError, ops::Deref};

use heart_identity::{
    ArtifactHasher, ArtifactId, IrFragmentDomain, IrFragmentEncoding, IrFragmentRangeEncoding,
};
use thiserror::Error;

use crate::{
    FragmentError, FragmentView, SectionKind, SourceIdentity,
    view::validate_fragment_layout,
    wire::{FragmentLayout, LaneLayout},
};

const REQUIRED_RANGE_COUNT: usize = 6;

/// One immutable section range committed by a validated fragment manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FragmentRange {
    /// Closed IR section kind.
    pub section: SectionKind,
    /// Byte offset in the complete fragment artifact.
    pub offset: u32,
    /// Exact section-body byte length.
    pub length: u32,
    /// Typed commitment over the section, offset, length, and exact body.
    pub identity: ArtifactId<IrFragmentRangeEncoding, IrFragmentDomain>,
}

/// Immutable facts committed from one complete validated fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FragmentRangeManifestView {
    /// Typed identity of the exact complete canonical fragment bytes.
    pub fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    /// Complete fragment length shared by every range request.
    pub fragment_length: u32,
    /// Typed source fact retained in the complete fragment.
    pub source: SourceIdentity,
    /// Typed recipe fact retained in the complete fragment.
    pub recipe: crate::RecipeFact,
    /// Canonically ordered commitments for every required current semantic lane.
    pub ranges: [FragmentRange; REQUIRED_RANGE_COUNT],
}

/// A fixed manifest whose construction requires a completely validated fragment view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct FragmentRangeManifest(FragmentRangeManifestView);

impl Deref for FragmentRangeManifest {
    type Target = FragmentRangeManifestView;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl FragmentRangeManifest {
    /// Commits every required semantic lane of an already validated immutable fragment.
    pub fn from_view(view: &FragmentView<'_>) -> Result<Self, FragmentRangeManifestError> {
        let fragment_length = u32::try_from(view.as_ref().len()).map_err(|source| {
            FragmentRangeManifestError::FragmentLength {
                observed: view.as_ref().len(),
                source,
            }
        })?;
        let layout = view.layout;
        let ranges = [
            committed_range(SectionKind::EntityTypes, layout.entities, view.entities)?,
            committed_range(SectionKind::TypeNodes, layout.type_nodes, view.type_nodes)?,
            committed_range(SectionKind::AtomRecords, layout.atoms, view.atoms)?,
            committed_range(SectionKind::AtomBytes, layout.atom_bytes, view.atom_bytes)?,
            committed_range(
                SectionKind::SourceIdentity,
                layout.source_identity,
                &view.as_ref()[layout.source_identity.range()],
            )?,
            committed_range(
                SectionKind::RecipeFact,
                layout.recipe_fact,
                &view.as_ref()[layout.recipe_fact.range()],
            )?,
        ];
        Ok(Self(FragmentRangeManifestView {
            fragment: ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                view.as_ref(),
            ),
            fragment_length,
            source: view.source,
            recipe: view.recipe,
            ranges,
        }))
    }

    /// Validates one fetched body before yielding a borrow carrying its exact range facts.
    pub fn verify<'body>(
        &self,
        request: FragmentRangeRequest<'body>,
    ) -> Result<VerifiedFragmentRange<'body>, FragmentRangeVerifyError> {
        if request.fragment_length != self.fragment_length {
            return Err(FragmentRangeVerifyError::FragmentLength {
                expected: self.fragment_length,
                observed: request.fragment_length,
            });
        }
        let expected = self.range(request.section)?;
        if request.offset != expected.offset {
            return Err(FragmentRangeVerifyError::Offset {
                section: request.section,
                expected: expected.offset,
                observed: request.offset,
            });
        }
        let observed_length = u32::try_from(request.bytes.len()).map_err(|source| {
            FragmentRangeVerifyError::BodyLengthAddressSpace {
                section: request.section,
                observed: request.bytes.len(),
                source,
            }
        })?;
        if observed_length != expected.length {
            return Err(FragmentRangeVerifyError::BodyLength {
                section: request.section,
                expected: expected.length,
                observed: observed_length,
            });
        }
        let observed = range_identity(
            request.section,
            request.offset,
            observed_length,
            request.bytes,
        );
        if observed != expected.identity {
            return Err(FragmentRangeVerifyError::Identity {
                section: request.section,
                expected: expected.identity,
                observed,
            });
        }
        Ok(VerifiedFragmentRange(VerifiedFragmentRangeView {
            section: request.section,
            offset: request.offset,
            fragment_length: request.fragment_length,
            bytes: request.bytes,
        }))
    }

    /// Validates the complete artifact commitment and IR grammar before yielding its view.
    pub fn verify_fragment<'fragment>(
        &self,
        fragment: &'fragment [u8],
    ) -> Result<FragmentView<'fragment>, FragmentRangeVerifyError> {
        let layout = self.verify_fragment_layout(fragment)?;
        Ok(FragmentView::from_validated_layout(fragment, layout))
    }

    pub(crate) fn verify_fragment_layout(
        &self,
        fragment: &[u8],
    ) -> Result<FragmentLayout, FragmentRangeVerifyError> {
        let observed_length = u32::try_from(fragment.len()).map_err(|source| {
            FragmentRangeVerifyError::FragmentLengthAddressSpace {
                observed: fragment.len(),
                source,
            }
        })?;
        if observed_length != self.fragment_length {
            return Err(FragmentRangeVerifyError::FragmentLength {
                expected: self.fragment_length,
                observed: observed_length,
            });
        }
        let observed =
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(fragment);
        if observed != self.fragment {
            return Err(FragmentRangeVerifyError::FragmentIdentity {
                expected: self.fragment,
                observed,
            });
        }
        validate_fragment_layout(fragment).map_err(FragmentRangeVerifyError::Fragment)
    }

    fn range(&self, section: SectionKind) -> Result<FragmentRange, FragmentRangeVerifyError> {
        let index = match section {
            SectionKind::EntityTypes => 0,
            SectionKind::TypeNodes => 1,
            SectionKind::AtomRecords => 2,
            SectionKind::AtomBytes => 3,
            SectionKind::SourceIdentity => 4,
            SectionKind::RecipeFact => 5,
            // The range manifest commits required semantic lanes only; the
            // optional semantic-data and occurrence sections have no
            // manifest row.
            SectionKind::SemanticData | SectionKind::Occurrences => {
                return Err(FragmentRangeVerifyError::SectionNotCommitted { section });
            }
            SectionKind::TypeFacts => {
                return Err(FragmentRangeVerifyError::SectionNotCommitted { section });
            }
        };
        Ok(self.ranges[index])
    }
}

/// One untrusted range body and its external addressing claims.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FragmentRangeRequest<'body> {
    /// Section claimed by the response.
    pub section: SectionKind,
    /// Claimed byte offset in the complete fragment.
    pub offset: u32,
    /// Claimed complete fragment length.
    pub fragment_length: u32,
    /// Borrowed response body.
    pub bytes: &'body [u8],
}

/// Immutable facts yielded only after manifest validation succeeds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedFragmentRangeView<'body> {
    /// Verified section kind.
    pub section: SectionKind,
    /// Verified byte offset in the complete fragment.
    pub offset: u32,
    /// Verified complete fragment length.
    pub fragment_length: u32,
    /// Exact body whose length and identity were verified.
    pub bytes: &'body [u8],
}

/// A borrowed section body validated against one immutable range manifest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct VerifiedFragmentRange<'body>(VerifiedFragmentRangeView<'body>);

impl<'body> Deref for VerifiedFragmentRange<'body> {
    type Target = VerifiedFragmentRangeView<'body>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Failure while deriving fixed range commitments from a validated fragment.
#[derive(Debug, Error)]
pub enum FragmentRangeManifestError {
    /// Complete fragment length cannot fit the fixed range coordinate.
    #[error("validated fragment length {observed} exceeds the range coordinate")]
    FragmentLength {
        /// Complete observed fragment length.
        observed: usize,
        /// Original checked conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// Section offset cannot fit the fixed range coordinate.
    #[error("section {section:?} offset {observed} exceeds the range coordinate")]
    Offset {
        /// Section whose offset was rejected.
        section: SectionKind,
        /// Complete observed offset.
        observed: usize,
        /// Original checked conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// Section length cannot fit the fixed range coordinate.
    #[error("section {section:?} length {observed} exceeds the range coordinate")]
    SectionLength {
        /// Section whose length was rejected.
        section: SectionKind,
        /// Complete observed section length.
        observed: usize,
        /// Original checked conversion failure.
        #[source]
        source: TryFromIntError,
    },
}

/// Typed rejection before an untrusted range body can become a borrowed verified view.
#[derive(Debug, Error)]
pub enum FragmentRangeVerifyError {
    /// Native fragment length cannot fit the fixed range coordinate.
    #[error("fragment has unaddressable length {observed}")]
    FragmentLengthAddressSpace {
        /// Complete native fragment length.
        observed: usize,
        /// Original checked conversion failure.
        #[source]
        source: TryFromIntError,
    },
    /// Response names a different complete fragment length.
    #[error("range response fragment length is {observed}, expected {expected}")]
    FragmentLength { expected: u32, observed: u32 },
    /// Complete fragment bytes do not satisfy the manifest artifact commitment.
    #[error("complete fragment identity mismatch")]
    FragmentIdentity {
        expected: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
        observed: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    },
    /// Complete bytes satisfy the artifact commitment but violate the IR grammar.
    #[error("committed fragment grammar is invalid")]
    Fragment(#[source] FragmentError),
    /// The request names an optional section that this manifest does not commit.
    #[error("range manifest does not commit optional section {section:?}")]
    SectionNotCommitted {
        /// Requested section without a manifest commitment.
        section: SectionKind,
    },
    /// Response offset differs from its section commitment.
    #[error("range response section {section:?} starts at {observed}, expected {expected}")]
    Offset {
        section: SectionKind,
        expected: u32,
        observed: u32,
    },
    /// Native response length cannot fit the fixed range coordinate.
    #[error("range response section {section:?} has unaddressable length {observed}")]
    BodyLengthAddressSpace {
        section: SectionKind,
        observed: usize,
        #[source]
        source: TryFromIntError,
    },
    /// Response body length differs from its section commitment.
    #[error("range response section {section:?} has {observed} bytes, expected {expected}")]
    BodyLength {
        section: SectionKind,
        expected: u32,
        observed: u32,
    },
    /// Response body does not satisfy its exact typed commitment.
    #[error("range response section {section:?} identity mismatch")]
    Identity {
        section: SectionKind,
        expected: ArtifactId<IrFragmentRangeEncoding, IrFragmentDomain>,
        observed: ArtifactId<IrFragmentRangeEncoding, IrFragmentDomain>,
    },
}

fn committed_range(
    section: SectionKind,
    lane: LaneLayout,
    bytes: &[u8],
) -> Result<FragmentRange, FragmentRangeManifestError> {
    let offset =
        u32::try_from(lane.start_index).map_err(|source| FragmentRangeManifestError::Offset {
            section,
            observed: lane.start_index,
            source,
        })?;
    let length =
        u32::try_from(bytes.len()).map_err(|source| FragmentRangeManifestError::SectionLength {
            section,
            observed: bytes.len(),
            source,
        })?;
    Ok(FragmentRange {
        section,
        offset,
        length,
        identity: range_identity(section, offset, length, bytes),
    })
}

fn range_identity(
    section: SectionKind,
    offset: u32,
    length: u32,
    bytes: &[u8],
) -> ArtifactId<IrFragmentRangeEncoding, IrFragmentDomain> {
    let mut hasher = ArtifactHasher::<IrFragmentRangeEncoding, IrFragmentDomain>::new();
    hasher.write_chunk(&u16::from(section).to_le_bytes());
    hasher.write_chunk(&offset.to_le_bytes());
    hasher.write_chunk(&length.to_le_bytes());
    hasher.write_chunk(bytes);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use compiler_ir_vocabulary::{AtomId, TypeId};
    use compiler_vocabulary::{CompileRecipeFact, Language, NativeTool, Stage};
    use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
    use thiserror::Error;

    use super::{ArtifactId, FragmentRangeManifest, IrFragmentDomain, IrFragmentEncoding};
    use crate::{
        AtomInput, EntityKind, EntityRecord, EntityRecordFault, FragmentError,
        FragmentRangeVerifyError, FragmentView, PrepareError, PreparedFragment, PrimitiveType,
        SourceIdentity, TypeNode, WriteError,
    };

    #[derive(Debug, Error)]
    enum TestFailure {
        #[error(transparent)]
        Prepare(#[from] PrepareError),
        #[error(transparent)]
        Write(#[from] WriteError),
        #[error(transparent)]
        Validate(#[from] FragmentError),
        #[error(transparent)]
        Manifest(#[from] super::FragmentRangeManifestError),
        #[error("fixture source length {actual} does not fit the compact source fact")]
        SourceLength {
            actual: usize,
            #[source]
            source: core::num::TryFromIntError,
        },
        #[error("fixture entity offset {actual} does not fit the current address space")]
        EntityOffset {
            actual: u32,
            #[source]
            source: core::num::TryFromIntError,
        },
    }

    #[test]
    fn committed_bytes_grammar_is_rejected_after_forced_artifact_identity_match()
    -> Result<(), TestFailure> {
        let source_bytes = b"grammar-source";
        let source = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
            byte_len: u32::try_from(source_bytes.len()).map_err(|source| {
                TestFailure::SourceLength {
                    actual: source_bytes.len(),
                    source,
                }
            })?,
        };
        let recipe = CompileRecipeFact::derive(
            Language::Rust,
            Stage::LowerIr,
            NativeTool::Rustc,
            source.identity,
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"grammar-toolchain"),
        );
        let entities = [EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        }];
        let nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
        let atoms = [AtomInput { bytes: b"alpha" }];
        let prepared = PreparedFragment::prepare(source, recipe, &entities, &nodes, &atoms)?;
        let mut bytes = [0; 256];
        let length = prepared.write_into(&mut bytes)?.len();
        let view = FragmentView::validate(&bytes[..length])?;
        let mut manifest = FragmentRangeManifest::from_view(&view)?;
        let entity_start = usize::try_from(manifest.ranges[0].offset).map_err(|source| {
            TestFailure::EntityOffset {
                actual: manifest.ranges[0].offset,
                source,
            }
        })?;
        let mut corrupted = bytes;
        corrupted[entity_start + 8] = u8::MAX;
        manifest.0.fragment =
            ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
                &corrupted[..length],
            );
        assert!(matches!(
            manifest.verify_fragment(&corrupted[..length]),
            Err(FragmentRangeVerifyError::Fragment(
                FragmentError::EntityRecord {
                    fault: EntityRecordFault::Kind { actual: 255 },
                    ..
                }
            ))
        ));
        Ok(())
    }
}
