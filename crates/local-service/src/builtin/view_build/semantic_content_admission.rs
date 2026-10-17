//! Projection keeps canonical admission even when it does not request canonical bytes.
#![allow(clippy::expect_used)]

use super::semantic::semantic_row_content;
use backend_engine::application::DocumentationSession;
use backend_engine::{Fragment, package_key};
use backend_semantic::ir::{
    BorrowedTree, BuiltinType, CanonicalTypeRenderLimits, ConcreteType, CorePayloadHash,
    DeclarationFamilyId, DocInput, EntityAuthorityFacts, EntityId, EntityVersion, FactAvailability,
    IrBuilder, ItemKind, ParentageAuthority, SemanticDocumentError, SemanticImageView,
    SourceIdentity, TreeEntityId, TreeItemInput, TreeLinkTarget, TypeExpr, VariantFingerprint,
    Visibility, encode_full_semantic_image, full_semantic_image_len, prepare_semantic_document,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, PythonVersion, RustEdition, Stage,
};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use std::num::NonZeroUsize;

const PROFILE: LanguageProfile = LanguageProfile::Rust(RustEdition::Rust2024);

fn fixture(docs: &[DocInput<'_>], depth: usize) -> Vec<u8> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"fixture-source"),
        byte_len: 14,
    };
    let recipe = CompileRecipeFact::derive(
        PROFILE,
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture-toolchain"),
    );
    let mut builder = IrBuilder::new();
    builder
        .set_image_provenance_for_package(
            source,
            recipe,
            &PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned()).expect("package"),
            "src/worker.rs",
        )
        .expect("provenance");
    let mut ty = builder
        .intern_type(TypeExpr::Concrete(ConcreteType::Builtin(BuiltinType::Bool)))
        .expect("bool");
    for _ in 0..depth {
        ty = builder
            .intern_type(TypeExpr::Concrete(ConcreteType::Optional(ty)))
            .expect("optional");
    }
    let authority = EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        visibility: FactAvailability::Captured,
        documentation: if docs.is_empty() {
            FactAvailability::Unavailable
        } else {
            FactAvailability::Captured
        },
        semantic_type: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let item = TreeItemInput {
        name: b"enabled",
        anonymous_callable_anchor: None,
        kind: ItemKind::Static,
        visibility: Visibility::Public,
        authority,
        parent: None,
        semantic_type: Some(ty),
        members: &[],
        docs,
        attributes: &[],
        source: None,
        extension: None,
    };
    let target = TreeItemInput {
        name: b"target",
        docs: &[],
        authority: EntityAuthorityFacts {
            documentation: FactAvailability::Unavailable,
            ..authority
        },
        ..item
    };
    let version = |n| EntityVersion {
        family: DeclarationFamilyId::from_raw([n; 16]),
        variant: VariantFingerprint::from_raw([n; 16]),
        core_payload: CorePayloadHash::from_raw([n; 16]),
    };
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &[version(1), version(2)],
            items: &[item, target],
            links: &[],
        })
        .expect("tree");
    let ir = builder.finish().expect("finish");
    let mut bytes = vec![0; full_semantic_image_len(&ir).expect("image length")];
    encode_full_semantic_image(&ir, &mut bytes).expect("encode");
    bytes
}

#[test]
fn admitted_content_preserves_unicode_fragments_signature_and_exact_link_identity() {
    let docs = [
        DocInput::Text("π prose"),
        DocInput::SoftBreak,
        DocInput::Code("enabled = true"),
        DocInput::HardBreak,
        DocInput::Link {
            label: "destination",
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
        },
    ];
    let bytes = fixture(&docs, 0);
    let image = SemanticImageView::reopen(&bytes).expect("reopen");
    let session = DocumentationSession::new(&image);
    let entity = session.entity(EntityId::new(0)).expect("entity");
    let target = session.entity(EntityId::new(1)).expect("target");
    let prepared = prepare_semantic_document(
        PROFILE,
        &image,
        entity.entity.id,
        CanonicalTypeRenderLimits::new(
            NonZeroUsize::new(super::MAX_SEMANTIC_TYPE_DEPTH).expect("depth"),
        ),
    )
    .expect("admission");
    let mut canonical = vec![0; prepared.encoded_len];
    prepared
        .write_into(&mut canonical)
        .expect("canonical bytes remain available to byte consumers");
    let content = semantic_row_content(PROFILE, &image, &entity, package_key("fixture"), [9; 32])
        .expect("content");
    assert_eq!(
        content.document,
        vec![
            Fragment::Text("π prose".to_owned()),
            Fragment::Break,
            Fragment::Code("enabled = true".to_owned()),
            Fragment::Break,
            Fragment::Link {
                label: "destination".to_owned(),
                target: super::identity::semantic_symbol(
                    package_key("fixture"),
                    target.entity.version.identity()
                ),
            }
        ]
    );
    assert_eq!(content.signature.as_deref(), Some("bool"));
    assert!(content.documentation_targets.is_empty());
}

#[test]
fn projection_still_rejects_wrong_profile_and_excessive_type_depth() {
    for (depth, profile) in [
        (0, LanguageProfile::Python(PythonVersion::Python314)),
        (super::MAX_SEMANTIC_TYPE_DEPTH + 1, PROFILE),
    ] {
        let bytes = fixture(&[], depth);
        let image = SemanticImageView::reopen(&bytes).expect("reopen");
        let entity = DocumentationSession::new(&image)
            .entity(EntityId::new(0))
            .expect("entity");
        let error = prepare_semantic_document(
            profile,
            &image,
            entity.entity.id,
            CanonicalTypeRenderLimits::new(
                NonZeroUsize::new(super::MAX_SEMANTIC_TYPE_DEPTH).expect("depth"),
            ),
        )
        .err()
        .expect("admission must fail");
        if depth == 0 {
            assert!(matches!(
                error,
                SemanticDocumentError::ProfileMismatch { .. }
            ));
        } else {
            assert!(matches!(error, SemanticDocumentError::CanonicalType { .. }));
        }
        assert!(
            semantic_row_content(profile, &image, &entity, package_key("fixture"), [9; 32])
                .is_err()
        );
    }
}

#[test]
fn projection_still_bounds_the_canonical_document_before_projecting_text() {
    let prose = "x".repeat(super::MAX_SEMANTIC_DOCUMENT_BYTES);
    let bytes = fixture(&[DocInput::Text(&prose)], 0);
    let image = SemanticImageView::reopen(&bytes).expect("reopen");
    let entity = DocumentationSession::new(&image)
        .entity(EntityId::new(0))
        .expect("entity");
    let error = semantic_row_content(PROFILE, &image, &entity, package_key("fixture"), [9; 32])
        .err()
        .expect("size admission");
    assert_eq!(
        error.to_string(),
        "project semantic document exceeds its output bound"
    );
}
