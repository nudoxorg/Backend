//! T-0 acceptance suite for `nudox-f1` (DEPTH1-STRUCTURED-RECORD-PLAN §9).
//!
//! Tests are grouped by concern:
//!   1. Registry shape checks (counts, frozen order, helpers).
//!   2. `line_diff` property/golden/shape tests (L-1 … L-5).
//!   3. Fuzz with deterministic PRNG (xorshift64) asserting L-1…L-5.
//!   4. Layering / lockfile asserts (T-11 a, b, c mechanical parts).

use crate::{
    line_diff,
    pair::micros,
    registry::{class_of, key_of, Class, MAGIC_LINE, MAGIC_STR, REGISTRY},
    LineReplace,
};

// ===========================================================================
// Helpers shared across tests
// ===========================================================================

/// Apply `line_diff` output to `a`, reconstructing `b`.
///
/// Replay entries ascending: copy un-touched old lines, then substitute each
/// replacement with the corresponding new lines.  ~15 lines, pure, no allocs
/// beyond the output.
fn apply<'a>(a: &[&'a [u8]], entries: &[LineReplace], b: &[&'a [u8]]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut old_cursor = 0usize;
    for e in entries {
        // Copy unchanged old lines before this entry.
        out.extend(a[old_cursor..e.old].iter().map(|l| l.to_vec()));
        // Substitute: skip old_len lines from a, emit new_len lines from b.
        old_cursor = e.old + e.old_len;
        for k in 0..e.new_len {
            out.push(b[e.new + k].to_vec());
        }
    }
    // Copy remaining old lines.
    out.extend(a[old_cursor..].iter().map(|l| l.to_vec()));
    out
}

/// Check Replace-normal form (L-4), §4.5's frozen output contract:
/// entries sorted strictly ascending in both coordinates, non-overlapping,
/// `old_len + new_len ≥ 1`, and **at least one line surviving on each side**
/// between consecutive entries (strict gap on BOTH sides — one-side adjacency
/// is exactly the §4.4 up-context hazard and must never appear).
fn check_normal_form(entries: &[LineReplace]) -> Result<(), String> {
    for (i, e) in entries.iter().enumerate() {
        if e.old_len == 0 && e.new_len == 0 {
            return Err(format!("entry {i}: both old_len and new_len are 0"));
        }
        if i > 0 {
            let p = &entries[i - 1];
            if e.old <= p.old + p.old_len {
                return Err(format!(
                    "entry {i}: old {} must be > previous end {}+{}",
                    e.old, p.old, p.old_len
                ));
            }
            if e.new <= p.new + p.new_len {
                return Err(format!(
                    "entry {i}: new {} must be > previous end {}+{}",
                    e.new, p.new, p.new_len
                ));
            }
        }
    }
    Ok(())
}

/// Check field locality (L-5) — the strong form, on the pre-merge micro stream
/// from stages A–C (`pair::micros`).
///
/// Every **mixed** micro (`old_len > 0 && new_len > 0`) genuinely *pairs* old
/// lines against new lines, so it must be confined to a single key section:
/// all lines of both spans share one common key.  Pure deletes/inserts pair
/// nothing and §4.2 Replace-clusters consist only of those, so they are exempt
/// — as are stage D's §4.4 merges, which happen after this stream.
///
/// This is the Depth-1 guarantee Myers cannot make: a set member is never
/// paired against a line of another key, and seq LCS never escapes its section.
fn check_locality(a: &[&[u8]], b: &[&[u8]], micro_stream: &[LineReplace]) -> Result<(), String> {
    for (i, m) in micro_stream.iter().enumerate() {
        if m.old_len == 0 || m.new_len == 0 {
            continue;
        }
        let keys: std::collections::BTreeSet<&[u8]> = (m.old..m.old + m.old_len)
            .map(|k| key_of(a[k]))
            .chain((m.new..m.new + m.new_len).map(|k| key_of(b[k])))
            .collect();
        if keys.len() != 1 {
            return Err(format!(
                "micro {i} ({m:?}) pairs lines across keys {:?}",
                keys.iter()
                    .map(|k| String::from_utf8_lossy(k).into_owned())
                    .collect::<Vec<_>>()
            ));
        }
    }
    Ok(())
}

