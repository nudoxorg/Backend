//! The shelf: every project the owner indexed, and how ready each one is.
//!
//! Readiness is the first question anyone asks and the one today's surfaces
//! answer worst — a bare path with no state at all. [`Readiness`] makes the
//! four honest answers distinguishable at a glance, and an entry that failed
//! carries the [`Fault`] that explains it rather than a word.

use crate::fault::Fault;
use crate::identity::{Identity, KeyTag};
use crate::language::Language;
use crate::readiness::PublishedFacts;
use backend_library::Lane;

/// How ready one shelf entry is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Readiness {
    /// Every declared lane covered this project.
    Ready,
    /// Indexing is running and has not finished.
    Indexing {
        /// Rows published so far, when the reply carried the published count.
        /// Absent means "not read", never zero.
        rows: Option<RowCount>,
    },
    /// Indexing stopped with a typed failure.
    Failed {
        /// Why the project is not readable.
        fault: Fault,
    },
    /// An index request was accepted and no rows exist yet.
    Requested,
}

impl Readiness {
    /// Returns the stable lowercase name shared by every surface.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Indexing { .. } => "indexing",
            Self::Failed { .. } => "failed",
            Self::Requested => "requested",
        }
    }

    /// Returns the glyph that precedes an entry.
    #[must_use]
    pub const fn glyph(&self) -> &'static str {
        match self {
            Self::Ready => "●",
            Self::Indexing { .. } => "◐",
            Self::Failed { .. } => "✗",
            Self::Requested => "○",
        }
    }

    /// Returns the fault behind a failed entry.
    #[must_use]
    pub const fn fault(&self) -> Option<&Fault> {
        match self {
            Self::Failed { fault } => Some(fault),
            Self::Ready | Self::Indexing { .. } | Self::Requested => None,
        }
    }
}

/// A bounded published-row count.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RowCount(u64);

impl RowCount {
    /// Retains one published-row count.
    #[must_use]
    pub const fn new(rows: u64) -> Self {
        Self(rows)
    }

    /// Returns the published-row count.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// How many declarations one language contributes to a project.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LanguageCount {
    language: Language,
    declarations: RowCount,
}

impl LanguageCount {
    /// Records one language's contribution.
    #[must_use]
    pub const fn new(language: Language, declarations: u64) -> Self {
        Self {
            language,
            declarations: RowCount::new(declarations),
        }
    }

    /// Returns the language.
    #[must_use]
    pub const fn language(self) -> Language {
        self.language
    }

    /// Returns the declaration count.
    #[must_use]
    pub const fn declarations(self) -> RowCount {
        self.declarations
    }
}

/// One project on the shelf.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShelfEntry {
    identity: Identity,
    readiness: Readiness,
    languages: Box<[LanguageCount]>,
    declarations: Option<RowCount>,
    published_rows: Option<RowCount>,
    partial_lanes: Box<[Lane]>,
}

impl ShelfEntry {
    /// Records one project row.
    #[must_use]
    pub fn new(identity: Identity, readiness: Readiness) -> Self {
        Self {
            identity,
            readiness,
            languages: Box::new([]),
            declarations: None,
            published_rows: None,
            partial_lanes: Box::new([]),
        }
    }

    /// Attaches the published row count of the revision this entry was read at.
    #[must_use]
    pub const fn with_published_rows(mut self, rows: Option<RowCount>) -> Self {
        self.published_rows = rows;
        self
    }

    /// Marks the lanes which finished with settled source gaps.
    #[must_use]
    pub fn with_partial_lanes(mut self, lanes: Vec<Lane>) -> Self {
        self.partial_lanes = lanes.into_boxed_slice();
        self
    }

    /// Attaches per-language declaration counts, most declarations first.
    #[must_use]
    pub fn with_languages(mut self, languages: Vec<LanguageCount>) -> Self {
        let mut sorted = languages;
        sorted.sort_by(|left, right| {
            right
                .declarations
                .cmp(&left.declarations)
                .then_with(|| left.language.cmp(&right.language))
        });
        self.declarations = Some(RowCount::new(
            sorted
                .iter()
                .fold(0_u64, |total, row| total.saturating_add(row.declarations.get())),
        ));
        self.languages = sorted.into_boxed_slice();
        self
    }

