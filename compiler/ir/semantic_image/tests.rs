//! Unrun hostile falsifiers for the subordinate portable core-image grammar.

use alloc::{vec, vec::Vec};

use crate::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityVersion,
    FactAvailability, Ir, IrBuilder, ItemKind, PackageLineage, ParentageAuthority,
    SemanticCoreReader, SourceIdentity, SourceSpan, TreeEntityId, TreeItemInput,
    VariantFingerprint, Visibility,
};
use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};

use super::{
    core_semantic_image_len, encode_core_semantic_image, reopen_core_semantic_image,
    fault::{
        CoreAuthorityFault, CoreProvenanceFault, CoreSemanticImageFault,
        CoreSemanticImageField, ScopeComponent,
    },
    wire::{DirectoryKind, DIRECTORY_BYTES, ENTITY_ROW_BYTES, HEADER_BYTES},
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

fn rich_image(reverse_input: bool) -> Result<Ir, crate::BuildError> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"core-image-source"),
        byte_len: 17,
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"core-image-toolchain"),
    );
    let lineage = PackageLineage::new("cargo", "core-image")
        .expect("fixed core-image lineage is valid");
    let mut builder = IrBuilder::new();
    if reverse_input {
        builder.intern_atom(b"unrelated-b")?;
        builder.intern_atom(b"unrelated-a")?;
    }
    let source_file = builder.intern_atom(b"src/lib.rs")?;
    if !reverse_input {
        builder.intern_atom(b"unrelated-a")?;
        builder.intern_atom(b"unrelated-b")?;
    }
    builder.set_image_provenance(source, recipe, lineage, "src/lib.rs")?;
    let root_version = version(4);
    let child_version = version(5);
    let forward_members = [TreeEntityId::new(1)];
    let reverse_members = [TreeEntityId::new(0)];
    let root_forward = TreeItemInput {
            name: b"root",
            kind: ItemKind::Module,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                source: FactAvailability::Captured,
                source_file: FactAvailability::Captured,
                members: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &forward_members,
            docs: &[],
            attributes: &[],
            source: SourceSpan::new(source_file, 2, 8),
            extension: None,
        };
    let child_forward = TreeItemInput {
            name: b"child",
            kind: ItemKind::Record,
            visibility: Visibility::Private,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Bound(root_version.identity()),
                source: FactAvailability::Captured,
                source_file: FactAvailability::Captured,
                members: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: Some(TreeEntityId::new(0)),
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: SourceSpan::new(source_file, 10, 15),
            extension: None,
        };
    if reverse_input {
        let root_reverse = TreeItemInput {
            members: &reverse_members,
            ..root_forward
        };
        let child_reverse = TreeItemInput {
            parent: Some(TreeEntityId::new(1)),
            ..child_forward
        };
        let versions = [child_version, root_version];
        let items = [child_reverse, root_reverse];
        builder.add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })?;
    } else {
        let versions = [root_version, child_version];
        let items = [root_forward, child_forward];
        builder.add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })?;
    }
    builder.finish()
}

fn encoded(ir: &Ir) -> Result<alloc::vec::Vec<u8>, crate::BuildError> {
    let length = core_semantic_image_len(ir).expect("core image plan is admitted");
    let mut bytes = vec![0; length];
    encode_core_semantic_image(ir, &mut bytes).expect("admitted image writes");
    Ok(bytes)
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("fixed test cell"))
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn entity_rows(bytes: &[u8]) -> usize {
    usize::try_from(read_u32(bytes, HEADER_BYTES + DIRECTORY_BYTES * 2 + 4))
        .expect("portable test offset fits")
}

fn atom_count(bytes: &[u8]) -> u32 {
    read_u32(bytes, HEADER_BYTES + 12)
}

fn entity_count(bytes: &[u8]) -> u32 {
    read_u32(bytes, HEADER_BYTES + DIRECTORY_BYTES * 2 + 12)
}

