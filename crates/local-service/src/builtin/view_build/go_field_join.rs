//! Go cross-file field reads join through the module import path.

use super::{
    compiled_source_path, join_project_field, query_semantic_id, semantic_coordinate, semantic_symbol,
    ProjectCallableIndex,
};
use backend_extension_trustfall::{
    CompilerSemanticEvidence, PackageScopeEvidence, SemanticQueryCancellation,
    SemanticQueryCorpus, SemanticQueryEvent, SemanticQueryEvidence, SemanticQueryFact,
    SemanticQueryPresentation, SemanticQueryRequest, execute_semantic_query,
};
use backend_engine::{RowId, package_key};
use backend_semantic::ir::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity, EntityAuthorityFacts,
    EntityVersion, ExternalDeclarationIdentity, ExternalId, ExternalTarget, FactAvailability,
    ForeignDeclarationId, ForeignExternalTarget, ForeignTargetOrigin, IrBuilder, ItemKind,
    LinkKind, LinkTarget, OccurrenceAuthorityFacts, ParentageAuthority, SemanticImageView,
    SourceIdentity, TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget,
    VariantAvailability, VariantFingerprint, Visibility, encode_full_semantic_image,
    full_semantic_image_len, SemanticCoreReader, SemanticReader,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use futures_util::StreamExt as _;
use std::collections::{BTreeMap, BTreeSet};

const MODULE_PATH: &str = "example.com/demo";

struct ForeignFieldFixture {
    foreign_key: u8,
}

fn fixture_version(identity: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([identity; 16]),
        variant: VariantFingerprint::from_raw([identity; 16]),
        core_payload: CorePayloadHash::from_raw([identity; 16]),
    }
}

fn project_paths(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

fn project_item_image(
    path: &str,
    source_identity_byte: u8,
    item_name: &[u8],
    entity_id: TreeEntityId,
    item_kind: ItemKind,
    foreign_read: Option<ForeignFieldFixture>,
) -> Result<Vec<u8>, String> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&[source_identity_byte]),
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
        .set_image_provenance_for_package(source, recipe, &coordinate, path)
        .map_err(|error| error.to_string())?;
    let authority = |parentage| EntityAuthorityFacts {
        parentage,
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let items = [TreeItemInput {
        name: item_name,
        kind: item_kind,
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
    let mut links = Vec::new();
    if let Some(foreign_read) = foreign_read {
        let ecosystem = builder.intern_atom(b"go").map_err(|e| e.to_string())?;
        let package_atom = builder
            .intern_atom(MODULE_PATH.as_bytes())
            .map_err(|e| e.to_string())?;
        let display = builder.intern_atom(b"Note").map_err(|e| e.to_string())?;
        let path_atom = builder.intern_atom(b"Note").map_err(|e| e.to_string())?;
        let external = builder
            .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                identity: ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw([foreign_read.foreign_key; 16]),
                    variant: VariantAvailability::Unavailable,
                },
                origin: ForeignTargetOrigin::Package {
                    ecosystem,
                    package: package_atom,
                },
                path: path_atom,
                display,
                kind: Some(ItemKind::Field),
            }))
            .map_err(|e| e.to_string())?;
        links.push(TreeLinkInput {
            from: entity_id,
            target: TreeLinkTarget::External(external),
            kind: LinkKind::Reads,
            confidence: backend_semantic::ir::Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Unavailable,
            },
            source: None,
        });
    }
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &[fixture_version(source_identity_byte)],
            items: &items,
            links: &links,
        })
        .map_err(|error| error.to_string())?;
    let ir = builder.finish().map_err(|error| error.to_string())?;
    let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
    encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn go_note_drive_fixture(
    foreign_key: u8,
) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
    let service_bytes = project_item_image(
        "example.com/demo/service.go",
        1,
        b"Note",
        TreeEntityId::new(0),
        ItemKind::Field,
        None,
    )?;
    let caller_bytes = project_item_image(
        "example.com/demo/lib.go",
        2,
        b"Drive",
        TreeEntityId::new(0),
        ItemKind::Function,
        Some(ForeignFieldFixture { foreign_key }),
    )?;
    Ok((
        service_bytes,
        caller_bytes,
        fixture_version(1).identity(),
        fixture_version(2).identity(),
    ))
}

fn foreign_field_read_from_caller(
    caller_bytes: &[u8],
) -> Result<(ExternalId, LinkKind, String), String> {
    let image = SemanticImageView::reopen(caller_bytes).map_err(|error| error.to_string())?;
    let caller_path = compiled_source_path(&image).map_err(|error| error.to_string())?;
    for (_, link) in image.links_from(backend_semantic::ir::EntityId::new(0)) {
        if matches!(link.kind, LinkKind::Reads) {
            if let LinkTarget::External(external) = link.target {
                return Ok((external, link.kind, caller_path));
            }
        }
    }
    Err("caller fixture has no foreign field read".to_owned())
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
                kind: if name == "Note" {
                    "field".to_owned()
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
fn join_project_field_go_field_retargets_note() -> Result<(), String> {
    let (service_bytes, caller_bytes, note_identity, _) = go_note_drive_fixture(71)?;
    let paths = project_paths(&["example.com/demo/service.go", "example.com/demo/lib.go"]);
    let images = [&service_bytes[..], &caller_bytes[..]];
    let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
    let published = BTreeSet::from([note_identity, fixture_version(2).identity()]);
    let (external, link_kind, caller_path) = foreign_field_read_from_caller(&caller_bytes)?;
    let caller_image =
        SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
    let joined = join_project_field(
        &caller_image,
        link_kind,
        external,
        &caller_path,
        &paths,
        &index,
        &published,
    )
    .map_err(|error| error.to_string())?;
    if joined != Some(note_identity) {
        return Err(format!(
            "join_project_field should retarget to Note, got {joined:?}"
        ));
    }
    Ok(())
}

#[test]
fn join_project_field_go_query_corpus_referenced_by_names_drive() -> Result<(), String> {
    let package = package_key("fixture");
    let (service_bytes, caller_bytes, note_identity, drive_identity) =
        go_note_drive_fixture(72)?;
    let paths = project_paths(&["example.com/demo/service.go", "example.com/demo/lib.go"]);
    let images = [&service_bytes[..], &caller_bytes[..]];
    let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
    let published = BTreeSet::from([note_identity, drive_identity]);
    let (external, link_kind, caller_path) = foreign_field_read_from_caller(&caller_bytes)?;
    let caller_image =
        SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
    let joined = join_project_field(
        &caller_image,
        link_kind,
        external,
        &caller_path,
        &paths,
        &index,
        &published,
    )
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "join_project_field returned None".to_owned())?;
    let note_id = query_semantic_id(package, joined);
    let (note_fact, _) = compiler_query_presentation(
        package,
        "fixture",
        &service_bytes,
        note_identity,
        "Note",
        Box::new([]),
    )?;
    let (drive_fact, _) = compiler_query_presentation(
        package,
        "fixture",
        &caller_bytes,
        drive_identity,
        "Drive",
        vec![note_id.clone()].into_boxed_slice(),
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
            note_fact,
            drive_fact,
        ],
    )
    .map_err(|error| error.to_string())?;
    let (cancellation, _) = SemanticQueryCancellation::new();
    let request = SemanticQueryRequest::admit_page(
        corpus,
        "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
        BTreeMap::from([("name".to_owned(), "Note".into())]),
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
            "referencedBy on Note should name only Drive, got {callers:?}"
        ));
    }
    Ok(())
}
