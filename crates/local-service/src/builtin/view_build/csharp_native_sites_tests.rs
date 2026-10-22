//! Bounded C# source-site projection controls. Source bytes are recovered from
//! a selected persisted native frontier; only semantic images are synthetic.

use super::csharp_native_sites::{NativeCSharpProjection, prove_native_csharp_sites};
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
    CSharpFacts, CSharpMemberEffects, CSharpNullability, CSharpPartialRole, CSharpReferenceKind,
    DeclarationFamilyId, EntityAuthorityFacts, EntityVersion, FactAvailability, IrBuilder,
    ItemKind, LanguageExtensionInput, PackageLineage, ParentageAuthority, SemanticImageView,
    SemanticReader as _, SourceIdentity, SourceSpan, TreeItemInput, VariantFingerprint, Visibility,
};
use backend_semantic::vocabulary::{CSharpVersion, LanguageProfile, NativeTool, Stage};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use std::collections::BTreeSet;
use std::sync::Arc;

const LABEL: &str = "pkg:cargo/csharp-native-site-fixture@1.0.0";
const PATH: &str = "src/widget.cs";
const SOURCE: &[u8] = b"// \xCE\xBB\nnamespace Demo;\npublic class Widget {}\n";
const START: u32 = 22;
const END: u32 = 45;

struct TempWorkspace(tempfile::TempDir);

impl TempWorkspace {
    fn new() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("nudox-csharp-native-sites-")
            .tempdir()
            .expect("create private C# site workspace");
        crate::test_support::make_private(directory.path()).expect("make workspace private");
        Self(directory)
    }
}

fn selected_source() -> (
    TempWorkspace,
    backend_engine::builtin::VerifiedNativeSource,
    [u8; 32],
) {
    let workspace = TempWorkspace::new();
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
    let mut daemon = crate::Locald::open_with_dispatcher_and_registry(
        workspace.0.path(),
        crate::builtin::BuiltinModel,
        crate::builtin::genesis().expect("product genesis"),
        dispatcher,
        backend_engine::DaemonConfig::default(),
        registry,
    )
    .expect("open product daemon");
    let package = PackageKey::from_value(LABEL);
    let project = package.to_bytes();
    let profile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
    let path_key = product_source_file_key(project, PATH);
    let content_identity = ContentId::<SourceFactDomain>::from_canonical_bytes(SOURCE);
    let disposition = NativePathDisposition::CSharpMalformed {
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
    let source = ProductSourceRecord::file_unavailable(
        project,
        PATH,
        SourceLanguage::CSharp,
        [0x61; 32],
        backend_library::SourceUnavailableReason::Unparsed,
    )
    .expect("unparsed structural source row");
    let mut version_hash = blake3::Hasher::new();
    version_hash.update(b"backend.project-snapshot.v2\0");
    version_hash.update(b"nudox.project-native-frontier-source-version.v1\0");
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
        version_hash.update(&(encoded.len() as u64).to_be_bytes());
        version_hash.update(&encoded);
    }
    version_hash.update(&path_key);
    version_hash.update(&[0; 32]);
    version_hash.update(&[0x61; 32]);
    let project_record =
        BuiltinPackageRecord::project(LABEL, *version_hash.finalize().as_bytes(), vec![path_key])
            .expect("one-file project membership");
    let changes = vec![
        BuiltinSourceChange {
            key: project,
            after: Some(project_record),
        },
        BuiltinSourceChange {
            key: path_key,
            after: Some(source),
        },
    ];
    let intent = BuiltinIntent::index_with_semantics(package, LABEL, changes, Vec::new())
        .expect("source rows intent")
        .with_native_frontier(native_changes)
        .expect("selected C# frontier intent");
    crate::builtin::commands::commit_builtin_intent(&mut daemon, 0x4353, &intent)
        .expect("owner commits source and native frontier");
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let selected = ProductSourceSnapshot::from_workspace(&snapshot).expect("selected snapshot");
    let paths = BTreeSet::from([PATH.to_owned()]);
    let frontier = selected
        .admit_native_frontier(project, profile, &paths)
        .expect("admit selected frontier")
        .expect("frontier exists");
    let verified = selected
        .verified_native_csharp_source(&frontier, PATH)
        .expect("source bytes come from selected frontier");
    (workspace, verified, project)
}

