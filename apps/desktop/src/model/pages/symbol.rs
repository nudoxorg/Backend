//! The Page board's read model: one declaration and everything it touches.

use super::common::{ByteSpan, DeclRef, Known, LineSpan, PackageRef, Provenance, SymbolRef};
use backend_library::{SemanticConfidence, SemanticLinkKind, SymbolKey};
use std::sync::Arc;

/// Everything the Page board renders about one declaration, read once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolPage {
    /// The declaration itself.
    pub identity: DeclRef,
    /// Owning package, when the coordinate spells one.
    pub package: Known<PackageRef>,
    /// Declaration signature as linkable tokens.
    pub signature: Known<SignatureText>,
    /// Producer documentation, in order. Empty means the producer captured
    /// none for this declaration (the document reply is authoritative).
    pub docs: Arc<[DocFragment]>,
    /// Source location and excerpt availability.
    pub site: SourceSite,
    /// Members grouped for the ledger.
    pub members: Known<Members>,
    /// Relations for the rose, grouped by direction.
    pub rose: Rose,
    /// Use sites of this declaration.
    pub references: Known<Arc<[ReferenceSite]>>,
    /// Where the declaration sits in its package outline.
    pub outline: Known<OutlinePosition>,
}

/// A signature as text plus classified, linkable token spans.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureText {
    /// Exact signature text.
    pub text: Arc<str>,
    /// Tokens covering the text in order; spans index into `text`.
    pub tokens: Arc<[SignatureToken]>,
}

impl SignatureText {
    /// Returns the tokens that carry a link target.
    pub fn links(&self) -> impl Iterator<Item = &SignatureToken> {
        self.tokens.iter().filter(|token| token.link.is_some())
    }

    /// Returns the text of one token.
    #[must_use]
    pub fn token_text(&self, token: &SignatureToken) -> &str {
        self.text.get(token.span.range()).unwrap_or_default()
    }
}

/// What one signature token reads as.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TokenClass {
    /// Reserved word.
    Keyword,
    /// The declared name itself.
    Name,
    /// Identifier in type position.
    Type,
    /// Parameter or field binding.
    Binding,
    /// Lifetime or sigil marker.
    Lifetime,
    /// Structural punctuation.
    Punctuation,
    /// Literal.
    Literal,
    /// Anything else, including whitespace.
    Text,
}

/// One classified token of a signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignatureToken {
    /// Byte span inside [`SignatureText::text`].
    pub span: ByteSpan,
    /// Lexical class.
    pub class: TokenClass,
    /// Declaration this identifier may refer to.
    pub link: Option<SymbolLink>,
}

/// A navigable target with the evidence that justifies the link.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolLink {
    /// Exact target coordinate.
    pub target: SymbolRef,
    /// Why the link exists.
    pub provenance: Provenance,
}

/// One documentation fragment, in producer order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocFragment {
    /// Prose.
    Text(Arc<str>),
    /// Code captured as documentation.
    Code(Arc<str>),
    /// Link to another declaration.
    Link {
        /// Display label.
        label: Arc<str>,
        /// Stable target key from the producer.
        target: SymbolKey,
        /// Coordinate of the target, when the package outline names the key.
        coordinate: Option<SymbolRef>,
    },
    /// Explicit break.
    Break,
}

impl DocFragment {
    /// Joins the prose of a fragment run, dropping code and links' targets.
    #[must_use]
    pub fn plain_text(fragments: &[Self]) -> String {
        let mut out = String::new();
        for fragment in fragments {
            match fragment {
                Self::Text(text) | Self::Code(text) => out.push_str(text),
                Self::Link { label, .. } => out.push_str(label),
                Self::Break => out.push('\n'),
            }
        }
        out
    }
}

/// Where a declaration's source is, and whether its text is here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSite {
    /// Package-relative file and one-based start line.
    pub location: Known<SourceLocation>,
    /// Bounded declaration text.
    pub excerpt: Known<Excerpt>,
}

/// Package-relative file plus one-based start line.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceLocation {
    /// Package-relative path.
    pub path: Arc<str>,
    /// One-based start line.
    pub line: u32,
}

/// The producer's bounded declaration text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Excerpt {
    /// Excerpt text.
    pub text: Arc<str>,
    /// Lines the excerpt covers, when the start line is known.
    pub lines: Option<LineSpan>,
    /// Whether the producer retained the complete declaration.
    pub complete: bool,
}

