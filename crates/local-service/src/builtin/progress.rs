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
        IngestProgress::new(
            self.files_discovered,
            self.files_indexed,
            self.files_unavailable,
            self.declarations,
            languages,
            faults,
        )
        .map_err(|error| BuiltinModelError(format!("admit ingest progress: {error}")))
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
