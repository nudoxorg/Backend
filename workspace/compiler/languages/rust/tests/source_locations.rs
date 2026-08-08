//! L31/L42 proof: a real declaration in a real crate reports the file and the
//! line where it actually is.
//!
//! # Why this test is shaped the way it is
//!
//! The defect this closes was not "the span was missing". It was that the span
//! *looked* present: `Symbol::source` carried the literal string
//! `<file-id-806>` and `Symbol::span` carried byte offsets, and both
//! type-checked, serialised, and rendered. Every cheap assertion — `is_some()`,
//! `span != 0..0`, `!path.is_empty()` — was already green against that.
//!
//! So this test asserts nothing about shape. It **reads the fixture's own
//! source file** and independently derives where `pub fn memchr` is, then
//! requires the producer's answer to agree. If the producer invented a path,
//! the file read fails. If it invented a line, the arithmetic disagrees. There
//! is no stub that passes this.
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-producer-rust --test source_locations
//! ```

use std::path::PathBuf;

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, PackageLineageId, PackageName},
    entry::{SourceLocation, Unlocated},
    foreign::Unlinked,
    kind::KindDiscriminant,
};
use nudox_producer::{PackageSource, produce};
use nudox_producer_rust::RustProducer;

mod common;

/// The house test crate (`AGENTS-DOCTRINE.md` §8 calls it that), pinned to the
/// version the rest of this crate's corpus assertions use.
const FIXTURE_DIR: &str = "memchr-2.8.3";
const FIXTURE_PKG: &str = "memchr";
const FIXTURE_VERSION: &str = "2.8.3";

/// The declaration this test is about, and the file it is written in.
///
/// Both were read out of the checkout, not remembered: `src/memchr.rs:27` is
/// `pub fn memchr(needle: u8, haystack: &[u8]) -> Option<usize>`. The test
/// re-derives the line number from the file at run time rather than trusting
/// this comment — a comment is a claim, and claims rot (doctrine §8).
const DECL_FILE: &str = "src/memchr.rs";
const DECL_TEXT: &str = "pub fn memchr(needle: u8, haystack: &[u8]) -> Option<usize> {";

fn fixture_root() -> PathBuf {
    common::corpus_root().join(FIXTURE_DIR)
}

/// Lower the fixture through the real producer pipeline.
fn lower() -> PristineIntroTable {
    let root = fixture_root();
    assert!(
        root.join("Cargo.toml").is_file(),
        "corpus fixture missing: {} — provision `.real-crates/` before running this test",
        root.display()
    );

    let src = PackageSource::new(&root, FIXTURE_PKG, FIXTURE_VERSION);
    let lineage = PackageLineageId::new(
        EcosystemId::new("cargo"),
        PackageName::new(FIXTURE_PKG.to_owned()),
    );

    let case = format!("locate/cargo/{FIXTURE_PKG}-{FIXTURE_VERSION}");
    let (produced, _cost) = nudox_test_support::measured(&case, &root, || {
        produce(&RustProducer { direct_repo: false }, &src, &lineage, &Unlinked)
    });
    produced
        .unwrap_or_else(|e| panic!("memchr lowering failed: {}", common::chain(&e)))
        .table
}

/// 1-based line number of `offset` in `text`, derived independently of the
/// producer's line index so that agreement is evidence rather than tautology.
fn line_of(text: &str, offset: usize) -> u32 {
    (text[..offset].matches('\n').count() + 1) as u32
}

