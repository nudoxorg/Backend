//! Computing a symbol's history from the loaded generations of its package.
//!
//! # The whole idea in one sentence
//!
//! Two lowerings of the same package at different versions share `IntroId`s for
//! every symbol that persisted, so "the versions in which this `IntroId`
//! appears" *is* the symbol's timeline.
//!
//! That is not a convenience — it is the point of `IntroId`. An id assigned
//! once at first insertion and preserved through renames (K18) is precisely an
//! answer to "is this the same symbol as before", which is the question every
//! history view has to answer first and which no name-, path- or
//! signature-keyed scheme can answer correctly. A symbol renamed from `Router`
//! to `App` is one row-sequence here, not two, because the IR already decided
//! it was one symbol. Building a side table keyed on anything else would be
//! building a worse copy of a decision that has already been made.
//!
//! # Why `entry_content_hash` is not used
//!
//! `nudox_ir::content::entry_content_hash` is the only entry-comparison
//! primitive the IR offers, and it is the wrong one for this job. Its preimage
//! includes `Symbol::source` (the declaring file path) and `Symbol::span` (the
//! byte range), by design — it is the identity of an entry *as produced*, which
//! is what a change-tracking VCS wants.
//!
//! For a timeline it produces a false positive on almost every row. Two
//! versions of a crate are unpacked at two different roots, so every path
//! differs; and a declaration that merely moved down four lines because
//! something above it grew has a different span. Classifying on that hash would
//! report `SignatureChanged` for every symbol in every version — noise that is
//! indistinguishable from signal, which is worse than reporting nothing.
//!
//! It is also not salvageable as a fast path. "Equal hash implies unchanged" is
//! sound, but under differing roots the hashes are never equal, so the branch
//! would cost a full entry re-encode per version and never fire.
//!
//! # What is compared instead
//!
//! Location-free, user-visible facts, in the order a reader cares about them:
//!
//! | Axis | Source | Reported as |
//! |------|--------|-------------|
//! | name | `Symbol::name` | `Renamed` |
//! | deprecation | `Symbol::deprecation.is_some()` | `Deprecated` / `Undeprecated` |
//! | signature | `chunk::signature::tokens` | `SignatureChanged` |
//! | visibility | `Symbol::visibility` | `VisibilityChanged` |
//! | docs | `Symbol::documentation` | `DocsChanged` |
//!
//! The signature axis reuses `chunk::signature::tokens` — the single source of
//! signature rendering in the system (LR-4) — rather than a comparison written
//! for this module. That means a timeline row can never claim a signature
//! changed in a way the symbol page would not show, because both are looking at
//! the same tokens.
//!
//! At most one axis is reported per row; see [`TimelineRow::change`] for the
//! priority order and why the row carries enough state for a caller to recover
//! the rest.
//!
//! # Cost
//!
//! One `HashMap` lookup and one signature render per loaded generation. With
//! the handful of generations a caller realistically loads this is dominated by
//! the channel send that carries the result, which is why `doc.rs` computes it
//! inline on the streaming task rather than paying for a blocking-pool hop.

use nudox_ir::entry::Entry;

use crate::{
    chunk::signature,
    versions::VersionSlice,
    wire::{SharedStr, SigToken, SymbolKey, Timeline, TimelineChange, TimelineRow},
};

/// The previous generation in which the symbol was present.
///
/// Borrows the `Entry` out of the `Arc<PackageView>` held by the slice rather
/// than snapshotting its fields. Every generation's view is alive for the whole
/// walk, so there is nothing to copy and nothing to hash: comparison is direct
/// field access against the real IR.
pub(crate) struct Seen<'a> {
    pub(crate) entry: &'a Entry,
    pub(crate) sig: Vec<SigToken>,
}

