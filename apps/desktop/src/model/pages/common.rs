//! Vocabulary shared by every page read model.
//!
//! Every field a board renders either carries a producer fact or says, with a
//! typed [`Gap`], why it cannot. "Unknown" never collapses into zero, empty, or
//! a plausible default: a list that could not be read is `Known::Unknown`, and
//! an empty `Known::Known` list is a producer statement that there are none.

use backend_library::{DeclarationKind, SemanticConfidence, SemanticLinkKind, SymbolKey};
use backend_present::{Identity, IdentityKey, IdentityShape, Language};
use std::fmt;
use std::sync::Arc;

/// Exact engine coordinate of one declaration, never abbreviated.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SymbolRef(Arc<str>);

/// A page key that cannot cross the engine boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyError {
    /// The spelling was empty after trimming.
    Empty,
    /// The spelling carries a control character.
    ControlCharacter,
    /// The package spelling failed product admission.
    Package,
}

impl fmt::Display for KeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "page key is empty",
            Self::ControlCharacter => "page key contains a control character",
            Self::Package => "package reference failed admission",
        })
    }
}

impl std::error::Error for KeyError {}

impl SymbolRef {
    /// Admits one engine coordinate.
    ///
    /// # Errors
    /// Returns [`KeyError`] for an empty spelling or one with control characters.
    pub fn new(value: &str) -> Result<Self, KeyError> {
        if value.trim().is_empty() {
            return Err(KeyError::Empty);
        }
        if value.chars().any(char::is_control) {
            return Err(KeyError::ControlCharacter);
        }
        Ok(Self(Arc::from(value)))
    }

    /// Returns the exact coordinate spelling the engine accepts.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parses the coordinate into its display parts.
    #[must_use]
    pub fn identity(&self) -> Identity {
        Identity::parse(&self.0)
    }

    /// The same declaration inside `to` when this coordinate is spelled
    /// under `from` (a release re-scope); `None` otherwise.
    #[must_use]
    pub fn rebased(&self, from: &PackageRef, to: &PackageRef) -> Option<Self> {
        let rest = self.as_str().strip_prefix(from.as_str())?;
        Self::new(&format!("{}{rest}", to.as_str())).ok()
    }

    /// Returns the owning package, when the coordinate spells a project root.
    #[must_use]
    pub fn package(&self) -> Option<PackageRef> {
        self.identity()
            .project()
            .and_then(|project| PackageRef::parse(project.root()).ok())
    }
}

impl fmt::Display for SymbolRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Exact package locator: a local project root or a version-pinned purl.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageRef(backend_library::PackageReference);

impl PackageRef {
    /// Admits one package spelling.
    ///
    /// # Errors
    /// Returns [`KeyError::Package`] when product admission rejects it.
    pub fn parse(value: &str) -> Result<Self, KeyError> {
        if value.trim().is_empty() {
            return Err(KeyError::Empty);
        }
        backend_library::PackageReference::parse(value.to_owned())
            .map(Self)
            .map_err(|_| KeyError::Package)
    }

    /// Wraps an already admitted reference.
    #[must_use]
    pub const fn from_reference(reference: backend_library::PackageReference) -> Self {
        Self(reference)
    }

    /// Returns the typed reference the engine accepts.
    #[must_use]
    pub const fn reference(&self) -> &backend_library::PackageReference {
        &self.0
    }

    /// Returns the canonical spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Returns whether this is a local project rather than a registry release.
    #[must_use]
    pub const fn is_local(&self) -> bool {
        matches!(self.0, backend_library::PackageReference::Local(_))
    }

    /// The version this reference pins, when it is a registry release.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        if self.is_local() {
            return None;
        }
        let text = self.as_str();
        let at = text.rfind('@')?;
        let version = &text[at + 1..];
        let version = version.split(['?', '#']).next().unwrap_or(version);
        (!version.is_empty()).then_some(version)
    }

    /// The same package at another release, when it is a registry release
    /// (a local project has one state: `None`).
    #[must_use]
    pub fn at(&self, version: &str) -> Option<Self> {
        let pinned = self.version()?;
        let text = self.as_str();
        let at = text.rfind('@')?;
        let rest = &text[at + 1 + pinned.len()..];
        Self::parse(&format!("{}@{version}{rest}", &text[..at])).ok()
    }

    /// Returns the readable name: the last path component of a local root,
    /// or the purl name without its namespace and version.
    #[must_use]
    pub fn display_name(&self) -> &str {
        let text = self.as_str();
        if self.is_local() {
            return text
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(text);
        }
        let without_version = text.split('@').next().unwrap_or(text);
        without_version.rsplit('/').next().unwrap_or(without_version)
    }
}

impl fmt::Display for PackageRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One field that is either a producer fact or a typed gap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Known<T> {
    /// The producer answered with this value.
    Known(T),
    /// The value is not known, and this is why.
    Unknown(Gap),
}

