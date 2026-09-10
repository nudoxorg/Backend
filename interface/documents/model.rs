//! Defines model behavior for `interface-documents`, whose purpose is to project semantic images into one presentation-neutral document model every surface renders.
//! This module owns the model invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The page model: identity, signature, prose, members, relations, and source for one declaration.

use compiler_ir::{Confidence, EntityId, LinkKind, Visibility};
use compiler_ir_vocabulary::EntityKind;
use compiler_vocabulary::Language;
use interface_identity::{ContentKey, ExactAddress, PackageCoordinate};

use crate::{ByteSpan, Name, PageTruncation, Text};

/// The identity header every symbol-bearing row shares: hits, members, crumbs, nodes, and pages.
///
/// One struct rather than five near-copies, so a renderer that learns to draw a symbol draws it
/// the same way everywhere, and an address minted here is proven by construction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Symbol {
    /// Resolvable address with proven key.
    pub address: ExactAddress,
    /// Coordinate inside the owning image.
    pub entity: EntityId,
    /// Declaration name.
    pub name: Name,
    /// Declaration kind.
    pub kind: EntityKind,
    /// Declared visibility.
    pub visibility: Visibility,
}

impl Symbol {
    /// The proven key.
    #[must_use]
    pub const fn key(&self) -> ContentKey {
        self.address.key()
    }

    /// The owning package.
    #[must_use]
    pub const fn package(&self) -> &PackageCoordinate {
        &self.address.as_address().package
    }
}

/// One rendered declaration page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page {
    /// Identity header.
    pub symbol: Symbol,
    /// Source language the compiler proved, so fences and highlighting never guess.
    pub language: Language,
    /// Ancestors from the package root down to the parent, in order.
    pub crumbs: Box<[Symbol]>,
    /// Hyperlinked declaration signature.
    pub signature: Signature,
    /// Documentation prose with resolved links.
    pub prose: Prose,
    /// Members grouped by kind in canonical kind order.
    pub members: Box<[MemberGroup]>,
    /// Canonical graph relations grouped by role.
    pub relations: Box<[RelationGroup]>,
    /// Source location when the image retained one.
    pub source: Option<SourceLocation>,
    /// Retained attribute spellings.
    pub attributes: Box<[Text]>,
    /// What the projection budgets left out, if anything.
    pub truncation: PageTruncation,
}

/// Closed render role of one signature token.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TokenKind {
    /// Declaration or modifier keyword.
    Keyword,
    /// The declaration's own name.
    Name,
    /// A type spelling, linkable when resolved.
    Type,
    /// A lifetime or region.
    Lifetime,
    /// Structural punctuation.
    Punctuation,
    /// A literal value.
    Literal,
    /// A parameter or field name.
    Binding,
    /// Whitespace or other text.
    Text,
}

/// One signature token and its resolved target when the image proves one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    /// Render role.
    pub kind: TokenKind,
    /// Exact spelling.
    pub text: Text,
    /// Hyperlink target.
    pub target: Option<Target>,
}

/// A declaration signature as an ordered token list.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Signature(Box<[Token]>);

impl Signature {
    /// Wraps ordered tokens.
    #[must_use]
    pub fn new(tokens: Vec<Token>) -> Self {
        Self(tokens.into_boxed_slice())
    }

    /// Ordered tokens.
    #[must_use]
    pub fn tokens(&self) -> &[Token] {
        &self.0
    }

    /// Concatenated plain spelling.
    #[must_use]
    pub fn plain(&self) -> String {
        self.0.iter().map(|token| token.text.as_str()).collect()
    }

    /// Whether no tokens were retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Where a hyperlink lands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    /// A declaration in a loaded package.
    Local(Symbol),
    /// A declaration in another package the producer named exactly.
    External(ExternalRef),
    /// The producer retained only a display spelling.
    Unresolved(Text),
}

/// Producer-supplied origin of an external declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForeignOrigin {
    /// Ecosystem spelled by the producer.
    pub ecosystem: Text,
    /// Package or namespace when the producer supplied one.
    pub package: Option<Text>,
}

/// One external declaration reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalRef {
    /// Source display spelling.
    pub display: Text,
    /// Canonical remote path.
    pub path: Text,
    /// Origin facts.
    pub origin: Option<ForeignOrigin>,
    /// Expected kind when known.
    pub kind: Option<EntityKind>,
}

/// One inline prose run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Inline {
    /// Plain text.
    Text(Text),
    /// Inline code.
    Code(Text),
    /// A hyperlink.
    Link {
        /// Visible label.
        label: Text,
        /// Resolved target.
        target: Target,
    },
    /// A retained hard line break.
    Break,
}

/// One block of documentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Block {
    /// Flowing text.
    Paragraph(Box<[Inline]>),
    /// A fenced code block.
    Code(Text),
}

/// Documentation prose.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Prose(Box<[Block]>);

impl Prose {
    /// Wraps ordered blocks.
    #[must_use]
    pub fn new(blocks: Vec<Block>) -> Self {
        Self(blocks.into_boxed_slice())
    }

    /// Ordered blocks.
    #[must_use]
    pub fn blocks(&self) -> &[Block] {
        &self.0
    }

    /// Whether no documentation was retained.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The first paragraph's plain text, for one-line summaries.
    #[must_use]
    pub fn summary(&self) -> Option<Text> {
        self.0.iter().find_map(|block| match block {
            Block::Paragraph(inlines) => {
                let mut text = String::new();
                for inline in inlines {
                    match inline {
                        Inline::Text(run) | Inline::Code(run) => text.push_str(run.as_str()),
                        Inline::Link { label, .. } => text.push_str(label.as_str()),
                        Inline::Break => text.push(' '),
                    }
                }
                (!text.is_empty()).then(|| Text::new(text))
            }
            Block::Code(_) => None,
        })
    }
}

/// One member row beneath a page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberRow {
    /// Identity header.
    pub symbol: Symbol,
    /// Member signature.
    pub signature: Signature,
    /// First documentation line.
    pub summary: Option<Text>,
}

/// Members of one kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberGroup {
    /// Shared kind.
    pub kind: EntityKind,
    /// Rows in retained member order.
    pub rows: Box<[MemberRow]>,
}

/// Which way a relation was traversed relative to the page.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Direction {
    /// The page is the source.
    Outgoing,
    /// The page is the target.
    Incoming,
}

/// One relation role.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RelationRole {
    /// Canonical link kind.
    pub kind: LinkKind,
    /// Traversal direction.
    pub direction: Direction,
}

/// One relation row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationRow {
    /// Other end of the relation.
    pub target: Target,
    /// Strongest observed confidence.
    pub confidence: Confidence,
}

/// Relations sharing one role.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationGroup {
    /// Shared role.
    pub role: RelationRole,
    /// Rows in canonical link order.
    pub rows: Box<[RelationRow]>,
}

/// Source location retained by the image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocation {
    /// Package-relative file path.
    pub file: Text,
    /// Byte span of the declaration.
    pub span: ByteSpan,
}
