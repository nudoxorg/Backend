//! Borrowed presentation fields selected by a caller's detail level.
//!
//! Omitting a section is a rendering choice, never a claim that its producer
//! failed to capture it. The admitted page and its source remain unchanged.
//!
//! The same selection also *bounds* what a response carries. Documentation,
//! source and relation lists are cut to a budget the detail level sets, and
//! every cut says how much it left out, so the readable text and the typed
//! projection agree and neither can exceed the transport's budget.

use backend_library::DeclarationKind;

use crate::glyph::RelationLabel;
use crate::page::{Listing, Relation, SourceLine};
use crate::window::{Window, WindowKind, one_line};
use crate::{
    Detail, Fault, Identity, Language, MemberGroup, Page, Prose, RelationGroup, Signature, Source,
    SourceSite, Token,
};

/// Longest first documentation paragraph a summary carries, in characters.
const SUMMARY_PROSE_CHARS: usize = 240;
/// Documentation bytes a standard page carries.
const STANDARD_PROSE_BYTES: usize = 1_200;
/// Documentation bytes a full page carries.
const FULL_PROSE_BYTES: usize = 8_000;
/// Source lines and bytes a standard page carries.
const STANDARD_SOURCE: (usize, usize) = (40, 3_000);
/// Source lines and bytes a full page carries.
const FULL_SOURCE: (usize, usize) = (200, 16_000);
/// Relations per label, and in all, a standard page lists.
const STANDARD_RELATIONS: (usize, usize) = (4, 12);
/// Relations per label, and in all, a full page lists.
const FULL_RELATIONS: (usize, usize) = (10, 40);

/// One optional presentation section, separate from its availability.
#[derive(Debug)]
pub enum PageSection<'a, T: ?Sized> {
    /// The caller requested a projection without this section.
    OmittedByDetail,
    /// The producer's actual typed section is included without modification.
    Included(&'a T),
}

impl<'a, T: ?Sized> PageSection<'a, T> {
    /// Returns the included section, without synthesizing an unavailable one.
    #[must_use]
    pub const fn included(&self) -> Option<&'a T> {
        match self {
            Self::OmittedByDetail => None,
            Self::Included(value) => Some(*value),
        }
    }
}

/// The documentation a response carries, cut to its detail's budget.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProseView {
    blocks: Vec<Prose>,
    omitted_bytes: usize,
}

impl ProseView {
    /// Returns the paragraphs, links and code blocks that fit.
    #[must_use]
    pub fn blocks(&self) -> &[Prose] {
        &self.blocks
    }

    /// Returns how many bytes of documentation the cut left out.
    #[must_use]
    pub const fn omitted_bytes(&self) -> usize {
        self.omitted_bytes
    }
}

/// The captured source a response carries, cut to its detail's budget.
#[derive(Clone, Copy, Debug)]
pub struct SourceView<'a> {
    site: &'a SourceSite,
    lines: &'a [SourceLine],
    omitted_lines: usize,
    upstream_truncated: bool,
    window: Option<Window>,
}

impl<'a> SourceView<'a> {
    /// Returns the exact capture site.
    #[must_use]
    pub const fn site(&self) -> &'a SourceSite {
        self.site
    }

    /// Returns the lines that fit.
    #[must_use]
    pub const fn lines(&self) -> &'a [SourceLine] {
        self.lines
    }

    /// Returns how many captured lines the cut left out.
    #[must_use]
    pub const fn omitted_lines(&self) -> usize {
        self.omitted_lines
    }

    /// Returns whether the producer itself retained only a prefix.
    #[must_use]
    pub const fn upstream_truncated(&self) -> bool {
        self.upstream_truncated
    }

    /// Returns whether the text shown is a prefix of the declaration.
    #[must_use]
    pub const fn is_cut(&self) -> bool {
        self.omitted_lines > 0 || self.upstream_truncated
    }

    /// Returns the window a `source` answer was cut to, when it was: its
    /// continuation is paged by cursor, not by a larger detail.
    #[must_use]
    pub const fn window(&self) -> Option<Window> {
        self.window
    }
}

/// One relation label with how many relations it names and the first few.
#[derive(Clone, Copy, Debug)]
pub struct RelationView<'a> {
    label: RelationLabel,
    total: usize,
    shown: &'a [Relation],
}

impl<'a> RelationView<'a> {
    /// Returns the label this group prints.
    #[must_use]
    pub const fn label(&self) -> RelationLabel {
        self.label
    }

    /// Returns how many relations the page holds under this label.
    #[must_use]
    pub const fn total(&self) -> usize {
        self.total
    }

    /// Returns the relations this response lists.
    #[must_use]
    pub const fn shown(&self) -> &'a [Relation] {
        self.shown
    }
}

