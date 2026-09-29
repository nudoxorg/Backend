//! Language-extension row family fixtures and decoder tests.
#![deny(clippy::as_conversions, clippy::indexing_slicing, unsafe_code)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use alloc::{boxed::Box, vec, vec::Vec};

use backend_version::ScopeRoot;

use super::wire::{
    CSHARP_TAG, TYPESCRIPT_TAG, domain_code, extension_row_key, identity_bytes, validate_record,
};
use super::*;
use crate::ir::versioned_records::{
    CanonicalSemanticPlaneSegmentPayload, CanonicalSemanticPlaneSegmentView,
    SemanticPlaneRecordError, TypesRowDomainV2, TypesRows, decode_semantic_plane_segment,
    encode_canonical_plane_family, validate_types_family_v2,
};
use crate::ir::{
    BuiltinType, CSharpFacts, CSharpMemberEffects, CSharpNullability, CSharpPartialRole,
    CSharpReferenceKind, CSharpVersion, CStandard, ClangFacts, ClangLayout, ClangQualifiers,
    ClangStorageClass, Confidence, CorePayloadHash, CxxStandard, DeclarationFamilyId,
    EntityAuthorityFacts, EntityVersion, FactAvailability, GoFacts, GoSignature, GoVersion, Ir,
    IrBuilder, Item, ItemKind, JavaFacts, JavaRelease, Language, LanguageExtensionInput,
    LanguageProfile, ParentageAuthority, PythonFacts, PythonParameterKind, PythonVersion,
    RustEdition, RustFacts, RustOwnership, SemanticInputWitness, SemanticIrPlane,
    SemanticPlaneKind, SemanticReader, SourceSpan, TypeExpr, TypeScriptFacts, TypeScriptSource,
    VariantFingerprint, Visibility,
};

const MAXIMUM_BYTES: usize = crate::ir::MAX_SEMANTIC_SEGMENT_BYTES;

fn version(seed: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([seed; 16]),
        variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
        core_payload: CorePayloadHash::from_raw([seed.wrapping_add(2); 16]),
    }
}

fn add_extension(
    builder: &mut IrBuilder,
    seed: u8,
    extension: LanguageExtensionInput<'_>,
) -> crate::ir::EntityId {
    let item = Item {
        name: builder.intern_atom(b"extension-owner").expect("owner atom"),
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        parent: None,
        semantic_type: None,
        members: builder.intern_members(&[]).expect("empty members"),
        docs: builder.intern_docs(&[]).expect("empty docs"),
        attributes: builder.intern_attributes(&[]).expect("empty attributes"),
        source: None,
    };
    builder
        .add_item(
            version(seed),
            item,
            Some(extension),
            EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                visibility: FactAvailability::Captured,
                language_extension: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
        )
        .expect("valid extension owner")
}

fn add_plain_entity(builder: &mut IrBuilder, seed: u8) -> crate::ir::EntityId {
    let item = Item {
        name: builder
            .intern_atom(b"extension-target")
            .expect("target atom"),
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        parent: None,
        semantic_type: None,
        members: builder.intern_members(&[]).expect("empty members"),
        docs: builder.intern_docs(&[]).expect("empty docs"),
        attributes: builder.intern_attributes(&[]).expect("empty attributes"),
        source: None,
    };
    builder
        .add_item(
            version(seed),
            item,
            None,
            EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                ..EntityAuthorityFacts::default()
            },
        )
        .expect("valid target entity")
}

