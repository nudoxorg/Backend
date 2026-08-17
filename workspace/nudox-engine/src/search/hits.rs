//! Name, type/kind and signature hit collection, scoring, and disambiguation.

use nudox_ir::{entry::Visibility, kind::KindDiscriminant};

use crate::{
    chunk::signature,
    search::SearchQuery,
    wire::{HitRow, KindTag, Provenance, SharedStr},
};

// ---------------------------------------------------------------------------
// Display-name disambiguation (L20)
// ---------------------------------------------------------------------------
//
// Real crates repeat leaf names across modules constantly — `memchr` alone
// declares `mod memchr` once per architecture backend
// (`arch::x86_64::memchr`, `arch::aarch64::memchr`, `arch::generic::memchr`,
// …). A hit row that shows only the leaf name is then indistinguishable from
// its siblings: same text, same kind badge, nothing to click on with intent.
//
// The fix used to apply only when the corpus held more than one package,
// which is precisely wrong — leaf-name collisions are just as real *within*
// a single package (see `memchr` above), and a single-package corpus is the
// common case (open one crate's docs, search it). Disambiguation must key
// off whether the leaf name actually collides in the returned result set,
// not off how many packages are loaded.

/// One collected hit, still carrying both display-name candidates so the
/// collision pass (`finalize_candidates`) can choose after sorting and
/// truncation — a leaf name that only collides with a row cut for exceeding
/// `limit` renders in its short form, since nothing on screen would be
/// confused with it.
pub(crate) struct Candidate {
    /// The wire row. `row.display_name` holds the **leaf** (unqualified)
    /// name until `finalize_candidates` runs.
    pub(crate) row: HitRow,
    /// The fully-qualified in-package path, computed once so the final pass
    /// never re-walks the parent chain per query.
    pub(crate) qualified: SharedStr,
}

/// The fully-qualified display candidate for `intro`, used when its leaf
/// name collides with another hit in the same result set.
///
/// `path_of` is the *in-package* moniker, so on its own it cannot separate
/// `a::Router` from `b::Router` in a multi-package corpus — both roots
/// render the same string. The package name has to be part of it. The
/// moniker already begins with the crate's root module for most producers
/// (Rust's root module is named after the crate), so prefixing
/// unconditionally would yield `axum::axum.routing.…` — prepend only when
/// the first segment is not already the package name.
pub(crate) fn qualified_display_name(
    indexes: &crate::store::package::PackageIndexes,
    intro: nudox_ir::change::IntroId,
    leaf: &str,
    pkg_name: &str,
) -> SharedStr {
    let base = indexes
        .path_of(intro)
        .map(|p| p.as_ref().to_owned())
        .unwrap_or_else(|| leaf.to_owned());
    if base.split(['.', ':']).next() == Some(pkg_name) {
        SharedStr::from(base)
    } else {
        SharedStr::from(format!("{pkg_name}::{base}"))
    }
}

