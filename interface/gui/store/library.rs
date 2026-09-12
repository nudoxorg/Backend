//! Defines the shelf projection for `interface-gui`.
//! This module owns every package row, the active compile, and the add draft.
//! Its narrow surface keeps engine failure values intact all the way to the row.

use interface_core::{PackageCompilePhase, PackageEcosystem, PackageUrl};
use interface_documents::Census;
use interface_identity::{
    CoordinateParseError, PackageCoordinate, ecosystem_tag, parse_ecosystem_tag,
};
use interface_library::{
    AddFailure, AddOutcome, AddProgress, AddRejection, Capability, CapabilityState,
    CompilePhaseProgress, Health, LibraryEpoch, PackageCard, RejectedAdd, RemoveOutcome, Shelf,
    ShelfEntry, ShelfError, ShelfFailure, ShelfStatus, Timestamp,
    render::common::{
        Affordance, Fault, add_affordance, capability_glyph, census_line, rejection_slug,
        relative_age, shelf_failure_slug,
    },
};

/// How a package row is drawn, and the one glyph that says so.
#[derive(Clone, Debug, PartialEq)]
pub enum RowStatus {
    /// Admitted and waiting for the compile lock.
    Requested,
    /// Compiling, somewhere in the eight phases.
    Compiling {
        /// Which phase, and where it sits in the journey.
        progress: CompilePhaseProgress,
    },
    /// Readable.
    Ready {
        /// The card that reopens this package's image.
        card: PackageCard,
    },
    /// Refused, with the engine's own cause retained.
    Failed {
        /// The fault drawn in the row itself, never in a toast.
        fault: Fault,
    },
}

impl RowStatus {
    /// The glyph drawn at the tail of the row.
    #[must_use]
    pub const fn glyph(&self) -> &'static str {
        match self {
            Self::Requested => "◌",
            Self::Compiling { .. } => "◐",
            Self::Ready { .. } => "✓",
            Self::Failed { .. } => "✗",
        }
    }

    /// How far through the eight phases this row is, when it is compiling.
    #[must_use]
    pub const fn ordinal(&self) -> Option<u8> {
        match self {
            Self::Compiling { progress } => Some(progress.ordinal),
            _ => None,
        }
    }

    /// Whether a page can be opened from this row.
    #[must_use]
    pub const fn is_readable(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// One package as the library panel draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct ShelfRow {
    /// The pinned coordinate.
    pub coordinate: PackageCoordinate,
    /// How the row is drawn.
    pub status: RowStatus,
    /// When the reader asked for it.
    pub requested_at: Timestamp,
}

impl ShelfRow {
    /// The kind sparkline this row shows, or an empty line before the census exists.
    #[must_use]
    pub fn census_line(&self) -> String {
        match &self.status {
            RowStatus::Ready { card } => census_line(&card.census),
            RowStatus::Requested => "queued".to_owned(),
            RowStatus::Compiling { progress } => progress.label().to_owned(),
            RowStatus::Failed { fault } => fault.slug.to_owned(),
        }
    }

    /// The census, when the package is readable.
    #[must_use]
    pub const fn census(&self) -> Option<&Census> {
        match &self.status {
            RowStatus::Ready { card } => Some(&card.census),
            _ => None,
        }
    }

    /// How long ago the reader asked for this package.
    #[must_use]
    pub fn age(&self, now: Timestamp) -> String {
        relative_age(now, self.requested_at)
    }
}

/// The compile this window started and can cancel.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveJob {
    /// Which package is compiling.
    pub coordinate: PackageCoordinate,
    /// Which phase it entered last, once one has been reported.
    pub progress: Option<CompilePhaseProgress>,
}

impl ActiveJob {
    /// The eight-dot journey with the entered phase filled in.
    #[must_use]
    pub fn dots(&self) -> String {
        let filled = self
            .progress
            .map_or(0_usize, |progress| usize::from(progress.ordinal).saturating_add(1));
        let mut dots = String::with_capacity(24);
        for step in 0..8_usize {
            dots.push_str(if step < filled { "●" } else { "○" });
        }
        dots
    }

