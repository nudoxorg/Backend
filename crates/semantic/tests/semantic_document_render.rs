//! Ordinary contract falsifiers for authority-backed semantic documents.

use core::num::NonZeroUsize;

use backend_semantic::ir::{
    BorrowedTree, BuiltinType, CSharpFacts, CSharpMemberEffects, CSharpNullability,
    CSharpPartialRole, CSharpReferenceKind, CanonicalTypeRenderLimits, ClangFacts, ClangLayout,
    ClangQualifiers, ClangStorageClass, ConcreteType, CorePayloadHash, DeclarationFamilyId,
    DocInput, EntityAuthorityFacts, EntityId, EntityVersion, ExternalDeclarationIdentity,
    ExternalTarget, FactAvailability, ForeignDeclarationId, ForeignExternalTarget,
    ForeignTargetOrigin, GoFacts, GoSignature, Ir, IrBuilder, ItemKind, JavaFacts,
    LanguageExtensionInput, LanguageProfile, ParentageAuthority, PythonFacts, PythonParameterKind,
    RustFacts, RustOwnership, SemanticDocumentError, SemanticImageView, SourceSpan,
    SourceSyntaxError, TreeItemInput, TypeParameter, TypeParameterBound, TypeParameterInference,
    TypeParameterKind, TypeParameterPrimaryRequirement, TypeParameterRequirements, TypeScriptFacts,
    Variance, VariantAvailability, VariantFingerprint, Visibility, encode_full_semantic_image,
    full_semantic_image_len, prepare_semantic_document, prepare_source_syntax,
};
use backend_semantic::vocabulary::{
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
) -> Result<(), backend_semantic::ir::BuildError> {
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

fn rich_type_parameters(
    builder: &mut IrBuilder,
    semantic_type: backend_semantic::ir::TypeId,
) -> Result<backend_semantic::ir::TypeParameterListId, backend_semantic::ir::BuildError> {
    let name = builder.intern_atom(b"T")?;
    let lifetime = builder.intern_atom(b"'a")?;
    let bounds = builder.intern_type_parameter_bounds(&[
        TypeParameterBound::Type(semantic_type),
        TypeParameterBound::Lifetime(lifetime),
    ])?;
    builder.intern_type_parameters(&[TypeParameter {
        name,
        bounds,
        default: Some(semantic_type),
        variance: Variance::Covariant,
        kind: TypeParameterKind::Type {
            inference: TypeParameterInference::Const,
        },
        requirements: TypeParameterRequirements {
            primary: TypeParameterPrimaryRequirement::Reference { nullable: true },
            constructor: false,
            allows_ref_like: false,
        },
    }])
}

fn rich_typescript() -> Result<Ir, backend_semantic::ir::BuildError> {
    let mut builder = IrBuilder::new();
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    builder.set_language_profile(profile)?;
    let semantic_type = builder.intern_concrete(ConcreteType::Builtin(BuiltinType::I32))?;
    let type_parameters = rich_type_parameters(&mut builder, semantic_type.erase())?;
    add_one(
        &mut builder,
        profile,
        LanguageExtensionInput::TypeScript(&TypeScriptFacts {
            type_parameters,
            declared: Some(semantic_type.erase()),
            observed: Some(semantic_type.erase()),
        }),
    )?;
    builder.finish()
}

fn rich_csharp() -> Result<Ir, backend_semantic::ir::BuildError> {
    let mut builder = IrBuilder::new();
    let profile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
    builder.set_language_profile(profile)?;
    let semantic_type = builder.intern_concrete(ConcreteType::Builtin(BuiltinType::I32))?;
    let constraints = rich_type_parameters(&mut builder, semantic_type.erase())?;
    let attribute = builder.intern_atom(b"Obsolete")?;
    let attributes = builder.intern_attributes(&[attribute])?;
    let file = builder.intern_atom(b"src/lib.cs")?;
    let xml_provenance = SourceSpan::new(file, 4, 17);
    add_one(
        &mut builder,
        profile,
        LanguageExtensionInput::CSharp(&CSharpFacts {
            nullability: CSharpNullability::Nullable,
            reference_kind: CSharpReferenceKind::Ref,
            constraints,
            effects: CSharpMemberEffects {
                is_async: true,
                is_iterator: true,
                is_extension: true,
            },
            attributes,
            partial: CSharpPartialRole::Implementation,
            xml_provenance,
        }),
    )?;
    builder.finish()
}

fn rich_go() -> Result<Ir, backend_semantic::ir::BuildError> {
    let mut builder = IrBuilder::new();
    let profile = LanguageProfile::Go(GoVersion::Go125);
    builder.set_language_profile(profile)?;
    let semantic_type = builder.intern_concrete(ConcreteType::Builtin(BuiltinType::I32))?;
    let types = builder.intern_types(&[semantic_type.erase()])?;
    let type_parameters = rich_type_parameters(&mut builder, semantic_type.erase())?;
    let members = builder.intern_members(&[EntityId::new(0)])?;
    let build_atom = builder.intern_atom(b"linux")?;
    let build_constraints = builder.intern_attributes(&[build_atom])?;
    let constant_atom = builder.intern_atom(b"42")?;
    let constant_value = builder.intern_attributes(&[constant_atom])?;
    add_one(
        &mut builder,
        profile,
        LanguageExtensionInput::Go(&GoFacts {
            signature: GoSignature {
                parameters: types,
                results: types,
                variadic: true,
            },
            type_parameters,
            fields: members,
            method_set: members,
            build_constraints,
            constant_value,
            constant_group: -7,
            constant_flags: 0xA5,
        }),
    )?;
    builder.finish()
}

fn rich_rust() -> Result<Ir, backend_semantic::ir::BuildError> {
    let mut builder = IrBuilder::new();
    let profile = LanguageProfile::Rust(RustEdition::Rust2024);
    builder.set_language_profile(profile)?;
    let semantic_type = builder.intern_concrete(ConcreteType::Builtin(BuiltinType::I32))?;
    let type_parameters = rich_type_parameters(&mut builder, semantic_type.erase())?;
    let lifetime = builder.intern_atom(b"'static")?;
    let lifetimes = builder.intern_attributes(&[lifetime])?;
    let macro_name = builder.intern_atom(b"trace")?;
    let macros = builder.intern_attributes(&[macro_name])?;
    add_one(
        &mut builder,
        profile,
        LanguageExtensionInput::Rust(&RustFacts {
            ownership: RustOwnership::MutableBorrow,
            lifetimes,
            where_clauses: type_parameters,
            macros,
        }),
    )?;
    builder.finish()
}

fn rich_python() -> Result<Ir, backend_semantic::ir::BuildError> {
    let mut builder = IrBuilder::new();
    let profile = LanguageProfile::Python(PythonVersion::Python314);
    builder.set_language_profile(profile)?;
    let decorator = builder.intern_atom(b"classmethod")?;
    let decorators = builder.intern_attributes(&[decorator])?;
    add_one(
        &mut builder,
        profile,
        LanguageExtensionInput::Python(&PythonFacts {
            decorators,
            parameter_kind: PythonParameterKind::VariadicKeyword,
            dynamic_confidence: backend_semantic::ir::Confidence::Imported,
        }),
    )?;
    builder.finish()
}

fn rich_java() -> Result<Ir, backend_semantic::ir::BuildError> {
    let mut builder = IrBuilder::new();
    let profile = LanguageProfile::Java(JavaRelease::Java25);
    builder.set_language_profile(profile)?;
    let semantic_type = builder.intern_concrete(ConcreteType::Builtin(BuiltinType::I32))?;
    let throws = builder.intern_types(&[semantic_type.erase()])?;
    let annotation = builder.intern_atom(b"Override")?;
    let annotations = builder.intern_attributes(&[annotation])?;
    let members = builder.intern_members(&[EntityId::new(0)])?;
    add_one(
        &mut builder,
        profile,
        LanguageExtensionInput::Java(&JavaFacts {
            throws,
            annotations,
            overloads: members,
            record_components: members,
        }),
    )?;
    builder.finish()
}

fn rich_clang() -> Result<Ir, backend_semantic::ir::BuildError> {
    let mut builder = IrBuilder::new();
    let profile = LanguageProfile::C(CStandard::C23);
    builder.set_language_profile(profile)?;
    let semantic_type = builder.intern_concrete(ConcreteType::Builtin(BuiltinType::I32))?;
    let templates = rich_type_parameters(&mut builder, semantic_type.erase())?;
    let include = builder.intern_atom(b"header.h")?;
    let includes = builder.intern_attributes(&[include])?;
    add_one(
        &mut builder,
        profile,
        LanguageExtensionInput::Clang(&ClangFacts {
            qualifiers: ClangQualifiers {
                is_const: true,
                is_volatile: true,
                is_restrict: true,
            },
            storage: ClangStorageClass::ThreadLocal,
            layout: ClangLayout {
                size_bits: Some(64),
                align_bits: Some(8),
            },
            templates,
            includes,
        }),
    )?;
    builder.finish()
}

fn assert_reopened_payload(profile: LanguageProfile, image: &Ir, expected: &[&str]) {
    let owned = render(profile, image).expect("owned semantic document");
    let mut bytes = vec![0; full_semantic_image_len(image).expect("image size")];
    encode_full_semantic_image(image, &mut bytes).expect("encode full image");
    let reopened = SemanticImageView::reopen(&bytes).expect("reopen full image");
    let reopened_output = render(profile, &reopened).expect("reopened semantic document");
    assert_eq!(owned, reopened_output);
    let text = core::str::from_utf8(&owned).expect("semantic document is UTF-8");
    for needle in expected {
        assert!(text.contains(needle), "missing {needle:?} in {text}");
    }
}

#[test]
fn every_named_extension_payload_is_owned_reopen_byte_exact() -> Result<(), backend_semantic::ir::BuildError>
{
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let image = rich_typescript()?;
    assert_reopened_payload(
        profile,
        &image,
        &[
            "extension-facts=typescript(type-parameters=type-parameter-list(",
            "declared=some(type=builtin(i32))",
            "observed=some(type=builtin(i32))",
            "variance=covariant",
            "inference=const",
            "requirements=(primary=reference(nullable=true),constructor=false,allows-ref-like=false)",
        ],
    );

    let profile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
    let image = rich_csharp()?;
    assert_reopened_payload(
        profile,
        &image,
        &[
            "extension-facts=csharp(nullability=nullable,reference-kind=ref",
            "effects=(async=true,iterator=true,extension=true)",
            "attributes=atom-list(",
            "partial=implementation",
            "xml-provenance=span(file=x\"7372632F6C69622E6373\",start=4,end=17)",
        ],
    );

    let profile = LanguageProfile::Go(GoVersion::Go125);
    let image = rich_go()?;
    assert_reopened_payload(
        profile,
        &image,
        &[
            "extension-facts=go(signature(parameters=type-list(",
            "variadic=true",
            "fields=entity-list(",
            "build-constraints=atom-list(",
            "constant-group=-7,constant-flags=165",
        ],
    );

    let profile = LanguageProfile::Rust(RustEdition::Rust2024);
    let image = rich_rust()?;
    assert_reopened_payload(
        profile,
        &image,
        &[
            "extension-facts=rust(ownership=mutable-borrow",
            "lifetimes=atom-list(",
            "where-clauses=type-parameter-list(",
            "macros=atom-list(",
        ],
    );

    let profile = LanguageProfile::Python(PythonVersion::Python314);
    let image = rich_python()?;
    assert_reopened_payload(
        profile,
        &image,
        &[
            "extension-facts=python(decorators=atom-list(",
            "parameter-kind=variadic-keyword",
            "dynamic-confidence=imported",
        ],
    );

    let profile = LanguageProfile::Java(JavaRelease::Java25);
    let image = rich_java()?;
    assert_reopened_payload(
        profile,
        &image,
        &[
            "extension-facts=java(throws=type-list(",
            "annotations=atom-list(",
            "overloads=entity-list(",
            "record-components=entity-list(",
        ],
    );

    let profile = LanguageProfile::C(CStandard::C23);
    let image = rich_clang()?;
    assert_reopened_payload(
        profile,
        &image,
        &[
            "extension-facts=clang(qualifiers=(const=true,volatile=true,restrict=true),storage=thread-local",
            "layout=(size-bits=64,align-bits=8)",
            "templates=type-parameter-list(",
            "includes=atom-list(",
        ],
    );
    Ok(())
}

fn render<Reader: backend_semantic::ir::SemanticReader + ?Sized>(
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
-> Result<(), backend_semantic::ir::BuildError> {
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
            dynamic_confidence: backend_semantic::ir::Confidence::Compiler,
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
-> Result<(), backend_semantic::ir::BuildError> {
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
                target: backend_semantic::ir::TreeLinkTarget::External(external),
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
fn captured_empty_is_distinct_and_short_output_is_untouched() -> Result<(), backend_semantic::ir::BuildError>
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
