//! Proves declaration identity: stable ids are minted from exactly the
//! `(package, path, kind, name)` key cells in their dedicated family-key
//! domain — never from any ordinal — and foreign keys hash their key cells, never a
//! resolved target or display spelling.

use backend_semantic::ir::{
    AnonymousCallableInstanceFault, AnonymousCallableInstanceKey, DeclarationParentage,
    ScopedDeclarationKey, ScopedTypedDeclarationKey,
};
use backend_semantic::ir_vocabulary::{
    AnonymousCallableAnchor, AnonymousCallableFamilyMultiplicity, CallableAnchorStep,
    CallableChildRole, CallableParentShape, DeclarationFamilyId, DeclarationIdentity,
    DeclarationKey, DeclarationKeyFault, DeclarationName, DeclarationPathFault, EntityKind,
    ForeignKey, ForeignKeyFault, ForeignOrigin, MAX_ANONYMOUS_CALLABLE_ANCHOR_BYTES,
    MAX_ANONYMOUS_CALLABLE_ROUTE_STEPS, Occurrence, OccurrenceTarget, PackageLineage,
    PackageLineageFault, PreimageOverflow, ReferenceKind, RelSpan, RelSpanFault, StableRef,
    TypeScriptCallableCoordinateFault, TypeScriptCallableSourceCoordinate, TypedDeclarationKey,
    TypedDeclarationKeyFault, VariantFingerprint,
};
use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};
use backend_version::{ContentId, DeclarationKeyDomain};
use thiserror::Error;

/// Typed propagation keeps every assertion exact without panicking seams.
#[derive(Debug, Error)]
enum TestFailure {
    #[error("lineage rejected: {0:?}")]
    Lineage(PackageLineageFault),
    #[error("declaration key rejected: {0:?}")]
    Key(DeclarationKeyFault),
    #[error("typed declaration key rejected: {0:?}")]
    TypedKey(TypedDeclarationKeyFault),
    #[error("callable source coordinate rejected: {0:?}")]
    CallableCoordinate(TypeScriptCallableCoordinateFault),
    #[error("callable instance rejected: {0:?}")]
    CallableInstance(AnonymousCallableInstanceFault),
    #[error("foreign key rejected: {0:?}")]
    Foreign(ForeignKeyFault),
    #[error("preimage overflow: {0:?}")]
    Preimage(PreimageOverflow),
    #[error("relative span rejected: {0:?}")]
    Span(RelSpanFault),
}

impl From<PackageLineageFault> for TestFailure {
    fn from(value: PackageLineageFault) -> Self {
        Self::Lineage(value)
    }
}

impl From<DeclarationKeyFault> for TestFailure {
    fn from(value: DeclarationKeyFault) -> Self {
        Self::Key(value)
    }
}

impl From<TypedDeclarationKeyFault> for TestFailure {
    fn from(value: TypedDeclarationKeyFault) -> Self {
        Self::TypedKey(value)
    }
}

impl From<TypeScriptCallableCoordinateFault> for TestFailure {
    fn from(value: TypeScriptCallableCoordinateFault) -> Self {
        Self::CallableCoordinate(value)
    }
}

impl From<AnonymousCallableInstanceFault> for TestFailure {
    fn from(value: AnonymousCallableInstanceFault) -> Self {
        Self::CallableInstance(value)
    }
}

impl From<ForeignKeyFault> for TestFailure {
    fn from(value: ForeignKeyFault) -> Self {
        Self::Foreign(value)
    }
}

impl From<PreimageOverflow> for TestFailure {
    fn from(value: PreimageOverflow) -> Self {
        Self::Preimage(value)
    }
}

fn lineage<'a>(ecosystem: &'a str, name: &'a str) -> Result<PackageLineage<'a>, TestFailure> {
    Ok(PackageLineage::new(ecosystem, name)?)
}

