//! The shelf: every project the owner indexed, and how ready each one is.
//!
//! Readiness is the first question anyone asks and the one today's surfaces
//! answer worst — a bare path with no state at all. [`Readiness`] makes the
//! four honest answers distinguishable at a glance, and an entry that failed
//! carries the [`Fault`] that explains it rather than a word.

use crate::fault::Fault;
use crate::identity::{Identity, KeyTag};
use crate::language::Language;

/// How ready one shelf entry is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Readiness {
    /// Every declared lane covered this project.
    Ready,
    /// Indexing published rows but has not finished.
    Indexing {
        /// Rows published so far.
        rows: RowCount,
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
    declarations: RowCount,
}

impl ShelfEntry {
    /// Records one project row.
    #[must_use]
    pub fn new(identity: Identity, readiness: Readiness) -> Self {
        Self {
            identity,
            readiness,
            languages: Box::new([]),
            declarations: RowCount::new(0),
        }
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
        self.declarations = RowCount::new(
            sorted
                .iter()
                .fold(0_u64, |total, row| total.saturating_add(row.declarations.get())),
        );
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

    /// Returns the total declaration count.
    #[must_use]
    pub const fn declarations(&self) -> RowCount {
        self.declarations
    }
}

/// The whole shelf at one immutable revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shelf {
    revision: KeyTag,
    entries: Box<[ShelfEntry]>,
}

impl Shelf {
    /// Records one shelf at an exact revision.
    #[must_use]
    pub fn new(revision: KeyTag, entries: impl Into<Box<[ShelfEntry]>>) -> Self {
        Self {
            revision,
            entries: entries.into(),
        }
    }

    /// Returns the revision this shelf was read at.
    #[must_use]
    pub const fn revision(&self) -> KeyTag {
        self.revision
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
