//! Hostile full-image seam falsifiers.
//!
//! These remain deliberately small: the image builder admits an ordinary
//! captured-empty declaration plane, then the test exercises the complete
//! writer/reopen boundary rather than a second fixture-only semantic model.

use alloc::vec;

#[cfg(feature = "mmap")]
use crate::ir::DocInput;
use crate::ir::{
    BorrowedTree, BuiltinType, CSharpFacts, CSharpMemberEffects, CSharpNullability,
    CSharpPartialRole, CSharpReferenceKind, CSharpVersion, ConcreteType, CorePayloadHash,
    DeclarationFamilyId, EntityAuthorityFacts, EntityId, EntityVersion, FactAvailability,
    FreePredicate, Ir, IrBuilder, ItemKind, LanguageExtensionInput, LanguageProfile,
    ParentageAuthority, RustEdition, RustFacts, RustOwnership, SemanticCoreReader,
    SemanticImageAuthority, SemanticImageEncodeError, SemanticReader, TreeItemInput, TypeExpr,
    TypeParameterBound, TypeScriptSource, VariantFingerprint, Visibility,
};

use super::wire::{
    DIRECTORY_BYTES, FullDirectoryKind, HEADER_BYTES, RANGE_ROW_BYTES, SPARSE_BINDING_ROW_BYTES,
};
use super::{
    FullSemanticImageError, FullSemanticImageFault, SemanticImageProofOwner, SemanticImageView,
    encode_full_semantic_image, full_semantic_image_len,
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

#[cfg(feature = "mmap")]
fn large_encoded_image() -> Vec<u8> {
    let large_documentation = "selected native image mapping ".repeat(2_000);
    let docs = [DocInput::Text(&large_documentation)];
    let item = TreeItemInput {
        name: b"mapped",
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: authority(),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &docs,
        attributes: &[],
        source: None,
        extension: None,
    };
    let versions = [version(7)];
    let items = [item];
    let mut builder = IrBuilder::new();
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })
        .expect("large mapped fixture builds");
    encoded(&builder.finish().expect("large mapped IR finalizes"))
        .expect("large mapped fixture encodes")
}

#[test]
fn an_admission_proof_fails_closed_for_a_different_backing_allocation() {
    let image_a = encoded(&image(false).expect("first canonical IR builds"))
        .expect("first canonical image encodes");
    let image_b = encoded(&csharp_image().expect("second canonical IR builds"))
        .expect("second canonical image encodes");
    assert_ne!(image_a, image_b);

    let admitted = SemanticImageView::reopen(&image_a).expect("first image admits");
    let proof = admitted.proof();
    assert!(matches!(
        SemanticImageView::reopen_proven(&image_b, proof),
        Err(FullSemanticImageError::ProofBackingMismatch)
    ));
}

#[test]
fn owned_proof_cache_only_admits_successful_validation() {
    use crate::ir::{reset_semantic_image_validations, semantic_image_validations};

    let valid = encoded(&image(false).expect("canonical IR builds"))
        .expect("canonical image encodes")
        .into_boxed_slice();
    let owner = SemanticImageProofOwner::new(valid);
    reset_semantic_image_validations();
    assert!(owner.reopen().is_ok());
    assert!(owner.reopen().is_ok());
    assert_eq!(semantic_image_validations(), 1);

    let invalid = SemanticImageProofOwner::new(Box::<[u8]>::default());
    reset_semantic_image_validations();
    assert!(invalid.reopen().is_err());
    assert!(invalid.reopen().is_err());
    assert_eq!(semantic_image_validations(), 2);
}

