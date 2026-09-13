//! Falsifiers for the static semantic-reader and strict neutral-render seam.

use compiler_ir::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity, EntityAuthorityFacts,
    EntityId, EntityVersion, ExternalDeclarationIdentity, ExternalEntityRef, ExternalFragmentId,
    ExternalTarget, ExternalTargetIdentity, FactAvailability, ForeignDeclarationId,
    ForeignExternalTarget, ForeignTargetOrigin, IrBuilder, ItemKind, LanguageExtensionInput,
    LanguageProfile, ParentageAuthority, RenderFailure, SemanticReader, StableRef, TreeItemInput,
    TypeScriptFacts, VariantAvailability, VariantFingerprint, Visibility, prepare_neutral,
    prepare_profile,
};
use backend_semantic::vocabulary::{CStandard, TypeScriptSource};

fn version(family: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([family; 16]),
        variant: VariantFingerprint::from_raw([family.wrapping_add(1); 16]),
        core_payload: CorePayloadHash::from_raw([family.wrapping_add(2); 16]),
    }
}

fn authority(visibility: FactAvailability) -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        visibility,
        ..EntityAuthorityFacts::default()
    }
}

fn tree<'source>(
    versions: &'source [EntityVersion],
    items: &'source [TreeItemInput<'source>],
) -> BorrowedTree<'source> {
    BorrowedTree {
        versions,
        items,
        links: &[],
    }
}

fn generic_count<R: SemanticReader>(reader: &R) -> usize {
    reader.canonical_entities().len()
}