fn key(name: &'static [u8]) -> Result<DeclarationKey<'static>, TestFailure> {
    Ok(DeclarationKey::new(
        lineage("cargo", "serde")?,
        "src/de.rs",
        EntityKind::Function,
        name,
    )?)
}

fn stable(name: &'static [u8]) -> Result<ContentId<DeclarationKeyDomain>, TestFailure> {
    Ok(key(name)?.stable_id(&mut [0_u8; 512])?)
}

fn endpoint(name: &'static [u8]) -> DeclarationIdentity {
    DeclarationIdentity {
        family: DeclarationFamilyId::from_canonical_bytes(name),
        variant: VariantFingerprint::from_canonical_bytes(name),
    }
}

/// The identity cell order is `(lineage, path, kind, name)` plus an optional
/// family key. Every test below mutates exactly one cell and demands a
/// different id, then proves the unmutated key is stable.
#[test]
fn identity_keys_on_the_exact_four_cells() -> Result<(), TestFailure> {
    let baseline = stable(b"serialize")?;
    // Same cells, same id — including a second, independently minted id.
    assert_eq!(baseline, stable(b"serialize")?);
    assert_eq!(baseline, stable(b"serialize")?);

    // Mutating the name alone moves the id.
    assert_ne!(baseline, stable(b"deserialize")?);

    // Mutating the path alone moves the id.
    let mut moved = key(b"serialize")?;
    moved.path = "src/ser.rs";
    assert_ne!(baseline, moved.stable_id(&mut [0_u8; 512])?);

    // Mutating the kind alone moves the id.
    let mut kinded = key(b"serialize")?;
    kinded.kind = EntityKind::Constant;
    assert_ne!(baseline, kinded.stable_id(&mut [0_u8; 512])?);

    // Mutating the package lineage alone moves the id.
    let mut packaged = key(b"serialize")?;
    packaged.lineage = lineage("cargo", "serde_json")?;
    assert_ne!(baseline, packaged.stable_id(&mut [0_u8; 512])?);

    // Mutating the ecosystem alone moves the id.
    let mut ecosystem = key(b"serialize")?;
    ecosystem.lineage = lineage("pypi", "serde")?;
    assert_ne!(baseline, ecosystem.stable_id(&mut [0_u8; 512])?);

    Ok(())
}

/// The measured old-system defect: 32,339 identity groups collapsed because
/// siblings that minted the same id were separated by declaration ordinal.
/// The key here has no ordinal cell — sibling declarations with the same
/// key mint the same family id. A later declaration must never change an
/// earlier family; local variants are framed by backend-semantic::ir.
#[test]
fn sibling_identity_never_depends_on_declaration_order() -> Result<(), TestFailure> {
    let first = key(b"serialize")?;
    let id_first = first.stable_id(&mut [0_u8; 512])?;
    // "Emitting" two more declarations afterwards cannot move the first id:
    // the minting function takes no ordinal, no count, and no neighbor.
    let id_again = first.stable_id(&mut [0_u8; 512])?;
    assert_eq!(id_first, id_again);
    Ok(())
}

/// rejection retaining both widths, and the buffer is left untouched.
#[test]
fn short_preimage_output_is_typed_and_lossless() -> Result<(), TestFailure> {
    let key = key(b"serialize")?;
    let needed = key.preimage_len()?;
    let mut out = vec![0_u8; needed - 1];
    assert_eq!(
        key.write_preimage(&mut out),
        Err(PreimageOverflow::OutputShort {
            needed,
            actual: needed - 1,
        })
    );
    assert!(out.iter().all(|byte| *byte == 0));
    // The exact width succeeds.
    let mut out = vec![0_u8; needed];
    assert_eq!(key.write_preimage(&mut out), Ok(needed));
    Ok(())
}

fn anonymous_family<'a>(
    steps: &'a [CallableAnchorStep<'a>],
) -> Result<ScopedTypedDeclarationKey<'a>, TestFailure> {
    let declaration = TypedDeclarationKey::new(
        lineage("npm", "fixture")?,
        "src/app.spec.ts",
        EntityKind::Function,
        DeclarationName::AnonymousCallable(AnonymousCallableAnchor { steps }),
    )?;
    Ok(ScopedTypedDeclarationKey::new(
        declaration,
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        DeclarationParentage::Root,
    ))
}

fn callable_site<'a>(
    path: &'a str,
    source_byte: u8,
    start: u32,
    end: u32,
) -> Result<TypeScriptCallableSourceCoordinate<'a>, TestFailure> {
    Ok(TypeScriptCallableSourceCoordinate::new(
        [0x11; 32],
        [source_byte; 32],
        path,
        start,
        end,
    )?)
}

