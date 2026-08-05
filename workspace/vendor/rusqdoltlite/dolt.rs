//! Dolt version-control extension.
//!
//! [`DoltConnectionExtension`] adds the Git-like operations DoltLite exposes as
//! SQL functions and virtual tables — staging, commit, branch, checkout, merge,
//! garbage collection, and time-travel resolution — to [`crate::Connection`].
//! Every operation is a thin, typed wrapper over a `dolt_*` SQL call.
//!
//! ## Contract drift (documented deliberately)
//! - DoltLite has **no** `AS OF <timestamp>` SQL syntax and no native
//!   time→commit resolver. [`DoltConnectionExtension::resolve_as_of_time`] is
//!   therefore implemented in Rust by scanning the `dolt_log` virtual table for
//!   the newest commit whose timestamp is at or before the requested instant.
//! - `dolt_merge` reports conflicts as a SQL **error** (`"Merge has N
//!   conflict(s)…"`), not a result row. The wrapper detects that message and
//!   projects it into [`MergeOutcome::Conflicts`], reading the conflicted-table
//!   list from the `dolt_conflicts` virtual table when available.

use crate::connection::Connection;
use crate::dolt_types::{BranchName, CommitHash, MergeOutcome};
use crate::error::EngineError;
use crate::value::Value;

/// Version-control operations available on a DoltLite connection.
pub trait DoltConnectionExtension {
    /// Stage every table's working changes (`dolt_add('-A')`).
    fn dolt_add_all(&self) -> Result<(), EngineError>;

    /// Commit the staged changes with `message`, returning the new commit hash.
    ///
    /// This stages all changes first (`-A`), matching the catalog's "commit the
    /// batch" discipline. Committing with nothing staged is an error surfaced as
    /// [`EngineError::Dolt`].
    fn dolt_commit(&self, message: &str) -> Result<CommitHash, EngineError>;

    /// Create a new branch pointing at the current head.
    fn dolt_branch_create(&self, name: &BranchName) -> Result<(), EngineError>;

    /// Switch the working set to an existing branch.
    fn dolt_checkout(&self, name: &BranchName) -> Result<(), EngineError>;

    /// Merge `from` into the current branch, classifying the outcome.
    fn dolt_merge(&self, from: &BranchName) -> Result<MergeOutcome, EngineError>;

    /// Garbage-collect unreachable chunks from the store.
    fn dolt_gc(&self) -> Result<(), EngineError>;

    /// The commit hash at the tip of the current branch (`dolt_hashof('HEAD')`).
    fn head(&self) -> Result<CommitHash, EngineError>;

    /// Resolve the newest commit on the current branch at or before `unix_milliseconds`.
    ///
    /// Returns `None` when every commit is newer than the requested instant.
    /// See the module-level contract-drift note: this walks `dolt_log` rather than
    /// using engine-native time-travel, which DoltLite does not provide.
    fn resolve_as_of_time(&self, unix_milliseconds: i64)
    -> Result<Option<CommitHash>, EngineError>;
}

impl DoltConnectionExtension for Connection {
    fn dolt_add_all(&self) -> Result<(), EngineError> {
        self.execute("SELECT dolt_add('-A')", &[])
            .map(|_| ())
            .map_err(|error| relabel_dolt(error, "dolt_add"))
    }

    fn dolt_commit(&self, message: &str) -> Result<CommitHash, EngineError> {
        let hash_text = self
            .query_single_text("SELECT dolt_commit('-A', '-m', ?)", &[Value::from(message)])
            .map_err(|error| relabel_dolt(error, "dolt_commit"))?;
        let hash_text = hash_text.ok_or_else(|| EngineError::Dolt {
            operation: "dolt_commit".to_owned(),
            message: "commit returned no hash".to_owned(),
        })?;
        CommitHash::parse(&hash_text)
    }

    fn dolt_branch_create(&self, name: &BranchName) -> Result<(), EngineError> {
        self.execute("SELECT dolt_branch(?)", &[Value::from(name.as_str())])
            .map(|_| ())
            .map_err(|error| relabel_dolt(error, "dolt_branch"))
    }

