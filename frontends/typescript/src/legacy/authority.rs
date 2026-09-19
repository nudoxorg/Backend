//! Builds one in-process OXC syntax-and-binding authority from caller-owned source.
//! Holds the resulting program facts in the caller's arena for a single lowering transaction.
//! Selects grammar solely from the closed compiler TypeScript profile.

use backend_semantic::vocabulary::TypeScriptSource;
use oxc_allocator::Allocator;
use oxc_ast::ast::TSMappedTypeModifierOperator;
use oxc_parser::Parser;
use oxc_semantic::{Semantic, SemanticBuilder};
use oxc_span::SourceType;
use oxc_syntax::module_record::ModuleRecord;
use oxc_syntax::symbol::SymbolFlags;

use crate::legacy::{AuthorityError, Utf8Span};

/// The source-syntax meaning of one mapped-type modifier.
///
/// This is deliberately separate from the checker report modifier and the
/// compact lattice discriminant: each vocabulary orders its variants
/// differently, so raw casts would permute semantic values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxMappedModifier {
    /// The mapped member inherits the source property's modifier.
    Absent,
    /// The source explicitly adds the modifier (`readonly`, `?`, or `+`).
    Add,
    /// The source explicitly removes the modifier (`-readonly`, `-?`).
    Remove,
}

/// Decodes the OXC token attached to the mapped member itself.
///
/// The parser has already distinguished this token from any `?` nested in
/// the key constraint or `as` remap, so lowering never scans arbitrary type
/// text to infer optionality.
pub const fn syntax_mapped_modifier(
    operator: Option<TSMappedTypeModifierOperator>,
) -> SyntaxMappedModifier {
    match operator {
        None => SyntaxMappedModifier::Absent,
        Some(TSMappedTypeModifierOperator::True | TSMappedTypeModifierOperator::Plus) => {
            SyntaxMappedModifier::Add
        }
        Some(TSMappedTypeModifierOperator::Minus) => SyntaxMappedModifier::Remove,
    }
}

/// Declaration class proven by OXC's symbol table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OxcDeclarationKind {
    /// A `const` binding.
    Constant,
    /// A mutable `let` or `var` binding.
    Variable,
    /// A function declaration or named function expression.
    Function,
    /// A class declaration or named class expression.
    Class,
    /// A TypeScript type alias.
    TypeAlias,
    /// A TypeScript interface.
    Interface,
    /// A regular or const enum.
    Enum,
    /// One enum member.
    EnumMember,
    /// An instantiated or type-only namespace.
    Namespace,
    /// A generic type parameter.
    TypeParameter,
    /// A value or type import binding.
    Import,
}

/// One source-backed declaration emitted directly from OXC binding authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OxcDeclaration {
    /// Exact bound-name span in the original UTF-8 source.
    pub name: Utf8Span,
    /// Closed symbol class reported by OXC.
    pub kind: OxcDeclarationKind,
}

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

impl OxcModule<'_> {
    /// Streams every bound symbol with its exact original name span.
    pub fn declarations(&self) -> impl Iterator<Item = OxcDeclaration> + '_ {
        let scoping = self.semantic.scoping();
        scoping.symbol_ids().filter_map(move |symbol| {
            let kind = declaration_kind(scoping.symbol_flags(symbol))?;
            let name = Utf8Span::try_from(scoping.symbol_span(symbol)).ok()?;
            Some(OxcDeclaration { name, kind })
        })
    }
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
    analyze_with_declaration(profile, source, false, arena)
}

/// Parses and lexically resolves `source` under one closed TypeScript profile,
/// selecting the ambient declaration-file grammar when `declaration` is set.
///
/// A `.d.ts`/`.d.mts`/`.d.cts` source is entirely ambient: constructors and
/// overloaded methods legitimately carry no implementation, so OXC's
/// implementation-presence checks (TS2390/TS2391) must observe the
/// declaration-file source type. The caller owns that classification; this
/// boundary never guesses it from the source bytes.
///
/// # Errors
///
/// Returns every OXC parser diagnostic as [`AuthorityError::Syntax`] or every
/// lexical-resolution diagnostic as [`AuthorityError::Binding`].
pub fn analyze_with_declaration<'source>(
    profile: TypeScriptSource,
    source: &'source str,
    declaration: bool,
    arena: &'source Allocator,
) -> Result<OxcModule<'source>, AuthorityError> {
    let parsed = Parser::new(arena, source, source_type(profile, declaration)).parse();
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

/// Runs one non-escaping OXC authority transaction with an internally owned arena.
///
/// # Errors
///
/// Returns the same complete syntax or binding diagnostics as [`analyze`].
pub fn with_analysis<Output>(
    profile: TypeScriptSource,
    source: &str,
    consume: impl for<'analysis> FnOnce(OxcModule<'analysis>) -> Output,
) -> Result<Output, AuthorityError> {
    with_analysis_declaration(profile, source, false, consume)
}

/// Runs one non-escaping OXC authority transaction, selecting the ambient
/// declaration-file grammar when `declaration` is set.
///
/// # Errors
///
/// Returns the same complete syntax or binding diagnostics as
/// [`analyze_with_declaration`].
pub fn with_analysis_declaration<Output>(
    profile: TypeScriptSource,
    source: &str,
    declaration: bool,
    consume: impl for<'analysis> FnOnce(OxcModule<'analysis>) -> Output,
) -> Result<Output, AuthorityError> {
    let arena = Allocator::default();
    analyze_with_declaration(profile, source, declaration, &arena).map(consume)
}

fn declaration_kind(flags: SymbolFlags) -> Option<OxcDeclarationKind> {
    if flags.contains(SymbolFlags::ConstVariable) {
        Some(OxcDeclarationKind::Constant)
    } else if flags.contains(SymbolFlags::Function) {
        Some(OxcDeclarationKind::Function)
    } else if flags.contains(SymbolFlags::Class) {
        Some(OxcDeclarationKind::Class)
    } else if flags.contains(SymbolFlags::TypeAlias) {
        Some(OxcDeclarationKind::TypeAlias)
    } else if flags.contains(SymbolFlags::Interface) {
        Some(OxcDeclarationKind::Interface)
    } else if flags.intersects(SymbolFlags::Enum) {
        Some(OxcDeclarationKind::Enum)
    } else if flags.contains(SymbolFlags::EnumMember) {
        Some(OxcDeclarationKind::EnumMember)
    } else if flags.intersects(SymbolFlags::Namespace) {
        Some(OxcDeclarationKind::Namespace)
    } else if flags.contains(SymbolFlags::TypeParameter) {
        Some(OxcDeclarationKind::TypeParameter)
    } else if flags.intersects(SymbolFlags::Import | SymbolFlags::TypeImport) {
        Some(OxcDeclarationKind::Import)
    } else if flags.intersects(SymbolFlags::Variable) {
        Some(OxcDeclarationKind::Variable)
    } else {
        None
    }
}

/// Selects the OXC source grammar from the closed profile and the caller's
/// declaration-file classification. A declaration file is ambient under the
/// TypeScript definition grammar; JSX is never admitted in a `.d.ts`, so the
/// TSX profile still lowers declarations under the definition source type.
const fn source_type(profile: TypeScriptSource, declaration: bool) -> SourceType {
    if declaration {
        return SourceType::d_ts();
    }
    match profile {
        TypeScriptSource::TypeScript => SourceType::ts(),
        TypeScriptSource::Tsx => SourceType::tsx(),
    }
}
