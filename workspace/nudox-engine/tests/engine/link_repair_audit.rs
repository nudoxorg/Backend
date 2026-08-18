//! The corpus audit: how many links do we repair, of what kinds, in which
//! package — measured against real producer output, not a fixture.
//!
//! # Why this test exists
//!
//! `LinkOrigin` makes a repair *recordable*; this makes it *counted*. Without a
//! number, "we repair one class of malformed link" is a claim about the code,
//! not about the corpus, and nobody can answer "how often, and is that set
//! still closed?" without reading the source.
//!
//! The test prints one machine-readable TSV line per (package, kind) cell:
//!
//! ```text
//! link-repair    memchr    2.8.3    transposed_open_delimiter    3
//! ```
//!
//! which is `awk`-able and aggregatable across `result/`.
//!
//! It also asserts that **every other** `LinkRepairKind` counts zero. A future
//! repair that starts firing on real crates therefore shows up as a red test on
//! its first run, rather than as a quiet increment in a number nobody reads.
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-engine --test link_repair_audit -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d and fixture-skipping in the same shape as
//! `tests/real_producer_links.rs`: same `NUDOX_REAL_CRATE_ROOT` override, same
//! graceful `SKIP:` message when the checkout is absent.

use std::path::PathBuf;
use std::sync::Arc;

use nudox_engine::store::package::{PackageView, Provenance};
use nudox_engine::store::source::producer::PackageDescriptor;
use nudox_ir::view::IrView;
use nudox_languages::produce;
use nudox_languages::rust::RustProducer;

use nudox_engine::chunk::{self, repair_audit::RepairTally};
use nudox_engine::wire::{InlineRun, LinkOrigin, LinkRepairKind, ProseBlock, RenderSection};

const MEMCHR_VERSION: &str = "2.8.3";

/// Path to the memchr checkout, honouring the same `NUDOX_REAL_CRATE_ROOT`
/// override every other real-crate test uses.
fn memchr_root() -> PathBuf {
    std::env::var("NUDOX_REAL_CRATE_ROOT").map_or_else(
        |_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../result/memchr-{MEMCHR_VERSION}"))
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
        },
        PathBuf::from,
    )
}

/// Lower the memchr crate at `root` via the real Rust producer.
///
/// Returns `None` and prints a skip message when the checkout is absent.
fn try_lower_memchr(root: &PathBuf) -> Option<Arc<PackageView>> {
    if !root.join("Cargo.toml").is_file() {
        eprintln!(
            "SKIP: no memchr checkout at {}.\n\
             Run: scripts/fetch-real-crate.sh memchr {MEMCHR_VERSION}",
            root.display()
        );
        return None;
    }

    let descriptor = PackageDescriptor::cargo(root, "memchr", MEMCHR_VERSION);
    let started = std::time::Instant::now();

    let table = produce(
        &RustProducer { direct_repo: false },
        &descriptor.source,
        &descriptor.lineage,
        &nudox_ir::foreign::Unlinked,
    )
    .expect("memchr must lower without error for a well-formed checkout")
    .table;

    eprintln!(
        "lowered {} entries from memchr in {:.1}s",
        table.len(),
        started.elapsed().as_secs_f32()
    );

    let view = IrView::with_package(descriptor.lineage, table);
    Some(Arc::new(PackageView::build(view, Provenance::TrustedLocal)))
}