impl<T> Known<T> {
    /// Returns the value when it is known.
    #[must_use]
    pub const fn known(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown(_) => None,
        }
    }

    /// Returns the gap when the value is unknown.
    #[must_use]
    pub const fn gap(&self) -> Option<&Gap> {
        match self {
            Self::Known(_) => None,
            Self::Unknown(gap) => Some(gap),
        }
    }

    /// Returns a gap-typed unknown value.
    #[must_use]
    pub fn unknown(reason: GapReason, detail: impl Into<Arc<str>>) -> Self {
        Self::Unknown(Gap::new(reason, detail))
    }

    /// Maps a known value without touching a gap.
    #[must_use]
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Known<U> {
        match self {
            Self::Known(value) => Known::Known(f(value)),
            Self::Unknown(gap) => Known::Unknown(gap),
        }
    }
}

/// Why one field is unknown. Closed so a board can pick its hatch and words.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GapReason {
    /// The producer retained nothing for this field.
    NotCaptured,
    /// The value exists but its bytes are not resident locally.
    NotHydrated,
    /// This deployment has no provider for the field.
    Unconfigured,
    /// Typed relations and references need a compiler publication, and the
    /// package has none selected.
    NoSemanticPublication,
    /// The configured feed does not record this fact.
    NotRecorded,
    /// The source explicitly does not support the fact.
    Unsupported,
    /// The source should provide the fact but could not.
    Unavailable,
    /// The answer came from an older frontier.
    Stale,
    /// The source did not establish coverage.
    Unknown,
    /// A registry fact asked of a local project, which no registry records.
    LocalProject,
    /// The read failed; the detail carries the bounded diagnostic.
    ReadFailed,
    /// The engine returned an encoded compiler type instead of source text.
    Encoded,
    /// The engine exposes no command that answers this field.
    NotServed,
}

impl GapReason {
    /// Returns the stable lowercase name used by text renderings.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::NotCaptured => "not-captured",
            Self::NotHydrated => "not-hydrated",
            Self::Unconfigured => "unconfigured",
            Self::NoSemanticPublication => "no-semantic-publication",
            Self::NotRecorded => "not-recorded",
            Self::Unsupported => "unsupported",
            Self::Unavailable => "unavailable",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
            Self::LocalProject => "local-project",
            Self::ReadFailed => "read-failed",
            Self::Encoded => "encoded",
            Self::NotServed => "not-served",
        }
    }
}

/// A typed reason plus the producer's own bounded words, when it gave any.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Gap {
    /// Closed reason.
    pub reason: GapReason,
    /// Producer or runtime diagnostic, bounded to 512 bytes.
    pub detail: Arc<str>,
}

impl Gap {
    /// Builds one gap, bounding the detail.
    #[must_use]
    pub fn new(reason: GapReason, detail: impl Into<Arc<str>>) -> Self {
        let detail: Arc<str> = detail.into();
        let detail = if detail.len() > 512 {
            let mut end = 509;
            while !detail.is_char_boundary(end) {
                end -= 1;
            }
            Arc::from(format!("{}…", &detail[..end]))
        } else {
            detail
        };
        Self { reason, detail }
    }
}

impl fmt::Display for Gap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.detail.is_empty() {
            formatter.write_str(self.reason.name())
        } else {
            write!(formatter, "{}: {}", self.reason.name(), self.detail)
        }
    }
}

/// Kind family: the gem's hue. Shape is the [`DeclarationKind`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum KindFamily {
    /// Modules, packages, imports, unknown kinds.
    Namespace,
    /// Structs, classes, enums, unions, aliases.
    Type,
    /// Traits and interfaces.
    Contract,
    /// Functions, methods, constructors, macros.
    Callable,
    /// Constants, fields, properties, variables, variants.
    Value,
}

impl KindFamily {
    /// Returns the family of one declaration kind; an unknown kind is a namespace-hued placeholder.
    #[must_use]
    pub const fn of(kind: Option<DeclarationKind>) -> Self {
        match kind {
            Some(
                DeclarationKind::Struct
                | DeclarationKind::Class
                | DeclarationKind::Enum
                | DeclarationKind::Union
                | DeclarationKind::Type,
            ) => Self::Type,
            Some(DeclarationKind::Trait | DeclarationKind::Interface) => Self::Contract,
            Some(
                DeclarationKind::Function
                | DeclarationKind::Method
                | DeclarationKind::Constructor
                | DeclarationKind::Macro,
            ) => Self::Callable,
            Some(
                DeclarationKind::Constant
                | DeclarationKind::Field
                | DeclarationKind::Property
                | DeclarationKind::Variable
                | DeclarationKind::Variant,
            ) => Self::Value,
            Some(DeclarationKind::Module | DeclarationKind::Import | DeclarationKind::Unknown)
            | None => Self::Namespace,
        }
    }

    /// Returns the stable lowercase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Namespace => "namespace",
            Self::Type => "type",
            Self::Contract => "contract",
            Self::Callable => "callable",
            Self::Value => "value",
        }
    }
}

