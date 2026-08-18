//! A pre-built resolution table from the producer's intra-doc link attempts.
//!
//! # Why a separate type?
//!
//! `Symbol::doc_links` carries one entry for every path-shaped occurrence that
//! the producer saw. Resolved entries carry the canonical target; unresolved
//! entries retain their source spelling and span so rendering can decide each
//! occurrence independently. Resolved entries are mapped from the link target
//! string exactly as it appeared in the source (e.g. `"Router::with_state"`)
//! to the `SymbolKey` that the producer determined it refers to.
//!
//! The table is built once per entry at the top of `walk_doc` and threaded
//! into `build_prose_blocks`.  It is small (typically 0–20 entries) and
//! short-lived — there is no reason to cache it across entries.
//!
//! # Resolution algorithm
//!
//! For each `DocLink`:
//! 1. The `target` string from the producer is **normalised** first:
//!    strip any namespace tag (`!m`, `!v`, etc.), strip any rustdoc anchor
//!    prefix (`#method.`), and replace `::` with `.` so all path forms unify.
//! 2. We extract the leaf name (last `.` segment of the normalised path) and
//!    look it up in the `NameIndex` (case-folded exact match on the leaf).
//! 3. Among the hits, we pick the one whose precomputed dot-path most closely
//!    matches the normalised target suffix.  When multiple entries have the
//!    same leaf name, this narrows to the correct one; when only one exists,
//!    the suffix check is skipped.
//! 4. A successful match yields a `SymbolKey`; we store it under four aliases:
//!    the original target, the normalised form, the bare leaf, and the display
//!    label.  This ensures that `[Router::with_state]` (as the author wrote
//!    it) resolves through the same key as
//!    `"axum.routing.Router.with_state!m"` (as the producer stored it).
//!
//! # L17 case study — `memchr_iter` resolves, `memrchr_iter` doesn't
//!
//! `memchr-2.8.3`'s `struct Memchr` doc comment reads (verbatim, from
//! `result/memchr-2.8.3/src/memchr.rs:282`):
//!
//! ```text
//! This iterator is created by the [`memchr_iter`] or `[memrchr_iter`]
//! functions.
//! ```
//!
//! The second link has its backtick and bracket **swapped** — a typo in
//! upstream memchr, not a bug on our side. That is *not* a resolution
//! failure in this table: `rust-analyzer`'s own target extractor
//! (`extract_doc_link_targets` in
//! `workspace/compiler/languages/rust/src/ra/docs.rs`) is a byte-level
//! bracket scanner that finds `memrchr_iter` and resolves it exactly like
//! `memchr_iter` — both genuinely exist
//! (`result/memchr-2.8.3/src/memchr.rs:216,223`), so both aliases end
//! up in this table.
//!
//! The asymmetry is downstream, in `prose.rs`'s markdown *rendering* of the
//! same doc comment. `` [`memchr_iter`] `` — brackets outside the backticks
//! — tokenizes under CommonMark as `Text("[")`, `Code("memchr_iter")`,
//! `Text("]")`: three events our shortcut-link scan recognises as a link
//! attempt, so it reaches `resolve("memchr_iter")` and finds the entry this
//! table built. `` `[memrchr_iter`] `` — backtick *before* the bracket —
//! tokenizes completely differently: CommonMark gives code spans higher
//! precedence than link brackets, so the stray `[` is swallowed into the
//! code span itself (`Code("[memrchr_iter")`), followed by a bare
//! `Text("]")`. That shape never reaches this table at all — `resolve` is
//! never even called for it — because the two independent implementations
//! (the producer's lenient byte-scanner vs. the renderer's strict CommonMark
//! tokenizer) disagree about what counts as a link boundary on malformed
//! input, and only the renderer's opinion is visible to the reader.
//!
//! This table's resolution algorithm was never the problem for this case;
//! see `prose.rs`'s `consume_bracket_run` for the actual fix, which
//! recognises the swapped-backtick shape as a link-attempt input too (so it
//! benefits from whatever this table already resolved) and, independently,
//! guarantees neither shape can ever leak a bare `[` or `]` to the reader.
//!
//! ## What that fix actually is, named
//!
//! Rendering a link from `` `[memrchr_iter`] `` is **not** the same act as
//! rendering one from `` [`memchr_iter`] ``, and the paragraph above is
//! incomplete if it leaves that implicit. The second is rustdoc's documented
//! intra-doc-link syntax, which rustdoc and docs.rs both render as a link —
//! producing a link there is *fidelity*. The first is a spelling rustdoc
//! renders as literal text; producing a link there is a **repair**, and it is
//! the one and only repair this engine performs:
//! [`crate::wire::LinkRepairKind::TransposedOpenDelimiter`].
//!
//! That distinction is carried in the output, not just in this comment. Every
//! `InlineRun::Link` carries a mandatory `LinkOrigin` saying whether the
//! spelling was the author's (`Authored`) or ours (`Repaired`), the GUI paints
//! the two differently, and `chunk::repair_audit::RepairTally` counts the
//! repaired ones. See `wire/repair.rs` for the closed set and `docs/LIMITATIONS.md`
//! L46 for the ruling — including what a *second* repair would cost, which is
//! five compile errors in five files.

use std::collections::HashMap;

use crate::store::package::PackageView;
use nudox_ir::entry::Entry;

use crate::wire::SymbolKey;