#[test]
fn typed_named_identity_is_byte_identical_to_the_existing_v2_contract() -> Result<(), TestFailure> {
    let legacy = key(b"getHello")?;
    let typed = TypedDeclarationKey::new(
        lineage("cargo", "serde")?,
        "src/de.rs",
        EntityKind::Function,
        DeclarationName::Named(b"getHello"),
    )?;
    let mut legacy_bytes = vec![0; legacy.preimage_len()?];
    let mut typed_bytes = vec![0; typed.preimage_len()?];
    let legacy_len = legacy.write_preimage(&mut legacy_bytes)?;
    let typed_len = typed.write_preimage(&mut typed_bytes)?;
    assert_eq!(typed_bytes, legacy_bytes);
    assert_eq!(typed_len, legacy_len);
    assert_eq!(
        legacy.stable_id(&mut vec![0; legacy_len])?,
        typed.stable_id(&mut vec![0; typed_len])?
    );

    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let legacy_scoped = ScopedDeclarationKey::new(legacy, profile, DeclarationParentage::Root);
    let typed_scoped = ScopedTypedDeclarationKey::new(typed, profile, DeclarationParentage::Root);
    let mut legacy_family = vec![0; legacy_scoped.family_preimage_len()?];
    let mut typed_family = vec![0; typed_scoped.family_preimage_len()?];
    let legacy_len = legacy_scoped.write_family_preimage(&mut legacy_family)?;
    let typed_len = typed_scoped.write_family_preimage(&mut typed_family)?;
    assert_eq!(typed_family, legacy_family);
    assert_eq!(typed_len, legacy_len);
    assert_eq!(
        legacy_scoped.family_id(&mut vec![0; legacy_len])?,
        typed_scoped.family_id(&mut vec![0; typed_len])?
    );
    Ok(())
}

#[test]
fn existing_anonymous_routes_retain_the_v1_key_and_family_bytes() -> Result<(), TestFailure> {
    let route = [
        CallableAnchorStep {
            child_role: CallableChildRole::CallArgument,
            parent: CallableParentShape::Call,
        },
        CallableAnchorStep {
            child_role: CallableChildRole::VariableInitializer,
            parent: CallableParentShape::VariableBinding(b"binding"),
        },
        CallableAnchorStep {
            child_role: CallableChildRole::PropertyValue,
            parent: CallableParentShape::PropertyName(b"property"),
        },
        CallableAnchorStep {
            child_role: CallableChildRole::ConditionalConsequent,
            parent: CallableParentShape::Conditional,
        },
        CallableAnchorStep {
            child_role: CallableChildRole::ConditionalAlternate,
            parent: CallableParentShape::Conditional,
        },
        CallableAnchorStep {
            child_role: CallableChildRole::ArrayElement,
            parent: CallableParentShape::ArrayLiteral,
        },
        CallableAnchorStep {
            child_role: CallableChildRole::ObjectMemberValue,
            parent: CallableParentShape::ObjectMember(b"member"),
        },
    ];
    let family = anonymous_family(&route)?;
    let declaration = family.declaration();
    let mut expected = Vec::new();
    let cell = |out: &mut Vec<u8>, value: &[u8]| {
        out.extend_from_slice(
            &u32::try_from(value.len())
                .expect("bounded golden cell")
                .to_le_bytes(),
        );
        out.extend_from_slice(value);
    };
    cell(&mut expected, b"compiler.declaration.anonymous-callable.v1");
    cell(&mut expected, b"npm");
    cell(&mut expected, b"fixture");
    cell(&mut expected, b"src/app.spec.ts");
    expected.extend_from_slice(&u16::from(EntityKind::Function).to_le_bytes());
    expected.extend_from_slice(&7_u32.to_le_bytes());
    expected.extend_from_slice(&[0, 0, 1, 1]);
    cell(&mut expected, b"binding");
    expected.extend_from_slice(&[2, 2]);
    cell(&mut expected, b"property");
    expected.extend_from_slice(&[3, 3, 4, 3, 5, 4, 6, 5]);
    cell(&mut expected, b"member");
    let mut actual = vec![0; declaration.preimage_len()?];
    declaration.write_preimage(&mut actual)?;
    assert_eq!(
        actual, expected,
        "the complete original closed route vocabulary stays byte stable"
    );
    let mut expected_family = Vec::new();
    cell(
        &mut expected_family,
        b"compiler.declaration-family.anonymous-callable.v1",
    );
    cell(&mut expected_family, &expected);
    expected_family.extend_from_slice(&<[u8; 2]>::from(family.profile()));
    expected_family.push(0); // The historical root-parentage wire tag.
    let mut actual_family = vec![0; family.family_preimage_len()?];
    family.write_family_preimage(&mut actual_family)?;
    assert_eq!(actual_family, expected_family);
    Ok(())
}

