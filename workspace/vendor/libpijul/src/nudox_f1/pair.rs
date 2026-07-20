//! Field-aware line pairing for NdIrF1 blobs (§4, DEPTH1-STRUCTURED-RECORD-PLAN).
//!
//! # Algorithm overview
//!
//! Four stages produce a [`Vec<LineReplace>`] in Replace-normal form:
//!
//! ```text
//! A. Runs         — group consecutive lines by key_of(line)
//! B. Run-align    — diffs::myers over key tokens → RunOp stream
//! C. Section pair — per-class: scalar positional, set sorted-merge,
//!                   seq section-scoped diffs::myers (MicroSink)
//! D. Normalize    — fold adjacent micros that share a gap → Replace-normal form
//! ```
//!
//! # The one-vertex-per-gap law (§4.4)
//!
//! libpijul's stage-(3) lowering resolves an insertion's up-context to the old
//! line **above** its anchor, even when another entry deletes that line (§1.4
//! fact 2).  Two entries with `new_len > 0` anchored in the same gap between
//! surviving old lines would therefore emit two `NewVertex` atoms with no order
//! edge between them — an order-ambiguous graph.  Stock Myers never produces
//! that shape because `diffs::Replace` only flushes on `equal` (§1.4 fact 3).
//!
//! The pairer honours this law in stage D: when two consecutive micros touch
//! immediately adjacent positions on **both** sides (`next.old == prev.old +
//! prev.old_len && next.new == prev.new + prev.new_len`) they are merged into
//! one entry, collapsing the zero-width gap that would otherwise host two
//! `NewVertex` atoms.

use super::registry::{class_of, key_of, Class};
use std::cmp::Ordering;

// ---------------------------------------------------------------------------
// Public interface
// ---------------------------------------------------------------------------

/// One span replacement in line coordinates: `old_len` lines of the old blob
/// starting at `old` are replaced by `new_len` lines of the new blob starting
/// at `new`.
///
/// * `old_len == 0` — pure insertion anchored before old line `old`.
/// * `new_len == 0` — pure deletion.
///
/// Field-for-field identical to libpijul's `diff::Replacement` — by design,
/// not coincidence.  The fork bridges between the two types with plain struct
/// literals (all fields `pub`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineReplace {
    pub old:     usize,
    pub old_len: usize,
    pub new:     usize,
    pub new_len: usize,
}

/// Field-aware pairing of two line arrays.
///
/// **Total, pure, deterministic:** defined for every input (F1 or not, canonical
/// or corrupt), never errors, and depends only on the argument bytes.  Output is
/// in Replace-normal form (§4.5).  Lines carry their trailing separator, exactly
/// as pijul's `make_new_lines` / `make_old_lines` produce them.
pub fn line_diff(a: &[&[u8]], b: &[&[u8]]) -> Vec<LineReplace> {
    normalize(micros(a, b))
}