/// One declaration reference as every board names it: the exact coordinate
/// plus the parts a gem, a trail, and a tooltip need.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclRef {
    /// Exact coordinate the engine accepts.
    pub coordinate: SymbolRef,
    /// Stable row key, when the reply carried one.
    pub key: Option<SymbolKey>,
    /// Declaration name (the coordinate's leaf).
    pub name: Arc<str>,
    /// Declaration kind, when the producer typed it.
    pub kind: Option<DeclarationKind>,
    /// Kind family (gem hue).
    pub family: KindFamily,
    /// Package-relative source path, when known.
    pub path: Option<Arc<str>>,
    /// One-based declaration line, when known.
    pub line: Option<u32>,
    /// Source language implied by the path.
    pub language: Language,
    /// Whether the coordinate is compiler-addressed (`::semantic::`) rather
    /// than file-addressed.
    pub semantic: bool,
}

impl DeclRef {
    /// Builds a reference from one row label, key, kind, and captured site.
    #[must_use]
    pub fn from_label(
        label: &str,
        key: Option<SymbolKey>,
        kind: Option<DeclarationKind>,
        captured: Option<(&str, u32)>,
    ) -> Option<Self> {
        let coordinate = SymbolRef::new(label).ok()?;
        let identity = Identity::parse_with_key(
            label,
            key.map_or(IdentityKey::Absent, IdentityKey::Symbol),
        )
        .with_captured_source(captured.map(|(path, _)| path), captured.map(|(_, line)| line));
        Some(Self {
            name: Arc::from(identity.name()),
            path: identity.path().map(|path| Arc::from(path.as_str())),
            line: identity.line().map(backend_present::LineNumber::get),
            language: identity.language(),
            semantic: matches!(identity.shape(), IdentityShape::Semantic),
            coordinate,
            key,
            kind,
            family: KindFamily::of(kind),
        })
    }

    /// Builds a reference from one engine row.
    #[must_use]
    pub fn from_row(row: &backend_library::Row) -> Option<Self> {
        let key = match row.id {
            backend_library::RowId::Symbol(key) => Some(key),
            backend_library::RowId::Package(_) | backend_library::RowId::Object(_) => None,
        };
        let captured = row
            .source
            .captured()
            .map(|location| (location.path(), location.start_line()));
        Self::from_label(&row.label, key, row.kind, captured)
    }

    /// Returns the kind's stable lowercase name, or `unknown`.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        self.kind.map_or("unknown", DeclarationKind::name)
    }
}

/// Where one relation or link came from. A board never renders a name match
/// or a desktop derivation as if the compiler had proven it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Provenance {
    /// A typed edge from the compiler graph authority, with its confidence.
    Semantic(SemanticConfidence),
    /// Containment read from the outline's parent pointers.
    Structural,
    /// A lexical name match inside the same package; never a proven link.
    ByName,
    /// Derived by the desktop from compiler facts that do not state the
    /// relation directly; the confidence is that of the underlying edges.
    Derived {
        /// What the derivation read.
        via: Derivation,
        /// Weakest confidence among the edges the derivation used.
        confidence: SemanticConfidence,
    },
}

/// The closed set of desktop derivations over compiler facts.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Derivation {
    /// An impl block published as a `Type` row whose type references name
    /// both a nominal self type and a trait: "self type implements trait".
    ImplBlock,
}

impl Provenance {
    /// Returns the stable lowercase name used by text renderings.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Semantic(confidence) => confidence_name(confidence),
            Self::Structural => "structural",
            Self::ByName => "by-name",
            Self::Derived { .. } => "derived",
        }
    }
}

/// Returns the stable lowercase name of one semantic confidence.
#[must_use]
pub const fn confidence_name(confidence: SemanticConfidence) -> &'static str {
    match confidence {
        SemanticConfidence::Syntactic => "syntactic",
        SemanticConfidence::Heuristic => "heuristic",
        SemanticConfidence::Indexed => "indexed",
        SemanticConfidence::Imported => "imported",
        SemanticConfidence::Compiler => "compiler",
    }
}

/// Returns the stable lowercase name of one semantic relation kind.
#[must_use]
pub const fn link_name(kind: SemanticLinkKind) -> &'static str {
    match kind {
        SemanticLinkKind::Calls => "calls",
        SemanticLinkKind::MethodCall => "method-call",
        SemanticLinkKind::TypeReference => "type-reference",
        SemanticLinkKind::Reads => "reads",
        SemanticLinkKind::Writes => "writes",
        SemanticLinkKind::Imports => "imports",
        SemanticLinkKind::Implements => "implements",
        SemanticLinkKind::Overrides => "overrides",
        SemanticLinkKind::Reexports => "reexports",
        SemanticLinkKind::Inherits => "inherits",
        SemanticLinkKind::Documents => "documents",
    }
}

/// A half-open UTF-8 byte range inside one text.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteSpan {
    /// Inclusive start byte.
    pub start: u32,
    /// Exclusive end byte.
    pub end: u32,
}

impl ByteSpan {
    /// Builds a span, returning `None` when it is inverted.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Option<Self> {
        if start <= end {
            Some(Self { start, end })
        } else {
            None
        }
    }

    /// Returns the span as a `usize` range for slicing.
    #[must_use]
    pub const fn range(self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }
}

/// One contiguous inclusive range of one-based source lines.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LineSpan {
    /// First line, one-based.
    pub first: u32,
    /// Last line, one-based and inclusive.
    pub last: u32,
}
