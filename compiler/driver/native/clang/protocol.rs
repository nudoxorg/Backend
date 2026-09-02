//! Defines protocol behavior for the direct Clang semantic frontend of `compiler-driver`.
//! The frontend-local records carry one canonical semantic kind registry, exact byte-unit
//! source spans, and typed identity coordinates, so canonical fact admission never has to
//! decode raw cursor codes or reinvent a parallel vocabulary.
//! Kind codes are the canonical `EntityKind` discriminants (0..=12) shared by the compiler
//! plane; the closed match from libclang cursor kinds makes a wrong-kind mapping
//! unrepresentable by construction.

/// Exact half-open byte-unit source extent inside the analyzed source buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClangSourceSpan {
    /// First source byte of the extent.
    pub start: u32,
    /// One past the last source byte of the extent.
    pub end: u32,
}

/// Canonical semantic declaration kinds retained by frontend facts.
///
/// The discriminant set and codes are exactly the canonical entity-kind registry; codes 0..=2
/// predate the full set and never move.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SemanticKind {
    Function = 0,
    Constant = 1,
    Record = 2,
    Module = 3,
    Field = 4,
    Alias = 5,
    Trait = 6,
    Implementation = 7,
    Enum = 8,
    Variant = 9,
    Static = 10,
    Reexport = 11,
    Parameter = 12,
}

impl SemanticKind {
    /// Canonical declaration-kind order used by the compiler registry reports.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 13] = [
        Self::Function,
        Self::Constant,
        Self::Record,
        Self::Module,
        Self::Field,
        Self::Alias,
        Self::Trait,
        Self::Implementation,
        Self::Enum,
        Self::Variant,
        Self::Static,
        Self::Reexport,
        Self::Parameter,
    ];
}

impl From<SemanticKind> for u16 {
    fn from(value: SemanticKind) -> Self {
        match value {
            SemanticKind::Function => 0,
            SemanticKind::Constant => 1,
            SemanticKind::Record => 2,
            SemanticKind::Module => 3,
            SemanticKind::Field => 4,
            SemanticKind::Alias => 5,
            SemanticKind::Trait => 6,
            SemanticKind::Implementation => 7,
            SemanticKind::Enum => 8,
            SemanticKind::Variant => 9,
            SemanticKind::Static => 10,
            SemanticKind::Reexport => 11,
            SemanticKind::Parameter => 12,
        }
    }
}

impl TryFrom<u16> for SemanticKind {
    type Error = u16;

    fn try_from(actual: u16) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Function),
            1 => Ok(Self::Constant),
            2 => Ok(Self::Record),
            3 => Ok(Self::Module),
            4 => Ok(Self::Field),
            5 => Ok(Self::Alias),
            6 => Ok(Self::Trait),
            7 => Ok(Self::Implementation),
            8 => Ok(Self::Enum),
            9 => Ok(Self::Variant),
            10 => Ok(Self::Static),
            11 => Ok(Self::Reexport),
            12 => Ok(Self::Parameter),
            actual => Err(actual),
        }
    }
}

/// Closed source dialect bound by the caller instead of an ambient file-name sniff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClangSourceLanguage {
    /// ISO C source parsed with `-x c`.
    C,
    /// ISO C++ source parsed with `-x c++`.
    #[allow(
        dead_code,
        reason = "the closed driver tag parses C today; the proofs exercise C++ parsing until its registry tag lands"
    )]
    Cxx,
}

/// Closed analysis phase retained by cancellation and deadline terminals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClangPhase {
    /// The source parse and traversal.
    Analysis,
}

/// Closed diagnostic severity retained from the libclang diagnostic authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClangDiagnosticSeverity {
    /// The source is rejected.
    Error,
    /// The source is admitted with a warning.
    Warning,
    /// Explanatory note attached to another diagnostic.
    Note,
}

/// One structured main-file diagnostic retained by the libclang authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClangDiagnostic {
    /// Retained severity of the diagnostic.
    pub severity: ClangDiagnosticSeverity,
    /// One-based source line of the diagnostic.
    pub line: u32,
    /// One-based source column of the diagnostic.
    pub column: u32,
    /// Exact byte-unit source span of the diagnostic location.
    pub span: ClangSourceSpan,
}

