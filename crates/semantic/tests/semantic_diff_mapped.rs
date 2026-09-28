#![cfg(feature = "mmap")]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use backend_semantic::ir::GenerationId;
use backend_semantic::ir::{
    BorrowedTree, BuiltinType, ConcreteType, Confidence, CorePayloadHash, DeclarationFamilyId,
    DeclarationIdentity, Delta, EntityAuthorityFacts, EntityVersion, FactAvailability, Ir,
    IrBuilder, ItemKind, Link, LinkKind, MappedSemanticImage, OccurrenceAuthorityFacts,
    PackageLineage, SemanticDiff, SemanticEntityChange, SemanticImageIdentity,
    SemanticLinkChangeKind, SemanticReader, SemanticSnapshot, SourceIdentity, StableLinkKey,
    TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget, VariantFingerprint, Visibility,
    encode_full_semantic_image, full_semantic_image_len, load_semantic_image_mmap,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage,
};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

static NEXT_IMAGE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Eq, PartialEq)]
enum EntityDelta {
    Introduced(DeclarationIdentity),
    Deleted(DeclarationIdentity),
    Retained {
        identity: DeclarationIdentity,
        core_payload: Delta<CorePayloadHash>,
        parent: Delta<Option<DeclarationIdentity>>,
        facets: backend_semantic::ir::EntityFacetChanges,
    },
}

#[derive(Debug, Eq, PartialEq)]
struct LinkDelta {
    key: StableLinkKey,
    kind: SemanticLinkChangeKind,
    before: Option<Link>,
    after: Option<Link>,
}

#[derive(Debug, Eq, PartialEq)]
struct DiffSummary {
    provenance: backend_semantic::ir::FacetChange,
    entities: Vec<EntityDelta>,
    links: Vec<LinkDelta>,
}

struct TempImage(PathBuf);

impl TempImage {
    fn write(bytes: &[u8]) -> Self {
        let serial = NEXT_IMAGE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "backend-semantic-diff-mapped-{}-{serial}.nxf",
            std::process::id()
        ));
        fs::write(&path, bytes).expect("write canonical semantic image");
        Self(path)
    }

    fn open(
        &self,
        bytes: &[u8],
    ) -> Result<MappedSemanticImage, backend_semantic::ir::MappedSemanticImageError> {
        let identity = SemanticImageIdentity::from_encoded_bytes(bytes);
        let generation = GenerationId::from_canonical_bytes(bytes);
        load_semantic_image_mmap(&self.0, identity, generation, bytes.len())
    }
}

impl Drop for TempImage {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn version(family: u8, body: &[u8]) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([family; 16]),
        variant: VariantFingerprint::from_raw([family.wrapping_add(32); 16]),
        core_payload: CorePayloadHash::from_canonical_bytes(body),
    }
}

fn encode(ir: &Ir) -> Vec<u8> {
    let length = full_semantic_image_len(ir).expect("canonical image length");
    let mut bytes = vec![0; length];
    encode_full_semantic_image(ir, &mut bytes).expect("canonical image encoding");
    bytes
}

fn semantic_image(
    source_bytes: &[u8],
    after: bool,
) -> Result<Ir, backend_semantic::ir::BuildError> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
        byte_len: u32::try_from(source_bytes.len()).expect("short source fixture"),
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"semantic-diff-toolchain"),
    );
    let lineage = PackageLineage::new("cargo", "semantic-diff-fixture")
        .expect("fixed package lineage is valid");
    let mut builder = IrBuilder::new();
    builder.set_image_provenance(source, recipe, lineage, "src/lib.rs")?;

    // Keep the function's stable declaration identity and change its retained
    // canonical core-payload digest between generations.
    let body_type = builder
        .intern_concrete(ConcreteType::Builtin(if after {
            BuiltinType::U64
        } else {
            BuiltinType::I32
        }))?
        .erase();
    let versions = if after {
        [
            version(1, b"unchanged body"),
            version(2, b"body generation 2"),
            version(4, b"new function body"),
            version(5, b"stable target body"),
        ]
    } else {
        [
            version(1, b"unchanged body"),
            version(2, b"body generation 1"),
            version(3, b"deleted function body"),
            version(5, b"stable target body"),
        ]
    };
    let items = [
        TreeItemInput {
            name: b"unchanged",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"changed-body",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                semantic_type: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: Some(body_type),
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: if after { b"added" } else { b"deleted" },
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"stable-target",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
    ];
    let relation_target = if after {
        TreeEntityId::new(2)
    } else {
        TreeEntityId::new(3)
    };
    let links = [TreeLinkInput {
        from: TreeEntityId::new(0),
        target: TreeLinkTarget::Local(relation_target),
        kind: LinkKind::Calls,
        confidence: Confidence::Compiler,
        authority: OccurrenceAuthorityFacts::default(),
        source: None,
    }];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &links,
    })?;
    builder.finish()
}

