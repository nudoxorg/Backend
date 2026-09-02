//! Projects source-bound javac declarations through the shared canonical fact lane.
//! Retains the full validated javac image until dedicated type and occurrence admission is added.
//! Never recovers Java facts by scanning source text or a native parser fallback.

use compiler_ir::{EntityKind, SemanticProductConstructor};
use compiler_languages_java::{BoundImageError, DeclarationKind, JavaAuthorityImage, JavaRelease};
use compiler_vocabulary::{JavaRelease as ProfileRelease, LoweringUnsupported};
use sha2::Digest;

use crate::lower::{FactSet, FactType, LEAF_PRODUCT, SemanticFact, push_fact};

/// Exact rejection while lending source-bound javac declaration facts.
#[derive(Debug)]
pub(crate) enum JavaCollectError {
    /// The outer source-binding envelope or embedded javac image failed validation.
    Image(BoundImageError),
    /// The image was produced for a Java release other than the selected profile.
    Release {
        /// Requested compiler profile release.
        requested: ProfileRelease,
        /// Release retained by the attributed `JavacTask` image.
        observed: JavaRelease,
    },
    /// The source-bound image belongs to other source bytes than this compile request.
    SourceBinding {
        /// SHA-256 digest of exact compile request bytes.
        expected: [u8; 32],
        /// SHA-256 digest retained by the source-bound javac image.
        observed: [u8; 32],
    },
    /// The bounded canonical lane rejected an authority declaration.
    Lowering(LoweringUnsupported),
}

/// Streams all typed javac declarations into the shared fact lane.
pub(crate) fn collect<'source>(
    profile: ProfileRelease,
    source: &'source [u8],
    image_bytes: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), JavaCollectError> {
    let authority = JavaAuthorityImage::open(image_bytes).map_err(JavaCollectError::Image)?;
    if !release_matches(profile, authority.image.release) {
        return Err(JavaCollectError::Release {
            requested: profile,
            observed: authority.image.release,
        });
    }
    let expected: [u8; 32] = sha2::Sha256::digest(source).into();
    let observed = authority.source_digest();
    if observed != expected {
        return Err(JavaCollectError::SourceBinding { expected, observed });
    }
    for declaration in authority.image.declarations() {
        let declaration =
            declaration.map_err(|cause| JavaCollectError::Image(BoundImageError::Image(cause)))?;
        let kind = entity_kind(declaration.kind);
        push_fact(
            facts,
            SemanticFact::new(
                kind,
                declaration.name.bytes,
                FactType::Opaque,
                constructor(kind),
            ),
        )
        .map_err(JavaCollectError::Lowering)?;
    }
    Ok(())
}

const fn release_matches(profile: ProfileRelease, authority: JavaRelease) -> bool {
    matches!(
        (profile, authority),
        (ProfileRelease::Java8, JavaRelease::Java8)
            | (ProfileRelease::Java11, JavaRelease::Java11)
            | (ProfileRelease::Java17, JavaRelease::Java17)
            | (ProfileRelease::Java21, JavaRelease::Java21)
            | (ProfileRelease::Java25, JavaRelease::Java25)
    )
}

const fn entity_kind(kind: DeclarationKind) -> EntityKind {
    match kind {
        DeclarationKind::Module | DeclarationKind::Package => EntityKind::Module,
        DeclarationKind::Class | DeclarationKind::Record => EntityKind::Record,
        DeclarationKind::Interface | DeclarationKind::Annotation => EntityKind::Trait,
        DeclarationKind::Enum => EntityKind::Enum,
        DeclarationKind::Field => EntityKind::Field,
        DeclarationKind::EnumConstant => EntityKind::Variant,
        DeclarationKind::Constructor | DeclarationKind::Method => EntityKind::Function,
    }
}

const fn constructor(kind: EntityKind) -> SemanticProductConstructor {
    match kind {
        EntityKind::Function => SemanticProductConstructor::function(0, 0),
        EntityKind::Record | EntityKind::Module => SemanticProductConstructor::PRODUCT,
        EntityKind::Trait => SemanticProductConstructor::INTERSECTION,
        EntityKind::Enum => SemanticProductConstructor::UNION,
        EntityKind::Constant
        | EntityKind::Field
        | EntityKind::Alias
        | EntityKind::Implementation
        | EntityKind::Variant
        | EntityKind::Static
        | EntityKind::Reexport
        | EntityKind::Parameter => LEAF_PRODUCT,
    }
}