/// Closed use-site role of a type reference retained from the cursor authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClangTypeUseKind {
    /// Type annotation of a record field.
    Field,
    /// Result type of a function-like declaration.
    FunctionSignature,
    /// Type annotation of a parameter declaration.
    Parameter,
    /// Type annotation of a variable declaration.
    Variable,
}

/// Closed declared-type recipe derived by the libclang authority from the
/// exact canonical type spelling. The authority, never a source scanner,
/// proves which declared types fall inside the compact primitive recipe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClangTypeRecipe {
    /// The declared type stays outside the closed primitive recipe.
    None,
    /// Pointer-to-`char` object: the compact string recipe.
    String,
    /// `_Bool` object: the compact boolean recipe.
    Bool,
    /// `int` object: the compact integer recipe.
    Integer,
}

impl ClangTypeRecipe {
    /// Stable one-byte journal code.
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::String => 1,
            Self::Bool => 2,
            Self::Integer => 3,
        }
    }

    /// Decodes a journal recipe code.
    pub(crate) fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::None),
            1 => Some(Self::String),
            2 => Some(Self::Bool),
            3 => Some(Self::Integer),
            _ => None,
        }
    }
}

/// Closed resolution shape of one type use.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClangTypeUseResolution {
    /// The used type is a builtin and has no declaration inside the source.
    Builtin,
    /// The used type resolves to a declaration with its stable identity and exact target span.
    Declaration {
        /// Exact byte-unit name span of the resolved declaration.
        target: ClangSourceSpan,
        /// Stable USR-derived coordinate of the resolved type.
        identity: FactId,
    },
}

/// Closed reference role retained from the cursor authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClangReferenceKind {
    /// A free-function call resolved by libclang.
    FunctionCall,
    /// A C++ member-function call resolved by libclang.
    MethodCall,
    /// A variable or enumerator use resolved by libclang.
    VariableUse,
    /// A field access resolved by libclang.
    FieldAccess,
    /// A macro expansion resolved by libclang.
    MacroInvocation,
}

/// Oracle confidence carried by references resolved by libclang.
#[allow(
    dead_code,
    reason = "the closed confidence vocabulary stays a protocol constant; the emission seam maps tiers inline today"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Confidence {
    /// libclang supplied the reference target and source extent.
    Oracle,
}

/// Half-open byte span relative to an owning declaration's span start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelSpan {
    /// Relative first byte.
    pub start: u32,
    /// Relative one-past-last byte.
    pub end: u32,
}

impl ClangSourceSpan {
    /// Converts an absolute source span to an owner-relative span without saturation.
    pub(crate) fn relative_to(self, owner: Self) -> Option<RelSpan> {
        (owner.start <= self.start && self.start <= self.end && self.end <= owner.end).then(|| {
            RelSpan {
                start: self.start - owner.start,
                end: self.end - owner.start,
            }
        })
    }
}

/// Closed identity role separating symbol facts from type facts in the USR interner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FactRole {
    /// USR identity of a declared symbol.
    Symbol,
    /// USR identity of a named type.
    Type,
}

impl FactRole {
    /// Stable one-byte journal code for this role.
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Symbol => 1,
            Self::Type => 2,
        }
    }

    /// Decodes a journal role code.
    pub(crate) fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Symbol),
            2 => Some(Self::Type),
            _ => None,
        }
    }
}

/// Stable per-analysis fact coordinate derived from the declaration's unified symbol
/// resolution identity.
///
/// Repeated declarations of one symbol share one coordinate inside the analysis; the
/// coordinate is derived from source authority and is deliberately not a durable
/// cross-run identity by itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FactId {
    /// Closed USR identity role of the coordinate.
    pub(crate) role: FactRole,
    /// Dense coordinate assigned in first-appearance order inside the analysis.
    pub(crate) ordinal: u32,
}