    fn dolt_checkout(&self, name: &BranchName) -> Result<(), EngineError> {
        self.execute("SELECT dolt_checkout(?)", &[Value::from(name.as_str())])
            .map(|_| ())
            .map_err(|error| relabel_dolt(error, "dolt_checkout"))
    }

    fn dolt_merge(&self, from: &BranchName) -> Result<MergeOutcome, EngineError> {
        // Capture the source branch head first so we can distinguish a
        // fast-forward (result == source head) from a real merge commit.
        let source_head = self.resolve_ref_hash(from.as_str())?;

        // Run the merge with autocommit disabled (inside an explicit
        // transaction). This is required so that a conflicting merge reports the
        // clean "Merge has N conflict(s)" signal and leaves the `dolt_conflicts`
        // virtual table populated for inspection, rather than being auto-rolled
        // back with a terse message. On any conflict we roll the transaction back
        // so the working set is left untouched; the caller decides how to resolve.
        self.execute("BEGIN", &[])
            .map_err(|error| relabel_dolt(error, "dolt_merge"))?;

        let merge_result =
            self.query_single_text("SELECT dolt_merge(?)", &[Value::from(from.as_str())]);

        match merge_result {
            Ok(Some(result_text)) => {
                let outcome = classify_successful_merge(result_text, source_head.as_ref());
                match &outcome {
                    // A clean merge is committed durably. If the COMMIT itself fails
                    // we must roll back first so no path leaves the transaction open.
                    Ok(_) => {
                        if let Err(commit_error) = self.execute("COMMIT", &[]) {
                            let _ = self.execute("ROLLBACK", &[]);
                            return Err(relabel_dolt(commit_error, "dolt_merge"));
                        }
                    }
                    // Classification failed (e.g. an unparseable hash): abandon.
                    Err(_) => {
                        let _ = self.execute("ROLLBACK", &[]);
                    }
                }
                outcome
            }
            Ok(None) => {
                let _ = self.execute("ROLLBACK", &[]);
                Err(EngineError::Dolt {
                    operation: "dolt_merge".to_owned(),
                    message: "merge returned no result".to_owned(),
                })
            }
            Err(error) => {
                // Conflicts surface as a SQL error, not a row. While still inside
                // the transaction, read the conflicted-table list from
                // `dolt_conflicts`, then roll back so the merge leaves no partial
                // state behind.
                if error_indicates_conflict(&error) {
                    let conflicted_tables = self.list_conflicted_tables().unwrap_or_default();
                    let _ = self.execute("ROLLBACK", &[]);
                    Ok(MergeOutcome::Conflicts { conflicted_tables })
                } else {
                    let _ = self.execute("ROLLBACK", &[]);
                    Err(relabel_dolt(error, "dolt_merge"))
                }
            }
        }
    }

    fn dolt_gc(&self) -> Result<(), EngineError> {
        self.execute("SELECT dolt_gc()", &[])
            .map(|_| ())
            .map_err(|error| relabel_dolt(error, "dolt_gc"))
    }

    fn head(&self) -> Result<CommitHash, EngineError> {
        let hash_text = self
            .query_single_text("SELECT dolt_hashof('HEAD')", &[])
            .map_err(|error| relabel_dolt(error, "dolt_hashof"))?
            .ok_or_else(|| EngineError::Dolt {
                operation: "head".to_owned(),
                message: "no head commit".to_owned(),
            })?;
        CommitHash::parse(&hash_text)
    }

    fn resolve_as_of_time(
        &self,
        unix_milliseconds: i64,
    ) -> Result<Option<CommitHash>, EngineError> {
        // `dolt_log.date` is UTC text `YYYY-MM-DD HH:MM:SS`. We compute each
        // commit's unix-second timestamp from that text and keep the newest commit
        // at or before the requested instant. `dolt_log` is ordered newest-first,
        // so the first match is the answer.
        let requested_seconds = unix_milliseconds.div_euclid(1000);
        let candidates = self
            .query_rows("SELECT commit_hash, date FROM dolt_log", &[], |row| {
                let hash = row.get_text(0)?;
                let date = row.get_text(1)?;
                Ok((hash, date))
            })
            .map_err(|error| relabel_dolt(error, "dolt_log"))?;

        for (hash_text, date_text) in candidates {
            if let Some(commit_seconds) = parse_dolt_log_timestamp_seconds(&date_text)
                && commit_seconds <= requested_seconds
            {
                return CommitHash::parse(&hash_text).map(Some);
            }
        }
        Ok(None)
    }
}

