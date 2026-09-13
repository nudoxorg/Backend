//! The shelf: every project on it, with its readiness and its language mix.
//! Projected from one immutable view root plus whatever intents are in flight.
//! A project that was only just requested has no row yet, and says so.
//!
//! The language mix is counted here rather than in a view because it is a
//! presentational fact: the engine knows declarations and source paths, and
//! "this project is mostly Rust with a little C" is the sentence a reader
//! actually wants. Counting it once, in a pure function, is also what makes it
//! testable without a window.

use super::fault::Fault;
use super::identity::Identity;
use crate::theme::language::Language;
use backend_library::{PackageKey, Row, RowId, RowState, ViewRoot};

/// How far along a project is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Readiness {
    /// An intent was accepted but the shelf has no row for it yet.
    Requested,
    /// Rows are arriving; the count grows as the compiler works.
    Indexing {
        /// Declarations committed so far.
        declarations: usize,
    },
    /// The project is complete and readable.
    Ready,
    /// The project could not be indexed; the fault says why.
    Failed(Box<Fault>),
}

impl Readiness {
    /// Returns the glyph drawn in the shelf row.
    pub(crate) const fn glyph(&self) -> char {
        match self {
            Self::Requested => '○',
            Self::Indexing { .. } => '◐',
            Self::Ready => '✓',
            Self::Failed(_) => '✗',
        }
    }

    /// Returns the one-word state name, for tooltips and the status bar.
    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Indexing { .. } => "indexing",
            Self::Ready => "ready",
            Self::Failed(_) => "failed",
        }
    }

    /// Returns the fault, when this project failed.
    pub(crate) fn fault(&self) -> Option<&Fault> {
        match self {
            Self::Failed(fault) => Some(fault),
            _ => None,
        }
    }
}

/// One language and how many declarations it contributed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LanguageCount {
    language: Language,
    declarations: usize,
}

impl LanguageCount {
    /// Returns the language.
    pub(crate) const fn language(self) -> Language {
        self.language
    }

    /// Returns how many declarations this language contributed.
    pub(crate) const fn declarations(self) -> usize {
        self.declarations
    }
}

/// One project on the shelf.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ShelfEntry {
    id: RowId,
    package: Option<PackageKey>,
    identity: Identity,
    readiness: Readiness,
    declarations: usize,
    files: usize,
    languages: Vec<LanguageCount>,
    local: bool,
}

impl ShelfEntry {
    /// Returns the stable row identity.
    pub(crate) const fn id(&self) -> RowId {
        self.id
    }

    /// Returns the package key, when this entry came from a package row.
    pub(crate) const fn package(&self) -> Option<PackageKey> {
        self.package
    }

    /// Returns the project's identity.
    pub(crate) const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Returns how far along the project is.
    pub(crate) const fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// Returns how many declarations the project contributed.
    pub(crate) const fn declarations(&self) -> usize {
        self.declarations
    }

    /// Returns how many distinct source files the project contributed.
    pub(crate) const fn files(&self) -> usize {
        self.files
    }

    /// Returns the language mix, most declarations first.
    pub(crate) fn languages(&self) -> &[LanguageCount] {
        &self.languages
    }

    /// Returns whether this project is a local folder rather than a registry package.
    pub(crate) const fn is_local(&self) -> bool {
        self.local
    }

    /// Returns the badge text: `local`, or the package version.
    pub(crate) fn badge(&self) -> String {
        if self.local {
            return "local".to_owned();
        }
        self.identity
            .coordinate()
            .rsplit('@')
            .next()
            .unwrap_or("package")
            .to_owned()
    }
}

/// Every project on the shelf.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Shelf {
    entries: Vec<ShelfEntry>,
    declarations: usize,
}

impl Shelf {
    /// Projects a shelf from one immutable view root.
    pub(crate) fn project(root: &ViewRoot) -> Self {
        let rows = root.rows();
        let mut entries: Vec<ShelfEntry> = rows
            .iter()
            .filter_map(|row| package_entry(row))
            .collect();
        let mut loose: Vec<Tally> = Vec::new();
        let mut declarations = 0usize;
        for row in rows.iter().filter(|row| matches!(row.id, RowId::Symbol(_))) {
            declarations = declarations.saturating_add(1);
            accumulate(&mut entries, &mut loose, row);
        }
        for orphan in loose {
            entries.push(orphan.into_entry());
        }
        entries.sort_by(|left, right| left.identity.project_name().cmp(right.identity.project_name()));
        for entry in &mut entries {
            finish(entry);
        }
        Self {
            entries,
            declarations,
        }
    }

    /// Returns the projects, sorted by name.
    pub(crate) fn entries(&self) -> &[ShelfEntry] {
        &self.entries
    }

    /// Returns the total declaration count across every project.
    pub(crate) const fn declarations(&self) -> usize {
        self.declarations
    }

