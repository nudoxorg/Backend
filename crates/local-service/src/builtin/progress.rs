//! Typed ingest counts derived from the committed product source relation.
//!
//! Yesterday a surface that asked an owner "how far are you" got one word and
//! a coverage vector. A reader watching a real crate index therefore had no
//! denominator, no per-language shape, and no way to tell a project that is
//! still walking files from one that finished and found nothing. That is a
//! silent wait, and a silent wait is indistinguishable from a hang.
//!
//! The counts published here are read out of the relation the owner already
//! committed, never out of a side counter kept by the scan. That choice is the
//! whole design:
//!
//! * A count is a pure function of the visible revision, so two readers asking
//!   the same revision get the same answer and a restart loses nothing.
//! * A file the owner could not read is still a file it *discovered*. It keeps
//!   its place in `files_discovered` and contributes the typed reason it
//!   produced no declarations, rather than disappearing from a denominator and
//!   making a partial index look complete.
//! * Immutable project rows are not files and are never counted as such.
//!
//! `DeclarationRetention` and [`SourceUnavailableReason`] answer different
//! questions and only one of them belongs in a fault count. Retention says how
//! much of a row survived one canonical node — a row that shed its excerpts to
//! fit is still an indexed file with authoritative names — while the terminal
//! retention says a reader will find nothing there at all. Only the terminal
//! is a fault.

use super::BuiltinModelError;
use backend_engine::{
    DeclarationRetention, FaultRows, IngestProgress, LanguageRows,
    ProductSourceLanguage as SourceLanguage, ProductSourceRecord, SourceUnavailableReason,
    WorkspaceSnapshot,
};
use std::collections::BTreeMap;

/// Running per-language and per-terminal counts over the source relation.
#[derive(Debug, Default)]
struct ProgressTally {
    files_discovered: u64,
    files_indexed: u64,
    files_unavailable: u64,
    declarations: u64,
    languages: BTreeMap<SourceLanguage, (u64, u64)>,
    faults: BTreeMap<SourceUnavailableReason, u64>,
    observed_projects: u64,
    unknown_projects: u64,
    excluded: BTreeMap<String, u64>,
    extension_projects: BTreeMap<String, u64>,
}

/// One exact fault for a count that outgrew the report width.
fn count_overflow() -> BuiltinModelError {
    BuiltinModelError("ingest progress count exceeds the report width".to_owned())
}

/// Adds one to a counter, or reports the exact overflow.
fn bump(counter: &mut u64, amount: u64) -> Result<(), BuiltinModelError> {
    *counter = counter.checked_add(amount).ok_or_else(count_overflow)?;
    Ok(())
}

impl ProgressTally {
    /// Folds one relation row into the tally, ignoring project rows.
    fn observe(&mut self, record: &ProductSourceRecord) -> Result<(), BuiltinModelError> {
        if let Some(project) = record.project_fields() {
            match project.source_selection {
                None => bump(&mut self.unknown_projects, 1)?,
                Some(observation) => {
                    observation.admit().map_err(BuiltinModelError)?;
                    if observation.observed_projects != 1 || observation.unknown_projects != 0 {
                        return Err(BuiltinModelError(
                            "project source-selection observation has invalid scope".into(),
                        ));
                    }
                    bump(&mut self.observed_projects, 1)?;
                    for row in &observation.unsupported_extensions {
                        bump(
                            self.extension_projects
                                .entry(row.extension.clone())
                                .or_default(),
                            1,
                        )?;
                        bump(
                            self.excluded.entry(row.extension.clone()).or_default(),
                            row.files,
                        )?;
                    }
                }
            }
        }
        let Some(file) = record.file_fields() else {
            return Ok(());
        };
        bump(&mut self.files_discovered, 1)?;
        let declarations = u64::try_from(file.declarations.len()).map_err(|_| count_overflow())?;
        bump(&mut self.declarations, declarations)?;
        let entry = self.languages.entry(file.language).or_insert((0, 0));
        bump(&mut entry.0, 1)?;
        bump(&mut entry.1, declarations)?;
        match file.retention {
            DeclarationRetention::Unavailable(reason) => {
                bump(&mut self.files_unavailable, 1)?;
                let files = self.faults.entry(reason).or_insert(0);
                bump(files, 1)?;
            }
            DeclarationRetention::Complete
            | DeclarationRetention::ExcerptsElided
            | DeclarationRetention::NamesOnly
            | DeclarationRetention::Truncated(_) => bump(&mut self.files_indexed, 1)?,
        }
        Ok(())
    }

