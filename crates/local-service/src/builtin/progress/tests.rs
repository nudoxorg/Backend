//! Exact ingest counts produced by the committed source relation.
//!
//! Every assertion compares whole values — the discovered/indexed/unavailable
//! triple, the per-language rows, and the per-terminal rows — because a count
//! test that only checks a total passes for a report that attributes every
//! file to the wrong language, and a report that attributes files to the wrong
//! language is exactly what a surface would render.

use super::ProgressTally;
use backend_compile::{DeclarationKind, SourceDeclaration};
use backend_engine::{
    FaultRows, IngestProgress, LanguageRows, ProductSourceLanguage as SourceLanguage,
    ProductSourceRecord, SourceUnavailableReason,
};

const PROJECT: [u8; 32] = [7; 32];

/// Builds one declaration with a stable, small payload.
fn declaration(name: &str) -> SourceDeclaration {
    SourceDeclaration::new(name, DeclarationKind::Function, 1, "fn f()", "")
        .expect("checked declaration")
}

/// Builds one indexed file row carrying `count` declarations.
fn indexed(path: &str, language: SourceLanguage, count: usize) -> ProductSourceRecord {
    let declarations = (0..count)
        .map(|index| declaration(&format!("declaration_{index}")))
        .collect::<Vec<_>>();
    ProductSourceRecord::file(PROJECT, path, language, [1; 32], [2; 32], declarations)
        .expect("checked file record")
}

/// Builds one file row that produced nothing, for a typed reason.
fn unavailable(
    path: &str,
    language: SourceLanguage,
    reason: SourceUnavailableReason,
) -> ProductSourceRecord {
    ProductSourceRecord::file_unavailable(PROJECT, path, language, [2; 32], reason)
        .expect("checked unavailable record")
}

/// Folds owned rows into a tally and admits the report.
fn report(rows: &[ProductSourceRecord]) -> IngestProgress {
    let mut tally = ProgressTally::default();
    for row in rows {
        tally.observe(row).expect("observe row");
    }
    tally.finish().expect("admit progress")
}

#[test]
fn an_empty_relation_reports_nothing_discovered_rather_than_nothing_known() {
    let progress = report(&[]);
    assert_eq!(progress.files_discovered(), 0);
    assert_eq!(progress.files_indexed(), 0);
    assert_eq!(progress.files_unavailable(), 0);
    assert_eq!(progress.declarations(), 0);
    assert!(progress.languages().is_empty());
    assert!(progress.faults().is_empty());
    assert!(
        progress.is_empty(),
        "an empty relation must be distinguishable from a covered one"
    );
}

#[test]
fn an_immutable_project_row_is_never_counted_as_a_file() {
    let project =
        ProductSourceRecord::project("polyglot", [3; 32], vec![[9; 32]]).expect("project record");
    let progress = report(&[project, indexed("src/lib.rs", SourceLanguage::Rust, 2)]);
    assert_eq!(
        progress.files_discovered(),
        1,
        "a project row inflated the discovered file count"
    );
    assert_eq!(progress.declarations(), 2);
}

#[test]
fn every_discovered_file_is_accounted_for_by_exactly_one_outcome() {
    let progress = report(&[
        indexed("src/lib.rs", SourceLanguage::Rust, 3),
        indexed("src/main.ts", SourceLanguage::TypeScript, 2),
        unavailable("src/blob.rs", SourceLanguage::Rust, SourceUnavailableReason::NotText),
        unavailable("src/huge.rs", SourceLanguage::Rust, SourceUnavailableReason::TooLarge),
    ]);
    assert_eq!(progress.files_discovered(), 4);
    assert_eq!(progress.files_indexed(), 2);
    assert_eq!(progress.files_unavailable(), 2);
    assert_eq!(
        progress.files_indexed() + progress.files_unavailable(),
        progress.files_discovered(),
        "a discovered file fell out of both outcomes"
    );
}

#[test]
fn per_language_rows_carry_their_own_files_and_declarations() {
    let progress = report(&[
        indexed("src/lib.rs", SourceLanguage::Rust, 3),
        indexed("src/other.rs", SourceLanguage::Rust, 1),
        indexed("src/main.ts", SourceLanguage::TypeScript, 2),
        unavailable("src/blob.py", SourceLanguage::Python, SourceUnavailableReason::NotText),
    ]);
    assert_eq!(
        progress.languages(),
        [
            LanguageRows::new(SourceLanguage::Rust, 2, 4),
            LanguageRows::new(SourceLanguage::TypeScript, 1, 2),
            LanguageRows::new(SourceLanguage::Python, 1, 0),
        ]
        .as_slice(),
        "per-language rows lost their exact attribution"
    );
    assert_eq!(progress.declarations(), 6);
}

#[test]
fn an_unreadable_file_keeps_its_typed_reason_instead_of_disappearing() {
    let progress = report(&[
        indexed("src/lib.rs", SourceLanguage::Rust, 1),
        unavailable("src/a.rs", SourceLanguage::Rust, SourceUnavailableReason::Unreadable),
        unavailable("src/b.rs", SourceLanguage::Rust, SourceUnavailableReason::Unreadable),
        unavailable("src/c.rs", SourceLanguage::Rust, SourceUnavailableReason::Unparsed),
    ]);
    assert_eq!(
        progress.faults(),
        [
            FaultRows::new(SourceUnavailableReason::Unreadable, 2),
            FaultRows::new(SourceUnavailableReason::Unparsed, 1),
        ]
        .as_slice(),
        "typed per-file terminals were collapsed or dropped"
    );
    assert_eq!(
        progress.files_discovered(),
        4,
        "files that produced nothing were dropped from the denominator, which \
         makes a partial index look complete"
    );
}

#[test]
fn a_row_that_shed_detail_to_fit_one_node_is_still_an_indexed_file() {
    // `file_within_row_capacity` sheds excerpts, then signatures, then whole
    // declarations. Every one of those outcomes is a file a reader can use;
    // only the terminal retention means "there is nothing here".
    let dense = (0..4000)
        .map(|index| declaration(&format!("declaration_{index}")))
        .collect::<Vec<_>>();
    let record = ProductSourceRecord::file_within_row_capacity(
        PROJECT,
        "src/dense.rs",
        SourceLanguage::Rust,
        [1; 32],
        [2; 32],
        dense,
    )
    .expect("shed a dense file into one row");
    let progress = report(&[record]);
    assert_eq!(progress.files_indexed(), 1);
    assert_eq!(progress.files_unavailable(), 0);
    assert!(
        progress.faults().is_empty(),
        "a shed row was reported as a file that produced nothing"
    );
    assert!(
        progress.declarations() > 0,
        "a shed row published no declarations at all"
    );
}
