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
    pub(crate) fn has_declared_links(&self) -> bool {
        self.declared
    }
}