#[test]
fn owned_ir_reads_through_static_rows_and_preserves_empty_authority()
-> Result<(), compiler_ir::BuildError> {
    let versions = [version(1), version(2)];
    let items = [
        TreeItemInput {
            name: b"captured-empty",
            kind: ItemKind::Module,
            visibility: Visibility::Private,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                members: FactAvailability::Captured,
                documentation: FactAvailability::Captured,
                attributes: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"unavailable-empty",
            kind: ItemKind::Module,
            visibility: Visibility::Unknown,
            authority: authority(FactAvailability::Unavailable),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
    ];
    let mut builder = IrBuilder::new();
    builder.add_borrowed_tree(tree(&versions, &items))?;
    let ir = builder.finish()?;

    assert_eq!(generic_count(&ir), 2);
    let captured = SemanticReader::entity(&ir, EntityId::new(0)).expect("captured row");
    let unavailable = SemanticReader::entity(&ir, EntityId::new(1)).expect("unavailable row");
    assert_eq!(captured.authority.members, FactAvailability::Captured);
    assert_eq!(unavailable.authority.members, FactAvailability::Unavailable);
    assert_ne!(
        captured.authority.documentation,
        unavailable.authority.documentation
    );
    Ok(())
}

#[test]
fn neutral_writer_is_all_or_nothing_and_visibility_is_not_collapsed()
-> Result<(), compiler_ir::BuildError> {
    let versions = [version(1), version(2)];
    let items = [
        TreeItemInput {
            name: b"unknown",
            kind: ItemKind::Module,
            visibility: Visibility::Unknown,
            authority: authority(FactAvailability::Unavailable),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"private",
            kind: ItemKind::Module,
            visibility: Visibility::Private,
            authority: authority(FactAvailability::Captured),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
    ];
    let mut builder = IrBuilder::new();
    builder.add_borrowed_tree(tree(&versions, &items))?;
    let ir = builder.finish()?;
    let unknown = prepare_neutral(&ir, EntityId::new(0)).expect("neutral unknown");
    let private = prepare_neutral(&ir, EntityId::new(1)).expect("neutral private");
    let mut sentinel = [0xa5; 96];
    let before = sentinel;
    let short = unknown.encoded_len - 1;
    assert!(matches!(
        unknown.write_into(&mut sentinel[..short]),
        Err(RenderFailure::OutputTooSmall { available, required_at_least, .. })
            if available == short && required_at_least == unknown.encoded_len
    ));
    assert_eq!(sentinel, before);
    let mut unknown_output = [0; 96];
    let mut private_output = [0; 96];
    let unknown_text = unknown
        .write_into(&mut unknown_output)
        .expect("unknown output");
    let private_text = private
        .write_into(&mut private_output)
        .expect("private output");
    assert!(unknown_text.contains("visibility=unknown"));
    assert!(private_text.contains("visibility=private"));
    assert_ne!(unknown_text, private_text);
    assert!(matches!(
        prepare_profile(LanguageProfile::C(CStandard::C23), &ir, EntityId::new(0)),
        Err(RenderFailure::Unsupported(
            compiler_ir::UnsupportedSemanticStage::CFamilyDeclarator { .. }
        ))
    ));
    Ok(())
}

#[test]
fn neutral_output_uses_canonical_reader_order_not_authority_input_order()
-> Result<(), compiler_ir::BuildError> {
    let alpha = TreeItemInput {
        name: b"alpha",
        kind: ItemKind::Module,
        visibility: Visibility::Unknown,
        authority: authority(FactAvailability::Unavailable),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    };
    let beta = TreeItemInput {
        name: b"beta",
        kind: ItemKind::Module,
        visibility: Visibility::Unknown,
        authority: authority(FactAvailability::Unavailable),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    };
    let mut first_builder = IrBuilder::new();
    first_builder.add_borrowed_tree(tree(&[version(2), version(1)], &[alpha, beta]))?;
    let first = first_builder.finish()?;
    let mut reversed_builder = IrBuilder::new();
    reversed_builder.add_borrowed_tree(tree(&[version(1), version(2)], &[beta, alpha]))?;
    let reversed = reversed_builder.finish()?;

    fn render_all<R: SemanticReader>(reader: &R) -> Vec<(DeclarationIdentity, Vec<u8>, Vec<u8>)> {
        reader
            .canonical_entities()
            .map(|entity| {
                let name = reader
                    .atom(entity.name)
                    .expect("resolved canonical atom")
                    .to_vec();
                let prepared = prepare_neutral(reader, entity.id).expect("neutral row");
                let mut output = vec![0; prepared.encoded_len];
                prepared
                    .write_into(&mut output)
                    .expect("exact neutral output");
                (entity.version.identity(), name, output)
            })
            .collect()
    }

    assert_eq!(render_all(&first), render_all(&reversed));
    Ok(())
}

#[test]
fn reader_retains_distinct_stable_foreign_and_fragment_external_values()
-> Result<(), compiler_ir::BuildError> {
    let mut builder = IrBuilder::new();
    let ecosystem = builder.intern_atom(b"registry")?;
    let path = builder.intern_atom(b"pkg::Thing")?;
    let display = builder.intern_atom(b"Thing")?;
    let fragment = ExternalFragmentId::from_canonical_bytes(b"reader-render-fragment");
    let declaration = DeclarationIdentity {
        family: DeclarationFamilyId::from_raw([3; 16]),
        variant: VariantFingerprint::from_raw([4; 16]),
    };
    let stable = builder.intern_external(ExternalTarget::Stable {
        target: StableRef {
            fragment,
            declaration,
        },
    })?;
    let foreign = builder.intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
        identity: ExternalDeclarationIdentity {
            foreign: ForeignDeclarationId::from_raw([5; 16]),
            variant: VariantAvailability::Unavailable,
        },
        origin: ForeignTargetOrigin::Unspecified { ecosystem },
        path,
        display,
        kind: None,
    }))?;
    let fragment_entity = builder.intern_external(ExternalTarget::FragmentEntity {
        target: ExternalEntityRef::bind(fragment, 7),
        display,
    })?;
    let ir = builder.finish()?;
    assert!(
        matches!(SemanticReader::external(&ir, stable), Some(ExternalTarget::Stable { target }) if target.fragment == fragment && target.declaration == declaration)
    );
    assert!(matches!(
        SemanticReader::external(&ir, foreign),
        Some(ExternalTarget::Foreign(_))
    ));
    assert!(matches!(
        SemanticReader::external(&ir, fragment_entity),
        Some(ExternalTarget::FragmentEntity { .. })
    ));
    let stable_identity = ExternalTargetIdentity::capture(&ir, stable)
        .expect("stable external identity belongs to this image");
    assert_ne!(
        stable_identity,
        ExternalTargetIdentity::capture(&ir, foreign)
            .expect("foreign external identity belongs to this image")
    );
    assert_ne!(
        stable_identity.in_scope([1; 32], [9; 32]),
        stable_identity.in_scope([2; 32], [9; 32]),
        "the same external endpoint in two packages must not alias"
    );
    assert_ne!(
        stable_identity.in_scope([1; 32], [9; 32]),
        stable_identity.in_scope([1; 32], [8; 32]),
        "the same external endpoint in two images must not alias"
    );
    Ok(())
}

#[test]
fn typed_extension_cursor_resolves_shared_interned_facts_per_entity()
-> Result<(), compiler_ir::BuildError> {
    let mut builder = IrBuilder::new();
    builder.set_language_profile(LanguageProfile::TypeScript(TypeScriptSource::TypeScript))?;
    let facts = TypeScriptFacts {
        type_parameters: builder.intern_type_parameters(&[])?,
        declared: None,
        observed: None,
    };
    let versions = [version(1), version(2)];
    let items = [
        TreeItemInput {
            name: b"first",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                visibility: FactAvailability::Captured,
                language_extension: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: Some(LanguageExtensionInput::TypeScript(&facts)),
        },
        TreeItemInput {
            name: b"second",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                visibility: FactAvailability::Captured,
                language_extension: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: Some(LanguageExtensionInput::TypeScript(&facts)),
        },
    ];
    builder.add_borrowed_tree(tree(&versions, &items))?;
    let ir = builder.finish()?;
    let rows: Vec<_> = SemanticReader::typescript_extensions(&ir).collect();
    assert_eq!(
        rows,
        vec![(EntityId::new(0), facts), (EntityId::new(1), facts)]
    );
    Ok(())
}