fn fixture(profile: LanguageProfile) -> Ir {
    let mut builder = IrBuilder::new();
    builder.set_language_profile(profile).expect("profile");
    match profile {
        LanguageProfile::TypeScript(_) => {
            let ty = builder
                .intern_type(TypeExpr::Concrete(crate::ir::ConcreteType::Builtin(
                    BuiltinType::I32,
                )))
                .expect("declared type");
            let facts = TypeScriptFacts {
                type_parameters: builder
                    .intern_type_parameters(&[])
                    .expect("type parameters"),
                declared: Some(ty),
                observed: None,
            };
            add_extension(&mut builder, 1, LanguageExtensionInput::TypeScript(&facts));
        }
        LanguageProfile::CSharp(_) => {
            let attributes = builder.intern_attributes(&[]).expect("attributes");
            let path = builder
                .intern_atom(&[0xff, b'/', b's', b'r', b'c'])
                .expect("path");
            let facts = CSharpFacts {
                nullability: CSharpNullability::Nullable,
                reference_kind: CSharpReferenceKind::Ref,
                constraints: builder.intern_type_parameters(&[]).expect("constraints"),
                effects: CSharpMemberEffects {
                    is_async: true,
                    is_iterator: false,
                    is_extension: true,
                },
                attributes,
                partial: CSharpPartialRole::Implementation,
                xml_provenance: Some(SourceSpan::new(path, 2, 7).expect("span")),
            };
            add_extension(&mut builder, 2, LanguageExtensionInput::CSharp(&facts));
        }
        LanguageProfile::Go(_) => {
            let types = builder.intern_types(&[]).expect("types");
            let entities = builder.intern_members(&[]).expect("entities");
            let atoms = builder.intern_attributes(&[]).expect("atoms");
            let facts = GoFacts {
                signature: GoSignature {
                    parameters: types,
                    results: types,
                    variadic: true,
                },
                type_parameters: builder
                    .intern_type_parameters(&[])
                    .expect("type parameters"),
                fields: entities,
                method_set: entities,
                build_constraints: atoms,
                constant_value: atoms,
                constant_group: -9,
                constant_flags: 0x81,
            };
            add_extension(&mut builder, 3, LanguageExtensionInput::Go(&facts));
        }
        LanguageProfile::Rust(_) => {
            let atoms = builder.intern_attributes(&[]).expect("atoms");
            let facts = RustFacts {
                ownership: RustOwnership::MutableBorrow,
                lifetimes: atoms,
                where_clauses: builder.intern_type_parameters(&[]).expect("where clauses"),
                macros: atoms,
                const_defaults: atoms,
                free_predicates: builder
                    .intern_free_predicates(&[])
                    .expect("free predicates"),
            };
            add_extension(&mut builder, 4, LanguageExtensionInput::Rust(&facts));
        }
        LanguageProfile::Python(_) => {
            let facts = PythonFacts {
                decorators: builder.intern_attributes(&[]).expect("decorators"),
                parameter_kind: PythonParameterKind::VariadicKeyword,
                dynamic_confidence: Confidence::Compiler,
            };
            add_extension(&mut builder, 5, LanguageExtensionInput::Python(&facts));
        }
        LanguageProfile::Java(_) => {
            let entities = builder.intern_members(&[]).expect("entities");
            let facts = JavaFacts {
                throws: builder.intern_types(&[]).expect("throws"),
                annotations: builder.intern_attributes(&[]).expect("annotations"),
                overloads: entities,
                record_components: entities,
            };
            add_extension(&mut builder, 6, LanguageExtensionInput::Java(&facts));
        }
        LanguageProfile::C(_) | LanguageProfile::Cxx(_) => {
            let facts = ClangFacts {
                qualifiers: ClangQualifiers {
                    is_const: true,
                    is_volatile: false,
                    is_restrict: true,
                },
                storage: ClangStorageClass::ThreadLocal,
                layout: ClangLayout {
                    size_bits: Some(128),
                    align_bits: None,
                },
                templates: builder.intern_type_parameters(&[]).expect("templates"),
                includes: builder.intern_attributes(&[]).expect("includes"),
            };
            add_extension(&mut builder, 7, LanguageExtensionInput::Clang(&facts));
        }
    }
    builder.finish().expect("IR")
}

fn witness() -> SemanticInputWitness {
    SemanticInputWitness::claimed([0xa1; 32], ScopeRoot::from_bytes([0xb2; 32]))
}