/// Build the timeline for `key` across `slices`.
///
/// `slices` must be **oldest first** — [`crate::versions::VersionRegistry::slices`]
/// guarantees this. Classification is inherently directional: "introduced" and
/// "removed" only mean something relative to what came before.
///
/// The returned rows are **newest first**, matching
/// [`crate::wire::VersionList::versions`], so a caller renders both the
/// dropdown and the timeline top-down without reversing either.
///
/// # What an empty `slices` produces
///
/// A timeline with zero rows and `versions_examined: 0`. This is unreachable
/// through `open_symbol`, which substitutes a single-generation slice for the
/// resident package when the registry has nothing (see `doc.rs`), and is
/// therefore never what a user sees — but returning an empty list is the
/// correct answer to "what history does this corpus hold", and inventing a row
/// out of nothing would not be.
pub(crate) fn build(key: &SymbolKey, slices: &[VersionSlice]) -> Timeline {
    let mut rows: Vec<TimelineRow> = Vec::with_capacity(slices.len());
    // The generation immediately preceding this one, if the symbol was present
    // there. Cleared on absence, which is what makes both the removal
    // transition and a later re-introduction fall out without extra state.
    let mut prev: Option<Seen<'_>> = None;

    for (idx, slice) in slices.iter().enumerate() {
        let Some(entry) = slice.package.view().entry(key.intro) else {
            // Emit `Removed` only on the *transition* — the first version in
            // which the symbol is gone. Every version after that is also
            // missing it, and a run of identical `Removed` rows would state the
            // same fact once per release.
            if prev.is_some() {
                rows.push(TimelineRow {
                    version: slice.version.clone(),
                    change: TimelineChange::Removed,
                    name: SharedStr::from(""),
                    sig: Vec::new(),
                    deprecated: false,
                    is_current: slice.is_current,
                });
            }
            prev = None;
            continue;
        };

        let sig = signature::tokens(entry, &slice.package);

        let change = match &prev {
            Some(previous) => classify(previous, entry, &sig),
            // First appearance. If this is the oldest generation the corpus
            // holds, we cannot know whether the symbol predates it — see
            // `TimelineChange` for why that distinction is load-bearing.
            None if idx == 0 => TimelineChange::Present,
            // A strictly older generation is loaded and lacks this `IntroId`.
            // That is observed evidence of an introduction, not an inference.
            None => TimelineChange::Introduced,
        };

        rows.push(TimelineRow {
            version: slice.version.clone(),
            change,
            name: SharedStr::from(entry.sym().name.as_str()),
            sig: sig.clone(),
            deprecated: entry.sym().deprecation.is_some(),
            is_current: slice.is_current,
        });

        prev = Some(Seen { entry, sig });
    }

    rows.reverse();

    Timeline {
        key: key.clone(),
        rows: rows.into(),
        versions_examined: slices.len() as u32,
    }
}