/// Stages A–C: the pre-merge micro-op stream, ascending in both coordinates.
///
/// `pub(crate)` so the test suite can assert the strong form of L-5 (every
/// mixed micro pairs lines of one single key on both sides) before stage D's
/// §4.4 merges legitimately blur section boundaries.
pub(crate) fn micros(a: &[&[u8]], b: &[&[u8]]) -> Vec<LineReplace> {
    // Stage A.
    let runs_a = runs(a);
    let runs_b = runs(b);

    // Stage B: align runs by key.
    let keys_a: Vec<&[u8]> = runs_a.iter().map(|r| r.key).collect();
    let keys_b: Vec<&[u8]> = runs_b.iter().map(|r| r.key).collect();

    let mut align = RunAlign { ops: Vec::new() };
    diffs::myers::diff(
        &mut align,
        &keys_a[..],
        0,
        keys_a.len(),
        &keys_b[..],
        0,
        keys_b.len(),
    )
    .unwrap(); // Infallible

    // Helpers: absolute line start given a run index (or total length past end).
    let line_start_a = |ri: usize| -> usize {
        runs_a.get(ri).map(|r| r.start).unwrap_or(a.len())
    };
    let line_start_b = |ri: usize| -> usize {
        runs_b.get(ri).map(|r| r.start).unwrap_or(b.len())
    };

    // Stage C: expand each RunOp into per-class micros.
    let mut out: Vec<LineReplace> = Vec::new();

    for op in &align.ops {
        match *op {
            RunOp::Equal { ra, rb, len } => {
                for k in 0..len {
                    let sa = &runs_a[ra + k];
                    let sb = &runs_b[rb + k];
                    pair_section(a, b, sa, sb, &mut out);
                }
            }

            RunOp::Delete { ra, len, rb } => {
                // rb = current new-side cursor at the point of deletion.
                let new_anchor = line_start_b(rb);
                for k in 0..len {
                    let r = &runs_a[ra + k];
                    out.push(LineReplace {
                        old:     r.start,
                        old_len: r.len,
                        new:     new_anchor,
                        new_len: 0,
                    });
                }
            }

            RunOp::Insert { ra, rb, len } => {
                // ra = current old-side cursor at the point of insertion.
                let old_anchor = line_start_a(ra);
                for k in 0..len {
                    let r = &runs_b[rb + k];
                    out.push(LineReplace {
                        old:     old_anchor,
                        old_len: 0,
                        new:     r.start,
                        new_len: r.len,
                    });
                }
            }

            RunOp::Replace { ra, ra_len, rb, rb_len } => {
                // Keys differ — section was swapped for a different section.
                // Emit delete-micros for the old runs anchored at line_start_b(rb),
                // then insert-micros for the new runs anchored at the line *after*
                // the deleted old region.  Stage D fuses the whole cluster into one
                // entry (§4.2 Replace-cluster anchor convention).
                let new_anchor = line_start_b(rb);
                // The old-side anchor for inserts is the line immediately after the
                // last deleted old line — that is line_start_a(ra + ra_len).
                let old_after  = line_start_a(ra + ra_len);

                for k in 0..ra_len {
                    let r = &runs_a[ra + k];
                    out.push(LineReplace {
                        old:     r.start,
                        old_len: r.len,
                        new:     new_anchor,
                        new_len: 0,
                    });
                }
                for k in 0..rb_len {
                    let r = &runs_b[rb + k];
                    out.push(LineReplace {
                        old:     old_after,
                        old_len: 0,
                        new:     r.start,
                        new_len: r.len,
                    });
                }
            }
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Stage A — runs
// ---------------------------------------------------------------------------

struct Run<'x> {
    key:   &'x [u8],
    start: usize,
    len:   usize,
}

fn runs<'x>(lines: &[&'x [u8]]) -> Vec<Run<'x>> {
    let mut out: Vec<Run<'x>> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let key = key_of(line);
        match out.last_mut() {
            Some(r) if r.key == key => r.len += 1,
            _ => out.push(Run { key, start: i, len: 1 }),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Stage B — run alignment sink
// ---------------------------------------------------------------------------

enum RunOp {
    Equal   { ra: usize, rb: usize, len: usize },
    Delete  { ra: usize, len: usize, rb: usize },
    Insert  { ra: usize, rb: usize, len: usize },
    Replace { ra: usize, ra_len: usize, rb: usize, rb_len: usize },
}

struct RunAlign {
    ops: Vec<RunOp>,
}

impl diffs::Diff for RunAlign {
    type Error = std::convert::Infallible;

    fn equal(&mut self, ra: usize, rb: usize, len: usize) -> Result<(), Self::Error> {
        self.ops.push(RunOp::Equal { ra, rb, len });
        Ok(())
    }
    fn delete(&mut self, ra: usize, len: usize, rb: usize) -> Result<(), Self::Error> {
        self.ops.push(RunOp::Delete { ra, len, rb });
        Ok(())
    }
    fn insert(&mut self, ra: usize, rb: usize, len: usize) -> Result<(), Self::Error> {
        self.ops.push(RunOp::Insert { ra, rb, len });
        Ok(())
    }
    fn replace(
        &mut self,
        ra: usize,
        ra_len: usize,
        rb: usize,
        rb_len: usize,
    ) -> Result<(), Self::Error> {
        self.ops.push(RunOp::Replace { ra, ra_len, rb, rb_len });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Stage C — per-class section pairing
// ---------------------------------------------------------------------------

fn pair_section<'x>(
    a:      &[&'x [u8]],
    b:      &[&'x [u8]],
    sa:     &Run<'x>,
    sb:     &Run<'x>,
    out:    &mut Vec<LineReplace>,
) {
    // Structural guarantee of the `Equal` RunOp: paired runs share their key.
    debug_assert_eq!(sa.key, sb.key);

    match class_of(sa.key) {
        Class::Scalar => pair_scalar(a, b, sa, sb, out),
        Class::Set    => pair_set(a, b, sa, sb, out),
        Class::Seq    => pair_seq(a, b, sa, sb, out),
    }
}

/// Scalar — positional pairing.
///
/// Canonically both sides have exactly 1 line.  Non-canonical tails (extra or
/// missing lines) are handled gracefully: old extras become pure deletions
/// anchored at the end of the new section; new extras become pure insertions
/// anchored at the end of the old section.
fn pair_scalar(
    a:   &[&[u8]],
    b:   &[&[u8]],
    sa:  &Run<'_>,
    sb:  &Run<'_>,
    out: &mut Vec<LineReplace>,
) {
    let n = sa.len.min(sb.len);
    for k in 0..n {
        if a[sa.start + k] != b[sb.start + k] {
            out.push(LineReplace {
                old:     sa.start + k,
                old_len: 1,
                new:     sb.start + k,
                new_len: 1,
            });
        }
    }
    // Old tail (non-canonical): pure deletions anchored at end of new section.
    for k in n..sa.len {
        out.push(LineReplace {
            old:     sa.start + k,
            old_len: 1,
            new:     sb.start + sb.len,
            new_len: 0,
        });
    }
    // New tail (non-canonical): pure insertions anchored at end of old section.
    for k in n..sb.len {
        out.push(LineReplace {
            old:     sa.start + sa.len,
            old_len: 0,
            new:     sb.start + k,
            new_len: 1,
        });
    }
}

/// Set — sorted merge-join on full line bytes.
///
/// Canonical sets are emitted byte-sorted (§1.6), so equal members meet and
/// unequal members are independent insert/delete micros — **two differing
/// members are never paired as a rewrite**.  On unsorted (corrupt) input the
/// walk still terminates deterministically, pairing less.
fn pair_set(
    a:   &[&[u8]],
    b:   &[&[u8]],
    sa:  &Run<'_>,
    sb:  &Run<'_>,
    out: &mut Vec<LineReplace>,
) {
    let (mut i, mut j) = (0usize, 0usize);
    while i < sa.len && j < sb.len {
        let la = a[sa.start + i];
        let lb = b[sb.start + j];
        match la.cmp(lb) {
            Ordering::Equal => {
                i += 1;
                j += 1;
            }
            Ordering::Less => {
                // Member only in old — delete it; new anchor = sb cursor.
                out.push(LineReplace {
                    old:     sa.start + i,
                    old_len: 1,
                    new:     sb.start + j,
                    new_len: 0,
                });
                i += 1;
            }
            Ordering::Greater => {
                // Member only in new — insert it; old anchor = sa cursor.
                out.push(LineReplace {
                    old:     sa.start + i,
                    old_len: 0,
                    new:     sb.start + j,
                    new_len: 1,
                });
                j += 1;
            }
        }
    }
    // Old tail: deletions anchored at end of new section.
    while i < sa.len {
        out.push(LineReplace {
            old:     sa.start + i,
            old_len: 1,
            new:     sb.start + sb.len,
            new_len: 0,
        });
        i += 1;
    }
    // New tail: insertions anchored at end of old section.
    while j < sb.len {
        out.push(LineReplace {
            old:     sa.start + sa.len,
            old_len: 0,
            new:     sb.start + j,
            new_len: 1,
        });
        j += 1;
    }
}

/// Seq (and every unknown key, including the magic line) — section-scoped Myers.
///
/// Calls `diffs::myers::diff` over the section's absolute ranges; callbacks
/// arrive in absolute line coordinates.
fn pair_seq(
    a:   &[&[u8]],
    b:   &[&[u8]],
    sa:  &Run<'_>,
    sb:  &Run<'_>,
    out: &mut Vec<LineReplace>,
) {
    struct MicroSink<'o> {
        out: &'o mut Vec<LineReplace>,
    }

    impl<'o> diffs::Diff for MicroSink<'o> {
        type Error = std::convert::Infallible;

        fn delete(&mut self, old: usize, len: usize, new: usize) -> Result<(), Self::Error> {
            self.out.push(LineReplace { old, old_len: len, new, new_len: 0 });
            Ok(())
        }
        fn insert(&mut self, old: usize, new: usize, new_len: usize) -> Result<(), Self::Error> {
            self.out.push(LineReplace { old, old_len: 0, new, new_len });
            Ok(())
        }
        fn replace(
            &mut self,
            old: usize,
            old_len: usize,
            new: usize,
            new_len: usize,
        ) -> Result<(), Self::Error> {
            self.out.push(LineReplace { old, old_len, new, new_len });
            Ok(())
        }
    }

    diffs::myers::diff(
        &mut MicroSink { out },
        a,
        sa.start,
        sa.start + sa.len,
        b,
        sb.start,
        sb.start + sb.len,
    )
    .unwrap(); // Infallible
}

// ---------------------------------------------------------------------------
// Stage D — Replace-normal form (§4.5)
// ---------------------------------------------------------------------------

/// Fold micro-ops into Replace-normal form.
///
/// **Replace-normal form (output contract):** entries sorted strictly ascending
/// in both `old` and `new`; non-overlapping; `old_len + new_len ≥ 1`; and
/// between any two consecutive entries at least one line survives on each side.
///
/// Stages A–C emit micros with monotone, contiguous-cursor anchors.  The sole
/// transform needed is merging adjacent pairs that share **no** surviving line
/// on either side — the one-vertex-per-gap law (§4.4).
fn normalize(micros: Vec<LineReplace>) -> Vec<LineReplace> {
    let mut out: Vec<LineReplace> = Vec::new();
    for m in micros {
        if m.old_len == 0 && m.new_len == 0 {
            continue;
        }
        if let Some(t) = out.last_mut() {
            // Structural invariant of stages A–C: micros arrive in non-decreasing
            // order on both coordinates (anchors are monotone cursors regardless of
            // input content).
            debug_assert!(
                m.old >= t.old + t.old_len,
                "old anchor regressed: m.old={} t.old+t.old_len={}",
                m.old, t.old + t.old_len
            );
            debug_assert!(
                m.new >= t.new + t.new_len,
                "new anchor regressed: m.new={} t.new+t.new_len={}",
                m.new, t.new + t.new_len
            );
            if m.old == t.old + t.old_len && m.new == t.new + t.new_len {
                // No surviving line between the spans on either side:
                // graph-mandated merge (§4.4 one-vertex-per-gap law).
                t.old_len += m.old_len;
                t.new_len += m.new_len;
                continue;
            }
        }
        out.push(m);
    }
    out
}