/// Resolve each candidate's final `display_name`: the short leaf form when
/// it is unique in this result set, the fully-qualified path when it
/// collides with another row's leaf name (L20).
///
/// The invariant this exists to guarantee: **N rows sharing a leaf name
/// produce N distinct `display_name`s.**
pub(crate) fn finalize_candidates(candidates: Vec<Candidate>) -> Vec<HitRow> {
    // Keyed on cloned `SharedStr` (cheap — it's an `Arc<str>` under the hood)
    // rather than `&str` borrowed from `candidates`, so the counts table does
    // not keep `candidates` borrowed for the `into_iter()` below.
    let mut leaf_counts: std::collections::HashMap<SharedStr, usize> =
        std::collections::HashMap::new();
    for c in &candidates {
        *leaf_counts.entry(c.row.display_name.clone()).or_insert(0) += 1;
    }

    candidates
        .into_iter()
        .map(|mut c| {
            let collides = leaf_counts
                .get(&c.row.display_name)
                .copied()
                .unwrap_or(0)
                > 1;
            if collides {
                c.row.display_name = c.qualified;
            }
            c.row
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Relevance
// ---------------------------------------------------------------------------

/// How much of the query the matched name is: `1.0` for a case-folded exact
/// match, otherwise the fraction of the matched name the query covers.
///
/// This is the only factor derived from the *query*. The two below are
/// properties of the symbol alone.
fn text_relevance(entry_key: &str, prefix_lower: &str) -> f32 {
    if entry_key == prefix_lower {
        1.0
    } else {
        prefix_lower.len() as f32 / entry_key.len().max(1) as f32
    }
}

/// The relevance the "type" section assigns before quality weighting.
///
/// Kind-facet hits are not text matches at all — the query named a *kind*, not
/// a symbol — so they sit below every genuine name match by construction
/// rather than by tuning.
const TYPE_FACET_RELEVANCE: f32 = 0.5;

/// Weight by **audience breadth**: how large the set of callers is that the
/// author declared this symbol for.
///
/// The reader is looking at documentation, which means they are looking for
/// something they can actually reach. A `pub` item is the thing being
/// documented; a private one is an implementation detail that happens to share
/// a name with it — real crates are full of these (`memchr` declares a private
/// `mod memchr` once per architecture backend, all of them exact-matching a
/// search for `memchr`). Public is deliberately `1.0`, so this factor only ever
/// pushes *down*: it can reorder a tie, never promote a worse text match over a
/// better one.
fn visibility_weight(visibility: Visibility) -> f32 {
    match visibility {
        // Reachable from anywhere. The reference point.
        Visibility::Public => 1.0,
        // Reachable from subtypes of the declaring type.
        Visibility::Protected => 0.75,
        // Reachable within one compilation unit (crate / assembly / package).
        Visibility::Internal | Visibility::Package | Visibility::Crate => 0.6,
        // Reachable only inside the declaring scope.
        Visibility::Private => 0.4,
    }
}

/// Weight by **addressability**: whether this kind is something a reader
/// navigates *to*, or a part of something they navigate to.
///
/// A field, a variant and a parameter are only meaningful in the context of
/// their owner — landing on one is usually a step on the way to the owner
/// rather than the destination. Independently addressable declarations are the
/// reference point at `1.0`, for the same reason `Public` is: this factor must
/// not be able to lift a weaker match above a stronger one.
///
/// The match is exhaustive on purpose. `KindDiscriminant` gains a variant every
/// time a language contributes one, and "what is this kind worth in a ranked
/// list?" is a question the person adding it should have to answer here rather
/// than have silently answered for them by a `_` arm.
fn kind_weight(kind: KindDiscriminant) -> f32 {
    match kind {
        KindDiscriminant::Module
        | KindDiscriminant::Record
        | KindDiscriminant::Function
        | KindDiscriminant::Alias
        | KindDiscriminant::Trait
        | KindDiscriminant::Impl
        | KindDiscriminant::Enum
        | KindDiscriminant::Const
        | KindDiscriminant::Static
        | KindDiscriminant::Reexport => 1.0,
        // Members: addressable, but only through their owner.
        KindDiscriminant::Field | KindDiscriminant::Variant => 0.85,
        // Part of a signature, not a documented API surface of its own.
        KindDiscriminant::Param => 0.7,
    }
}

/// The full relevance of one hit: query fit scaled by the two symbol-quality
/// factors. See the module docs.
pub(crate) fn score_of(relevance: f32, visibility: Visibility, kind: KindDiscriminant) -> f32 {
    relevance * visibility_weight(visibility) * kind_weight(kind)
}

/// The **total** order search results are presented in.
///
/// Score descending, then display name ascending, then `StableRef` — package
/// lineage, then `IntroId`. Terminating in the symbol's identity is the whole
/// point: `IntroId` is a content hash, so every remaining tie is broken by a
/// value that is a property of the package rather than of the run, and no
/// upstream iteration order can reach the output. A comparator that stops at
/// `display_name` is *not* total here — every case-folded exact match scores
/// identically and carries the same leaf name at sort time, so a whole result
/// set can be one flat tie.
///
/// `total_cmp` rather than `partial_cmp(..).unwrap_or(Equal)`: an `Equal`
/// fallback for an unorderable score is exactly the silent tie this function
/// exists to eliminate.
pub(crate) fn compare_candidates(a: &Candidate, b: &Candidate) -> std::cmp::Ordering {
    b.row
        .score
        .total_cmp(&a.row.score)
        .then_with(|| {
            let a_str: &str = &a.row.display_name;
            let b_str: &str = &b.row.display_name;
            a_str.cmp(b_str)
        })
        .then_with(|| a.row.key.cmp(&b.row.key))
}

// ---------------------------------------------------------------------------
// Name-hit collection
// ---------------------------------------------------------------------------

pub(crate) fn collect_name_hits(
    packages: &[std::sync::Arc<crate::store::package::PackageView>],
    query: &SearchQuery,
) -> Vec<HitRow> {
    let text = query.text.trim();
    if text.is_empty() {
        return Vec::new();
    }

    let prefix_lower = text.to_lowercase();
    let limit = if query.limit == 0 {
        usize::MAX
    } else {
        query.limit
    };

    let mut candidates: Vec<Candidate> = Vec::new();

    for pkg in packages {
        if !query.package_matches(pkg.lineage()) {
            continue;
        }
        let indexes = pkg.indexes();
        let provenance: Provenance = pkg.provenance().into();
        let pkg_name = pkg.lineage().name.as_str();

        for entry in indexes.by_name.prefix(&prefix_lower) {
            if candidates.len() >= limit {
                break;
            }

            // Apply kind filter.
            let Some(ir_entry) = pkg.view().entry(entry.intro) else {
                continue;
            };
            let Some(disc) = ir_entry.kind().discriminant() else {
                continue;
            };
            if !query.kind_matches(disc) {
                continue;
            }

            let key = nudox_ir::change::StableRef::new(pkg.lineage().clone(), entry.intro);

            // LR-4: signature from the single renderer.
            let sig_preview = signature::tokens(ir_entry, pkg);

            let leaf: SharedStr = SharedStr::from(entry.display.as_str());
            let qualified =
                qualified_display_name(indexes, entry.intro, entry.display.as_str(), pkg_name);

            let score = score_of(
                text_relevance(&entry.key, &prefix_lower),
                ir_entry.sym().visibility,
                disc,
            );

            candidates.push(Candidate {
                row: HitRow {
                    key,
                    display_name: leaf,
                    sig_preview,
                    kind: KindTag::Known(disc),
                    provenance: provenance.clone(),
                    score,
                },
                qualified,
            });
        }
    }

    candidates.sort_by(compare_candidates);
    candidates.truncate(limit);
    finalize_candidates(candidates)
}

// ---------------------------------------------------------------------------
// Type / kind-filter hit collection
// ---------------------------------------------------------------------------

/// Section 1's entry point: signature search where the query is one, kind facet
/// otherwise.
///
/// # Why both live behind one section
///
/// They answer the same reader question — "show me things shaped like this" —
/// at two levels of precision, and the reader does not switch sections when
/// they get more specific. `struct` and `return:Result` are both type questions;
/// only the second is a *signature* question, and only the second was missing.
///
/// The kind path is unchanged and still runs for every query that names no
/// facet, so nothing that worked before stops working. See
/// [`crate::typequery`] for the grammar and for why it is a grammar rather than
/// free text.
pub(crate) fn collect_type_hits(
    packages: &[std::sync::Arc<crate::store::package::PackageView>],
    query: &SearchQuery,
) -> Vec<HitRow> {
    if let Some(type_query) = crate::typequery::TypeQuery::parse(&query.text) {
        return collect_signature_hits(packages, query, &type_query);
    }
    collect_kind_facet_hits(packages, query)
}

/// Declarations whose signature satisfies **every** facet of `type_query`.
///
/// # Shape of the walk
///
/// Postings are per-package: `PackageIndexes::type_refs` maps a target
/// `StableRef` to owners *in that same package*. So the intersection is taken
/// per package, and a package contributes nothing unless it satisfies the whole
/// conjunction on its own — which is correct, because a declaration lives in
/// exactly one package and it is declarations that are being selected.
///
/// A type *name* may resolve to several declarations (two packages both
/// declaring `Error`), and those are a union: `return:Error` means "returns
/// anything called `Error`", because the reader typed a name and names are what
/// they have. The resolved set is computed once over the whole corpus rather
/// than per package, so a package that merely *uses* a type declared elsewhere
/// still matches.
fn collect_signature_hits(
    packages: &[std::sync::Arc<crate::store::package::PackageView>],
    query: &SearchQuery,
    type_query: &crate::typequery::TypeQuery,
) -> Vec<HitRow> {
    use nudox_ir::change::StableRef;

    let limit = if query.limit == 0 {
        usize::MAX
    } else {
        query.limit
    };

    // ── Resolve every facet's type name to declarations, corpus-wide ────────
    //
    // Resolution deliberately ignores `query.packages`: that filter restricts
    // which packages may *answer*, not which may *declare the type asked
    // about*. Filtering here as well would make `packages: [memchr]` +
    // `param:Path` silently mean "a `Path` declared inside memchr", which is
    // never what the reader meant.
    let mut resolved: Vec<Vec<StableRef>> = Vec::with_capacity(type_query.facets.len());
    for facet in &type_query.facets {
        let mut targets: Vec<StableRef> = Vec::new();
        for pkg in packages {
            for entry in pkg.indexes().by_name.get_exact(&facet.type_name) {
                targets.push(StableRef::new(pkg.lineage().clone(), entry.intro));
            }
        }
        // A facet naming a type no loaded package declares can never be
        // satisfied, so the whole conjunction is empty. Returning early keeps
        // that a cheap answer rather than an empty intersection computed the
        // long way — and it is the common case for foreign types (see the
        // module docs on what the index cannot see).
        if targets.is_empty() {
            return Vec::new();
        }
        targets.sort_unstable();
        targets.dedup();
        resolved.push(targets);
    }

    let mut candidates: Vec<Candidate> = Vec::new();

    for pkg in packages {
        if !query.package_matches(pkg.lineage()) {
            continue;
        }
        let indexes = pkg.indexes();

        // ── Intersect the facets inside this package ─────────────────────────
        let mut owners: Option<std::collections::BTreeSet<nudox_ir::change::IntroId>> = None;
        for (facet, targets) in type_query.facets.iter().zip(resolved.iter()) {
            let mut this_facet: std::collections::BTreeSet<nudox_ir::change::IntroId> =
                std::collections::BTreeSet::new();
            for target in targets {
                for &position in facet.positions {
                    this_facet.extend(indexes.type_refs_in(target, position));
                }
            }
            owners = Some(match owners {
                None => this_facet,
                Some(previous) => previous.intersection(&this_facet).copied().collect(),
            });
            // Nothing left to intersect with — stop probing this package.
            if owners.as_ref().is_some_and(|o| o.is_empty()) {
                break;
            }
        }
        let Some(owners) = owners else {
            continue;
        };

        let provenance: Provenance = pkg.provenance().into();
        let pkg_name = pkg.lineage().name.as_str();

        for intro in owners {
            let Some(ir_entry) = pkg.view().entry(intro) else {
                continue;
            };
            let Some(disc) = ir_entry.kind().discriminant() else {
                continue;
            };
            // The kind filter still applies. `kinds: [Function]` +
            // `param:Path` is a legitimate narrowing, and dropping it here
            // would make the filter mean different things in section 0 and 1.
            if !query.kind_matches(disc) {
                continue;
            }

            let key = nudox_ir::change::StableRef::new(pkg.lineage().clone(), intro);
            let sig_preview = signature::tokens(ir_entry, pkg);
            let leaf_str = ir_entry.sym().name.as_str();
            let leaf: SharedStr = SharedStr::from(leaf_str);
            let qualified = qualified_display_name(indexes, intro, leaf_str, pkg_name);

            candidates.push(Candidate {
                row: HitRow {
                    key,
                    display_name: leaf,
                    sig_preview,
                    kind: KindTag::Known(disc),
                    provenance: provenance.clone(),
                    // A signature hit is an *exact structural* match: the
                    // declaration really does name that type in that position.
                    // So it takes the same 1.0 reference relevance an exact
                    // name match takes, discounted by the same two quality
                    // factors. There is nothing to tune here — every row in the
                    // section satisfies every facet, so relevance cannot
                    // separate them and visibility/kind are what remain.
                    score: score_of(1.0, ir_entry.sym().visibility, disc),
                },
                qualified,
            });
        }
    }

    candidates.sort_by(compare_candidates);
    candidates.truncate(limit);
    finalize_candidates(candidates)
}

/// Collect hits from the kind facet index.
///
/// If the query text matches a kind label (e.g. `"fn"` → `Function`) we emit
/// all symbols of that kind. Otherwise we use the kind filter from the query.
fn collect_kind_facet_hits(
    packages: &[std::sync::Arc<crate::store::package::PackageView>],
    query: &SearchQuery,
) -> Vec<HitRow> {
    let limit = if query.limit == 0 {
        usize::MAX
    } else {
        query.limit
    };

    // Determine which kind discriminant(s) to emit for the "type" section.
    // Priority:
    //   1. If the query text matches a kind keyword, use that kind.
    //   2. If an explicit kind filter is set, use that filter.
    //   3. Otherwise, emit nothing (name section already covers everything).
    let target_kinds: Vec<KindDiscriminant> = if !query.kinds.is_empty() {
        query.kinds.clone()
    } else {
        let lower = query.text.to_lowercase();
        keyword_to_kinds(&lower)
    };

    if target_kinds.is_empty() {
        return Vec::new();
    }

    let mut candidates: Vec<Candidate> = Vec::new();

    for pkg in packages {
        if candidates.len() >= limit {
            break;
        }
        if !query.package_matches(pkg.lineage()) {
            continue;
        }
        let indexes = pkg.indexes();
        let provenance: Provenance = pkg.provenance().into();
        let pkg_name = pkg.lineage().name.as_str();

        for &disc in &target_kinds {
            let Some(intros) = indexes.by_kind.get(&disc) else {
                continue;
            };
            for &intro in intros.iter() {
                if candidates.len() >= limit {
                    break;
                }
                let Some(ir_entry) = pkg.view().entry(intro) else {
                    continue;
                };
                let key = nudox_ir::change::StableRef::new(pkg.lineage().clone(), intro);
                let sig_preview = signature::tokens(ir_entry, pkg);
                let leaf_str = ir_entry.sym().name.as_str();
                let leaf: SharedStr = SharedStr::from(leaf_str);
                let qualified = qualified_display_name(indexes, intro, leaf_str, pkg_name);

                candidates.push(Candidate {
                    row: HitRow {
                        key,
                        display_name: leaf,
                        sig_preview,
                        kind: KindTag::Known(disc),
                        provenance: provenance.clone(),
                        // Kind-facet, not text-relevance: a fixed base relevance,
                        // weighted by the same quality factors the name section
                        // uses so that a public API item outranks a private one
                        // inside the facet too.
                        score: score_of(TYPE_FACET_RELEVANCE, ir_entry.sym().visibility, disc),
                    },
                    qualified,
                });
            }
        }
    }

    // The same total order the name section uses. This section used to ship
    // whatever order the package walk produced, which is how corpus-map hash
    // order reached the screen even for queries that never touched `by_name`.
    candidates.sort_by(compare_candidates);
    candidates.truncate(limit);
    finalize_candidates(candidates)
}

/// Map common kind-keyword strings to one or more [`KindDiscriminant`]s.
fn keyword_to_kinds(lower: &str) -> Vec<KindDiscriminant> {
    match lower {
        "fn" | "func" | "function" => vec![KindDiscriminant::Function],
        "struct" | "record" => vec![KindDiscriminant::Record],
        "trait" => vec![KindDiscriminant::Trait],
        "impl" => vec![KindDiscriminant::Impl],
        "enum" => vec![KindDiscriminant::Enum],
        "mod" | "module" => vec![KindDiscriminant::Module],
        "const" => vec![KindDiscriminant::Const],
        "static" => vec![KindDiscriminant::Static],
        "type" | "alias" => vec![KindDiscriminant::Alias],
        "field" => vec![KindDiscriminant::Field],
        "variant" => vec![KindDiscriminant::Variant],
        _ => Vec::new(),
    }
}

