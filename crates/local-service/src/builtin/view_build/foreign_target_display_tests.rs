//! Foreign names remain presentation; native endpoint and source authority do not change.

#![allow(clippy::expect_used, clippy::too_many_lines)]

use super::call_join::foreign_display_name;
use super::identity::external_semantic_symbol;
use super::semantic::project_image_rows;
use backend_engine::application::DocumentationSession;
use backend_engine::{Row, RowId, SourceAvailability, SourceExcerpt};
use backend_semantic::ir::{
    BorrowedTree, Confidence, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityId,
    EntityVersion, ExternalDeclarationIdentity, ExternalId, ExternalTarget, ExternalTargetIdentity,
    FactAvailability, ForeignDeclarationId, ForeignExternalTarget, ForeignTargetOrigin, IrBuilder,
    ItemKind, LinkKind, LinkTarget, OccurrenceAuthorityFacts, ParentageAuthority,
    SemanticCoreReader as _, SemanticImageView, SemanticReader as _, SourceIdentity, TreeEntityId,
    TreeItemInput, TreeLinkInput, TreeLinkTarget, VariantAvailability, VariantFingerprint,
    Visibility, encode_full_semantic_image, full_semantic_image_len,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, PythonVersion, RustEdition, Stage,
};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use std::collections::BTreeSet;
use std::sync::Arc;

const HTTP_IMAGE: &[u8] = include_bytes!("fixtures/httpie-retained/context.nxfi");
const HTTP_SOURCE: &[u8] = include_bytes!("fixtures/httpie-retained/context.py");
const HTTP_PROJECT: &str = "/Users/mileswirht/Downloads/nudox-demo-httpie";

fn project(label: &str) -> super::super::IndexedProject {
    super::super::IndexedProject {
        package: backend_engine::package_key(label),
        label: label.to_owned(),
        files: Arc::from([]),
    }
}

fn assert_foreign_source_refusal(row: &Row) {
    assert!(matches!(row.id, RowId::Symbol(_)));
    assert_eq!(row.package, None);
    assert_eq!(row.parent, None);
    assert_eq!(row.kind, None);
    assert_eq!(row.signature, None);
    assert_eq!(row.source, SourceAvailability::NotCaptured);
    assert_eq!(row.excerpt, SourceExcerpt::NotCaptured);
}