    /// The phase word shown beside the dots.
    #[must_use]
    pub fn label(&self) -> &'static str {
        self.progress.map_or("admitted", CompilePhaseProgress::label)
    }
}

/// What the coordinate field currently holds.
#[derive(Clone, Debug, PartialEq)]
pub enum AddDraft {
    /// Nothing typed yet; the placeholder line is showing.
    Empty,
    /// A complete pinned coordinate.
    Valid {
        /// The coordinate that would be added.
        coordinate: PackageCoordinate,
    },
    /// Text that is not yet a coordinate, with the reason stated in the reader's words.
    Invalid {
        /// The parser's own cause.
        cause: CoordinateParseError,
        /// One line explaining what is missing.
        message: String,
    },
}

impl AddDraft {
    /// Whether submitting would do anything.
    #[must_use]
    pub const fn is_submittable(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }
}

/// The inline add flow: one row until the reader opens it.
#[derive(Clone, Debug, PartialEq)]
pub struct AddFlow {
    /// Whether the row has expanded into a field.
    pub open: bool,
    /// Which ecosystem chip is selected.
    pub ecosystem: PackageEcosystem,
    /// Exactly what the reader typed.
    pub text: String,
    /// What that text parses to.
    pub draft: AddDraft,
    /// The refusal from the last submission, drawn in this row.
    pub fault: Option<Fault>,
}

impl Default for AddFlow {
    fn default() -> Self {
        Self {
            open: false,
            ecosystem: PackageEcosystem::Cargo,
            text: String::new(),
            draft: AddDraft::Empty,
            fault: None,
        }
    }
}

impl AddFlow {
    /// The coordinate text the field would submit, ecosystem prefix included.
    #[must_use]
    pub fn spelled(&self) -> String {
        let typed = self.text.trim();
        if typed.is_empty() {
            return String::new();
        }
        if carries_its_own_ecosystem(typed) {
            return typed.to_owned();
        }
        format!("{}:{typed}", ecosystem_tag(self.ecosystem).as_str())
    }

    /// Re-reads the typed text, replacing the placeholder line with live feedback.
    pub fn retype(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.fault = None;
        let spelled = self.spelled();
        self.draft = if spelled.is_empty() {
            AddDraft::Empty
        } else {
            match PackageCoordinate::parse(&spelled) {
                Ok(coordinate) => AddDraft::Valid { coordinate },
                Err(cause) => AddDraft::Invalid {
                    cause,
                    message: coordinate_message(cause),
                },
            }
        };
    }

    /// Selects an ecosystem chip, which re-validates whatever is already typed.
    pub fn choose(&mut self, ecosystem: PackageEcosystem) {
        self.ecosystem = ecosystem;
        let text = core::mem::take(&mut self.text);
        self.retype(text);
    }

    /// Opens the flow, seeded with optional text such as an example chip.
    pub fn open_with(&mut self, text: &str) {
        self.open = true;
        if let Some(ecosystem) = text.split_once(':').and_then(|(tag, _)| parse_ecosystem_tag(tag)) {
            self.ecosystem = ecosystem;
        }
        self.retype(text);
    }

    /// Collapses the flow back to one row and forgets the draft.
    pub fn close(&mut self) {
        self.open = false;
        self.text.clear();
        self.draft = AddDraft::Empty;
        self.fault = None;
    }

    /// The package URL a valid draft would submit, or `None`.
    #[must_use]
    pub fn package_url(&self) -> Option<PackageUrl> {
        match &self.draft {
            AddDraft::Valid { coordinate } => {
                PackageUrl::try_from(coordinate.package_url_text()).ok()
            }
            AddDraft::Empty | AddDraft::Invalid { .. } => None,
        }
    }
}

