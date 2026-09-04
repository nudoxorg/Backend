//! Hostile full-image seam falsifiers.
//!
//! These remain deliberately small: the image builder admits an ordinary
//! captured-empty declaration plane, then the test exercises the complete
//! writer/reopen boundary rather than a second fixture-only semantic model.

use alloc::vec;

use crate::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts,
    EntityVersion, FactAvailability, Ir, IrBuilder, ItemKind,
    ParentageAuthority, SemanticCoreReader, SemanticReader, TreeItemInput,
    SemanticImageEncodeError, VariantFingerprint, Visibility,
};

use super::{
    encode_full_semantic_image, full_semantic_image_len, FullDirectoryKind,
    FullSemanticImageFault, SemanticImageView,
};
use super::wire::HEADER_BYTES;

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

fn image(reversed: bool) -> Result<Ir, crate::BuildError> {
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
    let beta = TreeItemInput { name: b"beta", ..alpha };
    let mut builder = IrBuilder::new();
    if reversed {
        let versions = [version(2), version(1)];
        let items = [beta, alpha];
        builder.add_borrowed_tree(BorrowedTree { versions: &versions, items: &items, links: &[] })?;
    } else {
        let versions = [version(1), version(2)];
        let items = [alpha, beta];
        builder.add_borrowed_tree(BorrowedTree { versions: &versions, items: &items, links: &[] })?;
    }
    builder.finish()
}

fn encoded(ir: &Ir) -> Result<alloc::vec::Vec<u8>, crate::BuildError> {
    let length = full_semantic_image_len(ir).expect("full image plan is admitted");
    let mut bytes = vec![0; length];
    encode_full_semantic_image(ir, &mut bytes).expect("full image writes");
    Ok(bytes)
}

#[test]
fn full_image_is_canonical_reopens_complete_reader_and_keeps_borrowed_atoms()
-> Result<(), crate::BuildError> {
    let first = image(false)?;
    let reversed = image(true)?;
    let bytes = encoded(&first)?;
    assert_eq!(bytes, encoded(&reversed)?);

    let view = SemanticImageView::reopen(&bytes).expect("full image reopens");
    let owned = first
        .canonical_entities()
        .map(|entity| (entity.version.identity(), first.atom(entity.name).map(<[u8]>::to_vec)))
        .collect::<alloc::vec::Vec<_>>();
    let reopened = view
        .canonical_entities()
        .map(|entity| (entity.version.identity(), view.atom(entity.name).map(<[u8]>::to_vec)))
        .collect::<alloc::vec::Vec<_>>();
    assert_eq!(owned, reopened);
    assert_eq!(view.canonical_types().count(), first.canonical_types().count());
    assert_eq!(view.canonical_externals().count(), first.canonical_externals().count());
    let entity = view.canonical_entities().next().expect("first entity");
    assert_eq!(entity.authority.members, FactAvailability::Captured);
    assert_eq!(entity.authority.documentation, FactAvailability::Captured);
    let atom = view.atom(entity.name).expect("reopened atom");
    let repeated = view.atom(entity.name).expect("repeated atom");
    assert!(core::ptr::eq(atom.as_ptr(), repeated.as_ptr()));
    let start = bytes.as_ptr().addr();
    assert!(atom.as_ptr().addr() >= start && atom.as_ptr().addr() + atom.len() <= start + bytes.len());
    Ok(())
}

#[test]
fn full_image_short_output_and_directory_substitution_are_exact_and_non_mutating()
-> Result<(), crate::BuildError> {
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
        Err(super::FullSemanticImageError::Full(FullSemanticImageFault::DirectoryKind {
            expected: FullDirectoryKind::Atoms,
            observed: 0xff,
            ..
        }))
    ));
    Ok(())
}