/// How a method receives its value: the modifier mark on its ledger row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Receiver {
    /// Borrows `self` mutably: changes it.
    Changes,
    /// Borrows `self`: reads only.
    Reads,
    /// Takes `self`: the value is gone after.
    Consumes,
    /// No `self`: makes one or stands alone.
    Makes,
    /// The signature does not say (not captured, or not a Rust-like receiver).
    Unknown,
}

impl Receiver {
    /// Returns the stable lowercase name (the modifier mark's name).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Changes => "changes",
            Self::Reads => "reads",
            Self::Consumes => "consumes",
            Self::Makes => "makes",
            Self::Unknown => "unknown",
        }
    }
}

/// One ledger row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Member {
    /// The member declaration.
    pub decl: DeclRef,
    /// Its signature, when captured as source text.
    pub signature: Known<SignatureText>,
    /// First line of its own documentation.
    pub summary: Option<Arc<str>>,
}

/// Methods that share one receiver.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MethodGroup {
    /// Shared receiver.
    pub receiver: Receiver,
    /// Methods in name order.
    pub members: Arc<[Member]>,
}

/// The members ledger: what a declaration is made of and what it does.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Members {
    /// Fields and variants, in source order.
    pub made_of: Arc<[Member]>,
    /// Methods, functions, and constructors grouped by receiver.
    pub does: Arc<[MethodGroup]>,
    /// Everything else attached (nested types, constants, macros, modules).
    pub other: Arc<[Member]>,
}

impl Members {
    /// Returns every member in ledger order.
    pub fn all(&self) -> impl Iterator<Item = &Member> {
        self.made_of
            .iter()
            .chain(self.does.iter().flat_map(|group| group.members.iter()))
            .chain(self.other.iter())
    }

    /// Returns the total member count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.all().count()
    }

    /// Returns whether the ledger is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What connects the page's declaration to a related one.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RelationKind {
    /// A compiler relation kind.
    Semantic(SemanticLinkKind),
    /// Outline containment: the related declaration is a child.
    Contains,
}

/// How an implementation arrives, when the producer states it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Arrival {
    /// Written for this type.
    Direct,
    /// Arrives through a blanket impl.
    Blanket,
    /// The compiler proves it (auto trait).
    Auto,
    /// The producer does not say how it arrives.
    NotReported,
}

/// One related declaration on the rose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Relation {
    /// The related declaration.
    pub decl: DeclRef,
    /// Relation kind.
    pub kind: RelationKind,
    /// Evidence behind the edge.
    pub provenance: Provenance,
    /// How an implementation arrives (only meaningful for `is` relations).
    pub arrival: Arrival,
    /// The intermediate declaration a derived relation was read through
    /// (the impl block), when there is one.
    pub via: Option<DeclRef>,
}

/// Relations for the rose, grouped by direction.
///
/// Each direction is independently known: a structural-only reply knows
/// containment (`down`) but not typed edges, and says so per direction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rose {
    /// Up, "is": implements, inherits, overrides, supertraits.
    pub up: Known<Arc<[Relation]>>,
    /// Down, "made of": contained declarations.
    pub down: Known<Arc<[Relation]>>,
    /// Left, "from": incoming callers, type users, readers, importers.
    pub left: Known<Arc<[Relation]>>,
    /// Right, "to": outgoing calls, type references, reads, imports.
    pub right: Known<Arc<[Relation]>>,
    /// Implementors / subclasses / overriders of this declaration.
    pub implemented_by: Known<Arc<[Relation]>>,
}

/// Where the use points, as the producer resolved it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReferenceScope {
    /// Same package image.
    Local,
    /// Another immutable fragment.
    Stable,
    /// Unresolved foreign declaration.
    Foreign,
    /// Entity ordinal in another fragment.
    FragmentEntity,
}

/// A byte span inside one package-relative file.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileSpan {
    /// Package-relative path.
    pub file: Arc<str>,
    /// Byte span inside that file.
    pub bytes: ByteSpan,
}

/// One use of the page's declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceSite {
    /// The declaration whose source contains the use.
    pub site: DeclRef,
    /// Relation that makes the site a use.
    pub relation: SemanticLinkKind,
    /// Authority behind the relation.
    pub confidence: SemanticConfidence,
    /// Exact source span of the use, when the authority captured one.
    pub span: Known<FileSpan>,
    /// Where the use resolved to.
    pub scope: ReferenceScope,
}

/// The declaration's place in its package outline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutlinePosition {
    /// Root-first containment chain, excluding the declaration.
    pub ancestors: Arc<[DeclRef]>,
    /// Declarations sharing its parent, in outline order, including itself.
    pub siblings: Arc<[DeclRef]>,
    /// Index of the declaration inside `siblings`.
    pub index: Option<usize>,
}
