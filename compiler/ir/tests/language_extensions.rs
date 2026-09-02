use compiler_ir::{
    BuildError, CSharpFacts, CSharpMemberEffects, CSharpNullability, CSharpPartialRole,
    CSharpReferenceKind, ClangFacts, ClangLayout, ClangQualifiers, ClangStorageClass, Confidence,
    EntityId, EntityVersion, GoFacts, GoSignature, Ir, IrBuilder, Item, ItemKind, JavaFacts,
    LanguageExtensionInput, LanguageExtensionReopenError, LanguageProfile, PayloadHash,
    PythonFacts, PythonParameterKind, RustFacts, RustOwnership, StableEntityId, TreeItemInput,
    TypeScriptFacts, Visibility, encode_language_extension_section, language_extension_section_len,
    reopen_language_extension_section,
};

fn version() -> EntityVersion {
    EntityVersion {
        stable: StableEntityId::from_raw([1; 16]),
        payload: PayloadHash::from_raw([2; 16]),
    }
}

fn add_extension(builder: &mut IrBuilder, extension: LanguageExtensionInput<'_>) {
    let item = Item {
        name: builder.intern_atom(b"entity").expect("name"),
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        parent: None,
        semantic_type: None,
        members: builder.intern_members(&[]).expect("members"),
        docs: builder.intern_docs(&[]).expect("docs"),
        attributes: builder.intern_attributes(&[]).expect("attributes"),
        source: None,
    };
    builder
        .add_item(version(), item, Some(extension))
        .expect("profile-compatible extension");
}

fn encode(ir: &Ir) -> Vec<u8> {
    let extensions = ir.language_extensions();
    let length = language_extension_section_len(extensions).expect("wire length");
    let mut bytes = vec![0; length];
    assert_eq!(
        encode_language_extension_section(extensions, &mut bytes),
        Ok(length)
    );
    bytes
}

macro_rules! assert_typed_round_trip {
    ($ir:expr, $plane:ident, $facts:expr) => {{
        let ir = $ir;
        let bytes = encode(&ir);
        let section = reopen_language_extension_section(
            &bytes,
            ir.storage_columns().authority,
            ir.language_extension_common_bounds()
                .expect("common bounds"),
        )
        .expect("validated subordinate section");
        assert_eq!(section.$plane.get(EntityId::new(0)), Ok(Some($facts)));
    }};
}