fn checked_types(ir: &Ir) -> crate::ir::versioned_records::CheckedTypesFamilyV2 {
    let payloads = encode_canonical_plane_family(ir, &TypesRows, witness(), MAXIMUM_BYTES)
        .expect("Types rows");
    let views = payloads
        .iter()
        .map(|segment| {
            decode_semantic_plane_segment(
                segment.kind(),
                &segment.metadata().expect("Types descriptor"),
                segment.bytes(),
            )
            .expect("reopened Types segment")
        })
        .collect::<Vec<_>>();
    validate_types_family_v2(views).expect("checked Types family")
}

fn encode_extensions(
    ir: &Ir,
    profile: LanguageProfile,
) -> Box<[CanonicalSemanticPlaneSegmentPayload]> {
    encode_language_extension_plane(ir, profile, witness(), MAXIMUM_BYTES).expect("extension rows")
}

fn expected_extension_owners(ir: &Ir) -> Vec<[u8; 32]> {
    let mut owners = ir
        .canonical_entities()
        .filter(|entity| entity.authority.language_extension == FactAvailability::Captured)
        .map(|entity| identity_bytes(entity.version.identity()))
        .collect::<Vec<_>>();
    owners.sort_unstable();
    owners
}

fn views(
    payloads: &[CanonicalSemanticPlaneSegmentPayload],
) -> Vec<CanonicalSemanticPlaneSegmentView<'_>> {
    payloads
        .iter()
        .map(|segment| {
            decode_semantic_plane_segment(
                segment.kind(),
                &segment.metadata().expect("extension descriptor"),
                segment.bytes(),
            )
            .expect("reopened extension segment")
        })
        .collect()
}

#[test]
fn all_supported_extension_profiles_reopen_and_resolve_types_references() {
    let profiles = [
        LanguageProfile::TypeScript(TypeScriptSource::Tsx),
        LanguageProfile::CSharp(CSharpVersion::CSharp14),
        LanguageProfile::Go(GoVersion::Go125),
        LanguageProfile::Rust(RustEdition::Rust2024),
        LanguageProfile::Python(PythonVersion::Python314),
        LanguageProfile::Java(JavaRelease::Java25),
        LanguageProfile::C(CStandard::C23),
        LanguageProfile::Cxx(CxxStandard::Cxx26),
    ];
    for profile in profiles {
        let ir = fixture(profile);
        let payloads = encode_extensions(&ir, profile);
        let checked = checked_types(&ir);
        let expected_owners = expected_extension_owners(&ir);
        let checked_extensions = validate_language_extension_family_v2(
            profile,
            views(&payloads),
            &checked,
            &expected_owners,
        )
        .expect("all exact Types references resolve and Core owners match");
        assert_eq!(checked_extensions.row_count(), 1);
        assert_eq!(checked_extensions.owner_identities().len(), 1);
        let descriptors = payloads
            .iter()
            .map(|segment| segment.metadata().expect("descriptor"))
            .collect::<Vec<_>>();
        let bytes = payloads
            .iter()
            .map(|segment| segment.bytes())
            .collect::<Vec<_>>();
        verify_language_extension_plane_against_reader(
            &ir,
            profile,
            witness(),
            &descriptors,
            &bytes,
            MAXIMUM_BYTES,
        )
        .expect("independent reader census agrees");
    }
}

fn typescript_pair(reverse: bool) -> Ir {
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let mut builder = IrBuilder::new();
    builder.set_language_profile(profile).expect("profile");
    let type_parameters = builder.intern_type_parameters(&[]).expect("params");
    let order = if reverse { [2, 1] } else { [1, 2] };
    for seed in order {
        let facts = TypeScriptFacts {
            type_parameters,
            declared: None,
            observed: None,
        };
        add_extension(
            &mut builder,
            seed,
            LanguageExtensionInput::TypeScript(&facts),
        );
    }
    builder.finish().expect("IR")
}

