//! Java namespace enum-constant reads join through `join_project_value`.

use super::{
    ProjectCallableIndex, compiled_source_path, join_project_value, query_semantic_id,
    semantic_coordinate,
};
use backend_engine::{RowId, package_key};
use backend_extension_trustfall::{
    CompilerSemanticEvidence, PackageScopeEvidence, SemanticQueryCancellation,
    SemanticQueryCorpus, SemanticQueryEvent, SemanticQueryEvidence, SemanticQueryFact,
    SemanticQueryPresentation, SemanticQueryRequest, execute_semantic_query,
};
use backend_semantic::ir::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity, EntityAuthorityFacts,
    EntityVersion, ExternalDeclarationIdentity, ExternalId, ExternalTarget, FactAvailability,
    ForeignDeclarationId, ForeignExternalTarget, ForeignTargetOrigin, IrBuilder, ItemKind, LinkKind,
    LinkTarget, OccurrenceAuthorityFacts, ParentageAuthority, SemanticCoreReader, SemanticImageView,
    SemanticReader, SourceIdentity, TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget,
    VariantAvailability, VariantFingerprint, Visibility, encode_full_semantic_image,
    full_semantic_image_len,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use futures_util::StreamExt as _;
use std::collections::{BTreeMap, BTreeSet};

const ENUM_NAMESPACE: &str = "demo.Status";

struct NamespaceValueFixture {
    foreign_key: u8,
}

fn fixture_version(identity: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([identity; 16]),
        variant: VariantFingerprint::from_raw([identity; 16]),
        core_payload: CorePayloadHash::from_raw([identity; 16]),
    }
}

fn project_paths(files: &[&str]) -> BTreeSet<String> {
    files.iter().map(|path| (*path).to_owned()).collect()
}

fn status_enum_image() -> Result<Vec<u8>, String> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[1]),
        byte_len: 12,
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
    );
    let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
        .map_err(|error| format!("fixture coordinate: {error:?}"))?;
    let mut builder = IrBuilder::new();
    builder
        .set_image_provenance_for_package(source, recipe, &coordinate, "demo/Status.java")
        .map_err(|error| error.to_string())?;
    let authority = |parentage| EntityAuthorityFacts {
        parentage,
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let versions = [
        fixture_version(20),
        fixture_version(21),
        fixture_version(22),
    ];
    let items = [
        TreeItemInput {
            name: b"demo",
            kind: ItemKind::Module,
            visibility: Visibility::Public,
            authority: authority(ParentageAuthority::Root),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: ENUM_NAMESPACE.as_bytes(),
            kind: ItemKind::Enum,
            visibility: Visibility::Public,
            authority: authority(ParentageAuthority::Bound(versions[0].identity())),
            parent: Some(TreeEntityId::new(0)),
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"Active",
            kind: ItemKind::Variant,
            visibility: Visibility::Public,
            authority: authority(ParentageAuthority::Bound(versions[1].identity())),
            parent: Some(TreeEntityId::new(1)),
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
    ];
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })
        .map_err(|error| error.to_string())?;
    let ir = builder.finish().map_err(|error| error.to_string())?;
    let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
    encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn drive_enum_read_image(foreign_read: NamespaceValueFixture) -> Result<Vec<u8>, String> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[2]),
        byte_len: 12,
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture toolchain"),
    );
    let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
        .map_err(|error| format!("fixture coordinate: {error:?}"))?;
    let mut builder = IrBuilder::new();
    builder
        .set_image_provenance_for_package(source, recipe, &coordinate, "demo/Drive.java")
        .map_err(|error| error.to_string())?;
    let authority = |parentage| EntityAuthorityFacts {
        parentage,
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let items = [TreeItemInput {
        name: b"Drive",
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: authority(ParentageAuthority::Root),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    }];
    let ecosystem = builder.intern_atom(b"maven").map_err(|e| e.to_string())?;
    let namespace = builder
        .intern_atom(ENUM_NAMESPACE.as_bytes())
        .map_err(|e| e.to_string())?;
    let display = builder.intern_atom(b"Active").map_err(|e| e.to_string())?;
    let path_atom = builder.intern_atom(b"Active").map_err(|e| e.to_string())?;
    let external = builder
        .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
            identity: ExternalDeclarationIdentity {
                foreign: ForeignDeclarationId::from_raw([foreign_read.foreign_key; 16]),
                variant: VariantAvailability::Unavailable,
            },
            origin: ForeignTargetOrigin::Namespace {
                ecosystem,
                namespace,
            },
            path: path_atom,
            display,
            kind: Some(ItemKind::Variant),
        }))
        .map_err(|e| e.to_string())?;
    let links = [TreeLinkInput {
        from: TreeEntityId::new(0),
        target: TreeLinkTarget::External(external),
        kind: LinkKind::Reads,
        confidence: backend_semantic::ir::Confidence::Compiler,
        authority: OccurrenceAuthorityFacts {
            source: FactAvailability::Unavailable,
        },
        source: None,
    }];
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &[fixture_version(23)],
            items: &items,
            links: &links,
        })
        .map_err(|error| error.to_string())?;
    let ir = builder.finish().map_err(|error| error.to_string())?;
    let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
    encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn java_active_drive_fixture(
    foreign_key: u8,
) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
    Ok((
        status_enum_image()?,
        drive_enum_read_image(NamespaceValueFixture { foreign_key })?,
        fixture_version(22).identity(),
        fixture_version(23).identity(),
    ))
}