fn version(seed: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([seed; 16]),
        variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
        core_payload: backend_semantic::ir::CorePayloadHash::from_raw([seed.wrapping_add(2); 16]),
    }
}

fn image_for(
    bytes: &[u8],
    profile: LanguageProfile,
    tool: NativeTool,
    file: &str,
    owned: bool,
    extents: &[(u8, u32, u32)],
) -> Vec<u8> {
    let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(bytes);
    let mut builder = IrBuilder::new();
    builder.set_language_profile(profile).expect("profile");
    builder
        .set_image_provenance(
            SourceIdentity {
                identity,
                byte_len: u32::try_from(bytes.len()).expect("fixture fits"),
            },
            backend_semantic::vocabulary::CompileRecipeFact::derive(
                profile,
                Stage::LowerIr,
                tool,
                identity,
                ContentId::<ToolchainDomain>::from_canonical_bytes(b"C# site fixture toolchain"),
            ),
            PackageLineage::new("cargo", "csharp-native-site-fixture").expect("lineage"),
            PATH,
        )
        .expect("captured source provenance");
    let path = builder.intern_atom(file.as_bytes()).expect("file atom");
    let empty_types = builder
        .intern_type_parameters(&[])
        .expect("empty constraints");
    let empty_attributes = builder.intern_attributes(&[]).expect("empty attributes");
    let facts = CSharpFacts {
        nullability: CSharpNullability::Oblivious,
        reference_kind: CSharpReferenceKind::Value,
        constraints: empty_types,
        effects: CSharpMemberEffects {
            is_async: false,
            is_iterator: false,
            is_extension: false,
        },
        attributes: empty_attributes,
        partial: CSharpPartialRole::None,
        xml_provenance: None,
    };
    let versions = extents
        .iter()
        .map(|(seed, _, _)| version(*seed))
        .collect::<Vec<_>>();
    let authority = EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        publication: if owned {
            backend_semantic::ir::DeclarationPublicationRole::PrimaryOwned
        } else {
            backend_semantic::ir::DeclarationPublicationRole::ReferenceOnly
        },
        members: FactAvailability::Captured,
        documentation: FactAvailability::Captured,
        attributes: FactAvailability::Captured,
        visibility: FactAvailability::Captured,
        source: FactAvailability::Captured,
        source_file: FactAvailability::Captured,
        language_extension: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let items = extents
        .iter()
        .map(|(_, start, end)| TreeItemInput {
            name: b"Widget",
            anonymous_callable_anchor: None,
            kind: ItemKind::Record,
            visibility: Visibility::Public,
            authority,
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: SourceSpan::new(path, *start, *end),
            extension: Some(LanguageExtensionInput::CSharp(&facts)),
        })
        .collect::<Vec<_>>();
    builder
        .add_borrowed_tree(backend_semantic::ir::BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })
        .expect("fixture semantic entities");
    let ir = builder.finish().expect("finish image");
    let mut encoded =
        vec![0; backend_semantic::ir::full_semantic_image_len(&ir).expect("image size")];
    let written =
        backend_semantic::ir::encode_full_semantic_image(&ir, &mut encoded).expect("encode image");
    encoded.truncate(written);
    encoded
}