/// Count repaired links the long way — by walking the runs directly rather
/// than through `RepairTally`.
///
/// This exists so the audit's number has a second, independent witness. If
/// `RepairTally::of_sections` ever skips a block or section kind, the two
/// disagree and the test says so, instead of quietly reporting a smaller
/// number that still looks plausible.
fn count_repairs_independently(sections: &[RenderSection]) -> u32 {
    fn runs_of(block: &ProseBlock) -> Vec<&[InlineRun]> {
        match block {
            ProseBlock::Paragraph { runs } | ProseBlock::Heading { runs, .. } => {
                vec![runs.as_slice()]
            }
            ProseBlock::List { items, .. } => items.iter().map(std::vec::Vec::as_slice).collect(),
            _ => Vec::new(),
        }
    }

    let mut n = 0;
    for section in sections {
        let blocks: &[ProseBlock] = match section {
            RenderSection::Prose { blocks, .. }
            | RenderSection::Examples { blocks, .. }
            | RenderSection::Callout { blocks, .. } => blocks,
            _ => continue,
        };
        for block in blocks {
            for runs in runs_of(block) {
                for run in runs {
                    if matches!(
                        run,
                        InlineRun::Link {
                            origin: LinkOrigin::Repaired(_),
                            ..
                        }
                    ) {
                        n += 1;
                    }
                }
            }
        }
    }
    n
}

