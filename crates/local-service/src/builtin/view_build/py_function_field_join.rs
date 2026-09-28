//! Python package field reads fall through to module functions when no field/static/const matches.

use super::{compiled_source_path, join_project_field, query_semantic_id, semantic_coordinate};
use backend_engine::{RowId, package_key};
use backend_extension_trustfall::{
    CompilerSemanticEvidence, PackageScopeEvidence, SemanticQueryCancellation, SemanticQueryCorpus,
    SemanticQueryEvent, SemanticQueryEvidence, SemanticQueryFact, SemanticQueryPresentation,
    SemanticQueryRequest, execute_semantic_query,
};
use backend_semantic::ir::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity, EntityAuthorityFacts,
    EntityVersion, ExternalDeclarationIdentity, ExternalId, ExternalTarget, FactAvailability,
    ForeignDeclarationId, ForeignExternalTarget, ForeignTargetOrigin, IrBuilder, ItemKind,
    LinkKind, LinkTarget, OccurrenceAuthorityFacts, ParentageAuthority, SemanticCoreReader,
    SemanticImageView, SemanticReader, SourceIdentity, TreeEntityId, TreeItemInput, TreeLinkInput,
    TreeLinkTarget, VariantAvailability, VariantFingerprint, Visibility,
    encode_full_semantic_image, full_semantic_image_len,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use futures_util::StreamExt as _;
use std::collections::{BTreeMap, BTreeSet};

struct PackageFieldReadFixture {
    ecosystem: &'static [u8],
    package_specifier: &'static [u8],
    path_specifier: &'static [u8],
    display: &'static [u8],
    foreign_key: u8,
}