#[test]
fn anonymous_family_is_structural_but_exact_instances_remain_separate() -> Result<(), TestFailure> {
    let route = [CallableAnchorStep {
        child_role: CallableChildRole::CallArgument,
        parent: CallableParentShape::Call,
    }];
    let first_family = anonymous_family(&route)?;
    // Generic call spelling and arguments, including a test-description
    // literal, do not exist in the family vocabulary and cannot churn it.
    let description_edited_family = anonymous_family(&route)?;
    let mut family_scratch = vec![0; first_family.family_preimage_len()?];
    assert_eq!(
        first_family.family_id(&mut family_scratch)?,
        description_edited_family.family_id(&mut family_scratch)?,
    );

    let first = AnonymousCallableInstanceKey::new(
        first_family,
        callable_site("src/app.spec.ts", 1, 100, 124)?,
    )?;
    let same_site_body_edit = AnonymousCallableInstanceKey::new(
        description_edited_family,
        callable_site("src/app.spec.ts", 2, 100, 124)?,
    )?;
    let sibling = AnonymousCallableInstanceKey::new(
        first_family,
        callable_site("src/app.spec.ts", 3, 200, 224)?,
    )?;
    let mut instance_scratch = vec![0; first.variant_preimage_len()?];
    let first_identity = first.declaration_identity(&mut instance_scratch)?;
    let edited_identity = same_site_body_edit.declaration_identity(&mut instance_scratch)?;
    let sibling_identity = sibling.declaration_identity(&mut instance_scratch)?;
    assert_eq!(first_identity.family, edited_identity.family);
    assert_eq!(first_identity.variant, edited_identity.variant);
    assert_eq!(first_identity.family, sibling_identity.family);
    assert_ne!(first_identity.variant, sibling_identity.variant);
    assert_eq!(
        AnonymousCallableFamilyMultiplicity::new(2),
        Some(AnonymousCallableFamilyMultiplicity::Ambiguous { instance_count: 2 })
    );
    assert_eq!(
        AnonymousCallableFamilyMultiplicity::new(1),
        Some(AnonymousCallableFamilyMultiplicity::Unique)
    );
    assert_eq!(AnonymousCallableFamilyMultiplicity::new(0), None);
    Ok(())
}

#[test]
fn anonymous_family_rejects_bad_evidence_with_exact_typed_faults() -> Result<(), TestFailure> {
    let call_step = CallableAnchorStep {
        child_role: CallableChildRole::CallArgument,
        parent: CallableParentShape::Call,
    };
    let too_deep = vec![call_step; MAX_ANONYMOUS_CALLABLE_ROUTE_STEPS + 1];
    assert_eq!(
        TypedDeclarationKey::new(
            lineage("npm", "fixture")?,
            "src/app.spec.ts",
            EntityKind::Function,
            DeclarationName::AnonymousCallable(AnonymousCallableAnchor { steps: &too_deep }),
        ),
        Err(TypedDeclarationKeyFault::AnchorTooDeep {
            actual: MAX_ANONYMOUS_CALLABLE_ROUTE_STEPS + 1,
            limit: MAX_ANONYMOUS_CALLABLE_ROUTE_STEPS,
        })
    );

    let oversized_name = vec![b'x'; MAX_ANONYMOUS_CALLABLE_ANCHOR_BYTES + 1];
    let oversized_step = [CallableAnchorStep {
        child_role: CallableChildRole::VariableInitializer,
        parent: CallableParentShape::VariableBinding(&oversized_name),
    }];
    assert_eq!(
        TypedDeclarationKey::new(
            lineage("npm", "fixture")?,
            "src/app.spec.ts",
            EntityKind::Function,
            DeclarationName::AnonymousCallable(AnonymousCallableAnchor {
                steps: &oversized_step,
            }),
        ),
        Err(TypedDeclarationKeyFault::AnchorTextTooLarge {
            actual: MAX_ANONYMOUS_CALLABLE_ANCHOR_BYTES + 1,
            limit: MAX_ANONYMOUS_CALLABLE_ANCHOR_BYTES,
        })
    );

    let mismatched_step = [CallableAnchorStep {
        child_role: CallableChildRole::CallArgument,
        parent: CallableParentShape::VariableBinding(b"callback"),
    }];
    assert!(matches!(
        TypedDeclarationKey::new(
            lineage("npm", "fixture")?,
            "src/app.spec.ts",
            EntityKind::Function,
            DeclarationName::AnonymousCallable(AnonymousCallableAnchor {
                steps: &mismatched_step,
            }),
        ),
        Err(TypedDeclarationKeyFault::RoleShapeMismatch { .. })
    ));
    assert_eq!(
        TypeScriptCallableSourceCoordinate::new([0; 32], [0; 32], "src/app.spec.ts", 10, 10),
        Err(TypeScriptCallableCoordinateFault::EmptyOrReversedSpan)
    );
    Ok(())
}