fn carries_its_own_ecosystem(typed: &str) -> bool {
    match (typed.find(':'), typed.find('@')) {
        (Some(colon), Some(at)) => colon < at,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// One line, in the reader's words, for every way a coordinate can fail to parse.
///
/// The match is exhaustive on purpose: a new parse failure in the identity crate should stop this
/// build until somebody decides what the field says about it.
#[must_use]
pub fn coordinate_message(cause: CoordinateParseError) -> String {
    match cause {
        CoordinateParseError::Empty => "type a package name and version".to_owned(),
        CoordinateParseError::TooLong { observed, maximum } => {
            format!("{observed} bytes; the longest coordinate is {maximum}")
        }
        CoordinateParseError::MissingEcosystem => {
            "pick an ecosystem, or type one before the colon".to_owned()
        }
        CoordinateParseError::UnknownEcosystem { length } => {
            format!("no ecosystem is spelled with those {length} characters")
        }
        CoordinateParseError::MissingVersion => {
            "pin a version: append @ and the exact release".to_owned()
        }
        CoordinateParseError::EmptyName => "the name before @ is empty".to_owned(),
        CoordinateParseError::EmptyVersion => "the version after @ is empty".to_owned(),
        CoordinateParseError::Character { offset, observed } => {
            let shown = char::from(observed);
            format!("a coordinate cannot contain {shown:?} at byte {offset}")
        }
    }
}

/// Everything the library panel and the status bar read.
#[derive(Clone, Debug, Default)]
pub struct LibraryStore {
    rows: Box<[ShelfRow]>,
    epoch: LibraryEpoch,
    health: Option<Health>,
    active: Option<ActiveJob>,
    shelf_fault: Option<Fault>,
    last_orphan_fault: Option<Fault>,
    /// The inline add flow at the foot of the panel.
    pub add: AddFlow,
}

impl LibraryStore {
    /// Every package row, in shelf order.
    #[must_use]
    pub fn rows(&self) -> &[ShelfRow] {
        &self.rows
    }

    /// One row by coordinate.
    #[must_use]
    pub fn row(&self, coordinate: &PackageCoordinate) -> Option<&ShelfRow> {
        self.rows.iter().find(|row| &row.coordinate == coordinate)
    }

    /// The epoch the shelf was read at.
    #[must_use]
    pub const fn epoch(&self) -> LibraryEpoch {
        self.epoch
    }

    /// Whether the shelf holds nothing at all, which is the first-run state.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The compile this window is driving.
    #[must_use]
    pub const fn active(&self) -> Option<&ActiveJob> {
        self.active.as_ref()
    }

    /// The refusal from the last shelf read, when there was one.
    #[must_use]
    pub const fn shelf_fault(&self) -> Option<&Fault> {
        self.shelf_fault.as_ref()
    }

    /// The fault from the last add whose outcome outlived its job and could name no row.
    ///
    /// A window that restarted mid-compile, or a shelf that was refreshed underneath a running
    /// one, can receive a terminal for a job it no longer remembers. A rejection still names its
    /// URL and becomes a row; a failure without a coordinate names nobody, so its fault is kept
    /// here instead of being dropped.
    #[must_use]
    pub const fn last_orphan_fault(&self) -> Option<&Fault> {
        self.last_orphan_fault.as_ref()
    }

    /// The last capability report.
    #[must_use]
    pub const fn health(&self) -> Option<&Health> {
        self.health.as_ref()
    }

    /// The glyph for one capability, or a dot before health has been read.
    #[must_use]
    pub fn capability_glyph(&self, capability: Capability) -> &'static str {
        self.health
            .as_ref()
            .map_or("·", |health| capability_glyph(health.of(capability)))
    }

    /// Whether the compiler is attached, which gates every add affordance.
    #[must_use]
    pub fn compiler_attached(&self) -> bool {
        self.health
            .as_ref()
            .is_some_and(|health| !matches!(health.of(Capability::Compiler), CapabilityState::Detached))
    }

    /// Replaces the capability report.
    pub fn apply_health(&mut self, health: Health) {
        self.health = Some(health);
    }

    /// Replaces every row from one shelf read.
    pub fn apply_shelf(&mut self, shelf: Result<Shelf, ShelfError>) {
        match shelf {
            Ok(shelf) => {
                self.epoch = shelf.epoch;
                self.rows = shelf.entries.iter().map(project_entry).collect();
                self.shelf_fault = None;
            }
            Err(error) => self.shelf_fault = Some(shelf_fault(&error)),
        }
    }

    /// Records that a compile has been admitted for one coordinate.
    pub fn begin_job(&mut self, coordinate: PackageCoordinate) {
        self.active = Some(ActiveJob {
            coordinate,
            progress: None,
        });
    }

    /// Folds one progress report into the active job and its row.
    pub fn apply_progress(&mut self, progress: AddProgress) {
        let Some(phase) = phase_of(progress) else {
            return;
        };
        let Some(active) = self.active.as_mut() else {
            return;
        };
        active.progress = Some(CompilePhaseProgress::of(phase));
        let coordinate = active.coordinate.clone();
        if let Some(row) = self.rows.iter_mut().find(|row| row.coordinate == coordinate) {
            row.status = RowStatus::Compiling {
                progress: CompilePhaseProgress::of(phase),
            };
        }
    }

    /// Folds the terminal outcome of a compile into its row and clears the active job.
    ///
    /// An outcome that arrives with no active job is an orphan: the window restarted, the shelf
    /// was refreshed, or another surface drove the compile. A rejection still names its URL, so
    /// the failed row is derived from it and pushed; a failure without a coordinate names
    /// nobody, so its fault is retained on the store rather than dropped.
    pub fn finish_job(&mut self, outcome: &AddOutcome) {
        let coordinate = self.active.take().map(|job| job.coordinate);
        match outcome {
            AddOutcome::Ready { card } => {
                let Some(coordinate) = coordinate else {
                    return;
                };
                self.settle_row(coordinate, RowStatus::Ready { card: card.clone() });
            }
            AddOutcome::Rejected(rejected) => {
                let fault = rejection_fault(rejected);
                let derived = coordinate.or_else(|| PackageCoordinate::from_package_url(&rejected.url));
                match derived {
                    Some(coordinate) => self.settle_row(coordinate, RowStatus::Failed { fault }),
                    None => self.last_orphan_fault = Some(fault),
                }
            }
            AddOutcome::Failed(failure) => {
                let fault = failure_fault(coordinate.as_ref(), failure);
                match coordinate {
                    Some(coordinate) => self.settle_row(coordinate, RowStatus::Failed { fault }),
                    None => self.last_orphan_fault = Some(fault),
                }
            }
        }
    }

    /// Folds one compile terminal with the coordinate the errand itself carried, so every orphan
    /// still names its row: the errand's coordinate outlives the active job it may have outlived.
    pub fn finish_job_for(&mut self, errand: &PackageCoordinate, outcome: &AddOutcome) {
        if self.active.as_ref().is_some_and(|job| &job.coordinate == errand) {
            let outcome = outcome.clone();
            self.finish_job(&outcome);
            return;
        }
        let status = match outcome {
            AddOutcome::Ready { card } => RowStatus::Ready { card: card.clone() },
            AddOutcome::Rejected(rejected) => RowStatus::Failed {
                fault: rejection_fault(rejected),
            },
            AddOutcome::Failed(failure) => RowStatus::Failed {
                fault: failure_fault(Some(errand), failure),
            },
        };
        self.settle_row(errand.clone(), status);
    }

    /// Lands one terminal status on the row the coordinate names, or opens a new row for it.
    fn settle_row(&mut self, coordinate: PackageCoordinate, status: RowStatus) {
        match self.rows.iter_mut().find(|row| row.coordinate == coordinate) {
            Some(row) => row.status = status,
            None => self.push_row(coordinate, status),
        }
    }

    /// Folds one remove terminal into its row: a removal erases the row, a refusal erases nothing
    /// and states itself in the row that still stands.
    pub fn apply_removed(&mut self, coordinate: &PackageCoordinate, outcome: &RemoveOutcome) {
        match outcome {
            RemoveOutcome::Removed => {
                let mut rows = core::mem::take(&mut self.rows).into_vec();
                rows.retain(|row| &row.coordinate != coordinate);
                self.rows = rows.into_boxed_slice();
            }
            RemoveOutcome::Busy => {
                let fault = Fault::new(
                    "remove-refused",
                    coordinate.to_string(),
                    Affordance::Packages,
                )
                .detailed("another process holds the compile lock".to_owned());
                if let Some(row) = self.rows.iter_mut().find(|row| &row.coordinate == coordinate) {
                    row.status = RowStatus::Failed { fault };
                }
            }
            RemoveOutcome::Absent => {
                let fault = Fault::new(
                    "remove-refused",
                    coordinate.to_string(),
                    Affordance::Packages,
                );
                if let Some(row) = self.rows.iter_mut().find(|row| &row.coordinate == coordinate) {
                    row.status = RowStatus::Failed { fault };
                }
            }
        }
    }

    fn push_row(&mut self, coordinate: PackageCoordinate, status: RowStatus) {
        let mut rows = core::mem::take(&mut self.rows).into_vec();
        rows.push(ShelfRow {
            coordinate,
            status,
            requested_at: Timestamp::default(),
        });
        self.rows = rows.into_boxed_slice();
    }
}

fn phase_of(progress: AddProgress) -> Option<PackageCompilePhase> {
    match progress {
        AddProgress::Phase(phase) => Some(phase.phase),
        _ => None,
    }
}

fn project_entry(entry: &ShelfEntry) -> ShelfRow {
    let status = match &entry.status {
        ShelfStatus::Requested => RowStatus::Requested,
        ShelfStatus::Compiling { phase } => RowStatus::Compiling {
            progress: CompilePhaseProgress::of(*phase),
        },
        ShelfStatus::Ready { card } => RowStatus::Ready { card: card.clone() },
        ShelfStatus::Failed { cause } => RowStatus::Failed {
            fault: failure_cause_fault(&entry.coordinate, cause),
        },
    };
    ShelfRow {
        coordinate: entry.coordinate.clone(),
        status,
        requested_at: entry.requested_at,
    }
}

fn failure_cause_fault(coordinate: &PackageCoordinate, cause: &ShelfFailure) -> Fault {
    let fault = Fault::new(
        shelf_failure_slug(cause),
        coordinate.to_string(),
        add_affordance(coordinate),
    );
    match cause {
        ShelfFailure::Compiler { summary } => fault.detailed(summary.to_string()),
        _ => fault,
    }
}

fn failure_fault(coordinate: Option<&PackageCoordinate>, failure: &AddFailure) -> Fault {
    match coordinate {
        Some(coordinate) => failure_cause_fault(coordinate, &failure.cause),
        None => Fault::new(
            shelf_failure_slug(&failure.cause),
            String::new(),
            Affordance::Packages,
        ),
    }
}

fn rejection_fault(rejected: &RejectedAdd) -> Fault {
    let operand: String = AsRef::<str>::as_ref(&rejected.url).to_owned();
    let fault = Fault::new(
        rejection_slug(&rejected.rejection),
        operand.clone(),
        Affordance::Add { package: operand },
    );
    match &rejected.rejection {
        AddRejection::Busy { active: Some(busy) } => {
            fault.detailed(format!("{busy} holds the compile lock"))
        }
        AddRejection::CompilerDetached => {
            fault.detailed("this window opened without a compiler".to_owned())
        }
        _ => fault,
    }
}

fn shelf_fault(error: &ShelfError) -> Fault {
    let fault = Fault::new("shelf-unreadable", String::new(), Affordance::Health);
    match error {
        ShelfError::Store { detail } => fault.detailed(detail.to_string()),
        ShelfError::Capacity { maximum } => {
            fault.detailed(format!("the shelf holds at most {maximum} packages"))
        }
        ShelfError::Epoch(_) => fault.detailed("the epoch file could not be read".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interface_core::CorrelationId;

    /// The canonical URL of the package an orphan fixture names.
    const ORPHAN_URL: &str = "pkg:cargo/serde@1.0.196";

    /// The same package as a bare coordinate, which the add field would spell.
    const ORPHAN_COORDINATE: &str = "cargo:serde@1.0.196";

    /// One rejected add whose job the store never began, as a restart mid-flight produces.
    fn orphan_rejection() -> Option<AddOutcome> {
        let url = PackageUrl::try_from(ORPHAN_URL.to_owned()).ok()?;
        Some(AddOutcome::Rejected(RejectedAdd {
            url,
            rejection: AddRejection::CompilerDetached,
        }))
    }

    /// One failed add that names no coordinate, as a lost correlation produces.
    fn orphan_failure() -> AddOutcome {
        AddOutcome::Failed(AddFailure {
            correlation: CorrelationId(1),
            cause: ShelfFailure::PackageNotFound,
        })
    }

    #[test]
    fn an_orphan_rejection_becomes_a_failed_row_derived_from_its_url() {
        let mut store = LibraryStore::default();
        let built = orphan_rejection();
        assert!(built.is_some(), "the orphan rejection fixture must build");
        let Some(outcome) = built else {
            return;
        };
        store.finish_job(&outcome);
        assert!(store.active().is_none(), "no job was begun, so none is active");
        let rows = store.rows();
        assert_eq!(rows.len(), 1, "the rejection still names a row's coordinate");
        let Some(row) = rows.first() else {
            return;
        };
        let url = PackageUrl::try_from(ORPHAN_URL.to_owned());
        assert!(url.is_ok(), "the fixture URL must parse twice");
        let Some(url) = url.ok() else {
            return;
        };
        let Some(derived) = PackageCoordinate::from_package_url(&url) else {
            return;
        };
        assert_eq!(row.coordinate, derived);
        assert_eq!(
            row.status,
            RowStatus::Failed {
                fault: rejection_fault(&RejectedAdd {
                    url,
                    rejection: AddRejection::CompilerDetached,
                }),
            }
        );
        assert!(store.last_orphan_fault().is_none());
    }

    #[test]
    fn an_orphan_failure_retains_its_fault_instead_of_dropping_it() {
        let mut store = LibraryStore::default();
        let outcome = orphan_failure();
        store.finish_job(&outcome);
        assert!(
            store.rows().is_empty(),
            "a failure that names no coordinate cannot open a row"
        );
        let fault = store.last_orphan_fault();
        assert!(
            fault.is_some(),
            "the orphan fault is retained, never dropped"
        );
        let Some(fault) = fault else {
            return;
        };
        let AddOutcome::Failed(failure) = &outcome else {
            return;
        };
        assert_eq!(fault.slug, shelf_failure_slug(&failure.cause));
        assert_eq!(fault.operand, String::new());
    }

    #[test]
    fn an_active_job_still_lands_on_its_own_row_and_leaves_no_orphan() {
        let mut store = LibraryStore::default();
        let Some(coordinate) = PackageCoordinate::parse(ORPHAN_COORDINATE).ok() else {
            return;
        };
        store.begin_job(coordinate.clone());
        let outcome = orphan_failure();
        store.finish_job(&outcome);
        let row = store.row(&coordinate);
        assert!(row.is_some(), "the active job's own row carries the outcome");
        let Some(row) = row else {
            return;
        };
        assert!(matches!(row.status, RowStatus::Failed { .. }));
        assert!(store.last_orphan_fault().is_none(), "the job was not orphaned");
    }
}
