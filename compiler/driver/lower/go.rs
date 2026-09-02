//! Projects validated `go/packages` declaration facts into the canonical lane.
//! Binds every borrowed image to the exact configured Go source bytes.
//! Leaves richer Go type and reference planes explicit until their shared admission lands.

use compiler_ir::{EntityKind, SemanticProductConstructor};
use compiler_languages_go::{DeclarationKind, GoImage, ImageError};
use compiler_vocabulary::LoweringUnsupported;
use sha2::{Digest, Sha256};

use crate::lower::{FactSet, FactType, LEAF_PRODUCT, SemanticFact, push_fact};

/// Exact rejection while borrowing one validated Go authority image.
#[derive(Debug)]
pub(crate) enum GoCollectError {
    /// The fixed binary image failed structural or checksum validation.
    Image(ImageError),
    /// The image's producer-bound source digest differs from the compile source.
    SourceBinding {
        /// SHA-256 of the exact compile request bytes.
        expected: [u8; 32],
        /// SHA-256 lent by the Go authority image header.
        observed: [u8; 32],
    },
    /// The bounded canonical lane cannot admit every authority declaration.
    Lowering(LoweringUnsupported),
}

/// Streams all `go/packages` package-scope declarations through the shared fact lane.
pub(crate) fn collect<'source>(
    source: &'source [u8],
    image_bytes: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), GoCollectError> {
    let image = GoImage::open(image_bytes).map_err(GoCollectError::Image)?;
    let expected = Sha256::digest(source).into();
    let observed = image.source_digest();
    if observed != expected {
        return Err(GoCollectError::SourceBinding { expected, observed });
    }
    for declaration in image.declarations() {
        let declaration = declaration.map_err(GoCollectError::Image)?;
        let kind = entity_kind(declaration.kind);
        push_fact(
            facts,
            SemanticFact::new(kind, declaration.name, FactType::Opaque, constructor(kind)),
        )
        .map_err(GoCollectError::Lowering)?;
    }
    Ok(())
}

const fn entity_kind(kind: DeclarationKind) -> EntityKind {
    match kind {
        DeclarationKind::Type => EntityKind::Record,
        DeclarationKind::Alias => EntityKind::Alias,
        DeclarationKind::Function => EntityKind::Function,
        DeclarationKind::Constant => EntityKind::Constant,
        DeclarationKind::Static => EntityKind::Static,
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

#[cfg(test)]
mod tests {
    use compiler_ir::EntityKind;
    use sha2::{Digest, Sha256};

    use super::{FactSet, GoCollectError, collect};

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("Go authority image collection failed: {0:?}")]
        Collect(GoCollectError),
        #[error("Go authority declaration kind was not admitted")]
        Kind,
    }

    #[test]
    fn borrowed_go_authority_declaration_fills_the_shared_fact_lane() -> Result<(), TestError> {
        let source = b"package demo\nfunc Brew() {}\n";
        let image = fixture(source);
        let mut facts = FactSet::new();
        collect(source, &image, &mut facts).map_err(TestError::Collect)?;
        if facts.kind_at(0) == Some(EntityKind::Function) {
            Ok(())
        } else {
            Err(TestError::Kind)
        }
    }

    fn fixture(source: &[u8]) -> Vec<u8> {
        const HEADER: usize = 88;
        const ROW: usize = 12;
        const DOMAIN: &[u8] = b"nudox.go.authority.image.sha256.v1\0";
        let name = b"Brew";
        let mut image = vec![0; HEADER + ROW + name.len()];
        image[..4].copy_from_slice(b"NGAI");
        image[4..6].copy_from_slice(&1_u16.to_le_bytes());
        image[6..8].copy_from_slice(&(HEADER as u16).to_le_bytes());
        image[8..12].copy_from_slice(&1_u32.to_le_bytes());
        image[12..16].copy_from_slice(&(name.len() as u32).to_le_bytes());
        image[16..20].copy_from_slice(&((ROW + name.len()) as u32).to_le_bytes());
        image[20..52].copy_from_slice(Sha256::digest(source).as_slice());
        image[HEADER] = 3;
        image[HEADER + 1] = 1;
        image[HEADER + 8..HEADER + 12].copy_from_slice(&(name.len() as u32).to_le_bytes());
        image[HEADER + ROW..].copy_from_slice(name);
        let mut digest = Sha256::new();
        digest.update(DOMAIN);
        digest.update(&image[..52]);
        digest.update(&image[84..HEADER]);
        digest.update(&image[HEADER..]);
        image[52..84].copy_from_slice(digest.finalize().as_slice());
        image
    }
}
