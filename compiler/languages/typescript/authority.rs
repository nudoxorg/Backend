//! Builds one in-process OXC syntax-and-binding authority from caller-owned source.
//! Holds the resulting program facts in the caller's arena for a single lowering transaction.
//! Selects grammar solely from the closed compiler TypeScript profile.

use compiler_vocabulary::TypeScriptSource;
use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_semantic::{Semantic, SemanticBuilder};
use oxc_span::SourceType;
use oxc_syntax::module_record::ModuleRecord;

use crate::AuthorityError;

/// One parsed and lexically resolved TypeScript or TSX module.
///
/// The OXC values borrow `source` and `arena`; callers must lower them before
/// dropping either owner. The fields are deliberately direct so downstream
/// lowering can use OXC's typed vocabulary without another data-transfer DTO.
pub struct OxcModule<'source> {
    /// Exact caller-owned source retained by OXC spans and diagnostics.
    pub source: &'source str,
    /// Closed grammar profile used to parse this source.
    pub profile: TypeScriptSource,
    /// OXC's resolved ESM/CommonJS import and export record.
    pub module_record: ModuleRecord<'source>,
    /// OXC lexical scopes, symbols, references, syntax nodes, and diagnostics-free bindings.
    pub semantic: Semantic<'source>,
}

/// Parses and lexically resolves `source` under one closed TypeScript profile.
///
/// Parser and resolver diagnostics remain typed, complete, and distinct. This
/// boundary intentionally does not claim TypeScript checker-derived types;
/// those require the future typed TSZ authority adapter.
///
/// # Errors
///
/// Returns every OXC parser diagnostic as [`AuthorityError::Syntax`] or every
/// lexical-resolution diagnostic as [`AuthorityError::Binding`].
pub fn analyze<'source>(
    profile: TypeScriptSource,
    source: &'source str,
    arena: &'source Allocator,
) -> Result<OxcModule<'source>, AuthorityError> {
    let parsed = Parser::new(arena, source, source_type(profile)).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(AuthorityError::Syntax {
            diagnostics: parsed.diagnostics,
        });
    }

    let program = arena.alloc(parsed.program);
    let checked = SemanticBuilder::new_compiler()
        .with_build_nodes(true)
        .build(program);
    if !checked.diagnostics.is_empty() {
        return Err(AuthorityError::Binding {
            diagnostics: checked.diagnostics,
        });
    }

    Ok(OxcModule {
        source,
        profile,
        module_record: parsed.module_record,
        semantic: checked.semantic,
    })
}

const fn source_type(profile: TypeScriptSource) -> SourceType {
    match profile {
        TypeScriptSource::TypeScript => SourceType::ts(),
        TypeScriptSource::Tsx => SourceType::tsx(),
    }
}