#[test]
fn csharp_sites_require_exact_authority_and_refuse_ambiguous_or_over_budget_claims() {
    let (_workspace, source, project) = selected_source();
    let profile = source.profile();
    let matching = image_for(
        source.bytes(),
        profile,
        NativeTool::CSharpCompiler,
        PATH,
        true,
        &[(1, START, END)],
    );
    let image = SemanticImageView::reopen(&matching).expect("captured C# image");
    let projection = |max_sites, max_working_bytes| NativeCSharpProjection {
        source: &source,
        max_sites,
        max_working_bytes,
    };
    let sites = prove_native_csharp_sites(&image, project, projection(8, 32 * 1024))
        .expect("bounded site proof");
    let identity = image
        .entity(backend_semantic::ir::EntityId::new(0))
        .unwrap()
        .version
        .identity();
    let site = sites.get(&identity).expect("verified C# declaration site");
    assert_eq!(site.line, 3);
    assert_eq!(site.excerpt().text(), Some("public class Widget {}\n"));

    for (bytes, other_profile, tool, file, claimed_project) in [
        (source.bytes(), profile, NativeTool::Clang, PATH, project),
        (
            source.bytes(),
            LanguageProfile::CSharp(CSharpVersion::CSharp13),
            NativeTool::CSharpCompiler,
            PATH,
            project,
        ),
        (
            source.bytes(),
            profile,
            NativeTool::CSharpCompiler,
            "src/other.cs",
            project,
        ),
        (
            source.bytes(),
            profile,
            NativeTool::CSharpCompiler,
            PATH,
            [0x99; 32],
        ),
    ] {
        let candidate = image_for(bytes, other_profile, tool, file, true, &[(2, START, END)]);
        let candidate = SemanticImageView::reopen(&candidate).expect("candidate image");
        let projected =
            prove_native_csharp_sites(&candidate, claimed_project, projection(8, 32 * 1024))
                .expect("mismatch yields no site");
        let identity = candidate
            .entity(backend_semantic::ir::EntityId::new(0))
            .unwrap()
            .version
            .identity();
        assert!(projected.get(&identity).is_none());
    }

    let mut wrong_bytes = source.bytes().to_vec();
    wrong_bytes[START as usize] = b'c';
    let wrong_identity = image_for(
        &wrong_bytes,
        profile,
        NativeTool::CSharpCompiler,
        PATH,
        true,
        &[(6, START, END)],
    );
    let wrong_identity = SemanticImageView::reopen(&wrong_identity).expect("wrong identity image");
    let projected = prove_native_csharp_sites(&wrong_identity, project, projection(8, 32 * 1024))
        .expect("identity mismatch yields no source site");
    let identity = wrong_identity
        .entity(backend_semantic::ir::EntityId::new(0))
        .unwrap()
        .version
        .identity();
    assert!(projected.get(&identity).is_none());

    let duplicate = image_for(
        source.bytes(),
        profile,
        NativeTool::CSharpCompiler,
        PATH,
        true,
        &[(3, START, END), (4, START, END)],
    );
    let duplicate = SemanticImageView::reopen(&duplicate).expect("duplicate span image");
    let projected = prove_native_csharp_sites(&duplicate, project, projection(8, 32 * 1024))
        .expect("ambiguous spans are suppressed");
    for index in 0..2 {
        let identity = duplicate
            .entity(backend_semantic::ir::EntityId::new(index))
            .unwrap()
            .version
            .identity();
        assert!(projected.get(&identity).is_none());
    }
    let reference_only = image_for(
        source.bytes(),
        profile,
        NativeTool::CSharpCompiler,
        PATH,
        false,
        &[(5, START, END)],
    );
    let reference_only = SemanticImageView::reopen(&reference_only).expect("reference-only image");
    let projected = prove_native_csharp_sites(&reference_only, project, projection(8, 32 * 1024))
        .expect("reference-only rows remain non-owned");
    let identity = reference_only
        .entity(backend_semantic::ir::EntityId::new(0))
        .unwrap()
        .version
        .identity();
    assert!(projected.get(&identity).is_none());
    assert!(prove_native_csharp_sites(&image, project, projection(0, 32 * 1024)).is_err());
    assert!(
        prove_native_csharp_sites(&image, project, projection(8, source.bytes().len() + 4095))
            .is_err()
    );
}