#[test]
fn declaration_reordering_preserves_keys_and_independent_reader_oracle() {
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let first = typescript_pair(false);
    let reordered = typescript_pair(true);
    let first_payloads = encode_extensions(&first, profile);
    let reordered_payloads = encode_extensions(&reordered, profile);
    let first_bytes = first_payloads
        .iter()
        .map(|segment| segment.bytes())
        .collect::<Vec<_>>();
    let reordered_bytes = reordered_payloads
        .iter()
        .map(|segment| segment.bytes())
        .collect::<Vec<_>>();
    assert_eq!(first_bytes, reordered_bytes);
    let descriptors = first_payloads
        .iter()
        .map(|segment| segment.metadata().expect("descriptor"))
        .collect::<Vec<_>>();
    verify_language_extension_plane_against_reader(
        &reordered,
        profile,
        witness(),
        &descriptors,
        &first_bytes,
        MAXIMUM_BYTES,
    )
    .expect("independent reordered reader has the exact family");
}

#[test]
fn stable_key_domain_includes_profile_and_row_role() {
    let identity = version(1).identity();
    let typescript = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let tsx = LanguageProfile::TypeScript(TypeScriptSource::Tsx);
    assert_ne!(
        extension_row_key(typescript, identity, TYPESCRIPT_TAG),
        extension_row_key(tsx, identity, TYPESCRIPT_TAG)
    );
    assert_ne!(
        extension_row_key(typescript, identity, TYPESCRIPT_TAG),
        extension_row_key(typescript, identity, CSHARP_TAG)
    );
    let c = LanguageProfile::C(CStandard::C23);
    let cxx = LanguageProfile::Cxx(CxxStandard::Cxx26);
    assert_ne!(
        extension_row_key(c, identity, super::wire::CLANG_TAG),
        extension_row_key(cxx, identity, super::wire::CLANG_TAG)
    );
}

fn csharp_atom_order(reverse: bool) -> Ir {
    let profile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
    let mut builder = IrBuilder::new();
    builder.set_language_profile(profile).expect("profile");
    let (first, second, path) = if reverse {
        let second = builder.intern_atom(b"Attribute.Second").expect("second");
        let first = builder.intern_atom(b"Attribute.First").expect("first");
        let path = builder.intern_atom(b"\xff/src/file.cs").expect("path");
        (first, second, path)
    } else {
        let path = builder.intern_atom(b"\xff/src/file.cs").expect("path");
        let first = builder.intern_atom(b"Attribute.First").expect("first");
        let second = builder.intern_atom(b"Attribute.Second").expect("second");
        (first, second, path)
    };
    let attributes = builder
        .intern_attributes(&[first, second])
        .expect("ordered attributes");
    let facts = CSharpFacts {
        nullability: CSharpNullability::Nullable,
        reference_kind: CSharpReferenceKind::Ref,
        constraints: builder.intern_type_parameters(&[]).expect("constraints"),
        effects: CSharpMemberEffects {
            is_async: true,
            is_iterator: false,
            is_extension: true,
        },
        attributes,
        partial: CSharpPartialRole::Implementation,
        xml_provenance: Some(SourceSpan::new(path, 2, 7).expect("span")),
    };
    add_extension(&mut builder, 2, LanguageExtensionInput::CSharp(&facts));
    builder.finish().expect("IR")
}

#[test]
fn atom_id_reordering_preserves_types_and_extension_payloads() {
    let profile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
    let first = csharp_atom_order(false);
    let reordered = csharp_atom_order(true);
    let first_extensions = encode_extensions(&first, profile);
    let reordered_extensions = encode_extensions(&reordered, profile);
    let first_types = encode_canonical_plane_family(&first, &TypesRows, witness(), MAXIMUM_BYTES)
        .expect("Types rows");
    let reordered_types =
        encode_canonical_plane_family(&reordered, &TypesRows, witness(), MAXIMUM_BYTES)
            .expect("reordered Types rows");
    fn payload_bytes(payloads: &[CanonicalSemanticPlaneSegmentPayload]) -> Vec<&[u8]> {
        payloads
            .iter()
            .map(|segment| segment.bytes())
            .collect::<Vec<_>>()
    }
    assert_eq!(
        payload_bytes(&first_extensions),
        payload_bytes(&reordered_extensions)
    );
    assert_eq!(payload_bytes(&first_types), payload_bytes(&reordered_types));
}