impl Connection {
    /// Resolve a ref (branch/tag/commit) to its commit hash, if it exists.
    fn resolve_ref_hash(&self, reference: &str) -> Result<Option<CommitHash>, EngineError> {
        let hash_text = self
            .query_single_text("SELECT dolt_hashof(?)", &[Value::from(reference)])
            .map_err(|error| relabel_dolt(error, "dolt_hashof"))?;
        match hash_text {
            Some(text) => CommitHash::parse(&text).map(Some),
            None => Ok(None),
        }
    }

    /// Read the conflicted-table names from the `dolt_conflicts` virtual table.
    fn list_conflicted_tables(&self) -> Result<Vec<String>, EngineError> {
        self.query_rows("SELECT \"table\" FROM dolt_conflicts", &[], |row| {
            row.get_text(0)
        })
    }
}

/// Classify a `dolt_merge` result that returned a row (i.e. did not conflict).
fn classify_successful_merge(
    result_text: String,
    source_head: Option<&CommitHash>,
) -> Result<MergeOutcome, EngineError> {
    if result_text.eq_ignore_ascii_case("Already up to date") {
        return Ok(MergeOutcome::AlreadyUpToDate);
    }
    let new_head = CommitHash::parse(&result_text)?;
    // A fast-forward advances the branch pointer to the source head verbatim; a
    // three-way merge mints a fresh commit hash distinct from both parents.
    match source_head {
        Some(source) if source == &new_head => Ok(MergeOutcome::FastForward { new_head }),
        _ => Ok(MergeOutcome::MergeCommit { new_head }),
    }
}

/// Whether an engine error is DoltLite's row-level merge-conflict signal.
fn error_indicates_conflict(error: &EngineError) -> bool {
    let message = match error {
        EngineError::Sql {
            extended_message, ..
        } => extended_message.as_str(),
        EngineError::Dolt { message, .. } => message.as_str(),
        EngineError::Constraint { message } => message.as_str(),
        _ => return false,
    };
    message.contains("conflict(s)")
}

/// Convert a generic SQL error from a `dolt_*` call into an [`EngineError::Dolt`].
///
/// Structural errors (busy, corrupt, connection) are passed through unchanged so
/// callers can react to contention or corruption specifically.
fn relabel_dolt(error: EngineError, operation: &str) -> EngineError {
    match error {
        EngineError::Sql {
            extended_message, ..
        } => EngineError::Dolt {
            operation: operation.to_owned(),
            message: extended_message,
        },
        other => other,
    }
}