fn foreign_value_read_from_caller(caller_bytes: &[u8]) -> Result<(ExternalId, LinkKind, String), String> {
    let image = SemanticImageView::reopen(caller_bytes).map_err(|error| error.to_string())?;
    let caller_path = compiled_source_path(&image).map_err(|error| error.to_string())?;
    for (_, link) in image.links_from(backend_semantic::ir::EntityId::new(0)) {
        if link.kind == LinkKind::Reads {
            if let LinkTarget::External(external) = link.target {
                return Ok((external, link.kind, caller_path));
            }
        }
    }
    Err("caller fixture has no foreign enum constant read".to_owned())
}

fn compiler_query_presentation(
    package: backend_engine::PackageKey,
    label: &str,
    image_bytes: &[u8],
    identity: DeclarationIdentity,
    name: &str,
    related: Box<[String]>,
) -> Result<(SemanticQueryFact, String), String> {
    let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
        .map_err(|error| format!("fixture coordinate: {error:?}"))?;
    let image = SemanticImageView::reopen(image_bytes).map_err(|error| error.to_string())?;
    let image_digest = *blake3::hash(image_bytes).as_bytes();
    let evidence = CompilerSemanticEvidence::new(
        package,
        coordinate,
        LanguageProfile::Rust(RustEdition::Rust2024),
        identity,
        image_digest,
        image.image_facts(),
    );
    let id = evidence.row_id();
    Ok((
        SemanticQueryFact::new(
            SemanticQueryEvidence::Compiler(evidence),
            SemanticQueryPresentation {
                id: id.clone(),
                kind: if name == "Active" {
                    "variant".to_owned()
                } else {
                    "function".to_owned()
                },
                coordinate: semantic_coordinate(label, identity, name),
                name: name.to_owned(),
                signature: None,
                documentation: String::new(),
                score: None,
                project: Some(RowId::Package(package).stable_key()),
                parent: None,
                related,
            },
        ),
        id,
    ))
}

#[test]
fn join_project_value_java_enum_constant_retargets_active() -> Result<(), String> {
    let (status_bytes, caller_bytes, active_identity, _) = java_active_drive_fixture(91)?;
    let paths = project_paths(&["demo/Status.java", "demo/Drive.java"]);
    let images = [&status_bytes[..], &caller_bytes[..]];
    let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
    let published = BTreeSet::from([active_identity, fixture_version(23).identity()]);
    let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
    let caller_image =
        SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
    let joined = join_project_value(
        &caller_image,
        link_kind,
        external,
        &caller_path,
        &paths,
        &index,
        &published,
    )
    .map_err(|error| error.to_string())?;
    if joined != Some(active_identity) {
        return Err(format!(
            "join_project_value should retarget to Active, got {joined:?}"
        ));
    }
    Ok(())
}

#[test]
fn join_project_value_java_enum_constant_referenced_by_names_drive() -> Result<(), String> {
    let package = package_key("fixture");
    let (status_bytes, caller_bytes, active_identity, drive_identity) =
        java_active_drive_fixture(92)?;
    let paths = project_paths(&["demo/Status.java", "demo/Drive.java"]);
    let images = [&status_bytes[..], &caller_bytes[..]];
    let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
    let published = BTreeSet::from([active_identity, drive_identity]);
    let (external, link_kind, caller_path) = foreign_value_read_from_caller(&caller_bytes)?;
    let caller_image =
        SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
    let joined = join_project_value(
        &caller_image,
        link_kind,
        external,
        &caller_path,
        &paths,
        &index,
        &published,
    )
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "join_project_value returned None".to_owned())?;
    let active_id = query_semantic_id(package, joined);
    let (active_fact, _) = compiler_query_presentation(
        package,
        "fixture",
        &status_bytes,
        active_identity,
        "Active",
        Box::new([]),
    )?;
    let (drive_fact, _) = compiler_query_presentation(
        package,
        "fixture",
        &caller_bytes,
        drive_identity,
        "Drive",
        vec![active_id.clone()].into_boxed_slice(),
    )?;
    let workspace = super::super::genesis().map_err(|error| error.to_string())?;
    let corpus = SemanticQueryCorpus::admit(
        workspace.root(),
        vec![
            SemanticQueryFact::new(
                SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                SemanticQueryPresentation {
                    id: RowId::Package(package).stable_key(),
                    kind: "project".to_owned(),
                    coordinate: "fixture".to_owned(),
                    name: "fixture".to_owned(),
                    signature: None,
                    documentation: String::new(),
                    score: None,
                    project: None,
                    parent: None,
                    related: Box::new([]),
                },
            ),
            active_fact,
            drive_fact,
        ],
    )
    .map_err(|error| error.to_string())?;
    let (cancellation, _) = SemanticQueryCancellation::new();
    let request = SemanticQueryRequest::admit_page(
        corpus,
        "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
        BTreeMap::from([("name".to_owned(), "Active".into())]),
        0,
        8,
        cancellation,
    )
    .map_err(|error| error.to_string())?;
    let events = futures_executor::block_on(
        execute_semantic_query(request)
            .map_err(|error| error.to_string())?
            .collect::<Vec<_>>(),
    );
    let callers = events
        .iter()
        .filter_map(|event| match event {
            SemanticQueryEvent::Row(row) => row.row().get("name").cloned(),
            SemanticQueryEvent::Terminal(_) => None,
        })
        .collect::<Vec<_>>();
    if callers != ["Drive".into()] {
        return Err(format!(
            "referencedBy on Active should name only Drive, got {callers:?}"
        ));
    }
    Ok(())
}