    /// Admits the tally as the surface-facing report.
    fn finish(self) -> Result<IngestProgress, BuiltinModelError> {
        let languages = self
            .languages
            .into_iter()
            .map(|(language, (files, declarations))| {
                LanguageRows::new(language, files, declarations)
            })
            .collect();
        let faults = self
            .faults
            .into_iter()
            .map(|(reason, files)| FaultRows::new(reason, files))
            .collect();
        let progress = IngestProgress::new(
            self.files_discovered,
            self.files_indexed,
            self.files_unavailable,
            self.declarations,
            languages,
            faults,
        )
        .map_err(|error| BuiltinModelError(format!("admit ingest progress: {error}")))?;
        if self.observed_projects == 0 && self.unknown_projects == 0 {
            return Ok(progress);
        }
        let project_count = self
            .observed_projects
            .checked_add(self.unknown_projects)
            .ok_or_else(count_overflow)?;
        let scope = backend_library::SourceSelectionScopeCoverage {
            project_count,
            extensions: ["css", "sql", "vue"]
                .into_iter()
                .map(|extension| {
                    let observed = self.extension_projects.get(extension).copied().unwrap_or(0);
                    backend_library::SourceSelectionExtensionCoverage {
                        extension: extension.into(),
                        projects_observed: observed,
                        projects_unknown: project_count - observed,
                        excluded_files: self.excluded.get(extension).copied().unwrap_or(0),
                    }
                })
                .collect(),
        };
        let progress = progress
            .with_source_selection_scope(scope)
            .map_err(BuiltinModelError)?;
        // The legacy aggregate has one observation denominator for every row.
        // Only publish rows witnessed in all observed projects.
        let excluded = self
            .excluded
            .into_iter()
            .filter(|(extension, _)| {
                self.extension_projects.get(extension).copied() == Some(self.observed_projects)
            })
            .map(
                |(extension, files)| backend_library::SourceExtensionExclusion { extension, files },
            )
            .collect::<Vec<_>>();
        if self.observed_projects > 0 && excluded.is_empty() {
            return Ok(progress);
        }
        progress
            .with_source_selection(backend_library::SourceSelectionObservation {
                observed_projects: self.observed_projects,
                unknown_projects: self.unknown_projects,
                unsupported_extensions: excluded.into_boxed_slice(),
            })
            .map_err(BuiltinModelError)
    }
}

/// Reads the committed source relation and reports the owner's ingest counts.
///
/// # Errors
/// Returns an error when the relation cannot be opened or paged, or when a
/// count exceeds the width of the report.
pub(super) fn ingest_progress(
    snapshot: &WorkspaceSnapshot,
) -> Result<IngestProgress, BuiltinModelError> {
    let relation = snapshot
        .relation::<super::BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
    let mut tally = ProgressTally::default();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(format!("page product source: {error}")))?;
        for (_key, record) in page.entries() {
            tally.observe(record)?;
        }
        let Some(next) = page.next().copied() else {
            break;
        };
        after = Some(next);
    }
    tally.finish()
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a fixture that cannot be built is a broken test, and its panic \
              names the fixture more precisely than a propagated error would"
)]
#[path = "progress/tests.rs"]
mod tests;