#[cfg(feature = "mmap")]
#[test]
fn mapped_semantic_image_reopens_the_canonical_reader_without_heap_copy() {
    use crate::ir::{GenerationId, SemanticImageIdentity, load_semantic_image_mmap};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);
    let bytes =
        encoded(&image(false).expect("canonical IR builds")).expect("canonical image encodes");
    let identity = SemanticImageIdentity::from_encoded_bytes(&bytes);
    let generation = GenerationId::from_canonical_bytes(&bytes);
    let path = std::env::temp_dir().join(format!(
        "backend-semantic-image-mmap-{}-{}.nxf",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed),
    ));
    fs::write(&path, &bytes).expect("write immutable image fixture");

    let mapped = load_semantic_image_mmap(&path, identity, generation, bytes.len())
        .expect("full image maps and validates");
    assert_eq!(mapped.identity(), identity);
    assert_eq!(mapped.generation(), generation);
    assert_eq!(mapped.view().canonical_entities().len(), 2);
    drop(mapped);
    fs::remove_file(path).expect("remove image fixture");
}

#[cfg(feature = "mmap")]
#[test]
fn mapped_image_range_loader_writes_exact_ranges_into_the_mapping() {
    use crate::ir::{
        GenerationId, SEMANTIC_IMAGE_MMAP_RANGE_BYTES, SemanticImageIdentity,
        load_semantic_image_mmap_from_ranges,
    };

    let bytes = large_encoded_image();
    assert!(bytes.len() > SEMANTIC_IMAGE_MMAP_RANGE_BYTES * 2);

    let identity = SemanticImageIdentity::from_encoded_bytes(&bytes);
    let generation = GenerationId::from_canonical_bytes(&bytes);
    let mut read_calls = 0_u64;
    let (mapped, metrics) = load_semantic_image_mmap_from_ranges(
        u64::try_from(bytes.len()).expect("fixture length fits"),
        identity,
        generation,
        bytes.len(),
        |offset, output| {
            read_calls += 1;
            let start = usize::try_from(offset).expect("range offset fits");
            let end = start.checked_add(output.len()).expect("range end fits");
            output.copy_from_slice(bytes.get(start..end).expect("exact fixture range"));
            Ok::<usize, ()>(output.len())
        },
        || false,
    )
    .expect("exact source ranges fill and fully verify the mapped image");

    let expected_ranges = bytes.len().div_ceil(SEMANTIC_IMAGE_MMAP_RANGE_BYTES);
    assert_eq!(
        metrics.bytes_read,
        u64::try_from(bytes.len()).expect("length fits")
    );
    assert_eq!(
        usize::try_from(metrics.range_reads).expect("range count fits"),
        expected_ranges
    );
    assert_eq!(read_calls, metrics.range_reads);
    assert_eq!(mapped.identity(), identity);
    assert_eq!(mapped.generation(), generation);
    assert_eq!(mapped.view().canonical_entities().len(), 1);
}

#[cfg(feature = "mmap")]
#[test]
fn mapped_image_range_loader_rejects_short_and_truncated_sources() {
    use crate::ir::{
        GenerationId, MappedSemanticImageRangeError, SEMANTIC_IMAGE_MMAP_RANGE_BYTES,
        SemanticImageIdentity, load_semantic_image_mmap_from_ranges,
    };

    let bytes = large_encoded_image();
    let identity = SemanticImageIdentity::from_encoded_bytes(&bytes);
    let generation = GenerationId::from_canonical_bytes(&bytes);
    let total = u64::try_from(bytes.len()).expect("fixture length fits");
    let short = load_semantic_image_mmap_from_ranges(
        total,
        identity,
        generation,
        bytes.len(),
        |_offset, output| Ok::<usize, ()>(output.len().saturating_sub(1)),
        || false,
    );
    assert!(matches!(
        short,
        Err(MappedSemanticImageRangeError::ShortRead {
            offset: 0,
            expected,
            observed,
        }) if expected == bytes.len().min(SEMANTIC_IMAGE_MMAP_RANGE_BYTES)
            && observed + 1 == expected
    ));

    let source_length = bytes.len().saturating_sub(1);
    let final_offset =
        (source_length / SEMANTIC_IMAGE_MMAP_RANGE_BYTES) * SEMANTIC_IMAGE_MMAP_RANGE_BYTES;
    let truncated = load_semantic_image_mmap_from_ranges(
        total,
        identity,
        generation,
        bytes.len(),
        |offset, output| {
            let start = usize::try_from(offset).expect("range offset fits");
            let available = source_length.saturating_sub(start).min(output.len());
            output[..available].copy_from_slice(&bytes[start..start + available]);
            Ok::<usize, ()>(available)
        },
        || false,
    );
    assert!(matches!(
        truncated,
        Err(MappedSemanticImageRangeError::ShortRead {
            offset,
            expected,
            observed,
        }) if offset == u64::try_from(final_offset).expect("range offset fits")
            && expected == bytes.len() - final_offset
            && observed + 1 == expected
    ));
}

