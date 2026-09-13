//! Exercises the `compiler-ir` tests range-manifest contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use compiler_ir::{AtomId, TypeId};
use compiler_ir::{
    AtomInput, EntityKind, EntityRecord, FragmentRangeManifest, FragmentRangeRequest,
    FragmentRangeVerifyError, FragmentView, PreparedFragment, PrimitiveType, SectionKind,
    SourceIdentity, TypeNode,
};
use backend_semantic::vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use thiserror::Error;

#[derive(Debug, Error)]
enum TestFailure {
    #[error(transparent)]
    Prepare(#[from] compiler_ir::PrepareError),
    #[error(transparent)]
    Write(#[from] compiler_ir::WriteError),
    #[error(transparent)]
    Validate(#[from] compiler_ir::FragmentError),
    #[error(transparent)]
    Manifest(#[from] compiler_ir::FragmentRangeManifestError),
    #[error(transparent)]
    Verify(#[from] compiler_ir::FragmentRangeVerifyError),
    #[error("fixture coordinate {actual} does not fit the current address space")]
    Coordinate {
        actual: u32,
        #[source]
        source: core::num::TryFromIntError,
    },
}

fn fragment() -> Result<([u8; 256], usize), TestFailure> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"range-source"),
        byte_len: 12,
    };
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Constant,
    }];
    let nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
    let atoms = [AtomInput { bytes: b"alpha" }];
    let prepared = PreparedFragment::prepare(
        source,
        recipe_fact(source.identity),
        &entities,
        &nodes,
        &atoms,
    )?;
    let mut output = [0; 256];
    let length = prepared.required_capacity();
    let _bytes = prepared.write_into(&mut output)?;
    Ok((output, length))
}

fn recipe_fact(source: ContentId<SourceFactDomain>) -> CompileRecipeFact {
    CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"range-toolchain"),
    )
}

#[test]
fn complete_and_section_views_exist_only_after_exact_manifest_validation() -> Result<(), TestFailure>
{
    let (bytes, length) = fragment()?;
    let view = FragmentView::validate(&bytes[..length])?;
    let manifest = FragmentRangeManifest::from_view(&view)?;
    assert_eq!(manifest.ranges.len(), 6);
    assert_eq!(manifest.source, view.source);
    assert_eq!(
        manifest.ranges.map(|range| range.section),
        [
            SectionKind::EntityTypes,
            SectionKind::TypeNodes,
            SectionKind::AtomRecords,
            SectionKind::AtomBytes,
            SectionKind::SourceIdentity,
            SectionKind::RecipeFact,
        ]
    );
    for range in manifest.ranges {
        let start = usize::try_from(range.offset).map_err(|source| TestFailure::Coordinate {
            actual: range.offset,
            source,
        })?;
        let end = start
            + usize::try_from(range.length).map_err(|source| TestFailure::Coordinate {
                actual: range.length,
                source,
            })?;
        let verified = manifest.verify(FragmentRangeRequest {
            section: range.section,
            offset: range.offset,
            fragment_length: manifest.fragment_length,
            bytes: &view.as_ref()[start..end],
        })?;
        assert_eq!(verified.section, range.section);
    }
    let reopened = manifest.verify_fragment(view.as_ref())?;
    assert_eq!(reopened.as_ref(), view.as_ref());
    Ok(())
}

#[test]
fn empty_required_ranges_are_committed_and_verified_without_aliasing_sections()
-> Result<(), TestFailure> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"empty"),
        byte_len: 5,
    };
    let prepared = PreparedFragment::prepare(source, recipe_fact(source.identity), &[], &[], &[])?;
    let mut output = [0; 256];
    let bytes = prepared.write_into(&mut output)?;
    let view = FragmentView::validate(bytes)?;
    let manifest = FragmentRangeManifest::from_view(&view)?;
    assert_eq!(manifest.ranges[0].length, 0);
    assert_eq!(manifest.ranges[2].length, 0);
    assert_ne!(manifest.ranges[0].identity, manifest.ranges[2].identity);
    for range in [manifest.ranges[0], manifest.ranges[2]] {
        let verified = manifest.verify(FragmentRangeRequest {
            section: range.section,
            offset: range.offset,
            fragment_length: manifest.fragment_length,
            bytes: &[],
        })?;
        assert_eq!(verified.section, range.section);
        assert!(verified.bytes.is_empty());
    }
    Ok(())
}

#[test]
fn offset_length_and_body_mutations_retain_the_exact_rejected_facts() -> Result<(), TestFailure> {
    let (bytes, length) = fragment()?;
    let view = FragmentView::validate(&bytes[..length])?;
    let manifest = FragmentRangeManifest::from_view(&view)?;
    let expected = manifest.ranges[3];
    assert!(matches!(
        manifest.verify(FragmentRangeRequest {
            section: SectionKind::AtomBytes,
            offset: expected.offset,
            fragment_length: manifest.fragment_length + 1,
            bytes: b"alpha",
        }),
        Err(FragmentRangeVerifyError::FragmentLength {
            expected: observed_expected,
            observed,
        }) if observed_expected == manifest.fragment_length && observed == manifest.fragment_length + 1
    ));
    assert!(matches!(
        manifest.verify(FragmentRangeRequest {
            section: SectionKind::AtomBytes,
            offset: expected.offset + 1,
            fragment_length: manifest.fragment_length,
            bytes: b"alpha",
        }),
        Err(FragmentRangeVerifyError::Offset {
            section: SectionKind::AtomBytes,
            expected: observed_expected,
            observed,
        }) if observed_expected == expected.offset && observed == expected.offset + 1
    ));
    assert!(matches!(
        manifest.verify(FragmentRangeRequest {
            section: SectionKind::AtomBytes,
            offset: expected.offset,
            fragment_length: manifest.fragment_length,
            bytes: b"alph",
        }),
        Err(FragmentRangeVerifyError::BodyLength {
            section: SectionKind::AtomBytes,
            expected: observed_expected,
            observed: 4,
        }) if observed_expected == expected.length
    ));
    assert!(matches!(
        manifest.verify(FragmentRangeRequest {
            section: SectionKind::SourceIdentity,
            offset: expected.offset,
            fragment_length: manifest.fragment_length,
            bytes: b"alpha",
        }),
        Err(FragmentRangeVerifyError::Offset {
            section: SectionKind::SourceIdentity,
            observed,
            ..
        }) if observed == expected.offset
    ));
    assert!(matches!(
        manifest.verify(FragmentRangeRequest {
            section: SectionKind::AtomBytes,
            offset: expected.offset,
            fragment_length: manifest.fragment_length,
            bytes: b"bravo",
        }),
        Err(FragmentRangeVerifyError::Identity {
            section: SectionKind::AtomBytes,
            expected: expected_identity,
            observed,
        }) if expected_identity == expected.identity && observed != expected_identity
    ));
    assert!(matches!(
        manifest.verify_fragment(&view.as_ref()[..view.as_ref().len() - 1]),
        Err(FragmentRangeVerifyError::FragmentLength {
            expected: observed_expected,
            observed,
        }) if observed_expected == manifest.fragment_length && observed + 1 == observed_expected
    ));
    let mut changed = bytes;
    changed[length - 1] ^= 1;
    assert!(matches!(
        manifest.verify_fragment(&changed[..length]),
        Err(FragmentRangeVerifyError::FragmentIdentity {
            expected: expected_identity,
            observed,
        }) if expected_identity == manifest.fragment && observed != expected_identity
    ));
    Ok(())
}
