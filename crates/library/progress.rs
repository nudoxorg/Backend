//! Observable ingest progress facts a surface can render while work runs.
//!
//! Coverage answers "can this lane be trusted"; it deliberately says nothing
//! about how much of a project has been walked. A surface that has only
//! coverage must therefore render a silent wait: `indexing` with no denominator
//! it can show a reader, for as long as the owner takes.
//!
//! These counts close that gap without inventing a second source of truth.
//! Every number here is derived from the committed product source relation, so
//! it is a pure function of the published revision rather than a transient
//! counter that a restart would lose or a crash would freeze. A file the owner
//! could not read is still a *discovered* file: it is counted, and the reason it
//! carries no declarations is carried beside it, because a file silently
//! dropped from a denominator is exactly the fabricated success this product
//! refuses.
//!
//! The totals are deliberately small and closed:
//!
//! * `files_discovered` — source files the owner selected for this revision.
//! * `files_indexed` — of those, files that produced authoritative
//!   declarations, whether complete or shed to fit one canonical row.
//! * `files_unavailable` — of those, files that produced none, each carrying a
//!   typed [`SourceUnavailableReason`].
//! * one [`LanguageRows`] per language actually observed, never a padded row
//!   per supported language, so an absent language and an empty one stay
//!   distinguishable.
//!
//! `files_indexed + files_unavailable == files_discovered` is an invariant of
//! the constructor, not a convention.

use crate::SourceLanguage;

/// Why one discovered file contributed no declarations to a revision.
///
/// A file the owner walked either produced declarations or produced one of
/// these; there is deliberately no "unknown". The same closed set is written
/// into the canonical source relation as `DeclarationRetention::Unavailable`
/// and rendered by every surface, so it is defined once here rather than
/// duplicated on each side of that boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SourceUnavailableReason {
    /// The file could not be opened or read.
    Unreadable,
    /// The bytes are not valid UTF-8 text.
    NotText,
    /// The file is larger than the bounded per-file ingest limit.
    TooLarge,
    /// The language frontend rejected the file's contents.
    Unparsed,
}

impl SourceUnavailableReason {
    /// Returns the stable lowercase name every surface prints.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unreadable => "unreadable",
            Self::NotText => "not text",
            Self::TooLarge => "too large",
            Self::Unparsed => "unparsed",
        }
    }
}

impl core::fmt::Display for SourceUnavailableReason {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Largest number of distinct languages one progress report may carry.
///
/// The owner supports a closed set of language authorities, so a report that
/// exceeds this is describing something other than this product's ingest.
pub const MAX_PROGRESS_LANGUAGES: usize = 16;

/// Largest number of distinct unavailable reasons one report may carry.
pub const MAX_PROGRESS_FAULTS: usize = 8;

/// How many files and declarations one language contributed to a revision.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LanguageRows {
    language: SourceLanguage,
    files: u64,
    declarations: u64,
}

impl LanguageRows {
    /// Records one language's contribution to the current revision.
    #[must_use]
    pub const fn new(language: SourceLanguage, files: u64, declarations: u64) -> Self {
        Self {
            language,
            files,
            declarations,
        }
    }

    /// Returns the language this row describes.
    #[must_use]
    pub const fn language(self) -> SourceLanguage {
        self.language
    }

    /// Returns how many discovered files this language claimed.
    #[must_use]
    pub const fn files(self) -> u64 {
        self.files
    }

    /// Returns how many declarations those files published.
    #[must_use]
    pub const fn declarations(self) -> u64 {
        self.declarations
    }
}

/// How many files one typed terminal accounts for.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FaultRows {
    reason: SourceUnavailableReason,
    files: u64,
}

impl FaultRows {
    /// Records one terminal's file count.
    #[must_use]
    pub const fn new(reason: SourceUnavailableReason, files: u64) -> Self {
        Self { reason, files }
    }

    /// Returns the terminal this row describes.
    #[must_use]
    pub const fn reason(self) -> SourceUnavailableReason {
        self.reason
    }

    /// Returns how many discovered files ended on it.
    #[must_use]
    pub const fn files(self) -> u64 {
        self.files
    }
}

