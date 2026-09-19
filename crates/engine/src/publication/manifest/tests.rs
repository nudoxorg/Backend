//! Defines manifest tests behavior for the `backend-engine` publication, whose purpose is to publish verified compiler fragments as immutable generations.
//! This module owns the manifest tests invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use crate::driver::CompiledFragment;
use backend_semantic::ir::{AtomId, TypeId};
use backend_semantic::ir::{
    AtomInput, EntityKind, EntityRecord, FragmentView, PreparedFragment, PrimitiveType,
    SourceIdentity, TypeNode,
};
use backend_version::{
    ArtifactId, ContentId, IrManifestDomain, IrManifestEncoding, SourceFactDomain, ToolchainDomain,
};
use thiserror::Error;

use super::{
    CanonicalCompilation, CompilationManifestError, CompilationManifestView,
    CompilationPrepareError, CompilationWriteError,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage,
};

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Prepare(#[from] backend_semantic::ir::PrepareError),
    #[error(transparent)]
    Write(#[from] backend_semantic::ir::WriteError),
    #[error(transparent)]
    Fragment(#[from] backend_semantic::ir::FragmentError),
    #[error(transparent)]
    Canonical(#[from] CompilationPrepareError),
    #[error(transparent)]
    ManifestWrite(#[from] CompilationWriteError),
    #[error(transparent)]
    Manifest(#[from] CompilationManifestError),
    #[error("manifest was missing expected fragment {ordinal}")]
    MissingFragment { ordinal: u8 },
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "focused test retains exact canonicalization diagnostics"
)]
fn canonical_manifest_is_order_stable_and_uses_the_ir_manifest_identity() -> Result<(), TestError> {
    let (alpha_bytes, alpha_length) = fragment(b"alpha-source", b"alpha")?;
    let (bravo_bytes, bravo_length) = fragment(b"bravo-source", b"bravo")?;
    let mut first_scratch = [0; 2];
    let first_inputs = [
        compiled(FragmentView::validate(&bravo_bytes[..bravo_length])?),
        compiled(FragmentView::validate(&alpha_bytes[..alpha_length])?),
    ];
    let first = CanonicalCompilation::prepare(&first_inputs, &mut first_scratch)?;
    let mut first_output = [0_u8; 1024];
    let mut first_facts = [None; 2];
    let first_manifest = first.write_into(&mut first_output, &mut first_facts)?;
    requires_ir_manifest_identity(first_manifest.identity);

    let mut second_scratch = [0; 2];
    let second_inputs = [
        compiled(FragmentView::validate(&alpha_bytes[..alpha_length])?),
        compiled(FragmentView::validate(&bravo_bytes[..bravo_length])?),
    ];
    let second = CanonicalCompilation::prepare(&second_inputs, &mut second_scratch)?;
    let mut second_output = [0_u8; 1024];
    let mut second_facts = [None; 2];
    let second_manifest = second.write_into(&mut second_output, &mut second_facts)?;
    assert_eq!(first_manifest.as_ref(), second_manifest.as_ref());
    assert_eq!(first_manifest.identity, second_manifest.identity);

    let mut reopened_facts = [None; 2];
    let reopened = CompilationManifestView::validate(first_manifest.as_ref(), &mut reopened_facts)?;
    let first_entry = reopened
        .fragments()
        .next()
        .ok_or(TestError::MissingFragment { ordinal: 0 })?;
    let second_entry = reopened
        .fragments()
        .nth(1)
        .ok_or(TestError::MissingFragment { ordinal: 1 })?;
    assert!(first_entry.fragment < second_entry.fragment);
    assert_eq!(reopened.fragment_count, 2);
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "focused test retains exact canonicalization diagnostics"
)]
fn manifest_rejects_a_recipe_identity_mutant_without_losing_the_fragment_ordinal()
-> Result<(), TestError> {
    let (bytes, length) = fragment(b"alpha-source", b"alpha")?;
    let fragment = compiled(FragmentView::validate(&bytes[..length])?);
    let mut scratch = [0];
    let inputs = [fragment];
    let canonical = CanonicalCompilation::prepare(&inputs, &mut scratch)?;
    let mut output = [0_u8; 512];
    let mut facts = [None; 1];
    let manifest = canonical.write_into(&mut output, &mut facts)?;
    let mut mutant = [0_u8; 512];
    mutant[..manifest.as_ref().len()].copy_from_slice(manifest.as_ref());
    let identity_payload =
        super::COMPILATION_MANIFEST_HEADER_BYTES + super::build::RECIPE_IDENTITY_OFFSET + 2;
    mutant[identity_payload] ^= 1;
    let mut mutant_facts = [None; 1];
    assert!(matches!(
        CompilationManifestView::validate(&mutant[..manifest.as_ref().len()], &mut mutant_facts),
        Err(CompilationManifestError::RecipeIdentity { ordinal: 0, .. })
    ));
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "focused test retains exact canonicalization diagnostics"
)]
fn manifest_rejects_a_range_extent_overflow_without_losing_its_location() -> Result<(), TestError> {
    let (bytes, length) = fragment(b"alpha-source", b"alpha")?;
    let fragment = compiled(FragmentView::validate(&bytes[..length])?);
    let inputs = [fragment];
    let mut scratch = [0];
    let canonical = CanonicalCompilation::prepare(&inputs, &mut scratch)?;
    let mut output = [0_u8; 512];
    let mut facts = [None; 1];
    let manifest = canonical.write_into(&mut output, &mut facts)?;
    let mut mutant = [0_u8; 512];
    mutant[..manifest.as_ref().len()].copy_from_slice(manifest.as_ref());
    let offset = super::COMPILATION_MANIFEST_HEADER_BYTES + super::build::RANGE_OFFSET + 4;
    mutant[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let mut mutant_facts = [None; 1];
    assert!(matches!(
        CompilationManifestView::validate(&mutant[..manifest.as_ref().len()], &mut mutant_facts),
        Err(CompilationManifestError::RangeExtentOverflow {
            ordinal: 0,
            range: 0,
            ..
        })
    ));
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "focused test retains exact canonicalization diagnostics"
)]
fn empty_packages_use_empty_ordinal_scratch_and_have_a_canonical_header() -> Result<(), TestError> {
    let inputs: &[CompiledFragment<'_>] = &[];
    let mut scratch = [];
    let package = CanonicalCompilation::prepare(inputs, &mut scratch)?;
    assert_eq!(
        package.required_bytes()?,
        super::COMPILATION_MANIFEST_HEADER_BYTES
    );
    let mut output = [0_u8; super::COMPILATION_MANIFEST_HEADER_BYTES];
    let mut facts = [];
    let manifest = package.write_into(&mut output, &mut facts)?;
    assert_eq!(manifest.fragment_count, 0);
    assert!(manifest.fragments().next().is_none());
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "focused test retains exact canonicalization diagnostics"
)]
fn canonicalization_reports_undersized_ordinal_scratch() -> Result<(), TestError> {
    let (bytes, length) = fragment(b"alpha-source", b"alpha")?;
    let fragment = compiled(FragmentView::validate(&bytes[..length])?);
    let mut scratch = [];
    assert!(matches!(
        CanonicalCompilation::prepare(&[fragment], &mut scratch),
        Err(CompilationPrepareError::ScratchTooSmall {
            required: 1,
            available: 0,
        })
    ));
    Ok(())
}

fn requires_ir_manifest_identity(_: ArtifactId<IrManifestEncoding, IrManifestDomain>) {}

fn compiled(fragment: FragmentView<'_>) -> CompiledFragment<'_> {
    CompiledFragment {
        source: fragment.source,
        recipe: fragment.recipe,
        fragment,
    }
}

#[allow(
    clippy::result_large_err,
    reason = "focused fixture retains exact canonicalization diagnostics"
)]
fn fragment(source_bytes: &[u8], name: &[u8]) -> Result<([u8; 256], usize), TestError> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
        byte_len: 12,
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"manifest-toolchain"),
    );
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Constant,
    }];
    let nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
    let atoms = [AtomInput { bytes: name }];
    let prepared = PreparedFragment::prepare(source, recipe, &entities, &nodes, &atoms)?;
    let mut output = [0; 256];
    let length = prepared.required_capacity();
    let written = prepared.write_into(&mut output)?;
    assert_eq!(written.len(), length);
    Ok((output, length))
}