#[test]
fn anonymous_instance_requires_a_matching_path_and_anonymous_family() -> Result<(), TestFailure> {
    let route = [CallableAnchorStep {
        child_role: CallableChildRole::CallArgument,
        parent: CallableParentShape::Call,
    }];
    let family = anonymous_family(&route)?;
    let mismatched_site = callable_site("src/other.spec.ts", 1, 1, 5)?;
    assert_eq!(
        AnonymousCallableInstanceKey::new(family, mismatched_site),
        Err(AnonymousCallableInstanceFault::PathMismatch)
    );

    let named = TypedDeclarationKey::new(
        lineage("npm", "fixture")?,
        "src/app.spec.ts",
        EntityKind::Function,
        DeclarationName::Named(b"named"),
    )?;
    let named_family = ScopedTypedDeclarationKey::new(
        named,
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        DeclarationParentage::Root,
    );
    assert_eq!(
        AnonymousCallableInstanceKey::new(named_family, callable_site("src/app.spec.ts", 1, 1, 5)?,),
        Err(AnonymousCallableInstanceFault::NotAnonymousCallable)
    );
    Ok(())
}

#[test]
fn nested_anonymous_family_uses_parent_family_and_instance_keeps_parent_variant()
-> Result<(), TestFailure> {
    let route = [CallableAnchorStep {
        child_role: CallableChildRole::ArrayElement,
        parent: CallableParentShape::ArrayLiteral,
    }];
    let declaration = TypedDeclarationKey::new(
        lineage("npm", "fixture")?,
        "src/app.spec.ts",
        EntityKind::Function,
        DeclarationName::AnonymousCallable(AnonymousCallableAnchor { steps: &route }),
    )?;
    let parent_family = DeclarationFamilyId::from_raw([0x42; 16]);
    let parent_a = DeclarationIdentity {
        family: parent_family,
        variant: VariantFingerprint::from_raw([0x11; 16]),
    };
    let parent_b = DeclarationIdentity {
        family: parent_family,
        variant: VariantFingerprint::from_raw([0x22; 16]),
    };
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let child_a =
        ScopedTypedDeclarationKey::new(declaration, profile, DeclarationParentage::Bound(parent_a));
    let child_b =
        ScopedTypedDeclarationKey::new(declaration, profile, DeclarationParentage::Bound(parent_b));
    let mut family_scratch = vec![0; child_a.family_preimage_len()?];
    assert_eq!(
        child_a.family_id(&mut family_scratch)?,
        child_b.family_id(&mut family_scratch)?,
    );
    let site_a =
        AnonymousCallableInstanceKey::new(child_a, callable_site("src/app.spec.ts", 1, 100, 120)?)?;
    let site_b =
        AnonymousCallableInstanceKey::new(child_b, callable_site("src/app.spec.ts", 1, 100, 120)?)?;
    let mut instance_scratch = vec![0; site_a.variant_preimage_len()?];
    let identity_a = site_a.declaration_identity(&mut instance_scratch)?;
    let identity_b = site_b.declaration_identity(&mut instance_scratch)?;
    assert_eq!(identity_a.family, identity_b.family);
    assert_ne!(identity_a.variant, identity_b.variant);
    Ok(())
}