/// Convenience: build a `&[&[u8]]` from a slice of `&str`.
fn lines<'a>(raw: &[&'a str]) -> Vec<&'a [u8]> {
    raw.iter().map(|s| s.as_bytes()).collect()
}

// ===========================================================================
// 1. Registry shape
// ===========================================================================

#[test]
fn registry_has_34_entries() {
    assert_eq!(REGISTRY.len(), 34);
}

#[test]
fn registry_class_counts() {
    let (mut s, mut set, mut seq) = (0usize, 0usize, 0usize);
    for k in REGISTRY.iter() {
        match k.class {
            Class::Scalar => s   += 1,
            Class::Set    => set += 1,
            Class::Seq    => seq += 1,
        }
    }
    assert_eq!(s,   21, "scalar count");
    assert_eq!(set,  8, "set count");
    assert_eq!(seq,  5, "seq count");
}

#[test]
fn registry_frozen_order() {
    // §1.6 canonical order by (index, name).
    let expected: &[(&str, Class)] = &[
        ("name",       Class::Scalar),
        ("vis",        Class::Scalar),
        ("kind",       Class::Scalar),
        ("span",       Class::Scalar),
        ("src",        Class::Scalar),
        ("parent",     Class::Scalar),
        ("cfg",        Class::Scalar),
        ("attr",       Class::Set   ),
        ("deprecated", Class::Scalar),
        ("alias",      Class::Set   ),
        ("doc",        Class::Seq   ),
        ("dlink",      Class::Set   ),
        ("retgt",      Class::Scalar),
        ("fnsig",      Class::Scalar),
        ("gparam",     Class::Seq   ),
        ("where",      Class::Set   ),
        ("in",         Class::Seq   ),
        ("out",        Class::Seq   ),
        ("fieldty",    Class::Scalar),
        ("recform",    Class::Scalar),
        ("recfield",   Class::Seq   ),
        ("vform",      Class::Scalar),
        ("vdiscr",     Class::Scalar),
        ("super",      Class::Set   ),
        ("tflags",     Class::Scalar),
        ("iof",        Class::Scalar),
        ("ifor",       Class::Scalar),
        ("iflags",     Class::Scalar),
        ("cty",        Class::Scalar),
        ("cval",       Class::Scalar),
        ("auto",       Class::Set   ),
        ("type",       Class::Scalar),
        ("link",       Class::Set   ),
        ("lfact",      Class::Set   ),
    ];
    assert_eq!(REGISTRY.len(), expected.len());
    for (i, (k, (name, cls))) in REGISTRY.iter().zip(expected.iter()).enumerate() {
        assert_eq!(k.name, *name, "registry[{i}] name mismatch");
        assert_eq!(k.class, *cls, "registry[{i}] class mismatch");
    }
}

#[test]
fn key_of_first_tab() {
    assert_eq!(key_of(b"name\tparse\n"), b"name");
    assert_eq!(key_of(b"span\t0\t10\n"), b"span");  // value has further tabs
    assert_eq!(key_of(b"notab"),         b"notab");
    assert_eq!(key_of(b""),              b"");
    assert_eq!(key_of(MAGIC_LINE),       b"NdIrF1"); // magic line
}

#[test]
fn class_of_known_and_unknown() {
    assert_eq!(class_of(b"name"),   Class::Scalar);
    assert_eq!(class_of(b"attr"),   Class::Set);
    assert_eq!(class_of(b"doc"),    Class::Seq);
    // Unknown → Seq.
    assert_eq!(class_of(b"NdIrF1"), Class::Seq);
    assert_eq!(class_of(b""),       Class::Seq);
    assert_eq!(class_of(b"notab"),  Class::Seq);
    assert_eq!(class_of(b"UNKNOWN"), Class::Seq);
}