#[cfg(feature = "mmap")]
#[test]
fn mapped_image_range_loader_rejects_corruption_and_stops_when_cancelled() {
    use crate::ir::{
        GenerationId, MappedSemanticImageError, MappedSemanticImageRangeError,
        SEMANTIC_IMAGE_MMAP_RANGE_BYTES, SemanticImageIdentity,
        load_semantic_image_mmap_from_ranges,
    };

    let bytes = large_encoded_image();
    let identity = SemanticImageIdentity::from_encoded_bytes(&bytes);
    let generation = GenerationId::from_canonical_bytes(&bytes);
    let total = u64::try_from(bytes.len()).expect("fixture length fits");
    let mut corrupt = bytes.clone();
    corrupt[0] ^= 1;
    let rejected = load_semantic_image_mmap_from_ranges(
        total,
        identity,
        generation,
        bytes.len(),
        |offset, output| {
            let start = usize::try_from(offset).expect("range offset fits");
            let end = start.checked_add(output.len()).expect("range end fits");
            output.copy_from_slice(corrupt.get(start..end).expect("exact corrupt range"));
            Ok::<usize, ()>(output.len())
        },
        || false,
    );
    assert!(matches!(
        rejected,
        Err(MappedSemanticImageRangeError::Mapping(
            MappedSemanticImageError::Identity { .. }
        ))
    ));

    let mut read_calls = 0_u64;
    let mut cancel_checks = 0_u64;
    let cancelled = load_semantic_image_mmap_from_ranges(
        total,
        identity,
        generation,
        bytes.len(),
        |offset, output| {
            read_calls += 1;
            let start = usize::try_from(offset).expect("range offset fits");
            let end = start.checked_add(output.len()).expect("range end fits");
            output.copy_from_slice(bytes.get(start..end).expect("exact fixture range"));
            Ok::<usize, ()>(output.len())
        },
        || {
            cancel_checks += 1;
            cancel_checks >= 3
        },
    );
    assert!(matches!(
        cancelled,
        Err(MappedSemanticImageRangeError::Cancelled)
    ));
    assert_eq!(
        read_calls, 1,
        "cancellation is checked between direct mapped reads"
    );
    assert!(bytes.len() > SEMANTIC_IMAGE_MMAP_RANGE_BYTES);
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

fn rust_free_predicate_image() -> Result<Ir, crate::ir::BuildError> {
    let mut builder = IrBuilder::new();
    builder.set_language_profile(LanguageProfile::Rust(RustEdition::Rust2024))?;
    let subject =
        builder.intern_type(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::I32)))?;
    let bound_type =
        builder.intern_type(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::Bool)))?;
    let bound_lifetime = builder.intern_atom(b"'scope")?;
    let bounds = builder.intern_type_parameter_bounds(&[
        TypeParameterBound::Type(bound_type),
        TypeParameterBound::Lifetime(bound_lifetime),
    ])?;
    let free_predicates = builder.intern_free_predicates(&[FreePredicate { subject, bounds }])?;
    let facts = RustFacts {
        ownership: RustOwnership::SharedBorrow,
        lifetimes: builder.intern_attributes(&[])?,
        where_clauses: builder.intern_type_parameters(&[])?,
        macros: builder.intern_attributes(&[])?,
        const_defaults: builder.intern_attributes(&[])?,
        free_predicates,
    };
    let item = TreeItemInput {
        name: b"owner",
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: EntityAuthorityFacts {
            language_extension: FactAvailability::Captured,
            ..authority()
        },
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: Some(LanguageExtensionInput::Rust(&facts)),
    };
    let versions = [version(1)];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &[item],
        links: &[],
    })?;
    builder.finish()
}

