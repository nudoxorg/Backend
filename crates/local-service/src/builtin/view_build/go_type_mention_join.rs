//! Go cross-file type mentions join through the module import path.

use super::{
    compiled_source_path, join_project_mention, query_semantic_id, semantic_coordinate,
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

struct ForeignMentionFixture {
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
    foreign_mention: Option<ForeignMentionFixture>,
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
    if let Some(foreign_mention) = foreign_mention {
        let ecosystem = builder.intern_atom(b"go").map_err(|e| e.to_string())?;
        let package_atom = builder
            .intern_atom(MODULE_PATH.as_bytes())
            .map_err(|e| e.to_string())?;
        let display = builder.intern_atom(b"Workout").map_err(|e| e.to_string())?;
        let path_atom = builder.intern_atom(b"Workout").map_err(|e| e.to_string())?;
        let external = builder
            .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                identity: ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw([foreign_mention.foreign_key; 16]),
                    variant: VariantAvailability::Unavailable,
                },
                origin: ForeignTargetOrigin::Package {
                    ecosystem,
                    package: package_atom,
                },
                path: path_atom,
                display,
                kind: Some(ItemKind::Record),
            }))
            .map_err(|e| e.to_string())?;
        links.push(TreeLinkInput {
            from: entity_id,
            target: TreeLinkTarget::External(external),
            kind: LinkKind::TypeReference,
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

fn go_workout_drive_fixture(
    foreign_key: u8,
) -> Result<(Vec<u8>, Vec<u8>, DeclarationIdentity, DeclarationIdentity), String> {
    let service_bytes = project_item_image(
        "example.com/demo/service.go",
        1,
        b"Workout",
        TreeEntityId::new(0),
        ItemKind::Record,
        None,
    )?;
    let caller_bytes = project_item_image(
        "example.com/demo/lib.go",
        2,
        b"Drive",
        TreeEntityId::new(0),
        ItemKind::Function,
        Some(ForeignMentionFixture { foreign_key }),
    )?;
    Ok((
        service_bytes,
        caller_bytes,
        fixture_version(1).identity(),
        fixture_version(2).identity(),
    ))
}

fn foreign_mention_from_caller(
    caller_bytes: &[u8],
) -> Result<(ExternalId, LinkKind, String), String> {
    let image = SemanticImageView::reopen(caller_bytes).map_err(|error| error.to_string())?;
    let caller_path = compiled_source_path(&image).map_err(|error| error.to_string())?;
    for (_, link) in image.links_from(backend_semantic::ir::EntityId::new(0)) {
        if matches!(link.kind, LinkKind::TypeReference) {
            if let LinkTarget::External(external) = link.target {
                return Ok((external, link.kind, caller_path));
            }
        }
    }
    Err("caller fixture has no foreign type mention".to_owned())
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
                kind: if name == "Workout" {
                    "record".to_owned()
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
fn join_project_mention_go_record_retargets_workout() -> Result<(), String> {
    let (service_bytes, caller_bytes, workout_identity, _) = go_workout_drive_fixture(81)?;
    let paths = project_paths(&["example.com/demo/service.go", "example.com/demo/lib.go"]);
    let images = [&service_bytes[..], &caller_bytes[..]];
    let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
    let published = BTreeSet::from([workout_identity, fixture_version(2).identity()]);
    let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
    let caller_image =
        SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
    let joined = join_project_mention(
        &caller_image,
        link_kind,
        external,
        &caller_path,
        &paths,
        &index,
        &published,
    )
    .map_err(|error| error.to_string())?;
    if joined != Some(workout_identity) {
        return Err(format!(
            "join_project_mention should retarget to Workout, got {joined:?}"
        ));
    }
    Ok(())
}

#[test]
fn join_project_mention_go_query_corpus_referenced_by_names_drive() -> Result<(), String> {
    let package = package_key("fixture");
    let (service_bytes, caller_bytes, workout_identity, drive_identity) =
        go_workout_drive_fixture(82)?;
    let paths = project_paths(&["example.com/demo/service.go", "example.com/demo/lib.go"]);
    let images = [&service_bytes[..], &caller_bytes[..]];
    let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
    let published = BTreeSet::from([workout_identity, drive_identity]);
    let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
    let caller_image =
        SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
    let joined = join_project_mention(
        &caller_image,
        link_kind,
        external,
        &caller_path,
        &paths,
        &index,
        &published,
    )
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "join_project_mention returned None".to_owned())?;
    let workout_id = query_semantic_id(package, joined);
    let (workout_fact, _) = compiler_query_presentation(
        package,
        "fixture",
        &service_bytes,
        workout_identity,
        "Workout",
        Box::new([]),
    )?;
    let (drive_fact, _) = compiler_query_presentation(
        package,
        "fixture",
        &caller_bytes,
        drive_identity,
        "Drive",
        vec![workout_id.clone()].into_boxed_slice(),
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
            workout_fact,
            drive_fact,
        ],
    )
    .map_err(|error| error.to_string())?;
    let (cancellation, _) = SemanticQueryCancellation::new();
    let request = SemanticQueryRequest::admit_page(
        corpus,
        "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
        BTreeMap::from([("name".to_owned(), "Workout".into())]),
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
            "referencedBy on Workout should name only Drive, got {callers:?}"
        ));
    }
    Ok(())
}

#[test]
fn join_project_mention_ambiguous_go_service_paths_returns_none() -> Result<(), String> {
    let service_bytes = project_item_image(
        "example.com/demo/service.go",
        1,
        b"Workout",
        TreeEntityId::new(0),
        ItemKind::Record,
        None,
    )?;
    let duplicate_service_bytes = project_item_image(
        "example.com/demo/other.go",
        3,
        b"Workout",
        TreeEntityId::new(0),
        ItemKind::Record,
        None,
    )?;
    let caller_bytes = project_item_image(
        "example.com/demo/lib.go",
        2,
        b"Drive",
        TreeEntityId::new(0),
        ItemKind::Function,
        Some(ForeignMentionFixture { foreign_key: 83 }),
    )?;
    let paths = project_paths(&[
        "example.com/demo/service.go",
        "example.com/demo/other.go",
        "example.com/demo/lib.go",
    ]);
    let images = [
        &service_bytes[..],
        &duplicate_service_bytes[..],
        &caller_bytes[..],
    ];
    let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
    let published = BTreeSet::from([
        fixture_version(1).identity(),
        fixture_version(3).identity(),
        fixture_version(2).identity(),
    ]);
    let (external, link_kind, caller_path) = foreign_mention_from_caller(&caller_bytes)?;
    let caller_image =
        SemanticImageView::reopen(&caller_bytes).map_err(|error| error.to_string())?;
    if join_project_mention(
        &caller_image,
        link_kind,
        external,
        &caller_path,
        &paths,
        &index,
        &published,
    )
    .map_err(|error| error.to_string())?
    .is_some()
    {
        return Err("ambiguous example.com/demo/service matches must not retarget".to_owned());
    }
    Ok(())
}