#[test]
fn magic_str_and_line_agree() {
    assert_eq!(MAGIC_STR.as_bytes(), MAGIC_LINE);
    assert_eq!(MAGIC_LINE, b"NdIrF1\t1\n");
}

// ===========================================================================
// 2. line_diff golden + shape tests
// ===========================================================================

// ---------------------------------------------------------------------------
// §4.7 worked example (must match exactly)
// ---------------------------------------------------------------------------

#[test]
fn worked_example_golden() {
    let a = lines(&[
        "NdIrF1\t1\n",
        "name\tparse\n",
        "vis\tpub\n",
        "attr\tinline\n",
        "attr\tmust_use\n",
        "doc\tParses a string.\n",
    ]);
    let b = lines(&[
        "NdIrF1\t1\n",
        "name\tparse\n",
        "vis\tpub\n",
        "attr\tcold\n",
        "attr\tinline\n",
        "attr\tmust_use\n",
        "doc\tParses a string slice.\n",
    ]);
    let d = line_diff(&a, &b);

    // E1: insert "cold" before "inline" at old line 3 (new=3, pure insertion).
    let e1 = LineReplace { old: 3, old_len: 0, new: 3, new_len: 1 };
    // E2: replace "Parses a string." with "Parses a string slice." — two doc
    //     micros (delete line 5, insert line 6) merged by stage D.
    let e2 = LineReplace { old: 5, old_len: 1, new: 6, new_len: 1 };

    assert_eq!(d, vec![e1, e2], "golden E1/E2 mismatch:\n{d:#?}");

    // Round-trip.
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want, "round-trip failed");
}

// ---------------------------------------------------------------------------
// Trivial cases
// ---------------------------------------------------------------------------

#[test]
fn empty_vs_empty() {
    let d = line_diff(&[], &[]);
    assert!(d.is_empty());
}

#[test]
fn empty_vs_canonical_whole_insert() {
    let b = lines(&["NdIrF1\t1\n", "name\tfoo\n"]);
    let d = line_diff(&[], &b);
    assert_eq!(d, vec![LineReplace { old: 0, old_len: 0, new: 0, new_len: 2 }]);
    let got = apply(&[], &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want);
}

#[test]
fn canonical_vs_empty_whole_delete() {
    let a = lines(&["NdIrF1\t1\n", "name\tfoo\n"]);
    let d = line_diff(&a, &[]);
    assert_eq!(d, vec![LineReplace { old: 0, old_len: 2, new: 0, new_len: 0 }]);
    let got = apply(&a, &d, &[]);
    assert!(got.is_empty());
}

#[test]
fn identical_blobs_empty_output() {
    let blob = lines(&[
        "NdIrF1\t1\n",
        "name\tfoo\n",
        "vis\tpub\n",
        "doc\tSome doc.\n",
    ]);
    let d = line_diff(&blob, &blob);
    assert!(d.is_empty(), "identical blobs should produce empty diff: {d:#?}");
}

// ---------------------------------------------------------------------------
// Per-class shape tests
// ---------------------------------------------------------------------------

#[test]
fn scalar_change_produces_1_1_entry() {
    let a = lines(&["NdIrF1\t1\n", "vis\tpub\n"]);
    let b = lines(&["NdIrF1\t1\n", "vis\tpub(crate)\n"]);
    let d = line_diff(&a, &b);
    assert_eq!(d, vec![LineReplace { old: 1, old_len: 1, new: 1, new_len: 1 }]);
}

#[test]
fn set_add_at_sort_position() {
    // "cold" sorts before "inline" alphabetically.
    let a = lines(&["NdIrF1\t1\n", "attr\tinline\n", "attr\tmust_use\n"]);
    let b = lines(&["NdIrF1\t1\n", "attr\tcold\n",   "attr\tinline\n",   "attr\tmust_use\n"]);
    let d = line_diff(&a, &b);
    // Pure insertion of "cold" at old=1 (before "inline"), new=1.
    assert_eq!(d, vec![LineReplace { old: 1, old_len: 0, new: 1, new_len: 1 }]);
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want);
}

