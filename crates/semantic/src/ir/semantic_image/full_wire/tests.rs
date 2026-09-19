//! Hostile full-image seam falsifiers.
//!
//! These remain deliberately small: the image builder admits an ordinary
//! captured-empty declaration plane, then the test exercises the complete
//! writer/reopen boundary rather than a second fixture-only semantic model.

use alloc::vec;

use crate::ir::{
    BorrowedTree, CSharpFacts, CSharpMemberEffects, CSharpNullability, CSharpPartialRole,
    CSharpReferenceKind, CSharpVersion, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts,
    EntityVersion, FactAvailability, Ir, IrBuilder, ItemKind, LanguageExtensionInput,
    LanguageProfile, ParentageAuthority, SemanticCoreReader, SemanticImageAuthority,
    SemanticImageEncodeError, SemanticReader, TreeItemInput, TypeScriptSource, VariantFingerprint,
    Visibility,
};

use super::wire::{
    DIRECTORY_BYTES, FullDirectoryKind, HEADER_BYTES, RANGE_ROW_BYTES, SPARSE_BINDING_ROW_BYTES,
};
use super::{
    FullSemanticImageFault, SemanticImageView, encode_full_semantic_image, full_semantic_image_len,
};

fn version(value: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([value; 16]),
        variant: VariantFingerprint::from_raw([value.wrapping_add(1); 16]),
        core_payload: CorePayloadHash::from_raw([value.wrapping_add(2); 16]),
    }
}

fn authority() -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        members: FactAvailability::Captured,
        documentation: FactAvailability::Captured,
        attributes: FactAvailability::Captured,
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    }
}

fn image(reversed: bool) -> Result<Ir, crate::ir::BuildError> {
    let alpha = TreeItemInput {
        name: b"alpha",
        kind: ItemKind::Module,
        visibility: Visibility::Private,
        authority: authority(),
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
        ..alpha
    };
    let mut builder = IrBuilder::new();
    if reversed {
        let versions = [version(2), version(1)];
        let items = [beta, alpha];
        builder.add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })?;
    } else {
        let versions = [version(1), version(2)];
        let items = [alpha, beta];
        builder.add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })?;
    }
    builder.finish()
}

fn encoded(ir: &Ir) -> Result<alloc::vec::Vec<u8>, crate::ir::BuildError> {
    let length = full_semantic_image_len(ir).expect("full image plan is admitted");
    let mut bytes = vec![0; length];
    encode_full_semantic_image(ir, &mut bytes).expect("full image writes");
    Ok(bytes)
}

fn csharp_image() -> Result<Ir, crate::ir::BuildError> {
    let mut builder = IrBuilder::new();
    builder.set_language_profile(LanguageProfile::CSharp(CSharpVersion::CSharp14))?;
    let constraints = builder.intern_type_parameters(&[])?;
    let attributes = builder.intern_attributes(&[])?;
    let base = CSharpFacts {
        nullability: CSharpNullability::Oblivious,
        reference_kind: CSharpReferenceKind::Value,
        constraints,
        effects: CSharpMemberEffects {
            is_async: false,
            is_iterator: false,
            is_extension: false,
        },
        attributes,
        partial: CSharpPartialRole::None,
        xml_provenance: None,
    };
    let alternate = CSharpFacts {
        nullability: CSharpNullability::Nullable,
        ..base
    };
    let authority = EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        visibility: FactAvailability::Captured,
        language_extension: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let first = TreeItemInput {
        name: b"first",
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority,
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: Some(LanguageExtensionInput::CSharp(&base)),
    };
    let second = TreeItemInput {
        name: b"second",
        extension: Some(LanguageExtensionInput::CSharp(&alternate)),
        ..first
    };
    let versions = [version(1), version(2)];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &[first, second],
        links: &[],
    })?;
    builder.finish()
}

fn directory(bytes: &[u8], kind: FullDirectoryKind) -> (usize, u32) {
    let offset = HEADER_BYTES + kind.index() * DIRECTORY_BYTES;
    let payload = u32::from_le_bytes(
        bytes[offset + 4..offset + 8]
            .try_into()
            .expect("directory offset"),
    );
    let count = u32::from_le_bytes(
        bytes[offset + 12..offset + 16]
            .try_into()
            .expect("directory count"),
    );
    (usize::try_from(payload).expect("test address space"), count)
}