#[test]
fn catalog_requires_exact_core_captured_owner_inventory() {
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let ir = fixture(profile);
    let payloads = encode_extensions(&ir, profile);
    let types = checked_types(&ir);
    let actual = expected_extension_owners(&ir);
    assert!(matches!(
        validate_language_extension_family_v2(profile, views(&payloads), &types, &[]),
        Err(SemanticPlaneRecordError::RowGrammar)
    ));
    let mut extra = actual.clone();
    extra.push([0xff; 32]);
    extra.sort_unstable();
    assert!(matches!(
        validate_language_extension_family_v2(profile, views(&payloads), &types, &extra),
        Err(SemanticPlaneRecordError::RowGrammar)
    ));

    let mut empty_builder = IrBuilder::new();
    empty_builder
        .set_language_profile(profile)
        .expect("profile");
    add_plain_entity(&mut empty_builder, 30);
    let empty_ir = empty_builder.finish().expect("empty extension IR");
    let empty_payloads = encode_extensions(&empty_ir, profile);
    assert!(empty_payloads.is_empty());
    let empty_types = checked_types(&empty_ir);
    let empty_family =
        validate_language_extension_family_v2(profile, views(&empty_payloads), &empty_types, &[])
            .expect("empty captured-owner inventory is complete");
    assert_eq!(empty_family.row_count(), 0);
}

#[test]
fn clang_decoder_rejects_zero_alignment() {
    let profile = LanguageProfile::C(CStandard::C23);
    let ir = fixture(profile);
    let payloads = encode_extensions(&ir, profile);
    let segment = payloads.first().expect("Clang segment");
    let view = decode_semantic_plane_segment(
        segment.kind(),
        &segment.metadata().expect("descriptor"),
        segment.bytes(),
    )
    .expect("valid segment");
    let record = view.records().next().expect("Clang row");
    let kind = SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(profile));
    let mut zero_alignment = record.payload().to_vec();
    let alignment_tag = zero_alignment.get_mut(42).expect("alignment tag");
    *alignment_tag = 1;
    for _ in 0..4 {
        zero_alignment.insert(43, 0);
    }
    assert!(matches!(
        validate_record(kind, record.key(), record.tag(), &zero_alignment),
        Err(SemanticPlaneRecordError::RowGrammar)
    ));
}

#[test]
fn go_and_java_entity_list_order_survives_catalog_decoding() {
    for (profile, language) in [
        (LanguageProfile::Go(GoVersion::Go125), Language::Go),
        (LanguageProfile::Java(JavaRelease::Java25), Language::Java),
    ] {
        let mut builder = IrBuilder::new();
        builder.set_language_profile(profile).expect("profile");
        let first = add_plain_entity(&mut builder, 20);
        let second = add_plain_entity(&mut builder, 21);
        let forward = builder
            .intern_members(&[first, second])
            .expect("forward list");
        let reverse = builder
            .intern_members(&[second, first])
            .expect("reverse list");
        match language {
            Language::Go => {
                let atoms = builder.intern_attributes(&[]).expect("atoms");
                let facts = GoFacts {
                    signature: GoSignature {
                        parameters: builder.intern_types(&[]).expect("parameters"),
                        results: builder.intern_types(&[]).expect("results"),
                        variadic: false,
                    },
                    type_parameters: builder.intern_type_parameters(&[]).expect("params"),
                    fields: forward,
                    method_set: reverse,
                    build_constraints: atoms,
                    constant_value: atoms,
                    constant_group: 0,
                    constant_flags: 0,
                };
                add_extension(&mut builder, 3, LanguageExtensionInput::Go(&facts));
            }
            Language::Java => {
                let facts = JavaFacts {
                    throws: builder.intern_types(&[]).expect("throws"),
                    annotations: builder.intern_attributes(&[]).expect("annotations"),
                    overloads: reverse,
                    record_components: forward,
                };
                add_extension(&mut builder, 6, LanguageExtensionInput::Java(&facts));
            }
            _ => unreachable!(),
        }
        let ir = builder.finish().expect("IR");
        let payloads = encode_extensions(&ir, profile);
        let types = checked_types(&ir);
        let checked = validate_language_extension_family_v2(
            profile,
            views(&payloads),
            &types,
            &expected_extension_owners(&ir),
        )
        .expect("catalog closure");
        let first_identity = identity_bytes(version(20).identity());
        let second_identity = identity_bytes(version(21).identity());
        let expected = if language == Language::Go {
            vec![
                first_identity,
                second_identity,
                second_identity,
                first_identity,
            ]
        } else {
            vec![
                second_identity,
                first_identity,
                first_identity,
                second_identity,
            ]
        };
        assert_eq!(checked.declaration_references(), expected);
    }
}

