//! One declaration page: everything a reader needs about one identity.
//!
//! A page is assembled once and rendered by every surface. The order is fixed
//! and deliberate, because it is the order a reader answers questions in:
//!
//! 1. **who** — the identity trail and the exact coordinate;
//! 2. **what** — the kind, language, key tag, and source site;
//! 3. **shape** — the signature, as coloured tokens;
//! 4. **meaning** — the prose the producer captured;
//! 5. **parts** — members grouped by kind, one line each;
//! 6. **context** — relations, labelled and counted;
//! 7. **evidence** — the captured source, with real line numbers.
//!
//! Anything missing is stated as a typed [`crate::Fault`], never omitted
//! silently and never replaced with an empty success.

use crate::fault::Fault;
use crate::glyph::RelationLabel;
use crate::identity::{Identity, LineNumber, PackagePath};
use crate::language::Language;
use crate::signature::Signature;
use backend_library::{DeclarationKind, Fragment, SourceExcerptExtent, SymbolKey};

/// One block of producer-captured documentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Prose {
    /// A paragraph of readable text.
    Text(String),
    /// A block of code the producer captured as documentation.
    Code(String),
    /// A link to another declaration.
    Link {
        /// Display label.
        label: String,
        /// Stable declaration target.
        target: SymbolKey,
    },
}

impl Prose {
    /// Folds one fragment run into paragraphs, codes, and links.
    ///
    /// Consecutive text fragments join into one paragraph; an explicit break
    /// starts the next one. Empty paragraphs never survive.
    #[must_use]
    pub fn from_fragments(fragments: &[Fragment]) -> Box<[Self]> {
        let mut blocks = Vec::new();
        let mut paragraph = String::new();
        for fragment in fragments {
            match fragment {
                Fragment::Text(text) => paragraph.push_str(text),
                Fragment::Break => flush(&mut paragraph, &mut blocks),
                Fragment::Code(code) => {
                    flush(&mut paragraph, &mut blocks);
                    blocks.push(Self::Code(code.clone()));
                }
                Fragment::Link { label, target } => {
                    flush(&mut paragraph, &mut blocks);
                    blocks.push(Self::Link {
                        label: label.clone(),
                        target: *target,
                    });
                }
            }
        }
        flush(&mut paragraph, &mut blocks);
        blocks.into_boxed_slice()
    }
}

fn flush(paragraph: &mut String, blocks: &mut Vec<Prose>) {
    let trimmed = paragraph.trim();
    if !trimmed.is_empty() {
        blocks.push(Prose::Text(trimmed.to_owned()));
    }
    paragraph.clear();
}

/// One member of a declaration, as one line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Member {
    identity: Identity,
    kind: Option<DeclarationKind>,
    signature: Option<Signature>,
}

impl Member {
    /// Builds one member line.
    #[must_use]
    pub const fn new(
        identity: Identity,
        kind: Option<DeclarationKind>,
        signature: Option<Signature>,
    ) -> Self {
        Self {
            identity,
            kind,
            signature,
        }
    }

    /// Returns the member's identity.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns the member's declaration kind.
    #[must_use]
    pub const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Returns the member's signature, when the producer captured one.
    #[must_use]
    pub const fn signature(&self) -> Option<&Signature> {
        self.signature.as_ref()
    }
}

/// Members that share one declaration kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberGroup {
    kind: DeclarationKind,
    members: Box<[Member]>,
}

impl MemberGroup {
    /// Groups members by kind in the closed kind order.
    #[must_use]
    pub fn group(members: Vec<Member>) -> Box<[Self]> {
        let mut sorted = members;
        sorted.sort_by_key(|member| {
            (
                member.kind.unwrap_or(DeclarationKind::Unknown).wire_tag(),
                member.identity.name().to_owned(),
            )
        });
        let mut groups: Vec<(DeclarationKind, Vec<Member>)> = Vec::new();
        for member in sorted {
            let kind = member.kind.unwrap_or(DeclarationKind::Unknown);
            match groups.last_mut() {
                Some((current, rows)) if *current == kind => rows.push(member),
                _ => groups.push((kind, vec![member])),
            }
        }
        groups
            .into_iter()
            .map(|(kind, members)| Self {
                kind,
                members: members.into_boxed_slice(),
            })
            .collect()
    }

