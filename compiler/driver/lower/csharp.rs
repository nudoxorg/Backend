//! Projects validated Roslyn declarations into the shared canonical fact lane.
//! Verifies image source binding and source-backed identifier coordinates before admission.
//! Keeps unadmitted Roslyn type, member, documentation, and reference planes explicit.

use compiler_ir::{EntityKind, SemanticProductConstructor};
use compiler_languages_csharp::{CSharpImage, DeclarationKind, ImageError};
use compiler_vocabulary::LoweringUnsupported;
use sha2::{Digest, Sha256};

use crate::lower::{FactSet, FactType, LEAF_PRODUCT, SemanticFact, push_fact};

/// Exact rejection while lending source-bound Roslyn declaration rows.
#[derive(Debug)]
pub(crate) enum CSharpCollectError {
    /// The fixed binary authority image failed validation.
    Image(ImageError),
    /// The image's Roslyn source digest differs from the compile request.
    SourceBinding {
        /// SHA-256 digest of exact compile request bytes.
        expected: [u8; 32],
        /// SHA-256 digest retained by the authority image.
        observed: [u8; 32],
    },
    /// A validated declaration name span cannot name the exact source bytes.
    Span { start: u32, end: u32 },
    /// A bounded canonical lane rejected an authority declaration.
    Lowering(LoweringUnsupported),
}

/// Streams Roslyn-selected source declarations into the shared canonical lane.
pub(crate) fn collect<'source>(
    source: &'source [u8],
    image_bytes: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), CSharpCollectError> {
    let image = CSharpImage::open(image_bytes).map_err(CSharpCollectError::Image)?;
    let expected: [u8; 32] = Sha256::digest(source).into();
    let observed = image.source_digest();
    if observed != expected {
        return Err(CSharpCollectError::SourceBinding { expected, observed });
    }
    for declaration in image.declarations() {
        let declaration = declaration.map_err(CSharpCollectError::Image)?;
        let start = usize::try_from(declaration.start).map_err(|_| CSharpCollectError::Span {
            start: declaration.start,
            end: declaration.end,
        })?;
        let end = usize::try_from(declaration.end).map_err(|_| CSharpCollectError::Span {
            start: declaration.start,
            end: declaration.end,
        })?;
        let Some(name) = source.get(start..end) else {
            return Err(CSharpCollectError::Span {
                start: declaration.start,
                end: declaration.end,
            });
        };
        if name != declaration.name {
            return Err(CSharpCollectError::Span {
                start: declaration.start,
                end: declaration.end,
            });
        }
        let kind = entity_kind(declaration.kind);
        push_fact(
            facts,
            SemanticFact::new(kind, name, FactType::Opaque, constructor(kind)),
        )
        .map_err(CSharpCollectError::Lowering)?;
    }
    Ok(())
}

const fn entity_kind(kind: DeclarationKind) -> EntityKind {
    match kind {
        DeclarationKind::Class
        | DeclarationKind::Struct
        | DeclarationKind::Record
        | DeclarationKind::RecordStruct => EntityKind::Record,
        DeclarationKind::Interface => EntityKind::Trait,
        DeclarationKind::Enum => EntityKind::Enum,
        DeclarationKind::Delegate => EntityKind::Function,
    }
}

const fn constructor(kind: EntityKind) -> SemanticProductConstructor {
    match kind {
        EntityKind::Function => SemanticProductConstructor::function(0, 0),
        EntityKind::Record => SemanticProductConstructor::PRODUCT,
        EntityKind::Trait => SemanticProductConstructor::INTERSECTION,
        EntityKind::Enum => SemanticProductConstructor::UNION,
        EntityKind::Constant
        | EntityKind::Module
        | EntityKind::Field
        | EntityKind::Alias
        | EntityKind::Implementation
        | EntityKind::Variant
        | EntityKind::Static
        | EntityKind::Reexport
        | EntityKind::Parameter => LEAF_PRODUCT,
    }
}