#[test]
fn set_remove() {
    let a = lines(&["NdIrF1\t1\n", "attr\tcold\n", "attr\tinline\n"]);
    let b = lines(&["NdIrF1\t1\n", "attr\tinline\n"]);
    let d = line_diff(&a, &b);
    // Delete "cold" (old=1), anchored at new=1.
    assert_eq!(d, vec![LineReplace { old: 1, old_len: 1, new: 1, new_len: 0 }]);
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want);
}

#[test]
fn set_differing_members_not_paired_as_rewrite() {
    // old has attr\ta, new has attr\tz → must be delete+insert, NOT a {1,1} rewrite.
    // Stage D may or may not merge them depending on adjacency.
    let a = lines(&["NdIrF1\t1\n", "attr\ta\n"]);
    let b = lines(&["NdIrF1\t1\n", "attr\tz\n"]);
    // Stage C emits delete "a" then insert "z" as two independent micros —
    // never a mixed {1,1} pairing (the set-class guarantee, L-5).
    let m = micros(&a, &b);
    assert_eq!(
        m,
        vec![
            LineReplace { old: 1, old_len: 1, new: 1, new_len: 0 }, // delete "a"
            LineReplace { old: 2, old_len: 0, new: 1, new_len: 1 }, // insert "z"
        ],
        "set members must be independent delete+insert micros"
    );
    // The micros are adjacent on both sides, so stage D merges them — an
    // honest §4.4 merge, not a set-class rewrite.
    let d = line_diff(&a, &b);
    assert_eq!(d, vec![LineReplace { old: 1, old_len: 1, new: 1, new_len: 1 }]);
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want, "set differing members round-trip failed");
    check_normal_form(&d).unwrap();
}

#[test]
fn set_differing_members_non_adjacent_kept_separate() {
    // old: attr\ta, attr\tm, attr\tz   new: attr\ta, attr\tz
    // "a" equals "a" (paired).  "m" < "z" so "m" is deleted and "z" matches.
    // The delete micro for "m" and the subsequent pass-through of "z" leave
    // exactly one entry: a pure deletion of "m".
    let a = lines(&["NdIrF1\t1\n", "attr\ta\n", "attr\tm\n", "attr\tz\n"]);
    let b = lines(&["NdIrF1\t1\n", "attr\ta\n", "attr\tz\n"]);
    let d = line_diff(&a, &b);
    // "a"=="a" (equal, no output); "m" < "z" → delete "m" at old=2, new-anchor=2
    // (sb cursor j=1 at that point); "z"=="z" (equal, no output).
    assert_eq!(d, vec![LineReplace { old: 2, old_len: 1, new: 2, new_len: 0 }]);
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want);
}

#[test]
fn seq_edit_mid_section() {
    let a = lines(&["NdIrF1\t1\n", "doc\tFirst.\n", "doc\tSecond.\n", "doc\tThird.\n"]);
    let b = lines(&["NdIrF1\t1\n", "doc\tFirst.\n", "doc\tSecond EDITED.\n", "doc\tThird.\n"]);
    let d = line_diff(&a, &b);
    // Only line 2 changed.
    assert_eq!(d, vec![LineReplace { old: 2, old_len: 1, new: 2, new_len: 1 }]);
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want);
}

#[test]
fn magic_line_version_bump_produces_1_1_at_line_0() {
    // The magic line is treated as an unknown-key Seq section of length 1.
    // When it changes, Myers emits a {1,1} replace.
    let a = lines(&["NdIrF1\t1\n", "name\tfoo\n"]);
    let b = lines(&["NdIrF1\t2\n", "name\tfoo\n"]); // version bump
    let d = line_diff(&a, &b);
    assert_eq!(d, vec![LineReplace { old: 0, old_len: 1, new: 0, new_len: 1 }]);
}

