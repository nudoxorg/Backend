//! Ordinary contract falsifiers for authority-backed semantic documents.

use core::num::NonZeroUsize;

use compiler_ir::{
    BorrowedTree, CSharpFacts, CSharpMemberEffects, CSharpNullability, CSharpPartialRole,
    CSharpReferenceKind, CanonicalTypeRenderLimits, ClangFacts, ClangLayout, ClangQualifiers,
    ClangStorageClass, CorePayloadHash, DeclarationFamilyId, DocInput, EntityAuthorityFacts,
    EntityId, EntityVersion, ExternalDeclarationIdentity, ExternalTarget, FactAvailability,
    ForeignDeclarationId, ForeignExternalTarget, ForeignTargetOrigin, GoFacts, GoSignature,
    IrBuilder, ItemKind, JavaFacts, LanguageExtensionInput, LanguageProfile, ParentageAuthority,
    PythonFacts, PythonParameterKind, RustFacts, RustOwnership, SemanticDocumentError,
    SemanticImageView, SourceSyntaxError, TreeItemInput, TypeScriptFacts, VariantAvailability,
    VariantFingerprint, Visibility, encode_full_semantic_image, full_semantic_image_len,
    prepare_semantic_document, prepare_source_syntax,
};
use compiler_vocabulary::{
    CSharpVersion, CStandard, GoVersion, JavaRelease, PythonVersion, RustEdition, TypeScriptSource,
};

fn version(seed: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([seed; 16]),
        variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
        core_payload: CorePayloadHash::from_raw([seed.wrapping_add(2); 16]),
    }
}

fn limits() -> CanonicalTypeRenderLimits {
    CanonicalTypeRenderLimits::new(NonZeroUsize::new(32).expect("nonzero test limit"))
}

fn extension_authority(documentation: FactAvailability) -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        documentation,
        visibility: FactAvailability::Captured,
        language_extension: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    }
}

fn unavailable_authority() -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    }
}

fn add_one<'facts>(
    builder: &mut IrBuilder,
    profile: LanguageProfile,
    extension: LanguageExtensionInput<'facts>,
) -> Result<(), compiler_ir::BuildError> {
    builder.set_language_profile(profile)?;
    let versions = [version(1)];
    let items = [TreeItemInput {
        name: b"documented",
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: extension_authority(FactAvailability::Captured),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[DocInput::Text("authority-backed")],
        attributes: &[],
        source: None,
        extension: Some(extension),
    }];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    Ok(())
}

fn render<Reader: compiler_ir::SemanticReader + ?Sized>(
    profile: LanguageProfile,
    image: &Reader,
) -> Result<Vec<u8>, SemanticDocumentError> {
    let prepared = prepare_semantic_document(profile, image, EntityId::new(0), limits())?;
    let mut output = vec![0; prepared.encoded_len];
    prepared.write_into(&mut output)?;
    Ok(output)
}

