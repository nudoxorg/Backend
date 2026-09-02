//! Admits rust-analyzer HIR declarations into the shared canonical fact lane.
//! Keeps Cargo-root selection explicit and borrows every identifier from authority source.
//! Leaves HIR type, reference, and documentation planes explicitly unadmitted for now.

use std::sync::atomic::AtomicBool;

use compiler_ir::{EntityKind, SemanticProductConstructor};
use compiler_languages_rust::{
    ByteSpan, RustAnalysisControl, RustAuthorityError, RustProject, SemanticKind, SourceByteLimit,
};

use crate::lower::{FactSet, LEAF_PRODUCT, SemanticFact, push_fact};

/// Exact direct-authority rejection while rust-analyzer HIR is borrowed.
#[derive(Debug)]
pub(crate) enum RustCollectError {
    /// rust-analyzer could not open, resolve, or query the selected Cargo graph.
    Authority(RustAuthorityError),
    /// Canonical admission rejected one borrowed HIR declaration.
    Lowering(compiler_vocabulary::LoweringUnsupported),
}

/// Runs a non-escaping rust-analyzer transaction and emits borrowed declaration facts.
///
/// The selected Cargo project is caller-owned. This function never derives a
/// project root, invokes rustc, scans source bytes, or substitutes syntax
/// tokens for HIR definitions.
pub(crate) fn collect<'source>(
    project: &RustProject,
    maximum_source_bytes: SourceByteLimit,
    cancelled: &AtomicBool,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), RustCollectError> {
    project
        .analyze(
            RustAnalysisControl {
                cancelled,
                maximum_source_bytes,
            },
            |authority| {
                if authority.source != source {
                    return Err(RustAuthorityError::SourceBinding {
                        expected: source.len(),
                        observed: authority.source.len(),
                    });
                }
                for declaration in authority.declarations() {
                    let name = authority.declaration_name(&declaration)?;
                    let name = requested_source_at(source, name)?;
                    push_fact(
                        facts,
                        SemanticFact::new(
                            entity_kind(declaration.kind)?,
                            name, 
                            constructor(declaration.kind)?,
                        ),
                    )
                    .map_err(|cause| RustAuthorityError::Admission { cause })?;
                }
                Ok(())
            },
        )
        .map_err(|cause| match cause {
            RustAuthorityError::Admission { cause } => RustCollectError::Lowering(cause),
            cause => RustCollectError::Authority(cause),
        })
}

fn requested_source_at(source: &[u8], span: ByteSpan) -> Result<&[u8], RustAuthorityError> {
    let start = usize::try_from(span.start)
        .map_err(|source| RustAuthorityError::Coordinate { span, source })?;
    let end = usize::try_from(span.end)
        .map_err(|source| RustAuthorityError::Coordinate { span, source })?;
    source
        .get(start..end)
        .ok_or(RustAuthorityError::InvalidSpan {
            span,
            source_bytes: source.len(),
        })
}

fn entity_kind(kind: SemanticKind) -> Result<EntityKind, RustAuthorityError> {
    match kind {
        SemanticKind::Module => Ok(EntityKind::Module),
        SemanticKind::Function => Ok(EntityKind::Function),
        SemanticKind::Field => Ok(EntityKind::Field),
        SemanticKind::Record => Ok(EntityKind::Record),
        SemanticKind::Enum => Ok(EntityKind::Enum),
        SemanticKind::Trait => Ok(EntityKind::Trait),
        SemanticKind::Implementation => Ok(EntityKind::Implementation),
        SemanticKind::TypeAlias => Ok(EntityKind::Alias),
        SemanticKind::Constant => Ok(EntityKind::Constant),
        SemanticKind::Static => Ok(EntityKind::Static),
        SemanticKind::Variant => Ok(EntityKind::Variant),
        SemanticKind::Macro
        | SemanticKind::LocalBinding
        | SemanticKind::GenericParameter
        | SemanticKind::Builtin => Err(RustAuthorityError::MissingSemanticFact { fact: kind }),
    }
}

fn constructor(kind: SemanticKind) -> Result<SemanticProductConstructor, RustAuthorityError> {
    match kind {
        SemanticKind::Function => Ok(SemanticProductConstructor::function(0, 0)),
        SemanticKind::Record
        | SemanticKind::Module
        | SemanticKind::Field
        | SemanticKind::Implementation => Ok(SemanticProductConstructor::PRODUCT),
        SemanticKind::Enum => Ok(SemanticProductConstructor::UNION),
        SemanticKind::Trait => Ok(SemanticProductConstructor::INTERSECTION),
        SemanticKind::TypeAlias
        | SemanticKind::Constant
        | SemanticKind::Static
        | SemanticKind::Variant => Ok(LEAF_PRODUCT),
        SemanticKind::Macro
        | SemanticKind::LocalBinding
        | SemanticKind::GenericParameter
        | SemanticKind::Builtin => Err(RustAuthorityError::MissingSemanticFact { fact: kind }),
    }
}