    /// Returns the kind every member in this group shares.
    #[must_use]
    pub const fn kind(&self) -> DeclarationKind {
        self.kind
    }

    /// Returns the members in display order.
    #[must_use]
    pub fn members(&self) -> &[Member] {
        &self.members
    }
}

/// One related declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Relation {
    identity: Identity,
    kind: Option<DeclarationKind>,
}

impl Relation {
    /// Builds one relation row.
    #[must_use]
    pub const fn new(identity: Identity, kind: Option<DeclarationKind>) -> Self {
        Self { identity, kind }
    }

    /// Returns the related declaration's identity.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns the related declaration's kind.
    #[must_use]
    pub const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }
}

/// Relations that share one label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationGroup {
    label: RelationLabel,
    relations: Box<[Relation]>,
}

impl RelationGroup {
    /// Builds one labelled relation group.
    #[must_use]
    pub fn new(label: RelationLabel, relations: impl Into<Box<[Relation]>>) -> Self {
        Self {
            label,
            relations: relations.into(),
        }
    }

    /// Returns the label this group prints.
    #[must_use]
    pub const fn label(&self) -> RelationLabel {
        self.label
    }

    /// Returns the relations in display order.
    #[must_use]
    pub fn relations(&self) -> &[Relation] {
        &self.relations
    }
}

/// The exact file and line a declaration was captured at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceSite {
    path: PackagePath,
    line: LineNumber,
}

impl SourceSite {
    /// Records one exact capture site.
    #[must_use]
    pub const fn new(path: PackagePath, line: LineNumber) -> Self {
        Self { path, line }
    }

    /// Returns the captured path.
    #[must_use]
    pub const fn path(&self) -> &PackagePath {
        &self.path
    }

    /// Returns the one-based capture line.
    #[must_use]
    pub const fn line(&self) -> LineNumber {
        self.line
    }
}

/// One numbered line of captured source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLine {
    number: LineNumber,
    text: String,
}

impl SourceLine {
    /// Numbers one captured source line.
    #[must_use]
    pub fn new(number: LineNumber, text: impl Into<String>) -> Self {
        Self {
            number,
            text: text.into(),
        }
    }

    /// Returns the one-based line number.
    #[must_use]
    pub const fn number(&self) -> LineNumber {
        self.number
    }

    /// Returns the line text without its terminator.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Whether a bounded rendering carried everything it stands for.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Truncation {
    /// Everything the reply carried is rendered.
    #[default]
    Complete,
    /// The reply itself said its content continues past what it carried.
    Truncated,
}

impl Truncation {
    /// Lowers one excerpt extent into a truncation marker.
    #[must_use]
    pub const fn from_extent(extent: SourceExcerptExtent) -> Self {
        match extent {
            SourceExcerptExtent::Complete => Self::Complete,
            SourceExcerptExtent::Truncated => Self::Truncated,
        }
    }

    /// Returns the stable lowercase name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Truncated => "truncated",
        }
    }
}

/// The captured source behind one declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    /// Source text the producer captured, numbered from its capture site.
    Captured {
        /// Exact capture site.
        site: SourceSite,
        /// Numbered source lines.
        lines: Box<[SourceLine]>,
        /// Whether the retained text is complete.
        truncation: Truncation,
    },
    /// The capture site is known but its bytes are not here.
    Sited {
        /// Exact capture site.
        site: SourceSite,
        /// Why the bytes are not here.
        fault: Fault,
    },
    /// Neither the site nor the bytes are available.
    Absent {
        /// Why nothing is available.
        fault: Fault,
    },
}

