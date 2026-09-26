//! C# namespace field-read keys join through `join_project_field`.

use super::{
    ProjectCallableIndex, compiled_source_path, join_project_field, query_semantic_id,
    semantic_coordinate,
};
use backend_engine::{RowId, package_key};
use backend_extension_trustfall::{
    CompilerSemanticEvidence, SemanticQueryCorpus, SemanticQueryEvent, SemanticQueryEvidence,
    SemanticQueryFact, SemanticQueryPresentation, SemanticQueryRequest, execute_semantic_query,
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

struct NamespaceFieldFixture {
    ecosystem: &'static [u8],
    namespace: &'static [u8],
    display: &'static [u8],
    foreign_key: u8,
    link_kind: LinkKind,
}

struct AncestorFixture {
    name: &'static [u8],
    version_byte: u8,
    entity_id: TreeEntityId,
    parent_id: Option<TreeEntityId>,
    parent_version_byte: Option<u8>,
    kind: ItemKind,
}

fn fixture_version(identity: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([identity; 16]),
        variant: VariantFingerprint::from_raw([identity; 16]),
        core_payload: CorePayloadHash::from_raw([identity; 16]),
    }
}

fn csharp_owner_chain() -> [AncestorFixture; 2] {
    [
        AncestorFixture {
            name: b"Demo",
            version_byte: 10,
            entity_id: TreeEntityId::new(0),
            parent_id: None,
            parent_version_byte: None,
            kind: ItemKind::Module,
        },
        AncestorFixture {
            name: b"WorkoutService",
            version_byte: 11,
            entity_id: TreeEntityId::new(1),
            parent_id: Some(TreeEntityId::new(0)),
            parent_version_byte: Some(10),
            kind: ItemKind::Record,
        },
    ]
}

fn project_paths(files: &[&str]) -> BTreeSet<String> {
    files.iter().map(|path| (*path).to_owned()).collect()
}

fn project_namespace_field_image(
    path: &str,
    source_identity_byte: u8,
    ancestors: &[AncestorFixture],
    field_name: &'static [u8],
    field_version_byte: u8,
    field_id: TreeEntityId,
    caller_name: &'static [u8],
    caller_version_byte: u8,
    caller_id: TreeEntityId,
    foreign_field: NamespaceFieldFixture,
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
    let mut versions = Vec::new();
    let mut items = Vec::new();
    for ancestor in ancestors {
        let version = fixture_version(ancestor.version_byte);
        versions.push(version);
        let parentage = match ancestor.parent_version_byte {
            None => ParentageAuthority::Root,
            Some(parent_version_byte) => {
                ParentageAuthority::Bound(fixture_version(parent_version_byte).identity())
            }
        };
        items.push(TreeItemInput {
            name: ancestor.name,
            kind: ancestor.kind,
            visibility: Visibility::Public,
            authority: authority(parentage),
            parent: ancestor.parent_id,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        });
    }
    let field_version = fixture_version(field_version_byte);
    versions.push(field_version);
    let field_parent = ancestors.last().map(|ancestor| ancestor.entity_id);
    let field_parentage = ancestors
        .last()
        .and_then(|ancestor| Some(fixture_version(ancestor.version_byte).identity()))
        .map(ParentageAuthority::Bound)
        .unwrap_or(ParentageAuthority::Root);
    items.push(TreeItemInput {
        name: field_name,
        kind: ItemKind::Field,
        visibility: Visibility::Public,
        authority: authority(field_parentage),
        parent: field_parent,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    });
    let caller_version = fixture_version(caller_version_byte);
    versions.push(caller_version);
    items.push(TreeItemInput {
        name: caller_name,
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
    });
    let ecosystem = builder
        .intern_atom(foreign_field.ecosystem)
        .map_err(|e| e.to_string())?;
    let namespace_atom = builder
        .intern_atom(foreign_field.namespace)
        .map_err(|e| e.to_string())?;
    let display = builder
        .intern_atom(foreign_field.display)
        .map_err(|e| e.to_string())?;
    let path_atom = builder
        .intern_atom(foreign_field.display)
        .map_err(|e| e.to_string())?;
    let external = builder
        .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
            identity: ExternalDeclarationIdentity {
                foreign: ForeignDeclarationId::from_raw([foreign_field.foreign_key; 16]),
                variant: VariantAvailability::Unavailable,
            },
            origin: ForeignTargetOrigin::Namespace {
                ecosystem,
                namespace: namespace_atom,
            },
            path: path_atom,
            display,
            kind: Some(ItemKind::Field),
        }))
        .map_err(|e| e.to_string())?;
    let links = [TreeLinkInput {
        from: caller_id,
        target: TreeLinkTarget::External(external),
        kind: foreign_field.link_kind,
        confidence: backend_semantic::ir::Confidence::Compiler,
        authority: OccurrenceAuthorityFacts {
            source: FactAvailability::Unavailable,
        },
        source: None,
    }];
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &links,
        })
        .map_err(|error| error.to_string())?;
    let ir = builder.finish().map_err(|error| error.to_string())?;
    let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
    encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
    let _ = field_id;
    Ok(bytes)
}