/// Classify one generation against the one before it.
///
/// The order of these tests *is* the priority order documented on
/// [`TimelineRow::change`]: the first axis that differs wins, and it is ordered
/// rarest-and-most-consequential first. A rename is checked before a signature
/// change because a rename is almost always accompanied by one (the name is a
/// token in the signature), and "renamed" is the fact that explains the other.
///
/// `pub(crate)` so that [`crate::diff`] classifies a package-level diff with
/// **this** function rather than one written for it. Two classifiers over the
/// same five axes would eventually disagree, and then `get_symbol`'s timeline
/// and `diff_versions` would describe the same declaration differently — which
/// a caller would read as a bug in whichever one they looked at second.
pub(crate) fn classify(prev: &Seen<'_>, cur: &Entry, cur_sig: &[SigToken]) -> TimelineChange {
    let before = prev.entry.sym();
    let after = cur.sym();

    if before.name != after.name {
        return TimelineChange::Renamed {
            from: SharedStr::from(before.name.as_str()),
        };
    }

    let was_deprecated = before.deprecation.is_some();
    let is_deprecated = after.deprecation.is_some();
    if !was_deprecated && is_deprecated {
        return TimelineChange::Deprecated;
    }
    if was_deprecated && !is_deprecated {
        return TimelineChange::Undeprecated;
    }

    if prev.sig.as_slice() != cur_sig {
        return TimelineChange::SignatureChanged;
    }

    // Reached only when the rendered signature is identical, so this catches
    // visibility changes that the renderer does not surface as a token.
    if before.visibility != after.visibility {
        return TimelineChange::VisibilityChanged;
    }

    if before.documentation != after.documentation {
        return TimelineChange::DocsChanged;
    }

    // Nothing observable moved. Note what is *not* consulted here:
    // `Symbol::source` and `Symbol::span`. A declaration that only changed
    // file or line is unchanged as far as a reader is concerned, and saying
    // otherwise is the failure mode described in the module docs.
    TimelineChange::Unchanged
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nudox_ir::{change::StableRef, entry::Visibility};
    use nudox_store::package::PackageView;

    use super::*;
    use crate::{
        test_support::{entry as spec, intro, lineage, package},
        versions::VersionSlice,
    };

    /// Build oldest-first slices from `(version, package)` pairs, marking the
    /// last one current (which is what the registry does for an unpinned
    /// lineage).
    fn slices(pairs: Vec<(&str, Arc<PackageView>)>) -> Vec<VersionSlice> {
        let last = pairs.len().saturating_sub(1);
        pairs
            .into_iter()
            .enumerate()
            .map(|(i, (version, package))| VersionSlice {
                version: SharedStr::from(version),
                package,
                is_current: i == last,
            })
            .collect()
    }

    fn key(pkg: &str, n: u8) -> SymbolKey {
        StableRef::new(lineage(pkg), intro(n))
    }

    fn changes(t: &Timeline) -> Vec<TimelineChange> {
        t.rows.iter().map(|r| r.change.clone()).collect()
    }

    fn versions(t: &Timeline) -> Vec<String> {
        t.rows.iter().map(|r| r.version.to_string()).collect()
    }

    // ── The one-version case: the first thing a user sees ────────────────────

    #[test]
    fn a_single_loaded_version_yields_exactly_one_present_row() {
        let lid = lineage("axum");
        let slices = slices(vec![("0.8.9", package(&lid, vec![spec(1, "Router")]))]);

        let t = build(&key("axum", 1), &slices);

        assert_eq!(t.rows.len(), 1, "one generation must produce one row");
        assert_eq!(
            t.rows[0].change,
            TimelineChange::Present,
            "with one generation loaded there is no evidence of introduction"
        );
        assert_eq!(&*t.rows[0].version, "0.8.9");
        assert!(t.rows[0].is_current);
        assert_eq!(t.versions_examined, 1);
    }

    #[test]
    fn the_oldest_loaded_version_is_present_not_introduced() {
        let lid = lineage("axum");
        let slices = slices(vec![
            ("0.7.9", package(&lid, vec![spec(1, "Router")])),
            ("0.8.1", package(&lid, vec![spec(1, "Router")])),
        ]);

        let t = build(&key("axum", 1), &slices);

        // Newest first: [0.8.1, 0.7.9].
        assert_eq!(versions(&t), vec!["0.8.1", "0.7.9"]);
        assert_eq!(
            changes(&t),
            vec![TimelineChange::Unchanged, TimelineChange::Present]
        );
    }

    // ── Introduction and removal ─────────────────────────────────────────────

    #[test]
    fn a_symbol_absent_from_an_older_version_is_introduced() {
        let lid = lineage("axum");
        let slices = slices(vec![
            ("0.7.9", package(&lid, vec![spec(1, "Router")])),
            (
                "0.8.1",
                package(&lid, vec![spec(1, "Router"), spec(2, "Extractor")]),
            ),
        ]);

        let t = build(&key("axum", 2), &slices);

        assert_eq!(
            t.rows.len(),
            1,
            "no row for a version that lacks the symbol"
        );
        assert_eq!(t.rows[0].change, TimelineChange::Introduced);
        assert_eq!(&*t.rows[0].version, "0.8.1");
        assert_eq!(
            t.versions_examined, 2,
            "the denominator counts every generation walked, not every row"
        );
    }

    #[test]
    fn a_symbol_that_disappears_gets_a_removed_row() {
        let lid = lineage("axum");
        let slices = slices(vec![
            (
                "0.7.9",
                package(&lid, vec![spec(1, "Router"), spec(2, "Handler")]),
            ),
            ("0.8.1", package(&lid, vec![spec(1, "Router")])),
        ]);

        let t = build(&key("axum", 2), &slices);

        assert_eq!(
            changes(&t),
            vec![TimelineChange::Removed, TimelineChange::Present]
        );
        assert_eq!(&*t.rows[0].version, "0.8.1");
        assert!(
            t.rows[0].sig.is_empty(),
            "a removed symbol has no signature"
        );
        assert_eq!(&*t.rows[0].name, "");
    }

    #[test]
    fn removal_is_reported_once_not_once_per_later_version() {
        let lid = lineage("axum");
        let slices = slices(vec![
            ("0.1.0", package(&lid, vec![spec(1, "Gone")])),
            ("0.2.0", package(&lid, vec![])),
            ("0.3.0", package(&lid, vec![])),
        ]);

        let t = build(&key("axum", 1), &slices);

        assert_eq!(
            changes(&t),
            vec![TimelineChange::Removed, TimelineChange::Present]
        );
        assert_eq!(versions(&t), vec!["0.2.0", "0.1.0"]);
    }

    #[test]
    fn a_symbol_that_comes_back_is_introduced_again() {
        let lid = lineage("axum");
        let slices = slices(vec![
            ("0.1.0", package(&lid, vec![spec(1, "Flaky")])),
            ("0.2.0", package(&lid, vec![])),
            ("0.3.0", package(&lid, vec![spec(1, "Flaky")])),
        ]);

        let t = build(&key("axum", 1), &slices);

        assert_eq!(
            changes(&t),
            vec![
                TimelineChange::Introduced,
                TimelineChange::Removed,
                TimelineChange::Present
            ]
        );
    }

    #[test]
    fn a_symbol_absent_from_every_loaded_version_has_no_rows() {
        let lid = lineage("axum");
        let slices = slices(vec![("0.1.0", package(&lid, vec![spec(1, "Only")]))]);

        let t = build(&key("axum", 99), &slices);

        assert!(t.rows.is_empty());
        assert_eq!(t.versions_examined, 1);
    }

    // ── Classification axes ──────────────────────────────────────────────────

    #[test]
    fn a_rename_is_detected_because_intro_id_survives_it() {
        let lid = lineage("axum");
        let slices = slices(vec![
            ("0.1.0", package(&lid, vec![spec(1, "Router")])),
            ("0.2.0", package(&lid, vec![spec(1, "Router").named("App")])),
        ]);

        let t = build(&key("axum", 1), &slices);

        assert_eq!(
            t.rows[0].change,
            TimelineChange::Renamed {
                from: SharedStr::from("Router")
            }
        );
        assert_eq!(&*t.rows[0].name, "App");
    }

    #[test]
    fn a_rename_outranks_the_signature_change_it_causes() {
        // Renaming necessarily changes the signature tokens (the name is one
        // of them). The row must say `Renamed`, which explains the change,
        // not `SignatureChanged`, which does not.
        let lid = lineage("axum");
        let slices = slices(vec![
            ("0.1.0", package(&lid, vec![spec(1, "Router")])),
            (
                "0.2.0",
                package(&lid, vec![spec(1, "Router").named("App").reexport()]),
            ),
        ]);

        let t = build(&key("axum", 1), &slices);
        assert!(matches!(t.rows[0].change, TimelineChange::Renamed { .. }));
    }

    #[test]
    fn a_signature_change_with_a_stable_name_is_reported_as_such() {
        let lid = lineage("axum");
        let slices = slices(vec![
            // `mod Router`
            ("0.1.0", package(&lid, vec![spec(1, "Router")])),
            // `pub use Router`
            ("0.2.0", package(&lid, vec![spec(1, "Router").reexport()])),
        ]);

        let t = build(&key("axum", 1), &slices);
        assert_eq!(t.rows[0].change, TimelineChange::SignatureChanged);
        assert!(!t.rows[0].sig.is_empty());
    }

    #[test]
    fn a_doc_edit_alone_is_reported_as_docs_changed() {
        let lid = lineage("axum");
        let slices = slices(vec![
            ("0.1.0", package(&lid, vec![spec(1, "Router").docs("old")])),
            ("0.2.0", package(&lid, vec![spec(1, "Router").docs("new")])),
        ]);

        let t = build(&key("axum", 1), &slices);
        assert_eq!(t.rows[0].change, TimelineChange::DocsChanged);
    }

    #[test]
    fn a_visibility_change_alone_is_reported_as_such() {
        let lid = lineage("axum");
        let slices = slices(vec![
            ("0.1.0", package(&lid, vec![spec(1, "Router")])),
            (
                "0.2.0",
                package(&lid, vec![spec(1, "Router").visibility(Visibility::Crate)]),
            ),
        ]);

        let t = build(&key("axum", 1), &slices);
        assert_eq!(t.rows[0].change, TimelineChange::VisibilityChanged);
    }

    #[test]
    fn deprecation_is_an_event_and_deprecated_is_a_state() {
        let lid = lineage("axum");
        let slices = slices(vec![
            ("0.1.0", package(&lid, vec![spec(1, "Router")])),
            (
                "0.2.0",
                package(&lid, vec![spec(1, "Router").deprecated("use App")]),
            ),
            (
                "0.3.0",
                package(&lid, vec![spec(1, "Router").deprecated("use App")]),
            ),
        ]);

        let t = build(&key("axum", 1), &slices);

        // Newest first: 0.3.0, 0.2.0, 0.1.0.
        assert_eq!(
            changes(&t),
            vec![
                TimelineChange::Unchanged,
                TimelineChange::Deprecated,
                TimelineChange::Present
            ]
        );
        // The *state* persists after the event.
        assert!(t.rows[0].deprecated, "0.3.0 is still deprecated");
        assert!(t.rows[1].deprecated, "0.2.0 is where it landed");
        assert!(!t.rows[2].deprecated, "0.1.0 was not deprecated");
    }

    #[test]
    fn removing_a_deprecation_marker_is_reported() {
        let lid = lineage("axum");
        let slices = slices(vec![
            (
                "0.1.0",
                package(&lid, vec![spec(1, "Router").deprecated("soon")]),
            ),
            ("0.2.0", package(&lid, vec![spec(1, "Router")])),
        ]);

        let t = build(&key("axum", 1), &slices);
        assert_eq!(t.rows[0].change, TimelineChange::Undeprecated);
        assert!(!t.rows[0].deprecated);
    }

    // ── The anti-regression test this module exists for ──────────────────────

    #[test]
    fn a_declaration_that_only_moved_is_unchanged() {
        // Two versions of a crate unpack at different roots and the
        // declaration drifts down the file. `entry_content_hash` would report
        // a difference for both reasons; a reader would not. See the module
        // docs for why this is the failure mode that shaped the classifier.
        let lid = lineage("axum");
        let slices = slices(vec![
            (
                "0.7.9",
                package(
                    &lid,
                    vec![spec(1, "Router").moved_to("/tmp/axum-0.7.9/src/lib.rs", 100..200)],
                ),
            ),
            (
                "0.8.1",
                package(
                    &lid,
                    vec![spec(1, "Router").moved_to("/tmp/axum-0.8.1/src/routing.rs", 4000..4100)],
                ),
            ),
        ]);

        let t = build(&key("axum", 1), &slices);
        assert_eq!(
            t.rows[0].change,
            TimelineChange::Unchanged,
            "moving a declaration is not an API change"
        );
    }

    // ── Shape guarantees ─────────────────────────────────────────────────────

    #[test]
    fn rows_are_newest_first_and_mark_the_current_generation() {
        let lid = lineage("axum");
        let slices = slices(vec![
            ("0.1.0", package(&lid, vec![spec(1, "Router")])),
            ("0.2.0", package(&lid, vec![spec(1, "Router")])),
            ("0.3.0", package(&lid, vec![spec(1, "Router")])),
        ]);

        let t = build(&key("axum", 1), &slices);

        assert_eq!(versions(&t), vec!["0.3.0", "0.2.0", "0.1.0"]);
        assert!(t.rows[0].is_current);
        assert!(!t.rows[1].is_current);
        assert!(!t.rows[2].is_current);
    }

    #[test]
    fn signatures_match_what_the_symbol_page_would_render() {
        // Not a tautology: it pins the fact that the timeline goes through
        // `chunk::signature::tokens` (LR-4) rather than formatting its own.
        let lid = lineage("axum");
        let pkg = package(&lid, vec![spec(1, "Router")]);
        let expected = {
            let e = pkg.view().entry(intro(1)).unwrap();
            signature::tokens(e, &pkg)
        };

        let t = build(&key("axum", 1), &slices(vec![("0.1.0", pkg)]));
        assert_eq!(t.rows[0].sig, expected);
    }

    #[test]
    fn no_slices_yields_no_rows_rather_than_an_invented_one() {
        let t = build(&key("axum", 1), &[]);
        assert!(t.rows.is_empty());
        assert_eq!(t.versions_examined, 0);
    }
}