#[test]
fn scalar_dup_non_canonical_tail() {
    // Two "name" lines (non-canonical) — old tail handling.
    let a = lines(&["NdIrF1\t1\n", "name\tfoo\n", "name\tbar\n"]);
    let b = lines(&["NdIrF1\t1\n", "name\tfoo\n"]);
    let d = line_diff(&a, &b);
    // Old tail line 2 deleted, anchored at end of new section (new=2).
    assert_eq!(d, vec![LineReplace { old: 2, old_len: 1, new: 2, new_len: 0 }]);
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want);
}

#[test]
fn unsorted_set_still_terminates_and_round_trips() {
    // Non-canonical: set lines not sorted.
    let a = lines(&["NdIrF1\t1\n", "attr\tz\n", "attr\ta\n"]);
    let b = lines(&["NdIrF1\t1\n", "attr\tz\n", "attr\tb\n"]);
    let d = line_diff(&a, &b);
    check_normal_form(&d).unwrap();
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want, "unsorted set round-trip failed");
}

#[test]
fn section_swap_key_replaced_by_different_key() {
    // Old: vis scalar; New: cfg scalar (different key — a genuine section swap).
    let a = lines(&["NdIrF1\t1\n", "vis\tpub\n"]);
    let b = lines(&["NdIrF1\t1\n", "cfg\tfeature=std\n"]);
    let d = line_diff(&a, &b);
    // Run-align detects a Replace { "vis" → "cfg" } → cluster of delete + insert.
    // Stage D merges them since they are adjacent.
    check_normal_form(&d).unwrap();
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want);
    // Must be exactly 1 entry (the cluster merges).
    assert_eq!(d.len(), 1, "section swap should collapse to 1 entry after stage D");
}

#[test]
fn adjacent_multi_field_edits_merged_by_stage_d() {
    // Change vis AND kind — they become adjacent micros (vis is line 1, kind is
    // line 2; after vis's {1,1} there is no surviving line before kind's {1,1}).
    let a = lines(&["NdIrF1\t1\n", "vis\tpub\n", "kind\tfunction\n", "doc\tfoo\n"]);
    let b = lines(&["NdIrF1\t1\n", "vis\tpriv\n", "kind\tmethod\n",  "doc\tfoo\n"]);
    let d = line_diff(&a, &b);
    // vis {old:1,old_len:1,new:1,new_len:1} and kind {old:2,old_len:1,new:2,new_len:1}
    // are adjacent on both sides → merged into {old:1,old_len:2,new:1,new_len:2}.
    assert_eq!(
        d,
        vec![LineReplace { old: 1, old_len: 2, new: 1, new_len: 2 }],
        "adjacent edits should merge: {d:#?}"
    );
}

#[test]
fn non_adjacent_multi_field_edits_kept_separate() {
    // Change vis and doc — separated by surviving kind line.
    let a = lines(&["NdIrF1\t1\n", "vis\tpub\n",  "kind\tfunction\n", "doc\tOriginal.\n"]);
    let b = lines(&["NdIrF1\t1\n", "vis\tpriv\n", "kind\tfunction\n", "doc\tEdited.\n"]);
    let d = line_diff(&a, &b);
    assert_eq!(d.len(), 2, "non-adjacent edits should be separate: {d:#?}");
    assert_eq!(d[0], LineReplace { old: 1, old_len: 1, new: 1, new_len: 1 }); // vis
    assert_eq!(d[1], LineReplace { old: 3, old_len: 1, new: 3, new_len: 1 }); // doc
}

#[test]
fn magic_only_blob() {
    let a = lines(&["NdIrF1\t1\n"]);
    assert!(line_diff(&a, &a).is_empty());
    // Growing a field: pure insertion after the magic line.
    let b = lines(&["NdIrF1\t1\n", "name\tfoo\n"]);
    assert_eq!(
        line_diff(&a, &b),
        vec![LineReplace { old: 1, old_len: 0, new: 1, new_len: 1 }]
    );
}