#[test]
fn core_image_is_canonical_and_reopens_only_the_core_reader() -> Result<(), crate::BuildError> {
    let first = image(false)?;
    let reversed = image(true)?;
    let first_len = core_semantic_image_len(&first).expect("first core plan");
    let reversed_len = core_semantic_image_len(&reversed).expect("reverse core plan");
    assert_eq!(first_len, reversed_len);
    let mut first_bytes = vec![0; first_len];
    let mut reversed_bytes = vec![0; reversed_len];
    encode_core_semantic_image(&first, &mut first_bytes).expect("first core encode");
    encode_core_semantic_image(&reversed, &mut reversed_bytes).expect("reverse core encode");
    assert_eq!(first_bytes, reversed_bytes);

    let reopened = reopen_core_semantic_image(&first_bytes).expect("reopen core image");
    let owned = first
        .canonical_core_entities()
        .map(|entity| (entity.version.identity(), first.atom(entity.name).map(<[u8]>::to_vec)))
        .collect::<Vec<_>>();
    let borrowed = reopened
        .canonical_core_entities()
        .map(|entity| (entity.version.identity(), reopened.atom(entity.name).map(<[u8]>::to_vec)))
        .collect::<Vec<_>>();
    assert_eq!(owned, borrowed);
    let captured = reopened.core_entity(crate::EntityId::new(0)).expect("captured core entity");
    assert_eq!(captured.authority.members, FactAvailability::Captured);
    assert_eq!(captured.authority.documentation, FactAvailability::Captured);
    let atom = reopened.atom(captured.name).expect("reopened atom");
    let repeated = reopened.atom(captured.name).expect("repeated reopened atom");
    assert!(core::ptr::eq(atom.as_ptr(), repeated.as_ptr()));
    let image_start = first_bytes.as_ptr().addr();
    let image_end = image_start + first_bytes.len();
    let atom_start = atom.as_ptr().addr();
    assert!(atom_start >= image_start && atom_start + atom.len() <= image_end);
    Ok(())
}

#[test]
fn core_image_short_output_and_directory_corruption_retain_exact_causes() -> Result<(), crate::BuildError> {
    let ir = image(false)?;
    let length = core_semantic_image_len(&ir).expect("core plan");
    let mut short = vec![0xa5; length.saturating_sub(1)];
    let before = short.clone();
    assert!(matches!(
        encode_core_semantic_image(&ir, &mut short),
        Err(CoreSemanticImageFault::OutputTooShort { required, actual })
            if required == length && actual == before.len()
    ));
    assert_eq!(short, before);
    let mut bytes = vec![0; length];
    encode_core_semantic_image(&ir, &mut bytes).expect("core encode");
    bytes[HEADER_BYTES] = 0xff;
    assert!(matches!(
        reopen_core_semantic_image(&bytes),
        Err(CoreSemanticImageFault::DirectoryKind { expected: DirectoryKind::Atoms, observed: 0xff, .. })
    ));
    Ok(())
}

#[test]
fn core_image_canonicalizes_rich_scope_and_retains_parent_source_and_identity_lookup()
-> Result<(), crate::BuildError> {
    let first = rich_image(false)?;
    let reverse = rich_image(true)?;
    let first_bytes = encoded(&first)?;
    assert_eq!(first_bytes, encoded(&reverse)?);
    let reopened = reopen_core_semantic_image(&first_bytes).expect("rich core image reopens");
    let crate::ImageProvenance::Captured {
        source: owned_source,
        recipe: owned_recipe,
        claim: owned_claim,
        ..
    } = first.image_facts().provenance else {
        panic!("owned image retains captured provenance");
    };
    let crate::ImageProvenance::Captured { source, recipe, claim, scope } = reopened.image_facts().provenance else {
        panic!("rich image retains captured provenance");
    };
    assert_eq!(source, owned_source);
    assert_eq!(recipe, owned_recipe);
    assert_eq!(claim, owned_claim);
    assert_eq!(reopened.atom(scope.ecosystem), Some(b"cargo".as_slice()));
    assert_eq!(reopened.atom(scope.package), Some(b"core-image".as_slice()));
    assert_eq!(reopened.atom(scope.path), Some(b"src/lib.rs".as_slice()));
    let rows = reopened.canonical_core_entities().collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].parent, Some(rows[0].id));
    assert_eq!(rows[1].source.map(|span| span.start()), Some(10));
    assert_eq!(rows[1].authority.parentage, ParentageAuthority::Bound(rows[0].version.identity()));
    assert_eq!(reopened.core_entity_by_identity(rows[1].version.identity()), Some(rows[1]));
    let first_atom = reopened.atom(rows[0].name).expect("borrowed atom");
    let repeated = reopened.atom(rows[0].name).expect("same borrowed atom");
    assert!(core::ptr::eq(first_atom.as_ptr(), repeated.as_ptr()));
    Ok(())
}