#[test]
fn decoder_rejects_unknown_tags_wrong_domain_lengths_and_trailing_bytes() {
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let ir = fixture(profile);
    let payloads = encode_extensions(&ir, profile);
    let segment = payloads.first().expect("one extension segment");
    let view = decode_semantic_plane_segment(
        segment.kind(),
        &segment.metadata().expect("descriptor"),
        segment.bytes(),
    )
    .expect("valid segment");
    let record = view.records().next().expect("one row");
    let kind = SemanticPlaneKind::Ir(SemanticIrPlane::LanguageExtensions(profile));
    let mut trailing = record.payload().to_vec();
    trailing.push(0);
    assert!(matches!(
        validate_record(kind, record.key(), record.tag(), &trailing),
        Err(SemanticPlaneRecordError::RowTrailingBytes)
    ));
    assert!(matches!(
        validate_record(kind, record.key(), u8::MAX, record.payload()),
        Err(SemanticPlaneRecordError::RowGrammar)
    ));
    let mut wrong_domain = record.payload().to_vec();
    let domain_offset = 33;
    if let Some(domain) = wrong_domain.get_mut(domain_offset) {
        *domain = domain_code(TypesRowDomainV2::AtomList);
    }
    assert!(matches!(
        validate_record(kind, record.key(), record.tag(), &wrong_domain),
        Err(SemanticPlaneRecordError::RowGrammar)
    ));
    let mut unknown_domain = record.payload().to_vec();
    if let Some(domain) = unknown_domain.get_mut(domain_offset) {
        *domain = u8::MAX;
    }
    assert!(matches!(
        validate_record(kind, record.key(), record.tag(), &unknown_domain),
        Err(SemanticPlaneRecordError::RowGrammar)
    ));
    let mut unavailable = record.payload().to_vec();
    if let Some(availability) = unavailable.get_mut(32) {
        *availability = 0;
    }
    assert!(matches!(
        validate_record(kind, record.key(), record.tag(), &unavailable),
        Err(SemanticPlaneRecordError::RowGrammar)
    ));
    let mut short = record.payload().to_vec();
    let _ = short.pop();
    assert!(matches!(
        validate_record(kind, record.key(), record.tag(), &short),
        Err(SemanticPlaneRecordError::Truncated)
    ));
    let mut wrong_key = record.key();
    if let Some(first) = wrong_key.first_mut() {
        *first ^= 1;
    }
    assert!(matches!(
        validate_record(kind, wrong_key, record.tag(), record.payload()),
        Err(SemanticPlaneRecordError::StableKeyMismatch)
    ));
}

#[test]
fn independent_reader_oracle_rejects_a_missing_extension_range() {
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let ir = fixture(profile);
    assert!(matches!(
        verify_language_extension_plane_against_reader(
            &ir,
            profile,
            witness(),
            &[],
            &[],
            MAXIMUM_BYTES,
        ),
        Err(SemanticPlaneRecordError::SegmentCount { .. })
    ));
}
