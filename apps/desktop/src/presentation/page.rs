//! One declaration page, assembled from the rows and documents a service returns.
//! A page is a pure value: identity, signature, prose, members, relations, source.
//! Views render it; they never reach back into a view root to recompute it.
//!
//! Decision, recorded here because the shape of the relation section depends on
//! it: the library's graph and related replies return plain rows with no edge
//! role attached, so this module cannot honestly print "calls 3 · called-by 12".
//! It groups relations by the lane that produced them instead, and labels them
//! with what that lane actually means. Inventing a direction the producer did
//! not send would be exactly the kind of fabricated confidence this product
//! refuses elsewhere; when the library grows typed edges, only
//! [`RelationGroup::label`] has to change.

use super::identity::{Identity, KeyTag};
use super::prose::{self, Block};
use super::signature::Signature;
use crate::theme::language::Language;
use backend_library::{
    DeclarationKind, Document, Row, RowId, SourceAvailability, SourceExcerpt, SourceExcerptExtent,
    SymbolKey,
};

/// Character budget for a member's inline signature preview.
const MEMBER_PREVIEW: usize = 96;

/// One numbered line of captured source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceLine {
    number: u32,
    text: String,
}

impl SourceLine {
    /// Returns the one-based line number.
    pub(crate) const fn number(&self) -> u32 {
        self.number
    }

    /// Returns the line text, without its terminator.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }
}

/// The source region behind a declaration, with its availability stated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SourceBlock {
    /// Source text was captured and is shown.
    Captured {
        /// Package-relative source path.
        path: String,
        /// One-based first line of the excerpt.
        start: u32,
        /// The captured lines.
        lines: Vec<SourceLine>,
        /// Whether the declaration continues past the captured prefix.
        truncated: bool,
    },
    /// The producer retained no source span for this declaration.
    NotCaptured,
    /// A span exists but its bytes are not resident on this host.
    NotHydrated,
    /// This deployment has no source provider for the declaration's origin.
    Unconfigured,
}

impl SourceBlock {
    /// Returns the sentence shown when there is no source to draw.
    pub(crate) const fn absence(&self) -> Option<&'static str> {
        match self {
            Self::Captured { .. } => None,
            Self::NotCaptured => Some("The compiler retained no source span for this declaration."),
            Self::NotHydrated => Some("The source span exists but its bytes are not on this host."),
            Self::Unconfigured => Some("This deployment has no source provider for this origin."),
        }
    }

    /// Returns how many lines were captured.
    pub(crate) fn line_count(&self) -> usize {
        match self {
            Self::Captured { lines, .. } => lines.len(),
            _ => 0,
        }
    }
}

/// One declaration listed under a page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Member {
    symbol: SymbolKey,
    identity: Identity,
    kind: Option<DeclarationKind>,
    preview: String,
    summary: Option<String>,
}

impl Member {
    /// Projects one row into a member entry.
    pub(crate) fn from_row(row: &Row) -> Option<Self> {
        let RowId::Symbol(symbol) = row.id else {
            return None;
        };
        let identity = Identity::parse(&row.label);
        let preview = row.signature.as_deref().map_or_else(
            || identity.name().to_owned(),
            |text| Signature::parse(text, identity.name()).preview(MEMBER_PREVIEW),
        );
        Some(Self {
            symbol,
            identity,
            kind: row.kind,
            preview,
            summary: prose::summary(&prose::fold(&row.document)),
        })
    }

    /// Returns the declaration this member opens.
    pub(crate) const fn symbol(&self) -> SymbolKey {
        self.symbol
    }

    /// Returns the member's identity.
    pub(crate) const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns the member's typed kind.
    pub(crate) const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Returns the one-line signature preview.
    pub(crate) fn preview(&self) -> &str {
        &self.preview
    }

    /// Returns the first sentence of the member's documentation.
    pub(crate) fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }
}

/// Members sharing one declaration kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MemberGroup {
    kind: DeclarationKind,
    members: Vec<Member>,
}

impl MemberGroup {
    /// Returns the kind every member in this group shares.
    pub(crate) const fn kind(&self) -> DeclarationKind {
        self.kind
    }

    /// Returns the members, in producer order.
    pub(crate) fn members(&self) -> &[Member] {
        &self.members
    }
}

/// Which lane produced a set of related declarations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RelationLane {
    /// Declarations adjacent in the compiled relation graph.
    Graph,
    /// Declarations the service considers related to this one.
    Related,
}

/// Related declarations from one lane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RelationGroup {
    lane: RelationLane,
    entries: Vec<Member>,
}

impl RelationGroup {
    /// Builds a group from the rows one lane returned.
    pub(crate) fn new(lane: RelationLane, rows: &[Row], exclude: Option<SymbolKey>) -> Self {
        let entries = rows
            .iter()
            .filter(|row| !matches!((row.id, exclude), (RowId::Symbol(key), Some(self_key)) if key == self_key))
            .filter_map(Member::from_row)
            .collect();
        Self { lane, entries }
    }

    /// Returns the lane that produced this group.
    pub(crate) const fn lane(&self) -> RelationLane {
        self.lane
    }

    /// Returns the group header, which states what the lane means.
    pub(crate) const fn label(&self) -> &'static str {
        match self.lane {
            RelationLane::Graph => "Graph neighbours",
            RelationLane::Related => "Related declarations",
        }
    }

    /// Returns the related declarations.
    pub(crate) fn entries(&self) -> &[Member] {
        &self.entries
    }

    /// Returns whether this lane returned nothing.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// One rendered declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Page {
    identity: Identity,
    key: KeyTag,
    symbol: Option<SymbolKey>,
    kind: Option<DeclarationKind>,
    language: Language,
    signature: Signature,
    prose: Vec<Block>,
    members: Vec<MemberGroup>,
    relations: Vec<RelationGroup>,
    source: SourceBlock,
}