/// Why a progress report could not be admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgressError {
    /// The indexed and unavailable file counts do not sum to the discovered
    /// count, so the report describes files it cannot account for.
    UnaccountedFiles,
    /// More language rows than one report may carry.
    TooManyLanguages,
    /// More fault rows than one report may carry.
    TooManyFaults,
    /// One language appears twice, so its counts are ambiguous.
    DuplicateLanguage,
    /// One terminal appears twice, so its counts are ambiguous.
    DuplicateReason,
    /// A count exceeded the width of the report.
    CountOverflow,
}

impl core::fmt::Display for ProgressError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(match self {
            Self::UnaccountedFiles => {
                "indexed and unavailable file counts do not sum to the discovered count"
            }
            Self::TooManyLanguages => "more languages than one progress report may carry",
            Self::TooManyFaults => "more fault reasons than one progress report may carry",
            Self::DuplicateLanguage => "one language was reported twice",
            Self::DuplicateReason => "one unavailable reason was reported twice",
            Self::CountOverflow => "a progress count exceeded the report width",
        })
    }
}

impl core::error::Error for ProgressError {}

/// Typed counts describing how far one revision's ingest got.
///
/// An empty report is the honest description of a workspace that has indexed
/// nothing yet, which is why [`IngestProgress::default`] exists and is not the
/// same statement as "no progress information available".
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IngestProgress {
    files_discovered: u64,
    files_indexed: u64,
    files_unavailable: u64,
    declarations: u64,
    languages: Box<[LanguageRows]>,
    faults: Box<[FaultRows]>,
}

impl IngestProgress {
    /// Admits one complete progress report.
    ///
    /// The language rows are sorted by language and the fault rows by reason,
    /// so two owners that walked the same revision produce byte-identical
    /// reports whatever order their relation pages arrived in.
    ///
    /// # Errors
    /// Returns an error when the file counts do not account for one another,
    /// when a language or reason is reported twice, or when either row set is
    /// larger than one report may carry.
    pub fn new(
        files_discovered: u64,
        files_indexed: u64,
        files_unavailable: u64,
        declarations: u64,
        languages: Vec<LanguageRows>,
        faults: Vec<FaultRows>,
    ) -> Result<Self, ProgressError> {
        if files_indexed
            .checked_add(files_unavailable)
            .ok_or(ProgressError::CountOverflow)?
            != files_discovered
        {
            return Err(ProgressError::UnaccountedFiles);
        }
        if languages.len() > MAX_PROGRESS_LANGUAGES {
            return Err(ProgressError::TooManyLanguages);
        }
        if faults.len() > MAX_PROGRESS_FAULTS {
            return Err(ProgressError::TooManyFaults);
        }
        let mut languages = languages;
        languages.sort_by_key(|row: &LanguageRows| row.language());
        if languages.windows(2).any(|pair| {
            pair.first().map(|row| row.language()) == pair.get(1).map(|row| row.language())
        }) {
            return Err(ProgressError::DuplicateLanguage);
        }
        let mut faults = faults;
        faults.sort_by_key(|row: &FaultRows| row.reason());
        if faults
            .windows(2)
            .any(|pair| pair.first().map(|row| row.reason()) == pair.get(1).map(|row| row.reason()))
        {
            return Err(ProgressError::DuplicateReason);
        }
        Ok(Self {
            files_discovered,
            files_indexed,
            files_unavailable,
            declarations,
            languages: languages.into_boxed_slice(),
            faults: faults.into_boxed_slice(),
        })
    }

    /// Returns how many source files this revision selected.
    #[must_use]
    pub const fn files_discovered(&self) -> u64 {
        self.files_discovered
    }

    /// Returns how many of those files published declarations.
    #[must_use]
    pub const fn files_indexed(&self) -> u64 {
        self.files_indexed
    }

    /// Returns how many of those files published none.
    #[must_use]
    pub const fn files_unavailable(&self) -> u64 {
        self.files_unavailable
    }

    /// Returns how many declarations this revision published in total.
    #[must_use]
    pub const fn declarations(&self) -> u64 {
        self.declarations
    }

    /// Returns one row per language actually observed, in language order.
    #[must_use]
    pub fn languages(&self) -> &[LanguageRows] {
        &self.languages
    }

    /// Returns one row per terminal actually observed, in reason order.
    #[must_use]
    pub fn faults(&self) -> &[FaultRows] {
        &self.faults
    }

    /// Returns whether this revision selected no source files at all.
    ///
    /// A surface renders this as "nothing indexed yet", never as "complete".
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.files_discovered == 0
    }
}
