//! Borrowed presentation fields selected by a caller's detail level.
//!
//! Omitting a section is a rendering choice, never a claim that its producer
//! failed to capture it. The admitted page and its source remain unchanged.

use backend_library::DeclarationKind;

use crate::{
    Detail, Fault, Identity, Language, MemberGroup, Page, Prose, RelationGroup, Signature, Source,
    SourceSite, Token,
};

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

/// One page's shared typed policy for JSON, Markdown, and terminal output.
#[derive(Debug)]
pub struct PageProjection<'a> {
    identity: &'a Identity,
    kind: Option<DeclarationKind>,
    language: Language,
    signature: Option<&'a Signature>,
    signature_tokens: PageSection<'a, [Token]>,
    prose: PageSection<'a, [Prose]>,
    members: PageSection<'a, [MemberGroup]>,
    relations: PageSection<'a, [RelationGroup]>,
    source: PageSection<'a, Source>,
    source_site: Option<&'a SourceSite>,
    source_fault: Option<&'a Fault>,
    notes: &'a [Fault],
}

impl<'a> PageProjection<'a> {
    /// Borrows a page. Identity, signature, and actual faults survive summary;
    /// standard and full retain every existing page section.
    #[must_use]
    pub fn new(page: &'a Page, detail: Detail) -> Self {
        Self {
            identity: page.identity(),
            kind: page.kind(),
            language: page.language(),
            signature: page.signature(),
            signature_tokens: section(
                page.signature().map(Signature::tokens).unwrap_or_default(),
                detail,
            ),
            prose: section(page.prose(), detail),
            members: section(page.members(), detail),
            relations: section(page.relations(), detail),
            source: section(page.source(), detail),
            source_site: page.source().site(),
            source_fault: page.source().fault(),
            notes: page.notes(),
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

    /// Requested documentation section.
    #[must_use]
    pub const fn prose(&self) -> &PageSection<'a, [Prose]> {
        &self.prose
    }

    /// Requested members section.
    #[must_use]
    pub const fn members(&self) -> &PageSection<'a, [MemberGroup]> {
        &self.members
    }

    /// Requested relations section.
    #[must_use]
    pub const fn relations(&self) -> &PageSection<'a, [RelationGroup]> {
        &self.relations
    }

    /// Requested source section; omitted never means absent or unconfigured.
    #[must_use]
    pub const fn source(&self) -> &PageSection<'a, Source> {
        &self.source
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

fn section<T: ?Sized>(value: &T, detail: Detail) -> PageSection<'_, T> {
    match detail {
        Detail::Summary => PageSection::OmittedByDetail,
        Detail::Standard | Detail::Full => PageSection::Included(value),
    }
}