#[test]
fn typed_csharp_plane_is_profile_bound_canonical_and_reopens() {
    let mut builder = IrBuilder::new();
    builder
        .set_language_profile(LanguageProfile::CSharp(
            compiler_ir::CSharpVersion::CSharp14,
        ))
        .expect("profile");
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
    let mut short = vec![0xa5; length - 1];
    let sentinel = short.clone();
    assert!(matches!(
        encode_language_extension_section(extensions, &mut short),
        Err(compiler_ir::LanguageExtensionEncodeError::OutputTooShort {
            required,
            actual,
        }) if required == length && actual == length - 1
    ));
    assert_eq!(short, sentinel);
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
    builder
        .set_language_profile(LanguageProfile::Rust(compiler_ir::RustEdition::Rust2024))
        .expect("profile");
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

#[test]
fn every_language_plane_round_trips_through_its_typed_column() {
    let mut builder = IrBuilder::new();
    builder
        .set_language_profile(LanguageProfile::TypeScript(
            compiler_ir::TypeScriptSource::Tsx,
        ))
        .expect("TypeScript profile");
    let typescript = TypeScriptFacts {
        type_parameters: builder
            .intern_type_parameters(&[])
            .expect("type parameters"),
        declared: None,
        computed: None,
    };
    add_extension(
        &mut builder,
        LanguageExtensionInput::TypeScript(&typescript),
    );
    assert_typed_round_trip!(
        builder.finish().expect("TypeScript IR"),
        typescript,
        typescript
    );

    let mut builder = IrBuilder::new();
    builder
        .set_language_profile(LanguageProfile::Go(compiler_ir::GoVersion::Go125))
        .expect("Go profile");
    let empty_types = builder.intern_types(&[]).expect("types");
    let empty_entities = builder.intern_members(&[]).expect("entities");
    let go = GoFacts {
        signature: GoSignature {
            parameters: empty_types,
            results: empty_types,
            variadic: true,
        },
        type_parameters: builder
            .intern_type_parameters(&[])
            .expect("type parameters"),
        fields: empty_entities,
        method_set: empty_entities,
        build_constraints: builder.intern_attributes(&[]).expect("constraints"),
    };
    add_extension(&mut builder, LanguageExtensionInput::Go(&go));
    assert_typed_round_trip!(builder.finish().expect("Go IR"), go, go);

    let mut builder = IrBuilder::new();
    builder
        .set_language_profile(LanguageProfile::Rust(compiler_ir::RustEdition::Rust2024))
        .expect("Rust profile");
    let empty_atoms = builder.intern_attributes(&[]).expect("atoms");
    let rust = RustFacts {
        ownership: RustOwnership::MutableBorrow,
        lifetimes: empty_atoms,
        where_clauses: builder.intern_type_parameters(&[]).expect("where clauses"),
        macros: empty_atoms,
    };
    add_extension(&mut builder, LanguageExtensionInput::Rust(&rust));
    assert_typed_round_trip!(builder.finish().expect("Rust IR"), rust, rust);

    let mut builder = IrBuilder::new();
    builder
        .set_language_profile(LanguageProfile::Python(
            compiler_ir::PythonVersion::Python314,
        ))
        .expect("Python profile");
    let python = PythonFacts {
        decorators: builder.intern_attributes(&[]).expect("decorators"),
        parameter_kind: PythonParameterKind::VariadicKeyword,
        dynamic_confidence: Confidence::Compiler,
    };
    add_extension(&mut builder, LanguageExtensionInput::Python(&python));
    assert_typed_round_trip!(builder.finish().expect("Python IR"), python, python);

    let mut builder = IrBuilder::new();
    builder
        .set_language_profile(LanguageProfile::Java(compiler_ir::JavaRelease::Java25))
        .expect("Java profile");
    let empty_entities = builder.intern_members(&[]).expect("entities");
    let java = JavaFacts {
        throws: builder.intern_types(&[]).expect("throws"),
        annotations: builder.intern_attributes(&[]).expect("annotations"),
        overloads: empty_entities,
        record_components: empty_entities,
    };
    add_extension(&mut builder, LanguageExtensionInput::Java(&java));
    assert_typed_round_trip!(builder.finish().expect("Java IR"), java, java);

    let mut builder = IrBuilder::new();
    builder
        .set_language_profile(LanguageProfile::Cxx(compiler_ir::CxxStandard::Cxx26))
        .expect("Clang profile");
    let clang = ClangFacts {
        qualifiers: ClangQualifiers {
            is_const: true,
            is_volatile: false,
            is_restrict: true,
        },
        storage: ClangStorageClass::ThreadLocal,
        layout: ClangLayout {
            size_bits: Some(128),
            align_bits: Some(64),
        },
        templates: builder.intern_type_parameters(&[]).expect("templates"),
        includes: builder.intern_attributes(&[]).expect("includes"),
    };
    add_extension(&mut builder, LanguageExtensionInput::Clang(&clang));
    assert_typed_round_trip!(builder.finish().expect("Clang IR"), clang, clang);
}

#[test]
fn authority_and_fact_mutants_retain_the_exact_rejected_values() {
    let mut builder = IrBuilder::new();
    builder
        .set_language_profile(LanguageProfile::CSharp(
            compiler_ir::CSharpVersion::CSharp14,
        ))
        .expect("profile");
    let csharp = CSharpFacts {
        nullability: CSharpNullability::Nullable,
        reference_kind: CSharpReferenceKind::Ref,
        constraints: builder.intern_type_parameters(&[]).expect("constraints"),
        effects: CSharpMemberEffects {
            is_async: true,
            is_iterator: false,
            is_extension: false,
        },
        attributes: builder.intern_attributes(&[]).expect("attributes"),
        partial: CSharpPartialRole::Definition,
        xml_provenance: None,
    };
    add_extension(&mut builder, LanguageExtensionInput::CSharp(&csharp));
    let ir = builder.finish().expect("C# IR");
    let canonical = encode(&ir);
    let authority = ir.storage_columns().authority;
    let bounds = ir.language_extension_common_bounds().expect("bounds");

    let mut unknown_tag = canonical.clone();
    unknown_tag[7] = 9;
    assert!(matches!(
        reopen_language_extension_section(&unknown_tag, authority, bounds),
        Err(LanguageExtensionReopenError::AuthorityTag { observed: 9 })
    ));

    let mut unknown_profile = canonical.clone();
    unknown_profile[8] = u8::MAX;
    unknown_profile[9] = 31;
    assert!(matches!(
        reopen_language_extension_section(&unknown_profile, authority, bounds),
        Err(LanguageExtensionReopenError::Profile {
            source,
        }) if source.code == [u8::MAX, 31]
    ));

    let mut invalid_reference_kind = canonical.clone();
    invalid_reference_kind[164..168].copy_from_slice(&77_u32.to_le_bytes());
    assert!(matches!(
        reopen_language_extension_section(&invalid_reference_kind, authority, bounds),
        Err(LanguageExtensionReopenError::FactEncoding {
            kind: compiler_ir::LanguageExtensionDirectoryKind::CSharp,
            fact: 0,
            word: 1,
            observed: 77,
        })
    ));

    let mut noncanonical_absent_span = canonical;
    noncanonical_absent_span[188..192].copy_from_slice(&4_u32.to_le_bytes());
    assert!(matches!(
        reopen_language_extension_section(&noncanonical_absent_span, authority, bounds),
        Err(LanguageExtensionReopenError::SourceSpanEncoding {
            kind: compiler_ir::LanguageExtensionDirectoryKind::CSharp,
            fact: 0,
            file: u32::MAX,
            start: 4,
            end: 0,
        })
    ));
}

#[test]
fn admitted_language_facts_prevent_profile_rebinding() {
    let mut builder = IrBuilder::new();
    builder
        .set_language_profile(LanguageProfile::CSharp(
            compiler_ir::CSharpVersion::CSharp14,
        ))
        .expect("C# profile");
    let csharp = CSharpFacts {
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
    add_extension(&mut builder, LanguageExtensionInput::CSharp(&csharp));

    assert!(matches!(
        builder.set_language_profile(LanguageProfile::Go(compiler_ir::GoVersion::Go125)),
        Err(BuildError::LanguageProfileRebind {
            existing: compiler_ir::SemanticImageAuthority::Language(LanguageProfile::CSharp(_)),
            requested: LanguageProfile::Go(_),
        })
    ));
    assert!(
        builder
            .set_language_profile(LanguageProfile::CSharp(
                compiler_ir::CSharpVersion::CSharp14
            ))
            .is_ok()
    );
}

#[test]
fn common_only_images_encode_zero_bytes_for_every_empty_plane() {
    let mut builder = IrBuilder::new();
    let item = Item {
        name: builder.intern_atom(b"common").expect("name"),
        kind: ItemKind::Module,
        visibility: Visibility::Public,
        parent: None,
        semantic_type: None,
        members: builder.intern_members(&[]).expect("members"),
        docs: builder.intern_docs(&[]).expect("docs"),
        attributes: builder.intern_attributes(&[]).expect("attributes"),
        source: None,
    };
    builder.add_item(version(), item, None).expect("common row");
    let ir = builder.finish().expect("common IR");
    let bytes = encode(&ir);
    assert_eq!(bytes.len(), 156);
    let section = reopen_language_extension_section(
        &bytes,
        ir.storage_columns().authority,
        ir.language_extension_common_bounds().expect("bounds"),
    )
    .expect("empty subordinate section");
    assert_eq!(section.typescript.get(EntityId::new(0)), Ok(None));
    assert_eq!(section.csharp.get(EntityId::new(0)), Ok(None));
    assert_eq!(section.go.get(EntityId::new(0)), Ok(None));
    assert_eq!(section.rust.get(EntityId::new(0)), Ok(None));
    assert_eq!(section.python.get(EntityId::new(0)), Ok(None));
    assert_eq!(section.java.get(EntityId::new(0)), Ok(None));
    assert_eq!(section.clang.get(EntityId::new(0)), Ok(None));
}