/// One typed semantic fact lent from the analyzed source bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClangFact<'source> {
    /// A canonical declaration fact.
    Entity(EntityFact<'source>),
    /// A typed use of one type.
    TypeUse(TypeUseFact<'source>),
    /// A resolved reference to one declaration.
    Reference(ReferenceFact<'source>),
    /// Raw documentation attached to one declaration, with lightweight Doxygen markers.
    Documentation(DoxygenDocFact<'source>),
    /// One structured main-file diagnostic.
    Diagnostic(ClangDiagnostic),
}

/// Raw Doxygen documentation and the structured markers needed by consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DoxygenDocFact<'source> {
    /// The complete comment, including its original `///` or `/**` spelling.
    pub(crate) raw: &'source str,
    /// Declaration identity owning this documentation.
    pub(crate) owner: FactId,
    /// Whether a `\return` or `\returns` command occurs in the raw comment.
    pub(crate) has_return: bool,
    /// Deprecated text, retaining the raw command payload when present.
    pub(crate) deprecated: Option<&'source str>,
}

impl<'source> DoxygenDocFact<'source> {
    /// Iterates parameter names in source order without building a second document model.
    #[allow(
        dead_code,
        reason = "documentation consumers call this iterator after the frontend emits the fact"
    )]
    pub(crate) fn params(self) -> impl Iterator<Item = &'source str> {
        self.raw.split("\\param").skip(1).filter_map(|part| {
            let mut rest = part.trim_start();
            if rest.starts_with('[') {
                let direction_end = rest.find(']')?;
                rest = rest[direction_end + 1..].trim_start();
            }
            let name_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            (name_end != 0).then_some(&rest[..name_end])
        })
    }
}

/// Canonical declaration fact with its exact extent and stable identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EntityFact<'source> {
    /// Exact declaration name borrowed from the source bytes.
    pub(crate) name: &'source str,
    /// Canonical semantic kind of the declaration.
    pub(crate) kind: SemanticKind,
    /// Exact byte-unit name span.
    pub(crate) span: ClangSourceSpan,
    /// Exact byte-unit extent of the enclosing owner declaration.
    pub(crate) owner: ClangSourceSpan,
    /// Stable USR-derived fact coordinate.
    pub(crate) identity: FactId,
}

/// Typed type-use fact with its resolution and exact extent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TypeUseFact<'source> {
    /// Exact used type spelling borrowed from the source bytes.
    pub(crate) name: &'source str,
    /// Closed use-site role of the type reference.
    pub(crate) kind: ClangTypeUseKind,
    /// Closed resolution shape carrying the resolved type's identity when declared.
    pub(crate) resolution: ClangTypeUseResolution,
    /// Closed declared-type recipe the authority derived from the canonical
    /// type spelling, when the declared type falls inside the compact recipe.
    pub(crate) recipe: ClangTypeRecipe,
    /// Exact byte-unit use span.
    pub(crate) span: ClangSourceSpan,
    /// Exact byte-unit extent of the enclosing owner declaration.
    pub(crate) owner: ClangSourceSpan,
}

/// Resolved reference fact with its exact use and resolution extents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReferenceFact<'source> {
    /// Exact referenced spelling borrowed from the source bytes.
    pub(crate) target: &'source str,
    /// Closed reference role.
    pub(crate) kind: ClangReferenceKind,
    /// Exact byte-unit use span.
    pub(crate) use_span: ClangSourceSpan,
    /// Exact byte-unit span of the resolved declaration.
    pub(crate) resolved_span: ClangSourceSpan,
    /// Exact byte-unit extent of the enclosing owner declaration.
    pub(crate) owner: ClangSourceSpan,
    /// Stable USR-derived coordinate of the resolved symbol.
    pub(crate) identity: FactId,
}

impl<'source> ReferenceFact<'source> {
    /// Returns the oracle confidence of this libclang-resolved reference.
    #[allow(
        dead_code,
        reason = "the closed confidence accessor stays a protocol surface; the emission seam maps tiers inline today"
    )]
    pub(crate) const fn confidence(self) -> Confidence {
        Confidence::Oracle
    }

    /// Returns the use-site extent relative to the owning declaration.
    #[allow(
        dead_code,
        reason = "the relative-span accessor stays a protocol surface; the emission seam measures spans against the owning entity's extent"
    )]
    pub(crate) fn relative_span(self) -> Option<RelSpan> {
        self.use_span.relative_to(self.owner)
    }
}