impl Page {
    /// Projects the row a view root already holds into a complete page.
    pub(crate) fn from_row(row: &Row) -> Self {
        let identity = Identity::parse(&row.label);
        let language = Language::of_path(identity.path().unwrap_or_default());
        let signature = row
            .signature
            .as_deref()
            .map_or_else(Signature::default, |text| {
                Signature::parse(text, identity.name())
            });
        Self {
            key: key_tag(row.id),
            symbol: match row.id {
                RowId::Symbol(symbol) => Some(symbol),
                _ => None,
            },
            kind: row.kind,
            language,
            signature,
            prose: prose::fold(&row.document),
            members: Vec::new(),
            relations: Vec::new(),
            source: source_block(&identity, &row.source, &row.excerpt),
            identity,
        }
    }

    /// Replaces prose, signature, and source with a freshly read document.
    pub(crate) fn with_document(mut self, document: &Document) -> Self {
        if let Some(text) = document.signature.as_deref() {
            self.signature = Signature::parse(text, self.identity.name());
        }
        let folded = prose::fold(&document.fragments);
        if !folded.is_empty() {
            self.prose = folded;
        }
        self.source = source_block(&self.identity, &document.location, &document.excerpt);
        self
    }

    /// Groups child declarations by kind, in structural order.
    pub(crate) fn with_members(mut self, rows: &[&Row]) -> Self {
        let mut groups: Vec<MemberGroup> = Vec::new();
        for row in rows {
            let Some(member) = Member::from_row(row) else {
                continue;
            };
            let kind = member.kind.unwrap_or(DeclarationKind::Unknown);
            match groups.iter_mut().find(|group| group.kind == kind) {
                Some(group) => group.members.push(member),
                None => groups.push(MemberGroup {
                    kind,
                    members: vec![member],
                }),
            }
        }
        groups.sort_by_key(|group| crate::theme::kind::group_rank(group.kind));
        self.members = groups;
        self
    }

    /// Attaches the relation groups one or both lanes returned.
    pub(crate) fn with_relations(mut self, groups: Vec<RelationGroup>) -> Self {
        self.relations = groups.into_iter().filter(|group| !group.is_empty()).collect();
        self
    }

    /// Resolves every type token in the signature against declaration names.
    pub(crate) fn resolve_signature(mut self, lookup: &impl Fn(&str) -> Option<SymbolKey>) -> Self {
        self.signature = self.signature.resolve_with(lookup);
        self
    }

    /// Returns the page's identity.
    pub(crate) const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns the copyable short tag for this page's stable identity.
    pub(crate) const fn key(&self) -> &KeyTag {
        &self.key
    }

    /// Returns the declaration this page shows, when it is a declaration.
    pub(crate) const fn symbol(&self) -> Option<SymbolKey> {
        self.symbol
    }

    /// Returns the typed declaration kind.
    pub(crate) const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Returns the source language.
    pub(crate) const fn language(&self) -> Language {
        self.language
    }

    /// Returns the tokenized signature.
    pub(crate) const fn signature(&self) -> &Signature {
        &self.signature
    }

    /// Returns the documentation blocks.
    pub(crate) fn prose(&self) -> &[Block] {
        &self.prose
    }

    /// Returns the member groups, in structural order.
    pub(crate) fn members(&self) -> &[MemberGroup] {
        &self.members
    }

    /// Returns the relation groups.
    pub(crate) fn relations(&self) -> &[RelationGroup] {
        &self.relations
    }

    /// Returns the source region behind this declaration.
    pub(crate) const fn source(&self) -> &SourceBlock {
        &self.source
    }

    /// Returns the first sentence of documentation, for hover cards.
    pub(crate) fn summary(&self) -> Option<String> {
        prose::summary(&self.prose)
    }

    /// Returns the total number of members across every group.
    pub(crate) fn member_count(&self) -> usize {
        self.members.iter().map(|group| group.members.len()).sum()
    }
}

fn key_tag(id: RowId) -> KeyTag {
    match id {
        RowId::Symbol(symbol) => KeyTag::of_symbol(symbol),
        other => KeyTag::of_row(other),
    }
}

fn source_block(
    identity: &Identity,
    location: &SourceAvailability,
    excerpt: &SourceExcerpt,
) -> SourceBlock {
    let start = location
        .captured()
        .map_or_else(|| identity.line().unwrap_or(1), backend_library::SourceLocation::start_line);
    let path = location.captured().map_or_else(
        || identity.path().unwrap_or_default().to_owned(),
        |site| site.path().to_owned(),
    );
    match excerpt {
        SourceExcerpt::Captured { text, extent } => SourceBlock::Captured {
            path,
            start,
            lines: number_lines(text, start),
            truncated: matches!(extent, SourceExcerptExtent::Truncated),
        },
        SourceExcerpt::NotCaptured => match location {
            SourceAvailability::NotHydrated => SourceBlock::NotHydrated,
            SourceAvailability::Unconfigured => SourceBlock::Unconfigured,
            _ => SourceBlock::NotCaptured,
        },
        SourceExcerpt::NotHydrated => SourceBlock::NotHydrated,
        SourceExcerpt::Unconfigured => SourceBlock::Unconfigured,
    }
}

fn number_lines(text: &str, start: u32) -> Vec<SourceLine> {
    text.lines()
        .enumerate()
        .map(|(offset, line)| SourceLine {
            number: start.saturating_add(u32::try_from(offset).unwrap_or(u32::MAX)),
            text: line.to_owned(),
        })
        .collect()
}