/// Parse `dolt_log.date` (`YYYY-MM-DD HH:MM:SS`, UTC) into unix seconds.
///
/// A tiny dependency-free civil-date conversion; returns `None` on any malformed
/// field so a single bad row cannot poison time-travel resolution.
fn parse_dolt_log_timestamp_seconds(date_text: &str) -> Option<i64> {
    let (date_part, time_part) = date_text.split_once(' ')?;
    let mut date_fields = date_part.split('-');
    let year: i64 = date_fields.next()?.parse().ok()?;
    let month: i64 = date_fields.next()?.parse().ok()?;
    let day: i64 = date_fields.next()?.parse().ok()?;

    let mut time_fields = time_part.split(':');
    let hour: i64 = time_fields.next()?.parse().ok()?;
    let minute: i64 = time_fields.next()?.parse().ok()?;
    let second: i64 = time_fields.next()?.parse().ok()?;

    if !(1..=12).contains(&month) {
        return None;
    }
    // Reject a day that cannot exist in the given month/year so a malformed row
    // (e.g. `2021-04-31` or `2021-02-30`) is dropped rather than silently rolled
    // over into the following month by the civil-date arithmetic below. This also
    // makes leap-day handling explicit: 29 Feb is only valid in a leap year.
    if day < 1 || day > days_in_month(year, month) {
        return None;
    }
    // Reject out-of-range clock fields for the same reason.
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }

    // Days since the Unix epoch via Howard Hinnant's civil-from-days algorithm.
    let year_adjusted = if month <= 2 { year - 1 } else { year };
    let era = year_adjusted.div_euclid(400);
    let year_of_era = year_adjusted - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days_since_epoch = era * 146_097 + day_of_era - 719_468;

    Some(days_since_epoch * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Whether `year` is a Gregorian leap year.
fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The number of days in `month` (1–12) of `year`, honouring leap years for
/// February. `month` is assumed already validated to the `1..=12` range.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_dolt_log_timestamp_seconds;

    #[test]
    fn parses_epoch() {
        assert_eq!(
            parse_dolt_log_timestamp_seconds("1970-01-01 00:00:00"),
            Some(0)
        );
    }

    #[test]
    fn parses_known_instant() {
        // 2021-01-01 00:00:00 UTC == 1609459200.
        assert_eq!(
            parse_dolt_log_timestamp_seconds("2021-01-01 00:00:00"),
            Some(1_609_459_200)
        );
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(parse_dolt_log_timestamp_seconds("not-a-date"), None);
        assert_eq!(
            parse_dolt_log_timestamp_seconds("2021-13-01 00:00:00"),
            None
        );
    }

    #[test]
    fn accepts_valid_leap_day() {
        // 2020 is a leap year: 29 Feb is legal and equals 1582934400 UTC.
        assert_eq!(
            parse_dolt_log_timestamp_seconds("2020-02-29 00:00:00"),
            Some(1_582_934_400)
        );
    }

    #[test]
    fn rejects_leap_day_in_common_year() {
        // 2021 is not a leap year; 29 Feb must be rejected, not rolled into March.
        assert_eq!(
            parse_dolt_log_timestamp_seconds("2021-02-29 00:00:00"),
            None
        );
    }

    #[test]
    fn rejects_impossible_month_lengths() {
        // April has 30 days; February never has 30.
        assert_eq!(
            parse_dolt_log_timestamp_seconds("2021-04-31 00:00:00"),
            None
        );
        assert_eq!(
            parse_dolt_log_timestamp_seconds("2021-02-30 00:00:00"),
            None
        );
        assert_eq!(
            parse_dolt_log_timestamp_seconds("2021-01-32 00:00:00"),
            None
        );
        assert_eq!(
            parse_dolt_log_timestamp_seconds("2021-01-00 00:00:00"),
            None
        );
    }

    #[test]
    fn rejects_out_of_range_clock_fields() {
        assert_eq!(
            parse_dolt_log_timestamp_seconds("2021-01-01 24:00:00"),
            None
        );
        assert_eq!(
            parse_dolt_log_timestamp_seconds("2021-01-01 00:60:00"),
            None
        );
        assert_eq!(
            parse_dolt_log_timestamp_seconds("2021-01-01 00:00:61"),
            None
        );
    }

    #[test]
    fn century_leap_rules_hold() {
        // 1900 is NOT a leap year (divisible by 100, not 400).
        assert_eq!(
            parse_dolt_log_timestamp_seconds("1900-02-29 00:00:00"),
            None
        );
        // 2000 IS a leap year (divisible by 400).
        assert!(parse_dolt_log_timestamp_seconds("2000-02-29 00:00:00").is_some());
    }

    #[test]
    fn ordering_is_deterministic_at_the_same_second() {
        // Two identical instants must compare equal (deterministic tie handling in
        // `resolve_as_of_time`, which keeps the first newest-first match).
        let a = parse_dolt_log_timestamp_seconds("2021-06-15 12:30:45");
        let b = parse_dolt_log_timestamp_seconds("2021-06-15 12:30:45");
        assert_eq!(a, b);
        assert!(a.is_some());
    }
}