fn binding_offset(bytes: &[u8], kind: FullDirectoryKind, row: u32) -> usize {
    let (offset, count) = directory(bytes, kind);
    assert!(row < count, "binding row is present");
    offset + usize::try_from(row).expect("test row") * SPARSE_BINDING_ROW_BYTES
}

fn binding_fact(bytes: &[u8], kind: FullDirectoryKind, row: u32) -> u32 {
    let offset = binding_offset(bytes, kind, row);
    u32::from_le_bytes(
        bytes[offset + 4..offset + 8]
            .try_into()
            .expect("binding fact"),
    )
}

fn set_binding_fact(bytes: &mut [u8], kind: FullDirectoryKind, row: u32, fact: u32) {
    let offset = binding_offset(bytes, kind, row);
    bytes[offset + 4..offset + 8].copy_from_slice(&fact.to_le_bytes());
}

fn fact_value_range(bytes: &[u8], kind: FullDirectoryKind, row: u32) -> core::ops::Range<usize> {
    let (offset, count) = directory(bytes, kind);
    assert!(row < count, "fact row is present");
    let range = offset + usize::try_from(row).expect("test row") * RANGE_ROW_BYTES;
    let start = u32::from_le_bytes(bytes[range..range + 4].try_into().expect("fact start"));
    let len = u32::from_le_bytes(bytes[range + 4..range + 8].try_into().expect("fact length"));
    let values = offset + usize::try_from(count).expect("test count") * RANGE_ROW_BYTES;
    let start = usize::try_from(start).expect("test start");
    let len = usize::try_from(len).expect("test length");
    values + start..values + start + len
}

#[test]
fn full_image_is_canonical_reopens_complete_reader_and_keeps_borrowed_atoms()
-> Result<(), crate::ir::BuildError> {
    let first = image(false)?;
    let reversed = image(true)?;
    let bytes = encoded(&first)?;
    assert_eq!(bytes, encoded(&reversed)?);

    let view = SemanticImageView::reopen(&bytes).expect("full image reopens");
    let owned = first
        .canonical_entities()
        .map(|entity| {
            (
                entity.version.identity(),
                first.atom(entity.name).map(<[u8]>::to_vec),
            )
        })
        .collect::<alloc::vec::Vec<_>>();
    let reopened = view
        .canonical_entities()
        .map(|entity| {
            (
                entity.version.identity(),
                view.atom(entity.name).map(<[u8]>::to_vec),
            )
        })
        .collect::<alloc::vec::Vec<_>>();
    assert_eq!(owned, reopened);
    assert_eq!(
        view.canonical_types().count(),
        first.canonical_types().count()
    );
    assert_eq!(
        view.canonical_externals().count(),
        first.canonical_externals().count()
    );
    let entity = view.canonical_entities().next().expect("first entity");
    assert_eq!(entity.authority.members, FactAvailability::Captured);
    assert_eq!(entity.authority.documentation, FactAvailability::Captured);
    let atom = view.atom(entity.name).expect("reopened atom");
    let repeated = view.atom(entity.name).expect("repeated atom");
    assert!(core::ptr::eq(atom.as_ptr(), repeated.as_ptr()));
    let start = bytes.as_ptr().addr();
    assert!(
        atom.as_ptr().addr() >= start && atom.as_ptr().addr() + atom.len() <= start + bytes.len()
    );
    Ok(())
}

#[test]
fn full_image_short_output_and_directory_substitution_are_exact_and_non_mutating()
-> Result<(), crate::ir::BuildError> {
    let ir = image(false)?;
    let length = full_semantic_image_len(&ir).expect("full plan");
    let mut short = vec![0xa5; length.saturating_sub(1)];
    let before = short.clone();
    assert!(matches!(
        encode_full_semantic_image(&ir, &mut short),
        Err(SemanticImageEncodeError::Wire(FullSemanticImageFault::OutputTooShort { required, actual }))
            if required == length && actual == before.len()
    ));
    assert_eq!(short, before);
    let mut bytes = encoded(&ir)?;
    bytes[HEADER_BYTES] = 0xff;
    assert!(matches!(
        SemanticImageView::reopen(&bytes),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::DirectoryKind {
                expected: FullDirectoryKind::Atoms,
                observed: 0xff,
                ..
            }
        ))
    ));
    Ok(())
}