#[test]
fn section_swap_at_eof_collapses_to_one_entry() {
    // The last section's key is swapped; the insert anchor is a.len() (past
    // the end) and the cluster must still fuse into a single entry.
    let a = lines(&["NdIrF1\t1\n", "name\tf\n", "vis\tpub\n"]);
    let b = lines(&["NdIrF1\t1\n", "name\tf\n", "lfact\tz\n"]);
    let d = line_diff(&a, &b);
    assert_eq!(d, vec![LineReplace { old: 2, old_len: 1, new: 2, new_len: 1 }]);
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want);
}

#[test]
fn duplicated_key_sections_pair_independently() {
    // Non-canonical: two doc runs separated by attr.  Run alignment matches
    // them positionally; the edit stays confined to the second doc run.
    let a = lines(&["NdIrF1\t1\n", "doc\tA\n", "attr\tx\n", "doc\tB\n"]);
    let b = lines(&["NdIrF1\t1\n", "doc\tA\n", "attr\tx\n", "doc\tB2\n"]);
    let d = line_diff(&a, &b);
    assert_eq!(d, vec![LineReplace { old: 3, old_len: 1, new: 3, new_len: 1 }]);
    check_locality(&a, &b, &micros(&a, &b)).unwrap();
}

#[test]
fn merge_boundary_set_growth_plus_scalar_change() {
    // A set grows at its section end AND the immediately following scalar
    // changes: the two micros share a zero-width gap on both sides, so §4.4
    // mandates one merged entry spanning the section boundary.
    let a = lines(&["NdIrF1\t1\n", "attr\ta\n", "retgt\tX\n"]);
    let b = lines(&["NdIrF1\t1\n", "attr\ta\n", "attr\tz\n", "retgt\tY\n"]);
    let m = micros(&a, &b);
    assert_eq!(
        m,
        vec![
            LineReplace { old: 2, old_len: 0, new: 2, new_len: 1 }, // insert attr\tz
            LineReplace { old: 2, old_len: 1, new: 3, new_len: 1 }, // rewrite retgt
        ]
    );
    let d = line_diff(&a, &b);
    assert_eq!(d, vec![LineReplace { old: 2, old_len: 1, new: 2, new_len: 2 }]);
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want);
}

// ---------------------------------------------------------------------------
// L-1 … L-5 on a medium-size canonical blob
// ---------------------------------------------------------------------------

fn make_canonical_blob() -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = vec![
        b"NdIrF1\t1\n".to_vec(),
        b"name\tparse_value\n".to_vec(),
        b"vis\tpub\n".to_vec(),
    ];
    lines.push(b"kind\tfunction\n".to_vec());
    lines.push(b"span\t10\t42\n".to_vec());
    lines.push(b"attr\t#[inline]\n".to_vec());
    lines.push(b"attr\t#[must_use]\n".to_vec());
    lines.push(b"doc\tParses a value from a string.\n".to_vec());
    lines.push(b"doc\t\n".to_vec());
    lines.push(b"doc\t# Errors\n".to_vec());
    lines.push(b"doc\tReturns Err on invalid input.\n".to_vec());
    lines.push(b"fnsig\tasync:false const:false unsafe:false\n".to_vec());
    lines.push(b"gparam\tT:Clone\n".to_vec());
    lines.push(b"in\tstr\n".to_vec());
    lines.push(b"out\tResult<T,E>\n".to_vec());
    lines
}

