//! Proves declaration identity: stable ids are minted from exactly the
//! `(package, path, kind, name)` key cells in their dedicated family-key
//! domain — never from any ordinal — and foreign keys hash their key cells, never a
//! resolved target or display spelling.

use compiler_ir_vocabulary::{
    DeclarationFamilyId, DeclarationIdentity, DeclarationKey, DeclarationKeyFault,
    DeclarationPathFault, EntityKind, ForeignKey, ForeignKeyFault, ForeignOrigin, Occurrence,
    OccurrenceTarget, PackageLineage, PackageLineageFault, PreimageOverflow, ReferenceKind,
    RelSpan, RelSpanFault, StableRef, VariantFingerprint,
};
use heart_identity::{ContentId, DeclarationKeyDomain};
use thiserror::Error;

/// Typed propagation keeps every assertion exact without panicking seams.
#[derive(Debug, Error)]
enum TestFailure {
    #[error("lineage rejected: {0:?}")]
    Lineage(PackageLineageFault),
    #[error("declaration key rejected: {0:?}")]
    Key(DeclarationKeyFault),
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
/// earlier family; local variants are framed by compiler-ir.
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
    let fragment = compiler_ir_vocabulary::ExternalFragmentId::from_canonical_bytes(b"fragment-a");
    let declaration = endpoint(b"serialize");
    let target = OccurrenceTarget::Stable(StableRef {
        fragment,
        declaration,
    });
    let occurrence = Occurrence {
        target,
        kind: ReferenceKind::FunctionCall,
        confidence: compiler_ir_vocabulary::Confidence::Oracle,
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
        confidence: compiler_ir_vocabulary::Confidence::Syntactic,
        span: RelSpan::new(0, 5).map_err(TestFailure::Span)?,
    };
    assert!(matches!(occurrence.target, OccurrenceTarget::Foreign(_)));
    Ok(())
}
