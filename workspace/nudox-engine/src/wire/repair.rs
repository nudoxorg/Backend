//! Link repair — the typed, closed, counted record of every intra-doc link
//! whose spelling we corrected on the author's behalf.
//!
//! # Why this module exists
//!
//! The chunker renders a working hyperlink for one malformed rustdoc
//! intra-doc-link spelling that rustdoc itself renders as literal text (see
//! [`LinkRepairKind::TransposedOpenDelimiter`]).  That is a deliberate
//! improvement over docs.rs — but until this module existed it was an
//! *invisible* one: the repaired link and an authored link produced a
//! byte-identical [`crate::wire::InlineRun::Link`], so nothing in the wire,
//! the IR, or the GUI could answer "what else do we silently repair?".
//!
//! A silent repair is the same defect class as a silent failure (doctrine §8):
//! both present a degraded case as the good one.  This module makes the
//! degraded case unrepresentable-as-the-good-one by putting a mandatory
//! [`LinkOrigin`] on every link that crosses the protocol.

use schemars::JsonSchema;
use serde::Serialize;

use super::SharedStr;

/// The complete set of malformed intra-doc-link spellings we are willing to
/// turn into a working link.
///
/// # Why this enum is deliberately NOT `#[non_exhaustive]`
///
/// Doctrine §3 puts `#[non_exhaustive]` on wire enums to protect external
/// callers. This one takes the `ProducerError` exception for the same reason
/// that one does, and more sharply: the entire value of the enum is that
/// adding a variant **breaks every match in the workspace**, including
/// `lindsey`'s reader-facing legend and the corpus audit's column set. A
/// wildcard arm anywhere would let a future repair be born with no reader
/// affordance and no audit row — which is precisely the failure this type
/// exists to make impossible. "What else do we silently repair?" must have a
/// finite, readable answer, and this enum is that answer.
///
/// Every variant must name (a) the exact source spelling, (b) what rustdoc and
/// docs.rs do with it, and (c) why we differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkRepairKind {
    /// The opening backtick and bracket are transposed: the source reads
    /// `` `[foo`] `` where rustdoc's intra-doc syntax is `` [`foo`] ``.
    ///
    /// CommonMark gives code spans higher precedence than link brackets, so
    /// the stray `[` is swallowed into the code span and rustdoc — and
    /// therefore docs.rs — renders the whole thing as literal text. The
    /// producer's byte-level scanner (`extract_doc_link_targets` in
    /// `workspace/compiler/languages/rust/src/ra/docs.rs`) is lenient and
    /// resolves the target anyway, so we have a genuinely-resolved target in
    /// hand and choose to use it.
    ///
    /// Real instance: `memchr-2.8.3/src/memchr.rs:282` (and 358, 426) —
    /// `` This iterator is created by the [`memchr_iter`] or `[memrchr_iter`] ``.
    TransposedOpenDelimiter,
}

impl LinkRepairKind {
    /// Every variant, in a fixed order. The single definition — [`Self::index`]
    /// searches this rather than duplicating a match, so the two cannot drift.
    pub const ALL: &'static [Self] = &[Self::TransposedOpenDelimiter];

    /// Dense index for tally storage.
    pub(crate) fn index(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).expect(
            "LinkRepairKind::ALL must list every variant — pinned by \
                 `all_lists_every_link_repair_kind`",
        )
    }

    /// The reader-facing name of this repair, used by the GUI legend and by
    /// the audit's TSV. No wildcard arm: a new variant does not compile here.
    pub fn label(self) -> &'static str {
        match self {
            Self::TransposedOpenDelimiter => "transposed backtick and bracket",
        }
    }

    /// The stable machine-readable token for this repair, used as the kind
    /// column of the corpus audit's TSV so a report can be aggregated with
    /// `awk` across packages. No wildcard arm, same reason as [`Self::label`].
    pub fn token(self) -> &'static str {
        match self {
            Self::TransposedOpenDelimiter => "transposed_open_delimiter",
        }
    }
}

/// Evidence that a rendered link was repaired rather than authored.
///
/// Carried in-band on the link run itself. There is deliberately **no**
/// parallel counter anywhere: the runs are the single source of truth, so a
/// count and the thing it counts cannot drift (doctrine §8 — a degraded case
/// must not be representable as the good one).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct LinkRepair {
    /// Which of the closed set of repairs this was.
    pub kind: LinkRepairKind,
    /// The source span **exactly as the author wrote it**, e.g.
    /// `` `[memrchr_iter`] ``. This is what the reader is shown when they ask
    /// what we changed, so it must be the true bytes, not a reconstruction.
    pub raw: SharedStr,
    /// The link target we resolved it to, e.g. `memrchr_iter`.
    pub resolved: SharedStr,
    /// The complete reader-facing sentence explaining the repair, rendered
    /// once here (LR-3: every byte lindsey shows originates in the chunker).
    /// The GUI displays it verbatim and does no string work.
    pub note: SharedStr,
}

/// Whether a link's spelling was rustdoc's, or ours.
///
/// Not `#[non_exhaustive]`: it has exactly two states by construction and will
/// never grow. `Authored` vs `Repaired` is the whole question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum LinkOrigin {
    /// The source spelled this link the way rustdoc specifies. What we render
    /// is what rustdoc and docs.rs render.
    Authored,
    /// The source spelled it wrong; we resolved the link the author obviously
    /// intended. Carries the evidence.
    Repaired(LinkRepair),
}

impl LinkOrigin {
    /// The repair kind, when this link was repaired.
    ///
    /// Exists so a consumer that only needs the *kind* (the audit tally, a
    /// filter) does not have to destructure the whole [`LinkRepair`] and
    /// thereby depend on its field set.
    pub fn repair_kind(&self) -> Option<LinkRepairKind> {
        match self {
            Self::Authored => None,
            Self::Repaired(r) => Some(r.kind),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_lists_every_link_repair_kind() {
        // No wildcard: a new variant does not compile until it is listed here…
        for kind in LinkRepairKind::ALL {
            match kind {
                LinkRepairKind::TransposedOpenDelimiter => {}
            }
        }
        // …and this pins the count, so a variant added to the match but not to
        // ALL is caught too. Bumping this number is a deliberate act, which is
        // exactly the checkpoint this test exists to force.
        assert_eq!(LinkRepairKind::ALL.len(), 1);
    }

    /// `index()` must be a bijection onto `0..ALL.len()`, because
    /// `RepairTally` stores counts in a dense array addressed by it — a
    /// collision would merge two kinds' counts into one cell silently.
    #[test]
    fn index_is_dense_and_unique() {
        for (i, kind) in LinkRepairKind::ALL.iter().enumerate() {
            assert_eq!(kind.index(), i, "index must match position in ALL");
        }
    }

    /// Every kind must have a distinct, non-empty machine token — the audit
    /// TSV's kind column is only aggregatable if the tokens are unique.
    #[test]
    fn tokens_are_unique_and_non_empty() {
        let mut seen: Vec<&str> = Vec::new();
        for kind in LinkRepairKind::ALL {
            let t = kind.token();
            assert!(!t.is_empty(), "{kind:?} has an empty token");
            assert!(!t.contains('\t'), "{kind:?}'s token would break the TSV");
            assert!(!seen.contains(&t), "duplicate token {t:?}");
            seen.push(t);
        }
    }
}