    /// Returns whether nothing has been indexed yet.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the entry for one project coordinate.
    pub(crate) fn find(&self, coordinate: &str) -> Option<&ShelfEntry> {
        self.entries
            .iter()
            .find(|entry| entry.identity.coordinate() == coordinate)
    }

    /// Adds a placeholder row for an intent that has no shelf row yet.
    pub(crate) fn with_requested(mut self, coordinates: &[String]) -> Self {
        for coordinate in coordinates {
            if self.find(coordinate).is_some() {
                continue;
            }
            let identity = Identity::parse(coordinate);
            self.entries.push(ShelfEntry {
                id: RowId::Package(backend_library::package_key(coordinate)),
                package: None,
                local: !coordinate.starts_with("pkg:"),
                identity,
                readiness: Readiness::Requested,
                declarations: 0,
                files: 0,
                languages: Vec::new(),
            });
        }
        self.entries
            .sort_by(|left, right| left.identity.project_name().cmp(right.identity.project_name()));
        self
    }

    /// Replaces one project's readiness with a failure.
    pub(crate) fn with_failure(mut self, coordinate: &str, fault: Fault) -> Self {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.identity.coordinate() == coordinate)
        {
            entry.readiness = Readiness::Failed(Box::new(fault));
        }
        self
    }
}

struct Tally {
    identity: Identity,
    counts: Vec<LanguageCount>,
    files: Vec<String>,
}

impl Tally {
    fn into_entry(self) -> ShelfEntry {
        let declarations = self.counts.iter().map(|count| count.declarations).sum();
        let local = !self.identity.coordinate().starts_with("pkg:");
        ShelfEntry {
            id: RowId::Package(backend_library::package_key(self.identity.project())),
            package: None,
            readiness: Readiness::Ready,
            declarations,
            files: self.files.len(),
            languages: self.counts,
            local,
            identity: self.identity,
        }
    }
}

fn package_entry(row: &Row) -> Option<ShelfEntry> {
    let RowId::Package(package) = row.id else {
        return None;
    };
    let identity = Identity::parse(&row.label);
    let local = !identity.coordinate().starts_with("pkg:");
    Some(ShelfEntry {
        id: row.id,
        package: Some(package),
        readiness: readiness_of(row.state),
        declarations: 0,
        files: 0,
        languages: Vec::new(),
        local,
        identity,
    })
}

const fn readiness_of(state: RowState) -> Readiness {
    match state {
        RowState::Ready => Readiness::Ready,
        RowState::Loading => Readiness::Indexing { declarations: 0 },
        RowState::Failed => Readiness::Requested,
    }
}

fn accumulate(entries: &mut Vec<ShelfEntry>, loose: &mut Vec<Tally>, row: &Row) {
    let identity = Identity::parse(&row.label);
    let language = Language::of_path(identity.path().unwrap_or_default());
    let file = identity.path().unwrap_or_default().to_owned();
    if let Some(entry) = locate(entries, row, &identity) {
        bump(&mut entry.languages, language);
        entry.declarations = entry.declarations.saturating_add(1);
        entry.files = entry.files.saturating_add(usize::from(!file.is_empty()));
        return;
    }
    match loose
        .iter_mut()
        .find(|tally| tally.identity.project() == identity.project())
    {
        Some(tally) => {
            bump(&mut tally.counts, language);
            if !file.is_empty() && !tally.files.contains(&file) {
                tally.files.push(file);
            }
        }
        None => loose.push(Tally {
            counts: vec![LanguageCount {
                language,
                declarations: 1,
            }],
            files: if file.is_empty() { Vec::new() } else { vec![file] },
            identity: Identity::parse(identity.project()),
        }),
    }
}

fn locate<'a>(
    entries: &'a mut [ShelfEntry],
    row: &Row,
    identity: &Identity,
) -> Option<&'a mut ShelfEntry> {
    let by_key = entries
        .iter()
        .position(|entry| entry.package.is_some() && entry.package == row.package);
    let at = by_key.or_else(|| {
        entries
            .iter()
            .position(|entry| entry.identity.project() == identity.project())
    })?;
    entries.get_mut(at)
}

fn bump(counts: &mut Vec<LanguageCount>, language: Language) {
    match counts.iter_mut().find(|count| count.language == language) {
        Some(count) => count.declarations = count.declarations.saturating_add(1),
        None => counts.push(LanguageCount {
            language,
            declarations: 1,
        }),
    }
}

fn finish(entry: &mut ShelfEntry) {
    entry
        .languages
        .sort_by(|left, right| {
            right
                .declarations
                .cmp(&left.declarations)
                .then_with(|| left.language.cmp(&right.language))
        });
    if let Readiness::Indexing { .. } = entry.readiness {
        entry.readiness = Readiness::Indexing {
            declarations: entry.declarations,
        };
    }
}
