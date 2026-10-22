//! Adversarial source-site projection tests using a selected persisted native
//! frontier. The semantic images below are synthetic IR fixtures; the
//! `VerifiedNativeSource` is always recovered from the real workspace owner.

use super::native_sites::prove_native_sites;
use crate::builtin::BuiltinIntent;
use crate::builtin::profile::{
    BuiltinNativeFrontierChange, BuiltinPackageRecord, BuiltinSourceChange,
};
use backend_engine::builtin::{
    NativePathDisposition, ProductSourceRecord, ProductSourceSnapshot,
    build_native_frontier_records_with_content,
};
use backend_engine::{PackageKey, Relation, SourceLanguage, product_source_file_key};
use backend_semantic::ir::{
    ClangFacts, ClangLayout, ClangQualifiers, ClangStorageClass, DeclarationFamilyId,
    EntityAuthorityFacts, EntityVersion, FactAvailability, IrBuilder, ItemKind,
    LanguageExtensionInput, PackageLineage, ParentageAuthority, SemanticImageView,
    SemanticReader as _, SourceIdentity, SourceSpan, TreeItemInput, VariantFingerprint, Visibility,
};
use backend_semantic::vocabulary::{CxxStandard, LanguageProfile, NativeTool, Stage};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

const LABEL: &str = "pkg:cargo/native-site-projection-fixture@1.0.0";
const PATH: &str = "src/widget.cpp";
const SOURCE: &[u8] = b"struct Widget {};\nstruct Widget {};\n";
const NAME_START: u32 = 7;
const NAME_END: u32 = 13;
const SECOND_NAME_START: u32 = 25;
const SECOND_NAME_END: u32 = 31;

struct TempWorkspace(tempfile::TempDir);

impl TempWorkspace {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("nudox-native-sites-")
            .tempdir()
            .expect("create private native-site workspace");
        crate::test_support::make_private(directory.path())
            .expect("make native-site workspace private");
        Self(directory)
    }
}

fn open_daemon(workspace: &Path) -> crate::builtin::ProductDaemon {
    let profile = crate::builtin::profile::profile_descriptor(
        crate::builtin::profile::BuiltinProfile::Product,
    )
    .expect("product profile");
    let dispatcher = crate::builtin::profile::builtin_dispatcher(
        Some(crate::builtin::ECHO_AUTHORITY_SECRET),
        Arc::clone(&profile),
        60_000,
    )
    .expect("test dispatcher");
    let registry = crate::builtin::product_relation_registry().expect("product registry");
    crate::Locald::open_with_dispatcher_and_registry(
        workspace,
        crate::builtin::BuiltinModel,
        crate::builtin::genesis().expect("product genesis"),
        dispatcher,
        backend_engine::DaemonConfig::default(),
        registry,
    )
    .expect("open product daemon")
}

fn version(value: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([value; 16]),
        variant: VariantFingerprint::from_raw([value.wrapping_add(1); 16]),
        core_payload: backend_semantic::ir::CorePayloadHash::from_raw([value.wrapping_add(2); 16]),
    }
}

fn authority() -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        members: FactAvailability::Captured,
        documentation: FactAvailability::Captured,
        attributes: FactAvailability::Captured,
        visibility: FactAvailability::Captured,
        source: FactAvailability::Captured,
        source_file: FactAvailability::Captured,
        language_extension: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    }
}