#[test]
fn full_image_round_trips_a_rust_free_predicate() -> Result<(), crate::ir::BuildError> {
    let ir = rust_free_predicate_image()?;
    let bytes = encoded(&ir)?;
    let view = SemanticImageView::reopen(&bytes).expect("full image reopens");
    let facts = view
        .rust_extension(EntityId::new(0))
        .expect("reopened rust facts");
    assert_eq!(facts.ownership, RustOwnership::SharedBorrow);
    let predicates = view
        .free_predicates(facts.free_predicates)
        .expect("reopened free predicates");
    let rows = predicates.collect::<alloc::vec::Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        view.ty(rows[0].subject),
        Some(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::I32)))
    );
    let bounds = view
        .type_parameter_bounds(rows[0].bounds)
        .expect("reopened predicate bounds")
        .collect::<alloc::vec::Vec<_>>();
    assert_eq!(bounds.len(), 2);
    let TypeParameterBound::Type(first_bound) = bounds[0] else {
        panic!("first predicate bound is not a type");
    };
    assert_eq!(
        view.ty(first_bound),
        Some(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::Bool)))
    );
    assert!(matches!(
        bounds[1],
        TypeParameterBound::Lifetime(atom) if view.atom(atom) == Some(&b"'scope"[..])
    ));
    Ok(())
}