fn summarize<Before: SemanticReader, After: SemanticReader>(
    before_generation: GenerationId,
    before: &Before,
    after_generation: GenerationId,
    after: &After,
) -> DiffSummary {
    let diff = SemanticDiff::between(
        SemanticSnapshot {
            generation: before_generation,
            reader: before,
        },
        SemanticSnapshot {
            generation: after_generation,
            reader: after,
        },
    );
    let provenance = diff.entities.provenance;
    let entities = diff
        .entities
        .map(|change| match change {
            SemanticEntityChange::Introduced { identity, .. } => EntityDelta::Introduced(identity),
            SemanticEntityChange::Deleted { identity, .. } => EntityDelta::Deleted(identity),
            SemanticEntityChange::Retained {
                identity,
                core_payload,
                parent,
                facets,
                ..
            } => EntityDelta::Retained {
                identity,
                core_payload,
                parent,
                facets,
            },
        })
        .collect();
    let links = diff
        .links
        .map(|change| LinkDelta {
            key: change.key,
            kind: change.kind,
            before: change.before.map(|link| link.evidence),
            after: change.after.map(|link| link.evidence),
        })
        .collect();
    DiffSummary {
        provenance,
        entities,
        links,
    }
}

#[test]
fn mapped_semantic_diff_matches_the_owned_reader_across_generations() {
    let before_ir =
        semantic_image(b"source generation one", false).expect("first canonical IR builds");
    let after_ir =
        semantic_image(b"source generation two", true).expect("second canonical IR builds");
    let before_bytes = encode(&before_ir);
    let after_bytes = encode(&after_ir);
    let before_generation = GenerationId::from_canonical_bytes(&before_bytes);
    let after_generation = GenerationId::from_canonical_bytes(&after_bytes);
    assert_ne!(before_generation, after_generation);

    let expected = summarize(before_generation, &before_ir, after_generation, &after_ir);
    assert_eq!(
        expected.provenance.comparison,
        backend_semantic::ir::FacetComparison::Changed
    );
    assert_eq!(
        expected.entities.len(),
        3,
        "only edited, added, and deleted declarations diff"
    );
    assert_eq!(
        expected.links.len(),
        2,
        "the relation target change removes and adds one edge"
    );
    assert!(expected.entities.iter().any(|change| matches!(
        change,
        EntityDelta::Retained {
            identity,
            core_payload: Delta::Changed { .. },
            ..
        } if identity.family == DeclarationFamilyId::from_raw([2; 16])
    )));
    assert!(expected.entities.iter().any(|change| matches!(
        change,
        EntityDelta::Introduced(identity)
            if identity.family == DeclarationFamilyId::from_raw([4; 16])
    )));
    assert!(expected.entities.iter().any(|change| matches!(
        change,
        EntityDelta::Deleted(identity)
            if identity.family == DeclarationFamilyId::from_raw([3; 16])
    )));
    assert!(
        !expected.entities.iter().any(|change| match change {
            EntityDelta::Introduced(identity)
            | EntityDelta::Deleted(identity)
            | EntityDelta::Retained { identity, .. } => {
                identity.family == DeclarationFamilyId::from_raw([1; 16])
            }
        }),
        "the unchanged declaration must be absent from the delta"
    );
    assert!(expected.links.iter().any(|change| {
        change.kind == SemanticLinkChangeKind::Removed
            && change.before.is_some()
            && change.after.is_none()
    }));
    assert!(expected.links.iter().any(|change| {
        change.kind == SemanticLinkChangeKind::Added
            && change.before.is_none()
            && change.after.is_some()
    }));

    let before_file = TempImage::write(&before_bytes);
    let after_file = TempImage::write(&after_bytes);
    let before_mapped = before_file
        .open(&before_bytes)
        .expect("first image maps and validates");
    let after_mapped = after_file
        .open(&after_bytes)
        .expect("second image maps and validates");
    let before_view = before_mapped.view();
    let after_view = after_mapped.view();
    let actual = summarize(
        before_mapped.generation(),
        &before_view,
        after_mapped.generation(),
        &after_view,
    );

    assert_eq!(actual, expected);

    let owned_noop = summarize(before_generation, &before_ir, before_generation, &before_ir);
    let mapped_noop = summarize(
        before_generation,
        &before_view,
        before_generation,
        &before_view,
    );
    assert_eq!(mapped_noop, owned_noop);
    assert_eq!(
        mapped_noop.provenance.comparison,
        backend_semantic::ir::FacetComparison::Unchanged
    );
    assert!(mapped_noop.entities.is_empty());
    assert!(mapped_noop.links.is_empty());
}