fn selected_native_source() -> (
    TempWorkspace,
    backend_engine::builtin::VerifiedNativeSource,
    [u8; 32],
) {
    let workspace = TempWorkspace::new();
    let mut daemon = open_daemon(workspace.0.path());
    let package = PackageKey::from_value(LABEL);
    let project = package.to_bytes();
    let profile = LanguageProfile::Cxx(CxxStandard::Cxx23);
    let path_key = product_source_file_key(project, PATH);
    let content_identity = ContentId::<SourceFactDomain>::from_canonical_bytes(SOURCE);
    let disposition = NativePathDisposition::ClangMalformed {
        content: backend_compile::typed_of::<backend_compile::InputContentSchema>(SOURCE)
            .to_bytes(),
        source_identity: content_identity,
        byte_length: 0,
        chunk_count: 0,
    };
    let rows = build_native_frontier_records_with_content(
        project,
        profile,
        vec![(PATH.to_owned(), disposition)],
        vec![(PATH.to_owned(), SOURCE.to_vec())],
    )
    .expect("exact malformed source frontier rows");
    let native_changes = rows
        .iter()
        .map(|(key, after)| BuiltinNativeFrontierChange {
            key: *key,
            expected: None,
            after: Some(after.clone()),
        })
        .collect::<Vec<_>>();

    let file = ProductSourceRecord::file_unavailable(
        project,
        PATH,
        SourceLanguage::Clang,
        [0x61; 32],
        backend_library::SourceUnavailableReason::Unparsed,
    )
    .expect("typed unavailable structural source row");
    let mut source_version = blake3::Hasher::new();
    source_version.update(b"backend.project-snapshot.v2\0");
    source_version.update(b"nudox.project-native-frontier-source-version.v1\0");
    for record in rows.iter().map(|(_, record)| record).filter(|record| {
        matches!(
            record,
            backend_engine::builtin::ProductNativeFrontierRecord::Profile { .. }
        )
    }) {
        let mut encoded = Vec::new();
        <backend_engine::builtin::ProductNativeFrontierRelation as Relation>::encode_value(
            record,
            &mut encoded,
        );
        source_version.update(&(encoded.len() as u64).to_be_bytes());
        source_version.update(&encoded);
    }
    source_version.update(&path_key);
    source_version.update(&[0; 32]);
    source_version.update(&[0x61; 32]);
    let project_record =
        BuiltinPackageRecord::project(LABEL, *source_version.finalize().as_bytes(), vec![path_key])
            .expect("one-file project membership");
    let changes = vec![
        BuiltinSourceChange {
            key: project,
            after: Some(project_record),
        },
        BuiltinSourceChange {
            key: path_key,
            after: Some(file),
        },
    ];
    let intent = BuiltinIntent::index_with_semantics(package, LABEL, changes, Vec::new())
        .expect("source rows intent")
        .with_native_frontier(native_changes)
        .expect("selected native frontier intent");
    crate::builtin::commands::commit_builtin_intent(&mut daemon, 0x4e53, &intent)
        .expect("owner commits exact source and native frontier");

    let snapshot = daemon.engine().daemon().owner().snapshot();
    let selected =
        ProductSourceSnapshot::from_workspace(&snapshot).expect("selected source snapshot");
    let paths = BTreeSet::from([PATH.to_owned()]);
    let frontier = selected
        .admit_native_frontier(project, profile, &paths)
        .expect("admit complete selected frontier")
        .expect("native frontier is present");
    let verified = selected
        .verified_native_source(&frontier, PATH)
        .expect("source bytes come from the selected native frontier");
    (workspace, verified, project)
}

