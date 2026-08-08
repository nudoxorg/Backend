//! A pre-built resolution table from the producer's resolved intra-doc links.
//!
//! # Why a separate type?
//!
//! `Symbol::doc_links` carries entries that rust-analyzer already resolved
//! with full name resolution in scope — far more accurate than any path-string
//! heuristic the engine could compute on its own.  The table maps the link
//! target string exactly as it appeared in the source (e.g. `"Router::with_state"`)
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
//! `.real-crates/memchr-2.8.3/src/memchr.rs:282`):
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
//! (`.real-crates/memchr-2.8.3/src/memchr.rs:216,223`), so both aliases end
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
//! repaired ones. See `wire/repair.rs` for the closed set and `LIMITATIONS.md`
//! L46 for the ruling — including what a *second* repair would cost, which is
//! five compile errors in five files.

use std::collections::HashMap;

use nudox_ir::entry::Entry;
use nudox_store::package::PackageView;

use crate::wire::SymbolKey;

pub(crate) struct DocLinkTable {
    pub(crate) inner: HashMap<String, SymbolKey>,
    /// Whether the symbol *declared* any intra-doc links at all, regardless of
    /// how many of them resolved inside this package.
    pub(crate) declared: bool,
}

impl DocLinkTable {
    /// Build from the producer-resolved `doc_links` on one `Symbol`.
    pub(crate) fn build(entry: &Entry, package: &PackageView) -> Self {
        let mut inner: HashMap<String, SymbolKey> = HashMap::new();
        let declared = !entry.sym().doc_links.is_empty();

        for doc_link in entry.sym().doc_links.iter() {
            let target = doc_link.target.as_str();

            // Step 1: strip namespace tag.
            let stripped_tag = match target.find('!') {
                Some(idx) => &target[..idx],
                None => target,
            };

            // Step 2: strip rustdoc anchor prefix.
            let stripped_anchor: &str = if let Some(rest) = stripped_tag.strip_prefix('#') {
                match rest.find('.') {
                    Some(dot) => &rest[dot + 1..],
                    None => rest,
                }
            } else {
                stripped_tag
            };

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
                                if best.map_or(true, |(_, prev_len)| len > prev_len) {
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
                            .map(|s| s.to_lowercase())
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
                                        if let Some(ls) = current_label_seg {
                                            if stored_seg.to_lowercase() == *ls {
                                                current_label_seg = label_iter.next();
                                            }
                                        }
                                    }
                                    if current_label_seg.is_none() {
                                        let len = stored_path.len();
                                        if seg_best.map_or(true, |(_, prev)| len > prev) {
                                            seg_best = Some((hit.intro, len));
                                        }
                                    }
                                }
                            }
                        }

                        seg_best.map(|(id, _)| id).unwrap_or_else(|| hits[0].intro)
                    }
                }
            };

            let key = nudox_ir::change::StableRef::new(package.lineage().clone(), resolved);

            inner.insert(target.to_owned(), key.clone());

            if normalised != target {
                inner.entry(normalised.clone()).or_insert(key.clone());
            }

            inner.entry(leaf.to_owned()).or_insert(key.clone());

            if let Some(label) = &doc_link.label {
                inner.entry(label.clone()).or_insert(key);
            }
        }

        Self { inner, declared }
    }

    /// Resolve a link target string to a `SymbolKey`, if known.
    pub(crate) fn resolve(&self, target: &str) -> Option<&SymbolKey> {
        if let Some(key) = self.inner.get(target) {
            return Some(key);
        }
        if let Some(leaf) = target.rsplit("::").next() {
            if leaf != target {
                if let Some(key) = self.inner.get(leaf) {
                    return Some(key);
                }
            }
        }
        if let Some(leaf) = target.rsplit('.').next() {
            if !leaf.is_empty() {
                return self.inner.get(leaf);
            }
        }
        None
    }

    /// `true` when no links were resolved.
    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// `true` when the symbol declared intra-doc links, whether or not any of
    /// them resolved locally.
    ///
    /// This gates shortcut-link scanning in `prose.rs`, and that is the whole
    /// reason it exists. Brackets are only read as link syntax for a symbol
    /// that declared at least one link; for a symbol that declared none,
    /// `[NOTE]`, `arr[0]`, and a citation marker are all far likelier than a
    /// link attempt, so its prose passes through verbatim. That mirrors
    /// rustdoc, which renders an unresolved shortcut as literal text.
    ///
    /// The signal is per *symbol*, not per *link attempt* — see L27. A symbol
    /// that declares any link has brackets stripped from unrelated literal
    /// prose too. Narrowing that needs the producer to record byte spans of
    /// the link attempts it saw, which it does not currently do.
    pub(crate) fn has_declared_links(&self) -> bool {
        self.declared
    }
}