#[test]
fn all_named_extension_planes_admit_their_static_document_dialect()
-> Result<(), compiler_ir::BuildError> {
    let mut typescript_builder = IrBuilder::new();
    let ts_parameters = typescript_builder.intern_type_parameters(&[])?;
    add_one(
        &mut typescript_builder,
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        LanguageExtensionInput::TypeScript(&TypeScriptFacts {
            type_parameters: ts_parameters,
            declared: None,
            observed: None,
        }),
    )?;
    let ts = typescript_builder.finish()?;
    assert!(
        render(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            &ts
        )
        .expect("typescript document")
        .windows(b"dialect=typescript".len())
        .any(|value| value == b"dialect=typescript")
    );

    let mut csharp_builder = IrBuilder::new();
    let constraints = csharp_builder.intern_type_parameters(&[])?;
    let attributes = csharp_builder.intern_attributes(&[])?;
    add_one(
        &mut csharp_builder,
        LanguageProfile::CSharp(CSharpVersion::CSharp14),
        LanguageExtensionInput::CSharp(&CSharpFacts {
            nullability: CSharpNullability::NonNullable,
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
        }),
    )?;
    let csharp = csharp_builder.finish()?;
    assert!(
        render(LanguageProfile::CSharp(CSharpVersion::CSharp14), &csharp)
            .expect("csharp document")
            .windows(b"dialect=csharp".len())
            .any(|value| value == b"dialect=csharp")
    );

    let mut go_builder = IrBuilder::new();
    let go_types = go_builder.intern_types(&[])?;
    let go_parameters = go_builder.intern_type_parameters(&[])?;
    let go_members = go_builder.intern_members(&[])?;
    let go_atoms = go_builder.intern_attributes(&[])?;
    add_one(
        &mut go_builder,
        LanguageProfile::Go(GoVersion::Go125),
        LanguageExtensionInput::Go(&GoFacts {
            signature: GoSignature {
                parameters: go_types,
                results: go_types,
                variadic: false,
            },
            type_parameters: go_parameters,
            fields: go_members,
            method_set: go_members,
            build_constraints: go_atoms,
            constant_value: go_atoms,
            constant_group: 0,
            constant_flags: 0,
        }),
    )?;
    let go = go_builder.finish()?;
    assert!(
        render(LanguageProfile::Go(GoVersion::Go125), &go)
            .expect("go document")
            .windows(b"dialect=go".len())
            .any(|value| value == b"dialect=go")
    );

    let mut rust_builder = IrBuilder::new();
    let rust_atoms = rust_builder.intern_attributes(&[])?;
    let rust_parameters = rust_builder.intern_type_parameters(&[])?;
    add_one(
        &mut rust_builder,
        LanguageProfile::Rust(RustEdition::Rust2024),
        LanguageExtensionInput::Rust(&RustFacts {
            ownership: RustOwnership::Value,
            lifetimes: rust_atoms,
            where_clauses: rust_parameters,
            macros: rust_atoms,
        }),
    )?;
    let rust = rust_builder.finish()?;
    assert!(
        render(LanguageProfile::Rust(RustEdition::Rust2024), &rust)
            .expect("rust document")
            .windows(b"dialect=rust".len())
            .any(|value| value == b"dialect=rust")
    );

    let mut python_builder = IrBuilder::new();
    let decorators = python_builder.intern_attributes(&[])?;
    add_one(
        &mut python_builder,
        LanguageProfile::Python(PythonVersion::Python314),
        LanguageExtensionInput::Python(&PythonFacts {
            decorators,
            parameter_kind: PythonParameterKind::PositionalOrKeyword,
            dynamic_confidence: compiler_ir::Confidence::Compiler,
        }),
    )?;
    let python = python_builder.finish()?;
    assert!(
        render(LanguageProfile::Python(PythonVersion::Python314), &python)
            .expect("python document")
            .windows(b"dialect=python".len())
            .any(|value| value == b"dialect=python")
    );

    let mut java_builder = IrBuilder::new();
    let java_types = java_builder.intern_types(&[])?;
    let java_atoms = java_builder.intern_attributes(&[])?;
    let java_members = java_builder.intern_members(&[])?;
    add_one(
        &mut java_builder,
        LanguageProfile::Java(JavaRelease::Java25),
        LanguageExtensionInput::Java(&JavaFacts {
            throws: java_types,
            annotations: java_atoms,
            overloads: java_members,
            record_components: java_members,
        }),
    )?;
    let java = java_builder.finish()?;
    assert!(
        render(LanguageProfile::Java(JavaRelease::Java25), &java)
            .expect("java document")
            .windows(b"dialect=java".len())
            .any(|value| value == b"dialect=java")
    );

    let mut clang_builder = IrBuilder::new();
    let clang_parameters = clang_builder.intern_type_parameters(&[])?;
    let clang_atoms = clang_builder.intern_attributes(&[])?;
    add_one(
        &mut clang_builder,
        LanguageProfile::C(CStandard::C23),
        LanguageExtensionInput::Clang(&ClangFacts {
            qualifiers: ClangQualifiers {
                is_const: false,
                is_volatile: false,
                is_restrict: false,
            },
            storage: ClangStorageClass::None,
            layout: ClangLayout {
                size_bits: None,
                align_bits: None,
            },
            templates: clang_parameters,
            includes: clang_atoms,
        }),
    )?;
    let clang = clang_builder.finish()?;
    assert!(
        render(LanguageProfile::C(CStandard::C23), &clang)
            .expect("clang document")
            .windows(b"dialect=clang".len())
            .any(|value| value == b"dialect=clang")
    );
    Ok(())
}