fn try_image_for(
    source: &[u8],
    profile: LanguageProfile,
    claims: &[(u8, &[u8], u32, u32)],
) -> Result<Vec<u8>, backend_semantic::ir::BuildError> {
    let source_identity = ContentId::<SourceFactDomain>::from_canonical_bytes(source);
    let mut builder = IrBuilder::new();
    builder
        .set_language_profile(profile)
        .expect("native image profile");
    builder
        .set_image_provenance(
            SourceIdentity {
                identity: source_identity,
                byte_len: u32::try_from(source.len()).expect("fixture source fits"),
            },
            backend_semantic::vocabulary::CompileRecipeFact::derive(
                profile,
                Stage::LowerIr,
                NativeTool::Clang,
                source_identity,
                ContentId::<ToolchainDomain>::from_canonical_bytes(b"native site test toolchain"),
            ),
            PackageLineage::new("cargo", "native-site-projection-fixture")
                .expect("fixture package lineage"),
            PATH,
        )
        .expect("captured source provenance");
    let path = builder
        .intern_atom(PATH.as_bytes())
        .expect("source path atom");
    let empty_templates = builder
        .intern_type_parameters(&[])
        .expect("empty template list");
    let empty_includes = builder.intern_attributes(&[]).expect("empty include list");
    let mut versions = Vec::new();
    let mut items = Vec::new();
    let mut facts = Vec::new();
    for (identity_seed, _, name_start, name_end) in claims {
        versions.push(version(*identity_seed));
        facts.push(ClangFacts {
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
            templates: empty_templates,
            includes: empty_includes,
            declared_name: SourceSpan::new(path, *name_start, *name_end),
        });
    }
    for ((_, entity_name, name_start, name_end), fact) in claims.iter().zip(&facts) {
        let name_start_usize = usize::try_from(*name_start).expect("name offset");
        let name_end_usize = usize::try_from(*name_end).expect("name end");
        let declaration_start = source
            .get(..name_start_usize)
            .and_then(|prefix| prefix.iter().rposition(|byte| *byte == b'\n'))
            .map_or(0, |newline| newline + 1);
        let declaration_end = source
            .get(name_end_usize..)
            .and_then(|suffix| suffix.iter().position(|byte| *byte == b'\n'))
            .map_or(source.len(), |newline| name_end_usize + newline + 1);
        items.push(TreeItemInput {
            name: entity_name,
            anonymous_callable_anchor: None,
            kind: ItemKind::Record,
            visibility: Visibility::Public,
            authority: authority(),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: SourceSpan::new(
                path,
                u32::try_from(declaration_start).expect("declaration start"),
                u32::try_from(declaration_end).expect("declaration end"),
            ),
            extension: Some(LanguageExtensionInput::Clang(fact)),
        });
    }
    builder
        .add_borrowed_tree(backend_semantic::ir::BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })
        .expect("fixture semantic entities");
    let ir = builder.finish()?;
    let mut encoded = vec![
        0;
        backend_semantic::ir::full_semantic_image_len(&ir)
            .expect("measure provenance-bearing semantic image")
    ];
    let written = backend_semantic::ir::encode_full_semantic_image(&ir, &mut encoded)
        .expect("encode provenance-bearing semantic image");
    encoded.truncate(written);
    Ok(encoded)
}

fn image_for(source: &[u8], profile: LanguageProfile, claims: &[(u8, &[u8], u32, u32)]) -> Vec<u8> {
    try_image_for(source, profile, claims).expect("fixture IR")
}