fn cs_namespace_note_field_image(
    path: &str,
    source_identity_byte: u8,
    note_version_byte: u8,
    drive_version_byte: u8,
    foreign_key: u8,
) -> Result<Vec<u8>, String> {
    project_namespace_field_image(
        path,
        source_identity_byte,
        &csharp_owner_chain(),
        b"Note",
        note_version_byte,
        TreeEntityId::new(2),
        b"Drive",
        drive_version_byte,
        TreeEntityId::new(3),
        NamespaceFieldFixture {
            ecosystem: b"nuget",
            namespace: b"Demo.WorkoutService",
            display: b"Note",
            foreign_key,
            link_kind: LinkKind::Reads,
        },
    )
}

fn foreign_namespace_link_from_caller(
    caller_bytes: &[u8],
    caller_entity: TreeEntityId,
    expected_kind: LinkKind,
) -> Result<(ExternalId, LinkKind, String), String> {
    let image = SemanticImageView::reopen(caller_bytes).map_err(|error| error.to_string())?;
    let caller_path = compiled_source_path(&image).map_err(|error| error.to_string())?;
    for (_, link) in image.links_from(backend_semantic::ir::EntityId::new(caller_entity.raw)) {
        if link.kind == expected_kind {
            if let LinkTarget::External(external) = link.target {
                return Ok((external, link.kind, caller_path));
            }
        }
    }
    Err(format!(
        "caller fixture has no foreign {:?} link",
        expected_kind
    ))
}

fn query_corpus_coordinate() -> Result<PackageUrl, String> {
    PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
        .map_err(|error| format!("fixture coordinate: {error:?}"))
}

fn compiler_query_presentation(
    package: backend_engine::PackageKey,
    label: &str,
    image_bytes: &[u8],
    identity: DeclarationIdentity,
    name: &str,
    related: Box<[String]>,
) -> Result<(SemanticQueryFact, String), String> {
    let image = SemanticImageView::reopen(image_bytes).map_err(|error| error.to_string())?;
    let coordinate = query_corpus_coordinate()?;
    let profile = LanguageProfile::Rust(RustEdition::Rust2024);
    let image_digest = *blake3::hash(image_bytes).as_bytes();
    let evidence = CompilerSemanticEvidence::new(
        package,
        coordinate,
        profile,
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
                kind: "function".to_owned(),
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
fn csharp_namespace_field_read_retargets_note() -> Result<(), String> {
    let service_bytes = cs_namespace_note_field_image("WorkoutService.cs", 80, 81, 82, 80)?;
    let note_identity = fixture_version(81).identity();
    let paths = project_paths(&["WorkoutService.cs"]);
    let images = [&service_bytes[..]];
    let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
    let published = BTreeSet::from([note_identity, fixture_version(82).identity()]);
    let (external, link_kind, caller_path) =
        foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(3), LinkKind::Reads)?;
    let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
    let joined = join_project_field(
        &image,
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
            "owner-chain namespace field read should retarget to Note, got {joined:?}"
        ));
    }
    Ok(())
}

#[test]
fn csharp_namespace_field_read_referenced_by_names_drive() -> Result<(), String> {
    let package = package_key("fixture");
    let service_bytes = cs_namespace_note_field_image("WorkoutService.cs", 83, 84, 85, 83)?;
    let note_identity = fixture_version(84).identity();
    let drive_identity = fixture_version(85).identity();
    let paths = project_paths(&["WorkoutService.cs"]);
    let images = [&service_bytes[..]];
    let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
    let published = BTreeSet::from([note_identity, drive_identity]);
    let (external, link_kind, caller_path) =
        foreign_namespace_link_from_caller(&service_bytes, TreeEntityId::new(3), LinkKind::Reads)?;
    let image = SemanticImageView::reopen(&service_bytes).map_err(|error| error.to_string())?;
    let joined = join_project_field(
        &image,
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
        &service_bytes,
        drive_identity,
        "Drive",
        vec![note_id.clone()].into_boxed_slice(),
    )?;
    let workspace = crate::builtin::genesis().map_err(|error| error.to_string())?;
    let corpus = SemanticQueryCorpus::admit(
        workspace.root(),
        vec![
            SemanticQueryFact::new(
                SemanticQueryEvidence::Package(
                    backend_extension_trustfall::PackageScopeEvidence::new(package),
                ),
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
    let (cancellation, _) = backend_extension_trustfall::SemanticQueryCancellation::new();
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

#[test]
fn csharp_namespace_field_read_ambiguous_returns_none() -> Result<(), String> {
    let first_bytes = cs_namespace_note_field_image("WorkoutService.cs", 86, 87, 88, 86)?;
    let duplicate_bytes = cs_namespace_note_field_image("WorkoutService.cs", 89, 90, 91, 89)?;
    let paths = project_paths(&["WorkoutService.cs"]);
    let images = [&first_bytes[..], &duplicate_bytes[..]];
    let index = ProjectCallableIndex::build_from_bytes(&images).map_err(|error| error.to_string())?;
    let published = BTreeSet::from([
        fixture_version(87).identity(),
        fixture_version(90).identity(),
        fixture_version(88).identity(),
    ]);
    let (external, link_kind, caller_path) =
        foreign_namespace_link_from_caller(&first_bytes, TreeEntityId::new(3), LinkKind::Reads)?;
    let image = SemanticImageView::reopen(&first_bytes).map_err(|error| error.to_string())?;
    if join_project_field(
        &image,
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
        return Err("ambiguous namespace field matches must not retarget".to_owned());
    }
    Ok(())
}