#[test]
fn canonical_blob_laws() {
    let a_owned = make_canonical_blob();
    let a: Vec<&[u8]> = a_owned.iter().map(|v| v.as_slice()).collect();

    // Mutate: edit doc line 7 ("Parses..."), add a new attr, change vis.
    let mut b_owned = a_owned.clone();
    b_owned[2] = b"vis\tpub(crate)\n".to_vec();
    b_owned.insert(5, b"attr\t#[cold]\n".to_vec()); // sorts before #[inline]
    *b_owned.iter_mut().find(|l| l.starts_with(b"doc\tParses")).unwrap() =
        b"doc\tParses a value from a byte slice.\n".to_vec();
    let b: Vec<&[u8]> = b_owned.iter().map(|v| v.as_slice()).collect();

    // L-1: no panic.
    let d = line_diff(&a, &b);

    // L-2: round-trip.
    let got = apply(&a, &d, &b);
    let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
    assert_eq!(got, want, "L-2 round-trip failed");

    // L-3: determinism.
    let d2 = line_diff(&a, &b);
    assert_eq!(d, d2, "L-3 determinism failed");

    // L-4: normal form.
    check_normal_form(&d).unwrap();

    // L-5: strong locality on the pre-merge micro stream.
    check_locality(&a, &b, &micros(&a, &b)).unwrap();
}

// ===========================================================================
// 3. Fuzz (xorshift64 PRNG, several hundred iterations)
// ===========================================================================

/// Simple xorshift64 PRNG — no external dep.
struct Xorshift64(u64);

impl Xorshift64 {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn next_usize(&mut self, max: usize) -> usize {
        if max == 0 { return 0; }
        (self.next() as usize) % max
    }
}

/// Generate a random line drawn from:
///   - known registry keys (70 % of the time)
///   - garbage keys without TAB (10 %)
///   - empty line (5 %)
///   - line with no trailing newline (5 %)
///   - multi-TAB value (10 %)
fn random_line(rng: &mut Xorshift64, key_pool: &[&str]) -> Vec<u8> {
    let r = rng.next() % 100;
    if r < 70 {
        let key = key_pool[rng.next_usize(key_pool.len())];
        let val_len = rng.next_usize(20);
        let mut l: Vec<u8> = key.as_bytes().to_vec();
        l.push(b'\t');
        for _ in 0..val_len {
            l.push(b'a' + (rng.next() % 26) as u8);
        }
        l.push(b'\n');
        l
    } else if r < 80 {
        // Garbage: no TAB.
        let len = 1 + rng.next_usize(8);
        let mut l = vec![b'g'; len];
        l.push(b'\n');
        l
    } else if r < 85 {
        b"\n".to_vec()
    } else if r < 90 {
        // No trailing newline.
        let key = key_pool[rng.next_usize(key_pool.len())];
        let mut l: Vec<u8> = key.as_bytes().to_vec();
        l.push(b'\t');
        l.push(b'v');
        l
    } else {
        // Multi-TAB value.
        let key = key_pool[rng.next_usize(key_pool.len())];
        let mut l: Vec<u8> = key.as_bytes().to_vec();
        l.extend_from_slice(b"\tv1\tv2\n");
        l
    }
}

fn random_blob(rng: &mut Xorshift64, key_pool: &[&str], max_lines: usize) -> Vec<Vec<u8>> {
    let n = rng.next_usize(max_lines);
    (0..n).map(|_| random_line(rng, key_pool)).collect()
}

#[test]
fn fuzz_laws_l1_l2_l3_l4_l5() {
    let key_pool: Vec<&str> = REGISTRY.iter().map(|k| k.name).collect();

    let mut rng = Xorshift64(0xdeadbeef_cafebabe);

    for iteration in 0..500 {
        let a_owned = random_blob(&mut rng, &key_pool, 30);
        let b_owned = random_blob(&mut rng, &key_pool, 30);

        let a: Vec<&[u8]> = a_owned.iter().map(|v| v.as_slice()).collect();
        let b: Vec<&[u8]> = b_owned.iter().map(|v| v.as_slice()).collect();

        // L-1: no panic.
        let d = line_diff(&a, &b);

        // L-2: round-trip.
        let got = apply(&a, &d, &b);
        let want: Vec<Vec<u8>> = b.iter().map(|l| l.to_vec()).collect();
        assert_eq!(
            got, want,
            "L-2 failed at iteration {iteration}\na={a_owned:?}\nb={b_owned:?}\nd={d:#?}"
        );

        // L-3: determinism.
        let d2 = line_diff(&a, &b);
        assert_eq!(d, d2, "L-3 failed at iteration {iteration}");

        // L-4: normal form (strict gaps on both sides).
        check_normal_form(&d).unwrap_or_else(|e| {
            panic!("L-4 failed at iteration {iteration}: {e}\nd={d:#?}\na={a_owned:?}\nb={b_owned:?}")
        });

        // L-5: no mixed micro ever pairs lines across keys.
        check_locality(&a, &b, &micros(&a, &b)).unwrap_or_else(|e| {
            panic!("L-5 failed at iteration {iteration}: {e}\na={a_owned:?}\nb={b_owned:?}")
        });
    }
}