#[cfg(test)]
mod source_selection_tests {
    use super::*;

    #[test]
    fn scope_counts_extension_presence_per_project() {
        let mut tally = ProgressTally::default();
        let old = ProductSourceRecord::project("old", [1; 32], vec![]).unwrap()
            .with_source_selection(backend_library::SourceSelectionObservation::project_vue(0)).unwrap();
        let new = ProductSourceRecord::project("new", [2; 32], vec![]).unwrap()
            .with_source_selection(backend_library::SourceSelectionObservation::project_extensions(0, 2, 0)).unwrap();
        let legacy = ProductSourceRecord::project("legacy", [3; 32], vec![]).unwrap();
        for row in [&old, &new, &legacy] { tally.observe(row).unwrap(); }
        let progress = tally.finish().unwrap();
        let scope = progress.source_selection_scope().unwrap();
        assert_eq!(scope.project_count, 3);
        let rows = &scope.extensions;
        assert_eq!((rows[0].projects_observed, rows[0].projects_unknown, rows[0].excluded_files), (1, 2, 0));
        assert_eq!((rows[1].projects_observed, rows[1].projects_unknown, rows[1].excluded_files), (1, 2, 2));
        assert_eq!((rows[2].projects_observed, rows[2].projects_unknown, rows[2].excluded_files), (2, 1, 0));
        assert_eq!(progress.source_selection().unwrap().unsupported_extensions.len(), 1);
        assert!(ProgressTally::default().finish().unwrap().source_selection_scope().is_none());
        let mut complete = ProgressTally::default();
        for (name, id) in [("one", 4), ("two", 5), ("three", 6)] {
            let row = ProductSourceRecord::project(name, [id; 32], vec![]).unwrap()
                .with_source_selection(backend_library::SourceSelectionObservation::project_extensions(0, 0, 0)).unwrap();
            complete.observe(&row).unwrap();
        }
        let complete = complete.finish().unwrap();
        let css = &complete.source_selection_scope().unwrap().extensions[0];
        assert_eq!((css.projects_observed, css.projects_unknown, css.excluded_files), (3, 0, 0));
    }

    #[test]
    fn source_selection_tally_retains_known_exclusions_alongside_unknown_legacy_projects() {
        let mut tally = ProgressTally::default();
        let known = ProductSourceRecord::project("known", [1; 32], vec![])
            .expect("known project source record is valid")
            .with_source_selection(backend_library::SourceSelectionObservation::project_vue(12))
            .expect("known source-selection observation is valid");
        let historical = ProductSourceRecord::project("legacy", [2; 32], vec![])
            .expect("legacy project source record is valid");
        tally
            .observe(&known)
            .expect("known source record is tallyable");
        tally
            .observe(&historical)
            .expect("legacy source record is tallyable");
        let progress = tally
            .finish()
            .expect("test tally is within its count bounds");
        assert_eq!(progress.files_discovered(), 0);
        let observation = progress
            .source_selection()
            .expect("known and legacy records produce an observation");
        assert_eq!(observation.observed_projects, 1);
        assert_eq!(observation.unknown_projects, 1);
        assert_eq!(observation.unsupported_extensions[0].files, 12);
        let text = observation.render();
        assert!(text.contains(".vue: 12 unavailable inline scripts"));
        assert!(text.contains("unknown for 1 historical project"));
        assert!(text.contains("totals cover supported extensions"));
        let mut overflow = ProgressTally::default();
        let maximum = ProductSourceRecord::project("maximum", [3; 32], vec![])
            .expect("maximum-count source record is valid")
            .with_source_selection(backend_library::SourceSelectionObservation::project_vue(
                u64::MAX,
            ))
            .expect("maximum source-selection count is valid");
        overflow
            .observe(&maximum)
            .expect("maximum count is accepted before overflow control");
        assert!(
            overflow.observe(&known).is_err(),
            "counts never saturate or silently disappear"
        );
    }
}
