use compiler_ir::{
    CSharpFacts, CSharpMemberEffects, CSharpNullability, CSharpPartialRole, CSharpReferenceKind,
    EntityVersion, IrBuilder, ItemKind, LanguageExtensionInput, LanguageExtensionReopenError,
    LanguageProfile, PayloadHash, StableEntityId, TreeItemInput, Visibility,
    encode_language_extension_section, language_extension_section_len,
    reopen_language_extension_section,
};

fn version() -> EntityVersion {
    EntityVersion {
        stable: StableEntityId::from_raw([1; 16]),
        payload: PayloadHash::from_raw([2; 16]),
    }
}

#[test]
fn typed_csharp_plane_is_profile_bound_canonical_and_reopens() {
    let mut builder = IrBuilder::new();
    builder.set_language_profile(LanguageProfile::CSharp(
        compiler_ir::CSharpVersion::CSharp14,
    ));
    let constraints = builder
        .intern_type_parameters(&[])
        .expect("empty constraints");
    let attributes = builder.intern_attributes(&[]).expect("empty attributes");
    let facts = CSharpFacts {
        nullability: CSharpNullability::Nullable,
        reference_kind: CSharpReferenceKind::Ref,
        constraints,
        effects: CSharpMemberEffects {
            is_async: true,
            is_iterator: false,
            is_extension: true,
        },
        attributes,
        partial: CSharpPartialRole::Implementation,
        xml_provenance: None,
    };
    builder
        .add_borrowed_tree(compiler_ir::BorrowedTree {
            versions: &[version()],
            items: &[TreeItemInput {
                name: b"member",
                kind: ItemKind::Function,
                visibility: Visibility::Public,
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: Some(LanguageExtensionInput::CSharp(&facts)),
            }],
            links: &[],
        })
        .expect("matching profile");
    let ir = builder.finish().expect("validated extension");
    let extensions = ir.language_extensions();
    assert_eq!(
        extensions.csharp.get(compiler_ir::EntityId::new(0)),
        Some(&facts)
    );
    assert!(
        extensions
            .typescript
            .get(compiler_ir::EntityId::new(0))
            .is_none()
    );

    let length = language_extension_section_len(extensions).expect("length");
    let mut first = vec![0; length];
    let mut second = vec![0; length];
    assert_eq!(
        encode_language_extension_section(extensions, &mut first),
        Ok(length)
    );
    assert_eq!(
        encode_language_extension_section(extensions, &mut second),
        Ok(length)
    );
    assert_eq!(first, second);
    let authority = ir.storage_columns().authority;
    let bounds = ir.language_extension_common_bounds().expect("bounds");
    let section = reopen_language_extension_section(&first, authority, bounds).expect("reopen");
    assert_eq!(section.entity_rows, 1);
    assert_eq!(
        section.csharp.get(compiler_ir::EntityId::new(0)),
        Ok(Some(facts))
    );

    let mut cross_plane = first.clone();
    cross_plane[16] = 2;
    assert!(matches!(
        reopen_language_extension_section(&cross_plane, authority, bounds),
        Err(LanguageExtensionReopenError::DirectoryKind { .. })
    ));
    let mut bad_ordinal = first.clone();
    bad_ordinal[156..160].copy_from_slice(&9_u32.to_le_bytes());
    assert!(matches!(
        reopen_language_extension_section(&bad_ordinal, authority, bounds),
        Err(LanguageExtensionReopenError::Ordinal { .. })
    ));
    let mut bad_reference = first;
    bad_reference[184..188].copy_from_slice(&9_u32.to_le_bytes());
    assert!(matches!(
        reopen_language_extension_section(&bad_reference, authority, bounds),
        Err(LanguageExtensionReopenError::SharedReference { .. })
    ));
}

#[test]
fn extension_language_cannot_cross_the_image_profile() {
    let mut builder = IrBuilder::new();
    builder.set_language_profile(LanguageProfile::Rust(compiler_ir::RustEdition::Rust2024));
    let facts = CSharpFacts {
        nullability: CSharpNullability::Oblivious,
        reference_kind: CSharpReferenceKind::Value,
        constraints: builder.intern_type_parameters(&[]).expect("constraints"),
        effects: CSharpMemberEffects {
            is_async: false,
            is_iterator: false,
            is_extension: false,
        },
        attributes: builder.intern_attributes(&[]).expect("attributes"),
        partial: CSharpPartialRole::None,
        xml_provenance: None,
    };
    let item = compiler_ir::Item {
        name: builder.intern_atom(b"bad").expect("name"),
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        parent: None,
        semantic_type: None,
        members: builder.intern_members(&[]).expect("members"),
        docs: builder.intern_docs(&[]).expect("docs"),
        attributes: builder.intern_attributes(&[]).expect("attributes"),
        source: None,
    };
    assert!(matches!(
        builder.add_item(
            version(),
            item,
            Some(LanguageExtensionInput::CSharp(&facts))
        ),
        Err(compiler_ir::BuildError::LanguageProfileMismatch { .. })
    ));
}