// ===========================================================================
// 4. Layering / lockfile asserts (T-11 a, b, c mechanical parts)
// ===========================================================================

/// Read own Cargo.toml (via CARGO_MANIFEST_DIR) and return its contents.
fn own_manifest() -> String {
    let dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR not set — run via cargo test");
    std::fs::read_to_string(format!("{dir}/Cargo.toml"))
        .expect("failed to read Cargo.toml")
}

/// Read the workspace Cargo.lock (two levels up from CARGO_MANIFEST_DIR).
fn workspace_lock() -> String {
    let dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("CARGO_MANIFEST_DIR not set — run via cargo test");
    let lock_path = format!("{dir}/../../Cargo.lock");
    std::fs::read_to_string(&lock_path)
        .unwrap_or_else(|e| panic!("failed to read {lock_path}: {e}"))
}

#[test]
fn t11a_lockfile_exactly_one_libpijul_path_sourced() {
    let lock = workspace_lock();

    // Count `name = "libpijul"` occurrences.
    let count = lock.matches("name = \"libpijul\"").count();
    assert_eq!(count, 1, "expected exactly 1 libpijul in Cargo.lock, got {count}");

    // The single entry must be path-sourced: it must NOT have a `source =` line
    // immediately following the `name = "libpijul"` stanza.
    //
    // Parse: find the [[package]] block containing libpijul and check no `source`
    // key is present in that block.
    let mut in_libpijul = false;
    let mut found_name = false;
    for line in lock.lines() {
        if line == "[[package]]" {
            // Start of a new package stanza.
            in_libpijul = false;
            found_name = false;
        }
        if line == "name = \"libpijul\"" {
            in_libpijul = true;
            found_name = true;
        }
        if in_libpijul && found_name && line.starts_with("source = ") {
            panic!("libpijul in Cargo.lock has a `source =` line — it must be path-sourced");
        }
    }
}

#[test]
fn t11b_lockfile_exactly_one_diffs_0_5_1() {
    let lock = workspace_lock();
    let count = lock.matches("name = \"diffs\"").count();
    assert_eq!(count, 1, "expected exactly 1 diffs in Cargo.lock, got {count}");

    // Verify version is 0.5.1.
    let mut in_diffs = false;
    for line in lock.lines() {
        if line == "[[package]]" {
            in_diffs = false;
        }
        if line == "name = \"diffs\"" {
            in_diffs = true;
        }
        if in_diffs && line.starts_with("version = ") {
            let ver = line.trim_start_matches("version = ").trim_matches('"');
            assert_eq!(ver, "0.5.1", "diffs version mismatch: {ver}");
            return;
        }
    }
    panic!("diffs package not found in Cargo.lock");
}

#[test]
fn t11c_own_manifest_has_no_forbidden_deps() {
    // Comments may legitimately mention forbidden crate names; only dependency
    // lines count.
    let manifest: String = own_manifest()
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    for forbidden in &["libpijul", "nudox-ir", "nudox-change"] {
        assert!(
            !manifest.contains(forbidden),
            "Cargo.toml must not depend on {forbidden}, but it does:\n{manifest}"
        );
    }
    // Must have diffs = "=0.5.1".
    assert!(
        manifest.contains("diffs = \"=0.5.1\""),
        "Cargo.toml must have diffs = \"=0.5.1\""
    );
}