struct NamespaceFieldFixture {
    ecosystem: &'static [u8],
    namespace: &'static [u8],
    display: &'static [u8],
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

fn project_module_item_image(
    path: &str,
    source_identity_byte: u8,
    item_name: &[u8],
    _entity_id: TreeEntityId,
    item_kind: ItemKind,
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
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &[fixture_version(source_identity_byte)],
            items: &items,
            links: &[],
        })
        .map_err(|error| error.to_string())?;
    let ir = builder.finish().map_err(|error| error.to_string())?;
    let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
    encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn project_module_items_image(
    path: &str,
    source_identity_byte: u8,
    items: &[(&[u8], ItemKind, TreeEntityId, u8)],
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
    let mut tree_items = Vec::new();
    for (name, kind, entity_id, version_byte) in items {
        let version = fixture_version(*version_byte);
        versions.push(version);
        tree_items.push(TreeItemInput {
            name,
            kind: *kind,
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
        let _ = entity_id;
    }
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &tree_items,
            links: &[],
        })
        .map_err(|error| error.to_string())?;
    let ir = builder.finish().map_err(|error| error.to_string())?;
    let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
    encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn project_package_field_read_image(
    path: &str,
    source_identity_byte: u8,
    caller_name: &[u8],
    entity_id: TreeEntityId,
    foreign_read: PackageFieldReadFixture,
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
    }];
    let ecosystem = builder
        .intern_atom(foreign_read.ecosystem)
        .map_err(|e| e.to_string())?;
    let package_atom = builder
        .intern_atom(foreign_read.package_specifier)
        .map_err(|e| e.to_string())?;
    let path_atom = builder
        .intern_atom(foreign_read.path_specifier)
        .map_err(|e| e.to_string())?;
    let display = builder
        .intern_atom(foreign_read.display)
        .map_err(|e| e.to_string())?;
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
    let links = [TreeLinkInput {
        from: entity_id,
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

fn project_namespace_field_image(
    path: &str,
    source_identity_byte: u8,
    field_name: &'static [u8],
    field_version_byte: u8,
    _field_id: TreeEntityId,
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
    let record_version = fixture_version(field_version_byte - 1);
    let field_version = fixture_version(field_version_byte);
    let caller_version = fixture_version(caller_version_byte);
    let versions = [record_version, field_version, caller_version];
    let items = [
        TreeItemInput {
            name: b"WorkoutService",
            kind: ItemKind::Record,
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
            name: field_name,
            kind: ItemKind::Field,
            visibility: Visibility::Public,
            authority: authority(ParentageAuthority::Bound(record_version.identity())),
            parent: Some(TreeEntityId::new(0)),
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
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
        },
    ];
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
        kind: LinkKind::Reads,
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
    Ok(bytes)
}

fn python_field_read_fixture(foreign_key: u8) -> PackageFieldReadFixture {
    PackageFieldReadFixture {
        ecosystem: b"pypi",
        package_specifier: b"workout",
        path_specifier: b"workout.service",
        display: b"set_note",
        foreign_key,
    }
}

fn foreign_read_from_caller(
    caller_bytes: &[u8],
    caller: TreeEntityId,
) -> Result<(ExternalId, LinkKind, String), String> {
    let image = SemanticImageView::reopen(caller_bytes).map_err(|error| error.to_string())?;
    let caller_path = compiled_source_path(&image).map_err(|error| error.to_string())?;
    for (_, link) in image.links_from(backend_semantic::ir::EntityId::new(caller.raw)) {
        if matches!(link.kind, LinkKind::Reads) {
            if let LinkTarget::External(external) = link.target {
                return Ok((external, link.kind, caller_path));
            }
        }
    }
    Err("caller fixture has no foreign read".to_owned())
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
                kind: name.to_owned(),
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
fn join_project_field_python_function_retargets_set_note() -> Result<(), String> {
    let service_bytes = project_module_item_image(
        "workout/service.py",
        1,
        b"set_note",
        TreeEntityId::new(0),
        ItemKind::Function,
    )?;
    let caller_bytes = project_package_field_read_image(
        "weeks.py",
        2,
        b"drive",
        TreeEntityId::new(0),
        python_field_read_fixture(150),
    )?;
    let set_note_identity = fixture_version(1).identity();
    let paths = project_paths(&["workout/service.py", "weeks.py"]);
    let images = [&service_bytes[..], &caller_bytes[..]];
    let index = super::ProjectCallableIndex::build_from_bytes(&images)
        .map_err(|error| error.to_string())?;
    let published = BTreeSet::from([set_note_identity, fixture_version(2).identity()]);
    let (external, link_kind, caller_path) =
        foreign_read_from_caller(&caller_bytes, TreeEntityId::new(0))?;
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
    if joined != Some(set_note_identity) {
        return Err(format!(
            "join_project_field should retarget to set_note function, got {joined:?}"
        ));
    }
    Ok(())
}

#[test]
fn join_project_field_python_function_query_referenced_by_names_drive() -> Result<(), String> {
    let package = package_key("fixture");
    let service_bytes = project_module_item_image(
        "workout/service.py",
        1,
        b"set_note",
        TreeEntityId::new(0),
        ItemKind::Function,
    )?;
    let caller_bytes = project_package_field_read_image(
        "weeks.py",
        2,
        b"drive",
        TreeEntityId::new(0),
        python_field_read_fixture(151),
    )?;
    let set_note_identity = fixture_version(1).identity();
    let drive_identity = fixture_version(2).identity();
    let paths = project_paths(&["workout/service.py", "weeks.py"]);
    let images = [&service_bytes[..], &caller_bytes[..]];
    let index = super::ProjectCallableIndex::build_from_bytes(&images)
        .map_err(|error| error.to_string())?;
    let published = BTreeSet::from([set_note_identity, drive_identity]);
    let (external, link_kind, caller_path) =
        foreign_read_from_caller(&caller_bytes, TreeEntityId::new(0))?;
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
    let set_note_id = query_semantic_id(package, joined);
    let (set_note_fact, _) = compiler_query_presentation(
        package,
        "fixture",
        &service_bytes,
        set_note_identity,
        "set_note",
        Box::new([]),
    )?;
    let (drive_fact, _) = compiler_query_presentation(
        package,
        "fixture",
        &caller_bytes,
        drive_identity,
        "drive",
        vec![set_note_id.clone()].into_boxed_slice(),
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
            set_note_fact,
            drive_fact,
        ],
    )
    .map_err(|error| error.to_string())?;
    let (cancellation, _) = SemanticQueryCancellation::new();
    let request = SemanticQueryRequest::admit_page(
        corpus,
        "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) referencedBy @optional { name @output } } }",
        BTreeMap::from([("name".to_owned(), "set_note".into())]),
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
    if callers != ["drive".into()] {
        return Err(format!(
            "referencedBy on set_note should name only drive, got {callers:?}"
        ));
    }
    Ok(())
}

#[test]
fn join_project_field_python_field_wins_over_function() -> Result<(), String> {
    let service_bytes = project_module_items_image(
        "workout/service.py",
        1,
        &[
            (b"set_note", ItemKind::Field, TreeEntityId::new(0), 10),
            (b"set_note", ItemKind::Function, TreeEntityId::new(1), 11),
        ],
    )?;
    let caller_bytes = project_package_field_read_image(
        "weeks.py",
        2,
        b"drive",
        TreeEntityId::new(0),
        python_field_read_fixture(152),
    )?;
    let field_identity = fixture_version(10).identity();
    let paths = project_paths(&["workout/service.py", "weeks.py"]);
    let images = [&service_bytes[..], &caller_bytes[..]];
    let index = super::ProjectCallableIndex::build_from_bytes(&images)
        .map_err(|error| error.to_string())?;
    let published = BTreeSet::from([
        field_identity,
        fixture_version(11).identity(),
        fixture_version(2).identity(),
    ]);
    let (external, link_kind, caller_path) =
        foreign_read_from_caller(&caller_bytes, TreeEntityId::new(0))?;
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
    if joined != Some(field_identity) {
        return Err(format!(
            "join_project_field should retarget to Field set_note, got {joined:?}"
        ));
    }
    Ok(())
}

#[test]
fn join_project_field_python_static_wins_over_function() -> Result<(), String> {
    let service_bytes = project_module_items_image(
        "workout/service.py",
        1,
        &[
            (b"set_note", ItemKind::Static, TreeEntityId::new(0), 20),
            (b"set_note", ItemKind::Function, TreeEntityId::new(1), 21),
        ],
    )?;
    let caller_bytes = project_package_field_read_image(
        "weeks.py",
        2,
        b"drive",
        TreeEntityId::new(0),
        python_field_read_fixture(153),
    )?;
    let static_identity = fixture_version(20).identity();
    let paths = project_paths(&["workout/service.py", "weeks.py"]);
    let images = [&service_bytes[..], &caller_bytes[..]];
    let index = super::ProjectCallableIndex::build_from_bytes(&images)
        .map_err(|error| error.to_string())?;
    let published = BTreeSet::from([
        static_identity,
        fixture_version(21).identity(),
        fixture_version(2).identity(),
    ]);
    let (external, link_kind, caller_path) =
        foreign_read_from_caller(&caller_bytes, TreeEntityId::new(0))?;
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
    if joined != Some(static_identity) {
        return Err(format!(
            "join_project_field should retarget to Static set_note, got {joined:?}"
        ));
    }
    Ok(())
}

#[test]
fn join_project_field_python_static_and_constant_do_not_fall_through_to_function()
-> Result<(), String> {
    let service_bytes = project_module_items_image(
        "workout/service.py",
        1,
        &[
            (b"set_note", ItemKind::Static, TreeEntityId::new(0), 30),
            (b"set_note", ItemKind::Constant, TreeEntityId::new(1), 31),
            (b"set_note", ItemKind::Function, TreeEntityId::new(2), 32),
        ],
    )?;
    let caller_bytes = project_package_field_read_image(
        "weeks.py",
        2,
        b"drive",
        TreeEntityId::new(0),
        python_field_read_fixture(154),
    )?;
    let paths = project_paths(&["workout/service.py", "weeks.py"]);
    let images = [&service_bytes[..], &caller_bytes[..]];
    let index = super::ProjectCallableIndex::build_from_bytes(&images)
        .map_err(|error| error.to_string())?;
    let published = BTreeSet::from([
        fixture_version(30).identity(),
        fixture_version(31).identity(),
        fixture_version(32).identity(),
        fixture_version(2).identity(),
    ]);
    let (external, link_kind, caller_path) =
        foreign_read_from_caller(&caller_bytes, TreeEntityId::new(0))?;
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
    if joined != None {
        return Err(format!(
            "static and constant both named set_note must not fall through to function, got {joined:?}"
        ));
    }
    Ok(())
}

#[test]
fn join_project_field_python_ambiguous_functions_returns_none() -> Result<(), String> {
    let first_function = project_module_item_image(
        "workout/service.py",
        1,
        b"set_note",
        TreeEntityId::new(0),
        ItemKind::Function,
    )?;
    let second_function = project_module_item_image(
        "workout/service/__init__.py",
        3,
        b"set_note",
        TreeEntityId::new(0),
        ItemKind::Function,
    )?;
    let caller_bytes = project_package_field_read_image(
        "weeks.py",
        2,
        b"drive",
        TreeEntityId::new(0),
        python_field_read_fixture(155),
    )?;
    let paths = project_paths(&[
        "workout/service.py",
        "workout/service/__init__.py",
        "weeks.py",
    ]);
    let images = [&first_function[..], &second_function[..], &caller_bytes[..]];
    let index = super::ProjectCallableIndex::build_from_bytes(&images)
        .map_err(|error| error.to_string())?;
    let published = BTreeSet::from([
        fixture_version(1).identity(),
        fixture_version(3).identity(),
        fixture_version(2).identity(),
    ]);
    let (external, link_kind, caller_path) =
        foreign_read_from_caller(&caller_bytes, TreeEntityId::new(0))?;
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
    if joined != None {
        return Err(format!(
            "two functions named set_note must not retarget, got {joined:?}"
        ));
    }
    Ok(())
}

#[test]
fn join_project_field_python_namespace_field_does_not_join_function() -> Result<(), String> {
    let package_function = project_module_item_image(
        "workout/service.py",
        1,
        b"set_note",
        TreeEntityId::new(0),
        ItemKind::Function,
    )?;
    let caller_bytes = project_namespace_field_image(
        "weeks.py",
        2,
        b"set_note",
        41,
        TreeEntityId::new(1),
        b"drive",
        42,
        TreeEntityId::new(2),
        NamespaceFieldFixture {
            ecosystem: b"pypi",
            namespace: b"WorkoutService",
            display: b"set_note",
            foreign_key: 156,
        },
    )?;
    let paths = project_paths(&["workout/service.py", "weeks.py"]);
    let images = [&package_function[..], &caller_bytes[..]];
    let index = super::ProjectCallableIndex::build_from_bytes(&images)
        .map_err(|error| error.to_string())?;
    let published = BTreeSet::from([
        fixture_version(1).identity(),
        fixture_version(41).identity(),
        fixture_version(42).identity(),
    ]);
    let (external, link_kind, caller_path) =
        foreign_read_from_caller(&caller_bytes, TreeEntityId::new(2))?;
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
    let field_identity = fixture_version(41).identity();
    if joined != Some(field_identity) {
        return Err(format!(
            "namespace field read should retarget to WorkoutService.set_note, got {joined:?}"
        ));
    }
    Ok(())
}