    /// Returns the project identity.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns how ready the project is.
    #[must_use]
    pub const fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// Returns the per-language declaration counts.
    #[must_use]
    pub fn languages(&self) -> &[LanguageCount] {
        &self.languages
    }

    /// Returns the declaration count when the reply supplied count evidence.
    #[must_use]
    pub const fn declarations(&self) -> Option<RowCount> {
        self.declarations
    }

    /// Returns the count tags every renderer prints after the readiness word,
    /// in print order: the lanes which finished with settled gaps, the rows the
    /// owner published at this revision, then the declaration count.
    ///
    /// A count the reply did not carry is reported as unavailable. It is never
    /// printed as zero, and a row count is never offered as a declaration
    /// count: they are different statements.
    #[must_use]
    pub fn count_tags(&self) -> Vec<String> {
        let mut tags = Vec::new();
        let indexing_rows = match &self.readiness {
            Readiness::Indexing { rows } => *rows,
            Readiness::Ready | Readiness::Failed { .. } | Readiness::Requested => None,
        };
        for lane in self.partial_lanes() {
            tags.push(format!("{} partial", crate::coverage::lane_name(*lane)));
        }
        if let Some(rows) = indexing_rows.or(self.published_rows) {
            tags.push(format!("{} row(s)", rows.get()));
        }
        match self.declarations {
            Some(count) if count.get() > 0 => {
                tags.push(format!("{} declaration(s)", count.get()));
            }
            Some(_) => {}
            None => tags.push("declaration count unavailable".to_owned()),
        }
        tags
    }

    /// Returns the rows the owner published at this revision, when known.
    #[must_use]
    pub const fn published_rows(&self) -> Option<RowCount> {
        self.published_rows
    }

    /// Returns the lanes which finished with settled source gaps. A project
    /// with such a lane is ready and usable, and says where it is partial.
    #[must_use]
    pub fn partial_lanes(&self) -> &[Lane] {
        &self.partial_lanes
    }
}

/// The whole shelf at one immutable revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shelf {
    revision: KeyTag,
    entries: Box<[ShelfEntry]>,
    facts: PublishedFacts,
}

impl Shelf {
    pub(crate) fn expose_mcp_advice(&mut self, exposure: crate::McpExposure) {
        for entry in &mut self.entries {
            if let Readiness::Failed { fault } = &mut entry.readiness {
                *fault = fault.clone().with_mcp_exposure(exposure);
            }
        }
    }

    /// Records one shelf at an exact revision.
    #[must_use]
    pub fn new(revision: KeyTag, entries: impl Into<Box<[ShelfEntry]>>) -> Self {
        Self {
            revision,
            entries: entries.into(),
            facts: PublishedFacts::unread(),
        }
    }

    /// Attaches what the owner published at this revision, so the shelf can
    /// print the same lane statement `status` prints.
    #[must_use]
    pub fn with_facts(mut self, facts: PublishedFacts) -> Self {
        self.facts = facts;
        self
    }

    /// Returns the lane coverage and row count of the revision, when read.
    #[must_use]
    pub const fn facts(&self) -> &PublishedFacts {
        &self.facts
    }

    /// Returns the revision this shelf was read at.
    #[must_use]
    pub const fn revision(&self) -> KeyTag {
        self.revision
    }

    /// Returns the workspace-level statement every renderer prints under the
    /// project list: the readiness word, the published row count, and the lane
    /// line `status` prints for the same revision. Absent when the reply
    /// carried no lane coverage.
    #[must_use]
    pub fn lane_statement(&self) -> Option<(String, crate::coverage::CoverageLine)> {
        let line = *self.facts.lanes()?;
        let mut words = vec![format!("lanes {}", line.readiness())];
        if let Some(rows) = self.facts.rows() {
            words.push(format!("{} row(s)", rows.get()));
        }
        Some((words.join(" · "), line))
    }

    /// Returns every project in display order.
    #[must_use]
    pub fn entries(&self) -> &[ShelfEntry] {
        &self.entries
    }

    /// Returns whether the shelf holds no project.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