/// One page's shared typed policy for JSON, Markdown, and terminal output.
#[derive(Debug)]
pub struct PageProjection<'a> {
    identity: &'a Identity,
    kind: Option<DeclarationKind>,
    language: Language,
    detail: Detail,
    signature: Option<&'a Signature>,
    signature_tokens: PageSection<'a, [Token]>,
    prose: PageSection<'a, [Prose]>,
    members: PageSection<'a, [MemberGroup]>,
    relations: PageSection<'a, [RelationGroup]>,
    source: PageSection<'a, Source>,
    source_site: Option<&'a SourceSite>,
    source_fault: Option<&'a Fault>,
    notes: &'a [Fault],
    listing: Option<&'a Listing>,
}

impl<'a> PageProjection<'a> {
    /// Borrows a page. Identity, signature, and actual faults survive every
    /// detail; summary also carries the first sentence of the documentation,
    /// the names of the members, and how many relations each label holds.
    /// Standard and full add the documentation, the relations and the source,
    /// each cut to the detail's budget. A page resumed at a member cursor
    /// carries only the members: the rest was on the first page.
    #[must_use]
    pub fn new(page: &'a Page, detail: Detail) -> Self {
        let source_window = page
            .listing()
            .is_some_and(|listing| listing.window().kind() == WindowKind::Source);
        let continuation = page
            .listing()
            .is_some_and(|listing| listing.window().offset() > 0);
        let rich = !matches!(detail, Detail::Summary) && !continuation;
        Self {
            identity: page.identity(),
            kind: page.kind(),
            language: page.language(),
            detail,
            signature: page.signature(),
            signature_tokens: if rich {
                PageSection::Included(page.signature().map(Signature::tokens).unwrap_or_default())
            } else {
                PageSection::OmittedByDetail
            },
            prose: if continuation {
                PageSection::OmittedByDetail
            } else {
                PageSection::Included(page.prose())
            },
            members: PageSection::Included(page.members()),
            relations: if continuation {
                PageSection::OmittedByDetail
            } else {
                PageSection::Included(page.relations())
            },
            source: if rich || (source_window && !matches!(detail, Detail::Summary)) {
                PageSection::Included(page.source())
            } else {
                PageSection::OmittedByDetail
            },
            source_site: page.source().site(),
            source_fault: page.source().fault(),
            notes: page.notes(),
            listing: page.listing(),
        }
    }

    /// Exact identity, including any selected native symbol operand.
    #[must_use]
    pub const fn identity(&self) -> &'a Identity {
        self.identity
    }

    /// Producer-reported declaration kind.
    #[must_use]
    pub const fn kind(&self) -> Option<DeclarationKind> {
        self.kind
    }

    /// Source language for metadata and signature fences.
    #[must_use]
    pub const fn language(&self) -> Language {
        self.language
    }

    /// The detail this projection was selected at.
    #[must_use]
    pub const fn detail(&self) -> Detail {
        self.detail
    }

    /// Complete captured signature, at every detail level.
    #[must_use]
    pub const fn signature(&self) -> Option<&'a Signature> {
        self.signature
    }

    /// Requested classified tokens; summary retains only complete text.
    #[must_use]
    pub const fn signature_tokens(&self) -> &PageSection<'a, [Token]> {
        &self.signature_tokens
    }

    /// Requested documentation section, before its budget is applied.
    #[must_use]
    pub const fn prose(&self) -> &PageSection<'a, [Prose]> {
        &self.prose
    }

    /// The documentation cut to this detail's budget.
    #[must_use]
    pub fn prose_view(&self) -> ProseView {
        let Some(prose) = self.prose.included() else {
            return ProseView::default();
        };
        match self.detail {
            Detail::Summary => summary_prose(prose),
            Detail::Standard => cut_prose(prose, STANDARD_PROSE_BYTES),
            Detail::Full => cut_prose(prose, FULL_PROSE_BYTES),
        }
    }

    /// The members this response lists, grouped by kind. Always included: the
    /// question a declaration page most often answers is what is inside it.
    #[must_use]
    pub const fn members(&self) -> &PageSection<'a, [MemberGroup]> {
        &self.members
    }

    /// Where the page was cut, when it was windowed.
    #[must_use]
    pub const fn listing(&self) -> Option<&'a Listing> {
        self.listing
    }

    /// Where the member list was cut, when it was.
    #[must_use]
    pub fn member_window(&self) -> Option<Window> {
        self.listing
            .map(Listing::window)
            .filter(|window| window.kind() == WindowKind::Members)
    }

    /// Requested relations section, before its budget is applied.
    #[must_use]
    pub const fn relations(&self) -> &PageSection<'a, [RelationGroup]> {
        &self.relations
    }

    /// The relations cut to this detail's budget: how many each label holds,
    /// and the first few. A summary lists none, only the counts.
    #[must_use]
    pub fn relation_views(&self) -> Vec<RelationView<'a>> {
        let Some(groups) = self.relations.included() else {
            return Vec::new();
        };
        let (per_label, overall) = match self.detail {
            Detail::Summary => (0, 0),
            Detail::Standard => STANDARD_RELATIONS,
            Detail::Full => FULL_RELATIONS,
        };
        let mut remaining = overall;
        groups
            .iter()
            .map(|group| {
                let take = group.relations().len().min(per_label).min(remaining);
                remaining -= take;
                RelationView {
                    label: group.label(),
                    total: group.relations().len(),
                    shown: &group.relations()[..take],
                }
            })
            .collect()
    }

    /// Requested source section, before its budget is applied; omitted never
    /// means absent or unconfigured.
    #[must_use]
    pub const fn source(&self) -> &PageSection<'a, Source> {
        &self.source
    }

    /// The captured source cut to this detail's budget. A `source` answer was
    /// already cut to a window of lines, which is shown whole.
    #[must_use]
    pub fn source_view(&self) -> Option<SourceView<'a>> {
        let Some(Source::Captured {
            site,
            lines,
            truncation,
        }) = self.source.included()
        else {
            return None;
        };
        let window = self
            .listing
            .map(Listing::window)
            .filter(|window| window.kind() == WindowKind::Source);
        let upstream_truncated = *truncation == crate::page::Truncation::Truncated;
        if window.is_some() {
            return Some(SourceView {
                site,
                lines,
                omitted_lines: 0,
                upstream_truncated,
                window,
            });
        }
        let (max_lines, max_bytes) = match self.detail {
            Detail::Full => FULL_SOURCE,
            Detail::Standard | Detail::Summary => STANDARD_SOURCE,
        };
        let mut bytes = 0_usize;
        let mut shown = 0_usize;
        for line in lines.iter() {
            let next = bytes.saturating_add(line.text().len()).saturating_add(6);
            if shown >= max_lines || (shown > 0 && next > max_bytes) {
                break;
            }
            bytes = next;
            shown += 1;
        }
        Some(SourceView {
            site,
            lines: &lines[..shown],
            omitted_lines: lines.len() - shown,
            upstream_truncated,
            window: None,
        })
    }

    /// Real capture site, when known, independent of including its source body.
    #[must_use]
    pub const fn source_site(&self) -> Option<&'a SourceSite> {
        self.source_site
    }

    /// Actual producer fault, preserved even when source text is omitted.
    #[must_use]
    pub const fn source_fault(&self) -> Option<&'a Fault> {
        self.source_fault
    }

    /// Actual section faults, never discarded as optional documentation.
    #[must_use]
    pub const fn notes(&self) -> &'a [Fault] {
        self.notes
    }
}