#[test]
fn native_sites_require_exact_declared_name_and_suppress_ambiguous_claimants() {
    let (_workspace, source, project) = selected_native_source();
    let bytes = image_for(
        source.bytes(),
        source.profile(),
        &[(1, b"Widget", NAME_START, NAME_END)],
    );
    let image = SemanticImageView::reopen(&bytes).expect("captured semantic image");
    let sites = prove_native_sites(&image, project, &source, 8, 32 * 1024)
        .expect("bounded exact source projection");
    let entity = image
        .entity(backend_semantic::ir::EntityId::new(0))
        .unwrap();
    let identity = entity.version.identity();
    let site = sites.get(&identity).expect("matching declared-name proof");
    assert_eq!(site.line, 1);
    assert_eq!(site.excerpt().text(), Some("struct Widget {};\n"));

    let wrong_span = image_for(source.bytes(), source.profile(), &[(7, b"Widget", 0, 6)]);
    let wrong_span = SemanticImageView::reopen(&wrong_span).expect("wrong name-span image");
    let wrong_span_sites = prove_native_sites(&wrong_span, project, &source, 8, 32 * 1024)
        .expect("name bytes must match the declared-name span");
    assert!(
        wrong_span_sites
            .get(
                &wrong_span
                    .entity(backend_semantic::ir::EntityId::new(0))
                    .unwrap()
                    .version
                    .identity()
            )
            .is_none()
    );

    let ambiguous = image_for(
        source.bytes(),
        source.profile(),
        &[
            (2, b"Widget", NAME_START, NAME_END),
            (3, b"Widget", NAME_START, NAME_END),
        ],
    );
    let ambiguous = SemanticImageView::reopen(&ambiguous).expect("ambiguous image");
    let claims = prove_native_sites(&ambiguous, project, &source, 8, 32 * 1024)
        .expect("ambiguous claims are conservatively suppressed");
    assert!(
        claims
            .get(
                &ambiguous
                    .entity(backend_semantic::ir::EntityId::new(0))
                    .unwrap()
                    .version
                    .identity()
            )
            .is_none()
    );

    let overlapping = image_for(
        source.bytes(),
        source.profile(),
        &[
            (11, b"Widget", NAME_START, NAME_END),
            (12, b"get", 10, 13),
            (13, b"Widget", SECOND_NAME_START, SECOND_NAME_END),
        ],
    );
    let overlapping = SemanticImageView::reopen(&overlapping).expect("overlapping spans image");
    let overlap_sites = prove_native_sites(&overlapping, project, &source, 8, 32 * 1024)
        .expect("overlapping source claims are suppressed");
    for entity_index in [0, 1] {
        let identity = overlapping
            .entity(backend_semantic::ir::EntityId::new(entity_index))
            .unwrap()
            .version
            .identity();
        assert!(overlap_sites.get(&identity).is_none());
    }
    let disjoint_identity = overlapping
        .entity(backend_semantic::ir::EntityId::new(2))
        .unwrap()
        .version
        .identity();
    assert!(overlap_sites.get(&disjoint_identity).is_some());

    let duplicate_identity = try_image_for(
        source.bytes(),
        source.profile(),
        &[
            (8, b"Widget", NAME_START, NAME_END),
            (8, b"Widget", SECOND_NAME_START, SECOND_NAME_END),
        ],
    );
    assert!(matches!(
        duplicate_identity,
        Err(backend_semantic::ir::BuildError::DuplicateDeclarationIdentity { .. })
    ));
}

#[test]
fn native_site_proof_reaches_production_semantic_rows_without_a_structural_twin() {
    let (_workspace, source, project) = selected_native_source();
    let image_bytes = image_for(
        source.bytes(),
        source.profile(),
        &[(31, b"Widget", NAME_START, NAME_END)],
    );
    let image = SemanticImageView::reopen(&image_bytes).expect("captured semantic image");
    let package = PackageKey::from_value(LABEL);
    let indexed_project = super::super::IndexedProject {
        package,
        label: LABEL.to_owned(),
        files: Arc::from([]),
    };
    let (base, _) = crate::builtin::initial_view().expect("initial view");
    let projected = super::semantic::project_image_rows(
        &image,
        *blake3::hash(&image_bytes).as_bytes(),
        &indexed_project,
        source.profile(),
        base.basis(),
        false,
        PATH,
        &[],
        Some(super::native_sites::NativeProjection {
            source: &source,
            max_sites: 8,
            max_working_bytes: 32 * 1024,
        }),
        None,
    )
    .expect("production semantic row projection");
    let row = projected.rows.first().expect("semantic declaration row");
    let location = row.row.source.captured().expect("native source location");
    assert_eq!(location.path(), PATH);
    assert_eq!(location.start_line(), 1);
    assert_eq!(row.row.label, format!("{LABEL}::{PATH}:1::Widget"));
    assert_eq!(row.row.excerpt.text(), Some("struct Widget {};\n"));

    let stale = super::semantic::project_image_rows(
        &image,
        *blake3::hash(&image_bytes).as_bytes(),
        &indexed_project,
        source.profile(),
        base.basis(),
        true,
        PATH,
        &[],
        Some(super::native_sites::NativeProjection {
            source: &source,
            max_sites: 8,
            max_working_bytes: 32 * 1024,
        }),
        None,
    )
    .expect("stale semantic row projection");
    assert_eq!(
        stale.rows[0].row.source,
        backend_engine::SourceAvailability::NotCaptured,
    );
    assert_eq!(
        stale.rows[0].row.excerpt,
        backend_compile::SourceExcerpt::NotCaptured,
    );

    // The proof is tied to the exact captured image source. A same-name row
    // with different source bytes keeps the ordinary missing-source fallback.
    let other_source = b"struct WidgeX {};\nstruct Widget {};\n";
    let mismatched_bytes = image_for(
        other_source,
        source.profile(),
        &[(32, b"Widget", NAME_START, NAME_END)],
    );
    let mismatched =
        SemanticImageView::reopen(&mismatched_bytes).expect("mismatched semantic image");
    let refused = super::semantic::project_image_rows(
        &mismatched,
        *blake3::hash(&mismatched_bytes).as_bytes(),
        &indexed_project,
        source.profile(),
        base.basis(),
        false,
        PATH,
        &[],
        Some(super::native_sites::NativeProjection {
            source: &source,
            max_sites: 8,
            max_working_bytes: 32 * 1024,
        }),
        None,
    )
    .expect("mismatched proof remains a conservative row projection");
    assert_eq!(
        refused.rows[0].row.source,
        backend_engine::SourceAvailability::NotCaptured,
    );
    assert_eq!(
        refused.rows[0].row.excerpt,
        backend_compile::SourceExcerpt::NotCaptured,
    );
    assert_eq!(project, package.to_bytes());
}