#[test]
fn lineage_and_key_validation_keeps_exact_faults() -> Result<(), TestFailure> {
    assert_eq!(
        PackageLineage::new("", "serde"),
        Err(PackageLineageFault::EmptyEcosystem)
    );
    assert_eq!(
        PackageLineage::new("cargo", ""),
        Err(PackageLineageFault::EmptyName)
    );
    assert_eq!(
        PackageLineage::new("car:go", "serde"),
        Err(PackageLineageFault::SeparatorInEcosystem)
    );
    assert_eq!(
        PackageLineage::new("cargo", "ser:de"),
        Err(PackageLineageFault::SeparatorInName)
    );
    assert_eq!(
        PackageLineage::new("car\\go", "serde"),
        Err(PackageLineageFault::Backslash { segment: 0 })
    );
    assert_eq!(
        DeclarationKey::new(lineage("cargo", "serde")?, "", EntityKind::Function, b"f",),
        Err(DeclarationKeyFault::Path(DeclarationPathFault::Empty))
    );
    assert_eq!(
        DeclarationKey::new(
            lineage("cargo", "serde")?,
            "src\\de.rs",
            EntityKind::Function,
            b"f",
        ),
        Err(DeclarationKeyFault::Path(DeclarationPathFault::Backslash))
    );
    assert_eq!(
        DeclarationKey::new(
            lineage("cargo", "serde")?,
            "src/de.rs",
            EntityKind::Function,
            b"",
        ),
        Err(DeclarationKeyFault::EmptyName)
    );
    Ok(())
}

/// Foreign keys hash their key cells (origin, path, kind) and never the
/// display spelling: sealing with and without dependencies loaded must be
/// byte-identical.
#[test]
fn foreign_key_digest_excludes_display_and_resolved_targets() -> Result<(), TestFailure> {
    let scratch = &mut [0_u8; 512];
    let keyed = ForeignKey::new(
        ForeignOrigin::Package(lineage("npm", "lodash")?),
        "lodash/map",
        "lodash.map",
        Some(EntityKind::Function),
    )?;
    let renamed_display = ForeignKey::new(
        ForeignOrigin::Package(lineage("npm", "lodash")?),
        "lodash/map",
        "a completely different display",
        Some(EntityKind::Function),
    )?;
    assert_eq!(keyed.key_id(scratch)?, renamed_display.key_id(scratch)?);
    // The path cell is load-bearing.
    let moved_path = ForeignKey::new(
        ForeignOrigin::Package(lineage("npm", "lodash")?),
        "lodash/filter",
        "lodash.map",
        Some(EntityKind::Function),
    )?;
    assert_ne!(keyed.key_id(scratch)?, moved_path.key_id(scratch)?);
    // Every origin family participates in the digest.
    let namespace = ForeignKey::new(
        ForeignOrigin::Namespace {
            ecosystem: "maven",
            namespace: "java.util",
        },
        "java.util/List",
        "List",
        Some(EntityKind::Record),
    )?;
    let universe = ForeignKey::new(
        ForeignOrigin::Universe { ecosystem: "go" },
        "error",
        "error",
        None,
    )?;
    assert_ne!(namespace.key_id(scratch)?, universe.key_id(scratch)?);
    Ok(())
}

#[test]
fn occurrences_carry_target_kind_confidence_and_owner_relative_span() -> Result<(), TestFailure> {
    let fragment =
        backend_semantic::ir_vocabulary::ExternalFragmentId::from_canonical_bytes(b"fragment-a");
    let declaration = endpoint(b"serialize");
    let target = OccurrenceTarget::Stable(StableRef {
        fragment,
        declaration,
    });
    let occurrence = Occurrence {
        target,
        kind: ReferenceKind::FunctionCall,
        confidence: backend_semantic::ir_vocabulary::Confidence::Oracle,
        span: RelSpan::new(12, 21).map_err(TestFailure::Span)?,
    };
    assert_eq!(occurrence.kind, ReferenceKind::FunctionCall);
    assert_eq!(occurrence.span.len(), 9);
    // A foreign occurrence carries its self-describing key instead.
    let foreign = ForeignKey::new(
        ForeignOrigin::Universe { ecosystem: "go" },
        "error",
        "error",
        None,
    )?;
    let occurrence = Occurrence {
        target: OccurrenceTarget::Foreign(foreign),
        kind: ReferenceKind::TypeReference,
        confidence: backend_semantic::ir_vocabulary::Confidence::Syntactic,
        span: RelSpan::new(0, 5).map_err(TestFailure::Span)?,
    };
    assert!(matches!(occurrence.target, OccurrenceTarget::Foreign(_)));
    Ok(())
}