/// The first paragraph of the documentation, on one line.
fn summary_prose(prose: &[Prose]) -> ProseView {
    let total = prose_bytes(prose);
    let first = prose.iter().find_map(|block| match block {
        Prose::Text(text) => Some(text),
        Prose::Code(_) | Prose::Link { .. } => None,
    });
    let Some(first) = first else {
        return ProseView {
            blocks: Vec::new(),
            omitted_bytes: total,
        };
    };
    let line = one_line(first, SUMMARY_PROSE_CHARS);
    let kept = line.len();
    ProseView {
        blocks: vec![Prose::Text(line)],
        omitted_bytes: total.saturating_sub(kept),
    }
}

/// Keeps whole blocks while they fit and clips the one that crosses the line.
fn cut_prose(prose: &[Prose], budget: usize) -> ProseView {
    let mut blocks = Vec::new();
    let mut used = 0_usize;
    let mut omitted = 0_usize;
    for block in prose {
        let size = block_bytes(block);
        if omitted > 0 || used.saturating_add(size) > budget {
            if omitted == 0 {
                let room = budget.saturating_sub(used);
                if let Some(clipped) = clip_block(block, room) {
                    omitted = size.saturating_sub(clipped_len(&clipped));
                    blocks.push(clipped);
                    continue;
                }
            }
            omitted = omitted.saturating_add(size);
            continue;
        }
        used += size;
        blocks.push(block.clone());
    }
    ProseView {
        blocks,
        omitted_bytes: omitted,
    }
}

fn prose_bytes(prose: &[Prose]) -> usize {
    prose.iter().map(block_bytes).sum()
}

fn block_bytes(block: &Prose) -> usize {
    match block {
        Prose::Text(text) | Prose::Code(text) | Prose::Link { label: text, .. } => text.len(),
    }
}

fn clipped_len(block: &Prose) -> usize {
    block_bytes(block)
}

/// Clips a text or code block to `room` bytes on a character boundary, or
/// declines when almost nothing would remain.
fn clip_block(block: &Prose, room: usize) -> Option<Prose> {
    const MINIMUM: usize = 80;
    if room < MINIMUM {
        return None;
    }
    let clip = |text: &str| {
        let mut end = room.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut clipped = text[..end].trim_end().to_owned();
        clipped.push('…');
        clipped
    };
    match block {
        Prose::Text(text) => Some(Prose::Text(clip(text))),
        Prose::Code(code) => Some(Prose::Code(clip(code))),
        Prose::Link { .. } => None,
    }
}