#[test]
fn native_sites_reject_source_identity_project_and_profile_mismatch() {
    let (_workspace, source, project) = selected_native_source();
    let wrong_source = b"struct WidgeX {};\nstruct Widget {};\n";
    let mismatched = image_for(
        wrong_source,
        source.profile(),
        &[(4, b"Widget", NAME_START, NAME_END)],
    );
    let mismatched = SemanticImageView::reopen(&mismatched).expect("mismatched image");
    let mismatched_sites = prove_native_sites(&mismatched, project, &source, 8, 32 * 1024)
        .expect("identity mismatch has no sites");
    assert!(
        mismatched_sites
            .get(
                &mismatched
                    .entity(backend_semantic::ir::EntityId::new(0))
                    .unwrap()
                    .version
                    .identity()
            )
            .is_none()
    );

    let matching = image_for(
        source.bytes(),
        source.profile(),
        &[(5, b"Widget", NAME_START, NAME_END)],
    );
    let matching = SemanticImageView::reopen(&matching).expect("matching image");
    let wrong_project = prove_native_sites(&matching, [0x99; 32], &source, 8, 32 * 1024)
        .expect("wrong project has no sites");
    assert!(
        wrong_project
            .get(
                &matching
                    .entity(backend_semantic::ir::EntityId::new(0))
                    .unwrap()
                    .version
                    .identity()
            )
            .is_none()
    );
    let wrong_profile = image_for(
        source.bytes(),
        LanguageProfile::C(backend_semantic::vocabulary::CStandard::C23),
        &[(6, b"Widget", NAME_START, NAME_END)],
    );
    let wrong_profile = SemanticImageView::reopen(&wrong_profile).expect("other profile image");
    let wrong_profile_sites = prove_native_sites(&wrong_profile, project, &source, 8, 32 * 1024)
        .expect("profile mismatch has no sites");
    assert!(
        wrong_profile_sites
            .get(
                &wrong_profile
                    .entity(backend_semantic::ir::EntityId::new(0))
                    .unwrap()
                    .version
                    .identity()
            )
            .is_none()
    );

    assert!(
        prove_native_sites(&matching, [0x99; 32], &source, 8, source.bytes().len() - 1).is_err(),
        "source memory reservation is checked even when project provenance would produce no sites"
    );
}