impl Source {
    /// Returns the exact capture site, when one is known.
    #[must_use]
    pub const fn site(&self) -> Option<&SourceSite> {
        match self {
            Self::Captured { site, .. } | Self::Sited { site, .. } => Some(site),
            Self::Absent { .. } => None,
        }
    }

    /// Returns the fault explaining missing source, when there is one.
    #[must_use]
    pub const fn fault(&self) -> Option<&Fault> {
        match self {
            Self::Captured { .. } => None,
            Self::Sited { fault, .. } | Self::Absent { fault } => Some(fault),
        }
    }

    /// Numbers captured excerpt text from its one-based start line.
    #[must_use]
    pub fn number_lines(text: &str, start: LineNumber) -> Box<[SourceLine]> {
        text.lines()
            .enumerate()
            .filter_map(|(offset, line)| {
                u32::try_from(offset)
                    .ok()
                    .and_then(|offset| start.get().checked_add(offset))
                    .and_then(LineNumber::new)
                    .map(|number| SourceLine::new(number, line))
            })
            .collect()
    }
}

/// One complete declaration page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Page {
    identity: Identity,
    kind: Option<DeclarationKind>,
    language: Language,
    signature: Option<Signature>,
    prose: Box<[Prose]>,
    members: Box<[MemberGroup]>,
    relations: Box<[RelationGroup]>,
    source: Source,
    notes: Box<[Fault]>,
}

impl Page {
    /// Assembles one page from already-typed parts.
    #[must_use]
    pub fn new(identity: Identity, kind: Option<DeclarationKind>, source: Source) -> Self {
        let language = identity.language();
        Self {
            identity,
            kind,
            language,
            signature: None,
            prose: Box::new([]),
            members: Box::new([]),
            relations: Box::new([]),
            source,
            notes: Box::new([]),
        }
    }

    /// Overrides the language when the coordinate's extension did not name one.
    #[must_use]
    pub const fn with_language(mut self, language: Language) -> Self {
        self.language = language;
        self
    }

    /// Attaches the tokenized signature.
    #[must_use]
    pub fn with_signature(mut self, signature: Signature) -> Self {
        self.signature = (!signature.is_empty()).then_some(signature);
        self
    }

    /// Attaches the producer's captured documentation.
    #[must_use]
    pub fn with_prose(mut self, prose: impl Into<Box<[Prose]>>) -> Self {
        self.prose = prose.into();
        self
    }

    /// Attaches members grouped by kind.
    #[must_use]
    pub fn with_members(mut self, members: impl Into<Box<[MemberGroup]>>) -> Self {
        self.members = members.into();
        self
    }

    /// Attaches labelled relation groups.
    #[must_use]
    pub fn with_relations(mut self, relations: impl Into<Box<[RelationGroup]>>) -> Self {
        self.relations = relations.into();
        self
    }

    /// Attaches the faults that explain a section this page could not fill.
    #[must_use]
    pub fn with_notes(mut self, notes: impl Into<Box<[Fault]>>) -> Self {
        self.notes = notes.into();
        self
    }

    /// Returns the page's identity.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns the declaration kind.
    #[must_use]
    pub const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Returns the source language.
    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }

    /// Returns the tokenized signature.
    #[must_use]
    pub const fn signature(&self) -> Option<&Signature> {
        self.signature.as_ref()
    }

    /// Returns the producer's captured documentation.
    #[must_use]
    pub fn prose(&self) -> &[Prose] {
        &self.prose
    }

    /// Returns the members grouped by kind.
    #[must_use]
    pub fn members(&self) -> &[MemberGroup] {
        &self.members
    }

    /// Returns the labelled relation groups.
    #[must_use]
    pub fn relations(&self) -> &[RelationGroup] {
        &self.relations
    }

    /// Returns the captured source.
    #[must_use]
    pub const fn source(&self) -> &Source {
        &self.source
    }

    /// Returns the faults that explain a section this page could not fill.
    ///
    /// A page with no members and no note claims there are none; a page with
    /// a note claims only that it could not find out.
    #[must_use]
    pub fn notes(&self) -> &[Fault] {
        &self.notes
    }
}