/// The invariant: the free function `memchr` in `memchr 2.8.3` reports the path
/// and the line at which it is actually written.
///
/// Both halves are checked against the file on disk, read here rather than
/// hard-coded, so this test fails if the producer's path does not exist, if the
/// declaration moves in a future fixture, or if the line index is off by one.
#[test]
fn the_memchr_function_reports_the_file_and_line_where_it_is_actually_written() {
    let table = lower();

    let (_, entry) = table
        .iter()
        .find(|(_, e)| {
            e.sym().name == "memchr"
                && e.kind().discriminant() == Some(KindDiscriminant::Function)
        })
        .expect("memchr 2.8.3 must lower a Function named `memchr`");

    let SourceLocation::Declared {
        file,
        bytes,
        start,
        end,
    } = entry.location().clone()
    else {
        panic!(
            "`memchr` must report a fully Declared location, got {:?} — a byte \
             span or a named absence is not something a reader can jump to",
            entry.location()
        );
    };

    // Measured 2026-08-08 against `.real-crates/memchr-2.8.3`:
    //   src/memchr.rs, bytes 68..1134, lines 5:1..35:2
    // Byte 68 is the first `/` of `/// Search for the first occurrence…` (the
    // item's syntax node begins at its doc comment) and byte 1134 is the final
    // `}` on line 35. Both were checked against the file directly.
    // ── The path must name a file that really exists in the checkout ─────────
    assert_eq!(
        file.as_str(),
        DECL_FILE,
        "the path must be package-relative and real, not a producer-internal id"
    );
    let on_disk = fixture_root().join(file.as_str());
    let text = std::fs::read_to_string(&on_disk).unwrap_or_else(|e| {
        panic!(
            "producer reported {} but it cannot be read: {e} — the whole point \
             of L42.2 was that `<file-id-806>` named nothing",
            on_disk.display()
        )
    });

    // ── The byte range must cover the declaration, not merely be non-zero ────
    let slice = text
        .get(bytes.as_range())
        .expect("reported byte range must be inside the file it names");
    assert!(
        slice.contains(DECL_TEXT),
        "the reported byte range does not contain the declaration it claims to \
         locate; range covered:\n{}",
        &slice[..slice.len().min(200)]
    );

    // ── The line numbers must be the file's own, computed independently ──────
    let expected_start_line = line_of(&text, bytes.start as usize);
    let expected_end_line = line_of(&text, bytes.end as usize);
    assert_eq!(
        start.line(),
        expected_start_line,
        "start line disagrees with the file itself"
    );
    assert_eq!(
        end.line(),
        expected_end_line,
        "end line disagrees with the file itself"
    );

    // ── And the range must actually bracket the `pub fn` line ────────────────
    let decl_offset = text
        .find(DECL_TEXT)
        .expect("fixture drifted: the declaration text is no longer in this file");
    let decl_line = line_of(&text, decl_offset);
    assert!(
        (start.line()..=end.line()).contains(&decl_line),
        "reported lines {}..={} do not bracket the declaration's own line {}",
        start.line(),
        end.line(),
        decl_line
    );
    // memchr 2.8.3 writes `pub fn memchr` at line 27 behind a doc comment and
    // `#[inline]`; the reported range starts at the doc comment because that is
    // the item's syntax node. Pinning the exact number is what makes this a
    // regression test rather than a smoke test — if the fixture is ever
    // replaced, this is the assertion that says so.
    assert_eq!(decl_line, 27, "fixture drifted: `pub fn memchr` moved");

    // A `Declared` location is the only one that can be navigated to; prove the
    // rendered form is the `path:line:col` every editor accepts.
    assert_eq!(
        format!("{}:{}:{}", file.as_str(), start.line(), start.column()),
        format!("src/memchr.rs:{}:1", expected_start_line)
    );
}

/// The counterpart invariant: an entry the producer *invented* must report a
/// named absence, not a location.
///
/// Without this, "everything is Declared" would pass the test above — and a
/// producer that pointed every synthesized parameter at byte 0 of some file
/// would be indistinguishable from one that got it right.
#[test]
fn producer_synthesized_entries_report_absence_by_name_rather_than_a_location() {
    let table = lower();

    let synthesized = table
        .iter()
        .find(|(_, e)| {
            e.kind().discriminant() == Some(KindDiscriminant::Param)
                && matches!(
                    e.location(),
                    SourceLocation::Unlocated(Unlocated::Synthesized)
                )
        })
        .map(|(_, e)| e.sym().name.clone());

    assert!(
        synthesized.is_some(),
        "memchr lowers thousands of parameters, none of which exist as their own \
         declaration in any file; at least one must say `Synthesized`"
    );

    // No entry may claim a location it cannot support: every `Declared` must
    // name a file that exists.
    let mut declared = 0usize;
    for (_, e) in table.iter() {
        if let SourceLocation::Declared { file, .. } = e.location() {
            declared += 1;
            assert!(
                fixture_root().join(file.as_str()).is_file(),
                "entry '{}' claims to be declared in {}, which does not exist",
                e.sym().name,
                file.as_str()
            );
        }
    }
    assert!(
        declared > 100,
        "memchr must produce real locations in bulk, not for one lucky item; got {declared}"
    );
}