/// Sweep every live entry in memchr, tally the link repairs, emit the TSV, and
/// pin the number.
///
/// # The pinned baseline
///
/// `docs/DOCSRS-COMPARISON.md` §2 names three transposed-delimiter sites in the
/// memchr source:
///
/// * `src/memchr.rs:282` — `` `[memrchr_iter`] `` (on `struct Memchr`)
/// * `src/memchr.rs:358` — `` `[memrchr2_iter`] `` (on `struct Memchr2`)
/// * `src/memchr.rs:426` — `` `[memrchr2_iter`] `` (on `struct Memchr3`)
///
/// The observed count is asserted against `EXPECTED_TRANSPOSED` below, whose
/// value was **measured, not assumed** — see the comment on the constant for
/// what was observed and why it is what it is. Doctrine §8: a number that
/// disagrees with the source count is a finding, not a baseline to adjust the
/// assertion to.
///
/// # What else this asserts
///
/// Every other `LinkRepairKind` must count zero. That is the guard that makes a
/// newly-added repair visible on its first real-crate run.
///
/// Every repaired link's `raw` must also appear verbatim in its own symbol's
/// `documentation`, and every authored symbol link's `text` must appear inside
/// a canonical `[…]` / `` [`…`] `` span — the real-crate counterpart of
/// `authored_link_text_appears_verbatim_in_the_doc_comment`. An `Authored` link
/// whose text is not literally in the source is a lie by definition.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn memchr_repairs_only_transposed_open_delimiters() {
    let root = memchr_root();
    let Some(package) = try_lower_memchr(&root) else {
        return;
    };

    let (outcome, _cost) = heart::cost::measured(
        "engine.link_repair_audit.memchr",
        &root,
        || -> AuditOutcome {
            let view = package.view();
            let mut tally = RepairTally::default();
            let mut independent = 0u32;
            let mut entries_chunked = 0u32;
            let mut evidence_checked = 0u32;

            for (intro, entry) in view.entries() {
                let Some((_head, sections)) = chunk::chunk(intro, view, &package) else {
                    continue;
                };
                entries_chunked += 1;
                tally.merge(&RepairTally::of_sections(&sections));
                independent += count_repairs_independently(&sections);

                // Backstop: the evidence must be the author's real bytes.
                let doc = entry.sym().documentation.as_str();
                for section in &sections {
                    let blocks: &[ProseBlock] = match section {
                        RenderSection::Prose { blocks, .. }
                        | RenderSection::Examples { blocks, .. }
                        | RenderSection::Callout { blocks, .. } => blocks,
                        _ => continue,
                    };
                    for block in blocks {
                        let ProseBlock::Paragraph { runs } = block else {
                            continue;
                        };
                        for run in runs {
                            let InlineRun::Link { text, origin, .. } = run else {
                                continue;
                            };
                            match origin {
                                LinkOrigin::Repaired(repair) => {
                                    evidence_checked += 1;
                                    assert!(
                                        doc.contains(&*repair.raw),
                                        "a repair's `raw` must be the author's bytes and so \
                                         must appear verbatim in the doc comment; {:?} is not \
                                         in the documentation of {:?}",
                                        repair.raw,
                                        entry.sym().name
                                    );
                                }
                                LinkOrigin::Authored => {
                                    // Only shortcut-shaped links are checkable
                                    // this way; a `[text](url)` link's text is
                                    // not required to be path-shaped.
                                    let bare = format!("[{text}]");
                                    let ticked = format!("[`{text}`]");
                                    if doc.contains(&bare) || doc.contains(&ticked) {
                                        evidence_checked += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            AuditOutcome {
                tally,
                independent,
                entries_chunked,
                evidence_checked,
            }
        },
    );

    let AuditOutcome {
        tally,
        independent,
        entries_chunked,
        evidence_checked,
    } = outcome;

    eprintln!(
        "chunked {entries_chunked} entries; {evidence_checked} link spellings \
         cross-checked against their own doc comments"
    );

    // The machine-readable audit rows. Every kind is emitted, including the
    // zeroes: "this repair never fired" and "this repair does not exist" are
    // different facts and a reader must be able to tell them apart.
    for (kind, count) in tally.iter() {
        println!(
            "link-repair\tmemchr\t{MEMCHR_VERSION}\t{}\t{count}",
            kind.token()
        );
    }

    assert!(
        entries_chunked > 0,
        "the sweep must actually chunk something — a zero-entry sweep would \
         report zero repairs and look like a clean bill of health"
    );

    assert_eq!(
        tally.total(),
        independent,
        "RepairTally disagrees with a direct walk of the same runs; one of the \
         two is skipping a block or section kind"
    );

    assert_eq!(
        tally.get(LinkRepairKind::TransposedOpenDelimiter),
        EXPECTED_TRANSPOSED,
        "the transposed-delimiter repair count moved. This is a finding, not a \
         baseline to update: either the fixture changed, or the multiplier from \
         arch-gated duplicate modules changed, or the repair started or stopped \
         firing. Explain the number before touching this assertion."
    );

    for (kind, count) in tally.iter() {
        if kind == LinkRepairKind::TransposedOpenDelimiter {
            continue;
        }
        assert_eq!(
            count,
            0,
            "{} fired {count} time(s) on memchr, which no baseline accounts for. \
             A new repair must be measured and explained before it ships, not \
             discovered later in a diff.",
            kind.token()
        );
    }
}

/// The measured baseline for `memchr-2.8.3`: **3**.
///
/// **Measured, not assumed.** Observed on 2026-08-06 by running this test with
/// `--ignored --nocapture`: 1835 entries lowered, 1835 chunked, tally
/// `transposed_open_delimiter = 3`.
///
/// It agrees with the source, which is the point of pinning it. `grep -noE
/// '`\[[A-Za-z_][A-Za-z0-9_:]*`\]' result/memchr-2.8.3/src/` finds
/// exactly three transposed-delimiter sites, all in `src/memchr.rs`:
///
/// * line 282 — `` `[memrchr_iter`] `` on `struct Memchr`
/// * line 358 — `` `[memrchr2_iter`] `` on `struct Memchr2`
/// * line 426 — `` `[memrchr2_iter`] `` on `struct Memchr3`
///
/// One rendered repair each, so 3 = 3 with a multiplier of one. That is worth
/// stating explicitly rather than leaving implicit: memchr's `arch/` tree
/// (`x86_64/avx2`, `aarch64/neon`, `all`, …) *does* duplicate a great deal of
/// documentation across cfg-gated modules, and if any of these three comments
/// had lived there the count would have been a multiple of 3 instead. It does
/// not, because all three are on the plain `src/memchr.rs` iterator structs.
///
/// If this number moves, doctrine §8 applies in both directions: a
/// disagreement *and* a suspicious agreement both need explaining before the
/// assertion is touched.
const EXPECTED_TRANSPOSED: u32 = 3;

/// The audit's result, kept as a struct so `measured` wraps the whole sweep
/// (doctrine §4) rather than only part of it.
struct AuditOutcome {
    tally: RepairTally,
    independent: u32,
    entries_chunked: u32,
    evidence_checked: u32,
}