/// Focused decode benchmark over a synthetic image of roughly ten thousand
/// typed rows: `ENTITIES` distinct `Parameter`/`CPointer` type pairs plus one
/// shared 512-row type-parameter list that every extension facts row points
/// at (exactly the shape the audit's canonical extension digests sweep).  The
/// grouped decode must complete the whole reopen + types + parameter sweep in
/// bounded time; the former per-row linear edge scans made the same sweep
/// quadratic in the list length.
#[test]
fn grouped_decode_sweeps_ten_thousand_typed_rows_in_bounded_time()
-> Result<(), crate::ir::BuildError> {
    use std::time::{Duration, Instant};

    use crate::ir::{
        CSharpFacts, CSharpMemberEffects, CSharpNullability, CSharpPartialRole,
        CSharpReferenceKind, CSharpVersion, LanguageExtensionInput, Mutability,
        TypeParameterInference, TypeParameterKind,
    };

    const ENTITIES: usize = 4600;
    const SHARED_PARAMETERS: usize = 512;

    fn distinct_version(index: usize) -> crate::ir::EntityVersion {
        let low = u8::try_from(index & 0xff).unwrap_or(1);
        let high = u8::try_from((index >> 8) & 0xff).unwrap_or(1);
        let mut family = [7_u8; 16];
        family[0] = low;
        family[1] = high;
        let mut variant = [9_u8; 16];
        variant[0] = low.wrapping_add(1);
        variant[1] = high;
        crate::ir::EntityVersion {
            family: crate::ir::DeclarationFamilyId::from_raw(family),
            variant: crate::ir::VariantFingerprint::from_raw(variant),
            core_payload: crate::ir::CorePayloadHash::from_raw([11; 16]),
        }
    }

    let mut builder = IrBuilder::new();
    builder.set_language_profile(LanguageProfile::CSharp(CSharpVersion::CSharp14))?;
    let lifetime = builder.intern_atom(b"'a")?;
    let empty_bounds = builder.intern_type_parameter_bounds(&[])?;
    let mixed_bounds =
        builder.intern_type_parameter_bounds(&[TypeParameterBound::Lifetime(lifetime)])?;
    let mut shared = alloc::vec::Vec::with_capacity(SHARED_PARAMETERS);
    for index in 0..SHARED_PARAMETERS {
        let spelling = alloc::format!("T{index}");
        let name = builder.intern_atom(spelling.as_bytes())?;
        shared.push(crate::ir::TypeParameter {
            name,
            bounds: if index % 2 == 0 {
                mixed_bounds
            } else {
                empty_bounds
            },
            default: None,
            variance: crate::ir::Variance::Covariant,
            kind: TypeParameterKind::Type {
                inference: TypeParameterInference::Ordinary,
            },
            requirements: crate::ir::TypeParameterRequirements::none(),
        });
    }
    let constraints = builder.intern_type_parameters(&shared)?;
    let facts = CSharpFacts {
        nullability: CSharpNullability::Oblivious,
        reference_kind: CSharpReferenceKind::Value,
        constraints,
        effects: CSharpMemberEffects {
            is_async: false,
            is_iterator: false,
            is_extension: false,
        },
        attributes: builder.intern_attributes(&[])?,
        partial: CSharpPartialRole::None,
        xml_provenance: None,
    };
    let mut versions = alloc::vec::Vec::with_capacity(ENTITIES);
    let mut items = alloc::vec::Vec::with_capacity(ENTITIES);
    let mut spellings = alloc::vec::Vec::with_capacity(ENTITIES);
    for index in 0..ENTITIES {
        spellings.push(alloc::format!("synthetic-{index}"));
    }
    for index in 0..ENTITIES {
        let spelling = spellings[index].as_bytes();
        let parameter_spelling = alloc::format!("P{index}");
        let parameter = builder.intern_atom(parameter_spelling.as_bytes())?;
        let parameter_type =
            builder.intern_type(TypeExpr::Concrete(ConcreteType::Parameter(parameter)))?;
        let semantic_type = builder.intern_type(TypeExpr::Concrete(ConcreteType::CPointer {
            target: parameter_type,
        }))?;
        versions.push(distinct_version(index));
        items.push(TreeItemInput {
            name: spelling,
            kind: ItemKind::Function,
            visibility: Visibility::Private,
            authority: crate::ir::EntityAuthorityFacts {
                semantic_type: FactAvailability::Captured,
                language_extension: FactAvailability::Captured,
                ..authority()
            },
            parent: None,
            semantic_type: Some(semantic_type),
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: Some(LanguageExtensionInput::CSharp(&facts)),
        });
    }
    let borrowed = crate::ir::BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    };
    builder.add_borrowed_tree(borrowed)?;
    let ir = builder.finish()?;

    let started = Instant::now();
    let bytes = encoded(&ir)?;
    let view = SemanticImageView::reopen(&bytes).expect("synthetic full image reopens");
    let type_rows = view.canonical_types().count();
    let mut facts_rows = 0_usize;
    let mut parameter_rows = 0_usize;
    for (_, row_facts) in view.csharp_extensions() {
        facts_rows += 1;
        for row in view
            .type_parameters(row_facts.constraints)
            .expect("shared parameter list decodes")
        {
            std::hint::black_box(&row);
            parameter_rows += 1;
        }
    }
    let elapsed = started.elapsed();
    eprintln!(
        "grouped-decode benchmark: type_rows={type_rows} facts_rows={facts_rows} parameter_rows={parameter_rows} encoded_bytes={} elapsed_ms={}",
        bytes.len(),
        elapsed.as_millis(),
    );
    assert!(
        type_rows >= 9_000,
        "expected ~10k type rows, saw {type_rows}"
    );
    assert_eq!(facts_rows, ENTITIES);
    assert_eq!(parameter_rows, ENTITIES * SHARED_PARAMETERS);
    // Debug-profile CI headroom; the decode itself is single-pass and the
    // former linear-scan decoder could not finish this sweep at all.
    assert!(
        elapsed < Duration::from_secs(60),
        "grouped decode exceeded the bound: {elapsed:?}"
    );
    Ok(())
}