#[test]
fn retained_httpie_environment_ten_outgoing_foreign_names_keep_original_native_keys() {
    let image = SemanticImageView::reopen(HTTP_IMAGE).expect("retained original native image");
    let session = DocumentationSession::new(&image);
    let environment = session
        .entity(EntityId::new(71))
        .expect("original Environment");
    assert_eq!(
        super::semantic_display_name(environment.name).expect("name"),
        "Environment"
    );
    let identity = environment.entity.version.identity();
    assert_eq!(
        format!(
            "{}{}",
            backend_engine::encode_id(identity.family.as_bytes()),
            backend_engine::encode_id(identity.variant.as_bytes())
        ),
        "ee415fc203e1cb2ba7464c299efee7d6accc6b5ccdcbcedc6a878b43c65cf29d"
    );
    let project = project(HTTP_PROJECT);
    assert_eq!(
        backend_engine::encode_id(project.package.as_bytes()),
        "47262d6cd652dbb4b20e38e2b2c11dce7f0dda41ec3e2ebe2083b7ed1cc4cc9e"
    );
    let digest = *blake3::hash(HTTP_IMAGE).as_bytes();
    let (base, _) = super::super::initial_view().expect("initial basis");
    let projected = project_image_rows(
        &image,
        digest,
        &project,
        LanguageProfile::Python(PythonVersion::Python314),
        base.basis(),
        false,
        "httpie/context.py",
        &[],
    )
    .expect("actual image through current row projector");
    let expected = [
        ("initialise", LinkKind::Reads, 2124, 2134),
        ("error", LinkKind::Reads, 1979, 1984),
        ("isatty", LinkKind::MethodCall, 1436, 1442),
        ("setupterm", LinkKind::MethodCall, 1890, 1899),
        ("tigetnum", LinkKind::MethodCall, 1934, 1942),
        ("Namespace", LinkKind::MethodCall, 1249, 1258),
        ("wrap_stream", LinkKind::MethodCall, 2135, 2146),
        ("stderr", LinkKind::Reads, 1619, 1625),
        ("stdin", LinkKind::Reads, 1367, 1372),
        ("stdout", LinkKind::Reads, 1517, 1523),
    ];
    let links = image.links_from(environment.entity.id).collect::<Vec<_>>();
    assert_eq!(links.len(), expected.len());
    let mut keys = BTreeSet::new();
    for ((_, link), (name, kind, start, end)) in links.into_iter().zip(expected) {
        assert_eq!(link.kind, kind);
        let external = match link.target {
            LinkTarget::External(external) => Some(external),
            LinkTarget::Local(_) => None,
        }
        .expect("original target must stay external");
        assert!(matches!(
            image.external(external),
            Some(ExternalTarget::Foreign(_))
        ));
        assert_eq!(
            foreign_display_name(&image, external)
                .expect("captured spelling")
                .as_deref(),
            Some(name)
        );
        let site = link.source.expect("native occurrence span");
        assert_eq!(
            image.atom(site.file()).expect("source path"),
            b"httpie/context.py"
        );
        assert_eq!((site.start(), site.end()), (start, end));
        assert_eq!(&HTTP_SOURCE[start as usize..end as usize], name.as_bytes());
        let target = ExternalTargetIdentity::capture(&image, external)
            .expect("exact native external identity");
        let symbol = external_semantic_symbol(project.package, digest, target);
        let row = &projected
            .rows
            .iter()
            .find(|projected| projected.row.id == RowId::Symbol(symbol))
            .expect("foreign target row")
            .row;
        assert_eq!(row.label, name);
        assert_foreign_source_refusal(row);
        // The previous generic display and the corrected spelling select
        // the same producer key; neither spelling is used as its preimage.
        let old_display = Row::new(row.id, row.basis, "external semantic target");
        assert_eq!(old_display.id, row.id);
        assert_ne!(old_display.label, row.label);
        assert!(keys.insert(backend_engine::encode_id(symbol.as_bytes())));
    }
    let original_cli_keys = [
        "1f3f0be4478240b09215db531191fcde4a83fc4b8a8b1fa8f333b42a0b82b697",
        "34bdbb537f830fa49bd86220bc084475c48fb6946a5efab591f33f1b7e35f092",
        "39f2c8725d6b5c4de3fddfa50e7b4b264a5f1218c1187278952122afd2551c73",
        "3cca7c6e5fffd6ec7b5e794485a2d5897c4b93945a1daae371d56d004960666c",
        "6fc5796a08a8c3ee09c72cd1cb7ef93fce2f4fc0301ee17c5cd0b9676a4ebac4",
        "79adeaaeaf4b0d8a4e30744b6d78214d478603e9ad639982ffa3d0b7b6845e49",
        "97ba8b2e991e8371106c206402d50c1013d8bd3b556c8e47b984fb4896631214",
        "ba8d438968d96b9342d91ede821dde35aa9113e400fda97f8e9613d13a068aec",
        "f5e73fabdbe2519de24f5d89d585767cf6b0e6abe711aaaccd8db5db8f8004d4",
        "fe86519a18585296093bb87718203c430dd0978e5d4639422b99b3f5a2f7cc55",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert_eq!(keys, original_cli_keys);
}

fn foreign_fixture() -> Vec<u8> {
    let profile = LanguageProfile::Rust(RustEdition::Rust2024);
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"caller"),
        byte_len: 6,
    };
    let recipe = CompileRecipeFact::derive(
        profile,
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"foreign-display-fixture"),
    );
    let coordinate =
        PackageUrl::parse("pkg:cargo/foreign-display-fixture@1.0.0".to_owned()).expect("package");
    let mut builder = IrBuilder::new();
    builder
        .set_image_provenance_for_package(source, recipe, &coordinate, "caller.rs")
        .expect("provenance");
    let ecosystem = builder.intern_atom(b"cargo").expect("ecosystem");
    let path = builder.intern_atom(b"foreign").expect("path");
    let mut links = Vec::new();
    for (key, spelling) in [(1, &b"same"[..]), (2, &b"same"[..]), (3, &b""[..])] {
        let display = builder.intern_atom(spelling).expect("display atom");
        let target = builder
            .intern_external(ExternalTarget::Foreign(ForeignExternalTarget {
                identity: ExternalDeclarationIdentity {
                    foreign: ForeignDeclarationId::from_raw([key; 16]),
                    variant: VariantAvailability::Unavailable,
                },
                origin: ForeignTargetOrigin::Universe { ecosystem },
                path,
                display,
                kind: None,
            }))
            .expect("distinct native foreign declaration");
        links.push(TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::External(target),
            kind: LinkKind::Calls,
            confidence: Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Unavailable,
            },
            source: None,
        });
    }
    let versions = [EntityVersion {
        family: DeclarationFamilyId::from_raw([10; 16]),
        variant: VariantFingerprint::from_raw([11; 16]),
        core_payload: CorePayloadHash::from_raw([12; 16]),
    }];
    let items = [TreeItemInput {
        name: b"caller",
        anonymous_callable_anchor: None,
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
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
    }];
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &links,
        })
        .expect("tree");
    let ir = builder.finish().expect("image");
    let mut bytes = vec![0; full_semantic_image_len(&ir).expect("length")];
    encode_full_semantic_image(&ir, &mut bytes).expect("encode");
    bytes
}

#[test]
fn duplicate_foreign_names_keep_distinct_native_ids_and_empty_name_stays_unknown() {
    let bytes = foreign_fixture();
    let image = SemanticImageView::reopen(&bytes).expect("validated fixture");
    let project = project("foreign-display-fixture");
    let (base, _) = super::super::initial_view().expect("initial basis");
    let projected = project_image_rows(
        &image,
        *blake3::hash(&bytes).as_bytes(),
        &project,
        LanguageProfile::Rust(RustEdition::Rust2024),
        base.basis(),
        false,
        "caller.rs",
        &[],
    )
    .expect("project");
    let same = projected
        .rows
        .iter()
        .filter(|row| row.row.label == "same")
        .collect::<Vec<_>>();
    assert_eq!(same.len(), 2);
    assert_ne!(same[0].row.id, same[1].row.id);
    let unknown = projected
        .rows
        .iter()
        .filter(|row| row.row.label == "external semantic target")
        .collect::<Vec<_>>();
    assert_eq!(unknown.len(), 1);
    for row in same.into_iter().chain(unknown) {
        assert_foreign_source_refusal(&row.row);
    }
}

#[test]
fn missing_external_ordinal_cannot_manufacture_a_named_foreign_identity() {
    let bytes = foreign_fixture();
    let image = SemanticImageView::reopen(&bytes).expect("validated fixture");
    let missing = ExternalId::new(u32::MAX);
    assert!(image.external(missing).is_none());
    assert!(
        foreign_display_name(&image, missing)
            .expect("no display fact")
            .is_none()
    );
    assert!(ExternalTargetIdentity::capture(&image, missing).is_err());
}