#[test]
fn core_image_rejects_provenance_and_entity_mutants_with_exact_operands()
-> Result<(), crate::BuildError> {
    let bytes = encoded(&rich_image(false)? )?;
    let entity = entity_rows(&bytes);

    let mut recipe_stage = bytes.clone();
    recipe_stage[94] = 0xff;
    assert!(matches!(
        reopen_core_semantic_image(&recipe_stage),
        Err(CoreSemanticImageFault::Provenance {
            cause: CoreProvenanceFault::RecipeStage { observed: 0xff },
        })
    ));
    let mut recipe_identity = bytes.clone();
    recipe_identity[61] ^= 1;
    assert!(matches!(
        reopen_core_semantic_image(&recipe_identity),
        Err(CoreSemanticImageFault::Provenance {
            cause: CoreProvenanceFault::RecipeIdentity { .. },
        })
    ));
    let mut scope_claim = bytes.clone();
    scope_claim[129] ^= 1;
    assert!(matches!(
        reopen_core_semantic_image(&scope_claim),
        Err(CoreSemanticImageFault::Provenance {
            cause: CoreProvenanceFault::ScopeClaim { .. },
        })
    ));
    let mut scope_atom = bytes.clone();
    write_u32(&mut scope_atom, 160, u32::MAX);
    assert!(matches!(
        reopen_core_semantic_image(&scope_atom),
        Err(CoreSemanticImageFault::Provenance {
            cause: CoreProvenanceFault::ScopeAtom {
                component: ScopeComponent::Ecosystem,
                raw: u32::MAX,
                ..
            },
        })
    ));
    let mut source_file = bytes.clone();
    write_u32(&mut source_file, entity + 12, atom_count(&bytes));
    assert!(matches!(
        reopen_core_semantic_image(&source_file),
        Err(CoreSemanticImageFault::Reference {
            field: CoreSemanticImageField::EntitySource,
            row: 0,
            observed,
            ..
        }) if observed == atom_count(&bytes)
    ));
    let mut absent_source_payload = bytes.clone();
    write_u32(&mut absent_source_payload, entity + 12, u32::MAX);
    absent_source_payload[entity + 16] = 1;
    assert!(matches!(
        reopen_core_semantic_image(&absent_source_payload),
        Err(CoreSemanticImageFault::Reserved {
            field: CoreSemanticImageField::EntitySource,
            row: 0,
            observed: 1,
        })
    ));
    let mut kind = bytes.clone();
    kind[entity + 4] = 0;
    kind[entity + 5] = 0xff;
    assert!(matches!(
        reopen_core_semantic_image(&kind),
        Err(CoreSemanticImageFault::EntityKind { row: 0, observed: 0xff00 })
    ));
    let mut reserved = bytes.clone();
    reserved[entity + 84] = 1;
    assert!(matches!(
        reopen_core_semantic_image(&reserved),
        Err(CoreSemanticImageFault::Reserved {
            field: CoreSemanticImageField::EntityParentage,
            row: 0,
            observed: 1,
        })
    ));
    Ok(())
}

#[test]
fn core_image_rejects_parent_and_availability_cross_fact_mutants()
-> Result<(), crate::BuildError> {
    let bytes = encoded(&rich_image(false)? )?;
    let root = entity_rows(&bytes);
    let child = root + ENTITY_ROW_BYTES;

    let mut parent_bounds = bytes.clone();
    write_u32(&mut parent_bounds, child + 8, entity_count(&bytes));
    assert!(matches!(
        reopen_core_semantic_image(&parent_bounds),
        Err(CoreSemanticImageFault::Reference {
            field: CoreSemanticImageField::EntityParent,
            row: 1,
            observed,
            ..
        }) if observed == entity_count(&bytes)
    ));
    let mut cycle = bytes.clone();
    write_u32(&mut cycle, root + 8, 1);
    assert!(matches!(
        reopen_core_semantic_image(&cycle),
        Err(CoreSemanticImageFault::ParentCycle { entity: 0, parent: 0 })
    ));
    let mut bound_identity = bytes.clone();
    bound_identity[child + 84] ^= 1;
    assert!(matches!(
        reopen_core_semantic_image(&bound_identity),
        Err(CoreSemanticImageFault::Authority {
            row: 1,
            cause: CoreAuthorityFault::Parentage { .. },
        })
    ));
    let mut source_availability = bytes.clone();
    source_availability[child + 24] = 0;
    assert!(matches!(
        reopen_core_semantic_image(&source_availability),
        Err(CoreSemanticImageFault::Authority {
            row: 1,
            cause: CoreAuthorityFault::SourceAvailability {
                source: 0,
                source_file: 1,
                has_source: true,
            },
        })
    ));
    Ok(())
}