#[test]
fn full_image_rejects_noncanonical_extension_bindings_and_fact_pools()
-> Result<(), crate::ir::BuildError> {
    let bytes = encoded(&csharp_image()?)?;
    let facts = FullDirectoryKind::CSharpFacts;
    let bindings = FullDirectoryKind::CSharpBindings;
    assert_eq!(directory(&bytes, facts).1, 2);
    assert_eq!(directory(&bytes, bindings).1, 2);

    let mut duplicate_entity = bytes.clone();
    let first_binding = binding_offset(&duplicate_entity, bindings, 0);
    let second_binding = binding_offset(&duplicate_entity, bindings, 1);
    let entity: [u8; 4] = duplicate_entity[first_binding..first_binding + 4]
        .try_into()
        .expect("first entity");
    duplicate_entity[second_binding..second_binding + 4].copy_from_slice(&entity);
    assert!(matches!(
        SemanticImageView::reopen(&duplicate_entity),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::DuplicateExtensionBinding {
                plane: FullDirectoryKind::CSharpFacts,
                entity: 0,
                existing: 0,
                row: 1,
            }
        ))
    ));

    let mut orphan = bytes.clone();
    let first_fact = binding_fact(&orphan, bindings, 0);
    let second_fact = binding_fact(&orphan, bindings, 1);
    assert_ne!(first_fact, second_fact);
    set_binding_fact(&mut orphan, bindings, 1, first_fact);
    assert!(matches!(
        SemanticImageView::reopen(&orphan),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::UnboundExtensionFact {
                plane: FullDirectoryKind::CSharpFacts,
                fact,
            }
        )) if fact == second_fact
    ));

    let mut duplicate_fact = bytes.clone();
    let first = fact_value_range(&duplicate_fact, facts, 0);
    let second = fact_value_range(&duplicate_fact, facts, 1);
    assert_eq!(first.len(), second.len());
    let first_value = duplicate_fact[first].to_vec();
    duplicate_fact[second].copy_from_slice(&first_value);
    assert!(matches!(
        SemanticImageView::reopen(&duplicate_fact),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::DuplicateCanonicalRow {
                field: super::FullSemanticImageField::ExtensionFacts,
                existing: 0,
                row: 1,
            }
        ))
    ));

    let mut non_selected_plane = bytes.clone();
    let profile = <[u8; 2]>::from(LanguageProfile::TypeScript(TypeScriptSource::TypeScript));
    non_selected_plane[17..19].copy_from_slice(&profile);
    assert!(matches!(
        SemanticImageView::reopen(&non_selected_plane),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::ExtensionPlaneAuthority {
                plane: FullDirectoryKind::CSharpFacts,
                authority: SemanticImageAuthority::Language(LanguageProfile::TypeScript(
                    TypeScriptSource::TypeScript
                )),
                facts: 2,
                bindings: 2,
                ..
            }
        ))
    ));

    let mut permuted_facts = bytes;
    let first = fact_value_range(&permuted_facts, facts, 0);
    let second = fact_value_range(&permuted_facts, facts, 1);
    assert_eq!(first.len(), second.len());
    let first_value = permuted_facts[first.clone()].to_vec();
    let second_value = permuted_facts[second.clone()].to_vec();
    permuted_facts[first].copy_from_slice(&second_value);
    permuted_facts[second].copy_from_slice(&first_value);
    for row in 0..directory(&permuted_facts, bindings).1 {
        let fact = binding_fact(&permuted_facts, bindings, row);
        let remapped = match fact {
            0 => 1,
            1 => 0,
            _ => unreachable!("two-fact fixture"),
        };
        set_binding_fact(&mut permuted_facts, bindings, row, remapped);
    }
    assert!(matches!(
        SemanticImageView::reopen(&permuted_facts),
        Err(super::FullSemanticImageError::Full(
            FullSemanticImageFault::CanonicalOrder {
                field: super::FullSemanticImageField::ExtensionFacts,
                previous: 0,
                row: 1,
            }
        ))
    ));
    Ok(())
}