pub(crate) struct DocLinkTable {
    pub(crate) inner: HashMap<String, SymbolKey>,
    /// Whether the symbol contained any path-shaped intra-doc link attempts.
    pub(crate) declared: bool,
    pub(crate) inner_spans: Vec<std::ops::Range<usize>>,
}

impl DocLinkTable {
    /// Build from the producer-resolved `doc_links` on one `Symbol`.
    pub(crate) fn build(entry: &Entry, package: &PackageView) -> Self {
        let mut inner: HashMap<String, SymbolKey> = HashMap::new();
        let declared = !entry.sym().doc_links.is_empty();

        let inner_spans = entry
            .sym()
            .doc_links
            .iter()
            .filter_map(|link| link.source_span.clone())
            .collect();

        for doc_link in &entry.sym().doc_links {
            let target = doc_link.target.as_str();

            // Step 1: strip namespace tag.
            let stripped_tag = target.find('!').map_or(target, |idx| &target[..idx]);

            // Step 2: strip rustdoc anchor prefix.
            let stripped_anchor: &str =
                stripped_tag.strip_prefix('#').map_or(stripped_tag, |rest| {
                    rest.find('.').map_or(rest, |dot| &rest[dot + 1..])
                });

            // Step 3: unify path separators.
            let normalised: String = stripped_anchor.replace("::", ".");

            // Leaf: last `.` segment of the normalised path.
            let leaf: &str = normalised.rsplit('.').next().unwrap_or(&normalised);
            let leaf_lower = leaf.to_lowercase();

            // Normalised path lowercased for suffix matching.
            let normalised_lower = normalised.to_lowercase();

            // Look up all entries with this leaf name.
            let hits: Vec<_> = package.indexes().by_name.get_exact(&leaf_lower).to_vec();

            let resolved = match hits.len() {
                0 => {
                    continue;
                }
                1 => hits[0].intro,
                _ => {
                    let mut best: Option<(nudox_ir::change::IntroId, usize)> = None;
                    for hit in &hits {
                        if let Some(stored_path) = package.indexes().path_of(hit.intro) {
                            let path_lower = stored_path.to_lowercase();
                            if path_lower.ends_with(&normalised_lower) {
                                let len = stored_path.len();
                                if best.is_none_or(|(_, prev_len)| len > prev_len) {
                                    best = Some((hit.intro, len));
                                }
                            }
                        }
                    }

                    if let Some((id, _)) = best {
                        id
                    } else {
                        let label_segs_lower: Vec<String> = doc_link
                            .label
                            .as_deref()
                            .unwrap_or("")
                            .split("::")
                            .map(str::to_lowercase)
                            .filter(|s| !s.is_empty())
                            .collect();

                        let mut seg_best: Option<(nudox_ir::change::IntroId, usize)> = None;

                        if !label_segs_lower.is_empty() {
                            for hit in &hits {
                                if let Some(stored_path) = package.indexes().path_of(hit.intro) {
                                    let stored_segs: Vec<&str> = stored_path.split('.').collect();
                                    let mut label_iter = label_segs_lower.iter();
                                    let mut current_label_seg = label_iter.next();
                                    for stored_seg in &stored_segs {
                                        if let Some(ls) = current_label_seg
                                            && stored_seg.to_lowercase() == *ls
                                        {
                                            current_label_seg = label_iter.next();
                                        }
                                    }
                                    if current_label_seg.is_none() {
                                        let len = stored_path.len();
                                        if seg_best.is_none_or(|(_, prev)| len > prev) {
                                            seg_best = Some((hit.intro, len));
                                        }
                                    }
                                }
                            }
                        }

                        seg_best.map_or_else(|| hits[0].intro, |(id, _)| id)
                    }
                }
            };

            let key = nudox_ir::change::StableRef::new(package.lineage().clone(), resolved);

            inner.insert(target.to_owned(), key.clone());

            if normalised != target {
                inner
                    .entry(normalised.clone())
                    .or_insert_with(|| key.clone());
            }

            inner.entry(leaf.to_owned()).or_insert_with(|| key.clone());

            if let Some(label) = &doc_link.label {
                inner.entry(label.clone()).or_insert(key);
            }
        }

        Self {
            inner,
            declared,
            inner_spans,
        }
    }

    /// Resolve a link target string to a `SymbolKey`, if known.
    pub(crate) fn resolve(&self, target: &str) -> Option<&SymbolKey> {
        if let Some(key) = self.inner.get(target) {
            return Some(key);
        }
        if let Some(leaf) = target.rsplit("::").next()
            && leaf != target
            && let Some(key) = self.inner.get(leaf)
        {
            return Some(key);
        }
        if let Some(leaf) = target.rsplit('.').next()
            && !leaf.is_empty()
        {
            return self.inner.get(leaf);
        }
        None
    }

    /// `true` when no links were resolved.
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Return whether a producer-recorded link occupies this source range.
    /// Missing spans remain supported for older producers and fixtures.
    ///
    /// This also gates shortcut-link scanning in `prose.rs`: brackets are only
    /// read as link syntax when a declared link occupies the source span.
    pub(crate) fn has_declared_link_at(&self, span: std::ops::Range<usize>) -> bool {
        if self.inner_spans.is_empty() {
            return self.declared;
        }
        self.inner_spans
            .iter()
            .any(|declared| declared.start <= span.start && span.end <= declared.end)
    }
}