#[test]
fn semantic_document_is_reopen_stable_and_never_becomes_source_syntax()
-> Result<(), compiler_ir::BuildError> {
    let mut builder = IrBuilder::new();
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    builder.set_language_profile(profile)?;
    let parameters = builder.intern_type_parameters(&[])?;
    let ecosystem = builder.intern_atom(b"registry")?;
    let path = builder.intern_atom(b"remote::child")?;
    let display = builder.intern_atom(b"child")?;
    let external = builder.intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
        identity: ExternalDeclarationIdentity {
            foreign: ForeignDeclarationId::from_raw([0x31; 16]),
            variant: VariantAvailability::Unavailable,
        },
        origin: ForeignTargetOrigin::Unspecified { ecosystem },
        path,
        display,
        kind: Some(ItemKind::Record),
    }))?;
    let facts = TypeScriptFacts {
        type_parameters: parameters,
        declared: None,
        observed: None,
    };
    let versions = [version(4), version(5)];
    let items = [
        TreeItemInput {
            name: b"owner",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: extension_authority(FactAvailability::Captured),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[DocInput::Link {
                label: "child",
                target: compiler_ir::TreeLinkTarget::External(external),
            }],
            attributes: &[],
            source: None,
            extension: Some(LanguageExtensionInput::TypeScript(&facts)),
        },
        TreeItemInput {
            name: b"child",
            kind: ItemKind::Record,
            visibility: Visibility::Public,
            authority: extension_authority(FactAvailability::Captured),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: Some(LanguageExtensionInput::TypeScript(&facts)),
        },
    ];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    let image = builder.finish()?;
    let owned = render(profile, &image).expect("owned semantic document");
    assert!(
        owned
            .windows(
                b"target=external(foreign(identity=x\"31313131313131313131313131313131\"".len()
            )
            .any(|value| value
                == b"target=external(foreign(identity=x\"31313131313131313131313131313131\"")
    );
    let mut bytes = vec![0; full_semantic_image_len(&image).expect("image size")];
    encode_full_semantic_image(&image, &mut bytes).expect("encode full image");
    let reopened = SemanticImageView::reopen(&bytes).expect("reopen full image");
    assert_eq!(
        owned,
        render(profile, &reopened).expect("reopened semantic document")
    );
    assert!(matches!(
        prepare_source_syntax(profile, &reopened, EntityId::new(0)),
        Err(SourceSyntaxError::UnsupportedFact { .. })
    ));
    Ok(())
}

#[test]
fn captured_empty_is_distinct_and_short_output_is_untouched() -> Result<(), compiler_ir::BuildError>
{
    let mut builder = IrBuilder::new();
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    builder.set_language_profile(profile)?;
    let parameters = builder.intern_type_parameters(&[])?;
    let facts = TypeScriptFacts {
        type_parameters: parameters,
        declared: None,
        observed: None,
    };
    let versions = [version(8), version(9)];
    let items = [
        TreeItemInput {
            name: b"captured-empty",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: extension_authority(FactAvailability::Captured),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: Some(LanguageExtensionInput::TypeScript(&facts)),
        },
        TreeItemInput {
            name: b"unavailable",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: unavailable_authority(),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
    ];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    let image = builder.finish()?;
    let captured = prepare_semantic_document(profile, &image, EntityId::new(0), limits())
        .expect("captured empty document");
    let unavailable = prepare_semantic_document(profile, &image, EntityId::new(1), limits())
        .expect("unavailable document");
    let mut captured_bytes = vec![0; captured.encoded_len];
    let mut unavailable_bytes = vec![0; unavailable.encoded_len];
    let captured_output = captured
        .write_into(&mut captured_bytes)
        .expect("captured output");
    let unavailable_output = unavailable
        .write_into(&mut unavailable_bytes)
        .expect("unavailable output");
    assert!(captured_output.contains("extension=typescript(captured),source=unavailable,semantic-type=unavailable,documentation=[]"));
    assert!(unavailable_output.contains("extension=unavailable,source=unavailable,semantic-type=unavailable,documentation=unavailable"));
    let mut sentinel = vec![0xA5; captured.encoded_len - 1];
    let before = sentinel.clone();
    assert!(matches!(
        captured.write_into(&mut sentinel),
        Err(SemanticDocumentError::OutputTooSmall { .. })
    ));
    assert_eq!(sentinel, before);
    assert!(matches!(
        prepare_semantic_document(
            LanguageProfile::Rust(RustEdition::Rust2024),
            &image,
            EntityId::new(0),
            limits(),
        ),
        Err(SemanticDocumentError::ProfileMismatch { .. })
    ));
    Ok(())
}
