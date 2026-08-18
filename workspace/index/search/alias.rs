//! Query-time alias expansion for the registryless `cpp` ecosystem
//! (REGISTRYLESS-PLAN §9, P8).
//!
//! Users of C/C++ libraries type the names they know — `zlib`, `OpenSSL`,
//! `Threads` — not the repository slug (`github.com/madler/zlib`) or the
//! `system/pthread` model stem that the catalog actually keys on. This module is
//! the query-normalization hook (ECOSYSTEM-PLAN §8.4 pattern) that closes that
//! gap: a **bare-token** query in `cpp` scope consults `package_aliases` and, on
//! a hit, rewrites the free terms to the canonical stem *before* the tantivy
//! query is built. This is where "users never type slugs" is honored.
//!
//! # Scope discipline (adversarial)
//! - Expansion fires **only** for `cpp`-scoped queries. A `zlib` query in `rust`
//!   scope, or an unscoped query, passes through untouched.
//! - Expansion fires **only** for a *bare token* — a single term with no
//!   ecosystem separators (`-`, `_`, `/`, `.`). A slug-shaped query
//!   (`github.com/madler/zlib`) is already canonical and is left alone; a
//!   multi-word query (`fast compression`) is a description search, not a name.
//! - An empty alias table (or a miss) is a **pass-through, never an error**.
//!
//! # Where the real lookup lives
//! The pipeline (in the `registry` crate) has no catalog handle — the catalog is
//! the `index` crate, which `registry` does not depend on. So this module owns
//! only the pure [`AliasExpander`] *interface* and the pure rewrite logic; the
//! catalog-backed implementation is provided by the server (which holds both the
//! `MetaStore` and the search pipeline) and injected via
//! [`super::pipeline::PackageSearchDeps::alias_expander`].

use crate::ecosystem::Language;

/// The confidence tier of an alias, mirrored from the catalog vocabulary so the
/// expander can order candidates authoritative > curated > heuristic without a
/// dependency on the `index` crate's enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AliasConfidence {
    /// Derived heuristically (homepage sniff); lowest trust.
    Heuristic,
    /// A curated seed / human mapping.
    Curated,
    /// The feed itself declared the upstream repo; highest trust.
    Authoritative,
}

/// One resolved alias: the canonical stem name a token expands to, and how much
/// we trust it. The name is the search-index key (`packages.name_canonical`),
/// e.g. `github.com/madler/zlib` or `system/pthread`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAlias {
    /// The canonical stem name to search for.
    pub stem_name: String,
    /// The confidence of the mapping (for the caller's authoritative ordering).
    pub confidence: AliasConfidence,
}

/// The pure interface the pipeline uses to resolve a bare token to its canonical
/// stem name. Implemented by a catalog-backed adapter in the server; the
/// implementor is responsible for returning the **highest-confidence** match
/// (authoritative > curated > heuristic) when several alias kinds map the token.
pub trait AliasExpander: Send + Sync {
    /// Resolve `token` (already lowercased) in `ecosystem` to a canonical stem
    /// name, or `None` when no alias matches. Must be pure w.r.t. the query (a
    /// read of `package_aliases`), and must never error into the query path — a
    /// miss is `None`, honored as pass-through.
    fn resolve(&self, ecosystem: Language, token: &str) -> Option<ResolvedAlias>;
}

/// Whether `terms` is a single bare token eligible for cpp alias expansion.
///
/// A bare token has exactly one whitespace-delimited word and contains none of
/// the ecosystem specificity separators (`-`, `_`, `/`, `.`), so a slug or a
/// hyphenated/qualified name is *not* bare and passes through.
pub fn is_bare_token(terms: &str) -> bool {
    let trimmed = terms.trim();
    !trimmed.is_empty()
        && !trimmed.contains(char::is_whitespace)
        && !trimmed.contains(['-', '_', '/', '.'])
}

/// Rewrite the free terms of a `cpp`-scoped bare-token query to the canonical
/// stem name when an alias resolves it (P8).
///
/// Returns `true` when an expansion was applied (`terms` was rewritten), `false`
/// on any pass-through (non-cpp scope, non-bare token, or alias miss). The
/// rewrite is confined to the `terms` string; namespace/deps/license are
/// untouched.
pub fn expand_cpp_bare_token(
    ecosystem: Option<Language>,
    terms: &mut String,
    expander: &dyn AliasExpander,
) -> bool {
    // Non-cpp scopes (and unscoped queries) are never expanded.
    if ecosystem != Some(Language::Cpp) {
        return false;
    }
    if !is_bare_token(terms) {
        return false;
    }
    let token = terms.trim().to_ascii_lowercase();
    match expander.resolve(Language::Cpp, &token) {
        Some(resolved) => {
            *terms = resolved.stem_name;
            true
        }
        None => false,
    }
}

/// The derived `presence` facet (REGISTRYLESS-PLAN §9, RL-12).
///
/// C/C++ stems have no download counts; **presence** — the number of *distinct*
/// feed alias-kinds that reference a stem (a vcpkg port *and* a Homebrew formula
/// *and* a Conan recipe → presence 3) — is the honest popularity prior instead.
/// This is a pure fold over the `alias_kind` tokens of the alias rows pointing at
/// one stem; the facet pipeline computes it per stem and feeds it to ranking as a
/// small downloads substitute.
///
/// `alias_kinds` is every `alias_kind` token from `package_aliases` rows whose
/// `stem_id` is this stem; the result is the count of *distinct* tokens.
pub fn presence_facet<'a>(alias_kinds: impl IntoIterator<Item = &'a str>) -> u32 {
    let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for kind in alias_kinds {
        seen.insert(kind);
    }
    seen.len() as u32
}

#[cfg(test)]
pub(crate) mod test_support {
    //! A pure, in-memory [`AliasExpander`] for tests (no catalog).
    use super::*;
    use std::collections::HashMap;

    /// A fixed-table expander: `(ecosystem, token) → ResolvedAlias`.
    #[derive(Default)]
    pub struct MapExpander {
        table: HashMap<(Language, String), ResolvedAlias>,
    }

    impl MapExpander {
        /// Insert one `(token → stem_name)` mapping at a confidence tier.
        pub fn with(
            mut self,
            ecosystem: Language,
            token: &str,
            stem_name: &str,
            confidence: AliasConfidence,
        ) -> Self {
            self.table.insert(
                (ecosystem, token.to_ascii_lowercase()),
                ResolvedAlias {
                    stem_name: stem_name.to_owned(),
                    confidence,
                },
            );
            self
        }
    }

    impl AliasExpander for MapExpander {
        fn resolve(&self, ecosystem: Language, token: &str) -> Option<ResolvedAlias> {
            self.table.get(&(ecosystem, token.to_owned())).cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::MapExpander;
    use super::*;

    fn zlib_expander() -> MapExpander {
        MapExpander::default().with(
            Language::Cpp,
            "zlib",
            "github.com/madler/zlib",
            AliasConfidence::Curated,
        )
    }

    #[test]
    fn bare_cpp_token_expands_to_stem() {
        let mut terms = "zlib".to_owned();
        let expanded = expand_cpp_bare_token(Some(Language::Cpp), &mut terms, &zlib_expander());
        assert!(expanded, "a known cpp bare token must expand");
        assert_eq!(terms, "github.com/madler/zlib");
    }

    #[test]
    fn zlib_in_rust_scope_is_untouched() {
        let mut terms = "zlib".to_owned();
        let expanded = expand_cpp_bare_token(Some(Language::Rust), &mut terms, &zlib_expander());
        assert!(!expanded, "non-cpp scope must never expand");
        assert_eq!(
            terms, "zlib",
            "the rust-scoped query is passed through verbatim"
        );
    }

    #[test]
    fn unscoped_query_is_untouched() {
        let mut terms = "zlib".to_owned();
        let expanded = expand_cpp_bare_token(None, &mut terms, &zlib_expander());
        assert!(!expanded);
        assert_eq!(terms, "zlib");
    }

    #[test]
    fn slug_shaped_query_passes_through() {
        // Already-canonical slug: not a bare token, so no expansion.
        let mut terms = "github.com/madler/zlib".to_owned();
        let expanded = expand_cpp_bare_token(Some(Language::Cpp), &mut terms, &zlib_expander());
        assert!(!expanded, "a slug is not a bare token");
        assert_eq!(terms, "github.com/madler/zlib");
    }

    #[test]
    fn multi_word_query_passes_through() {
        let mut terms = "fast compression".to_owned();
        let expanded = expand_cpp_bare_token(Some(Language::Cpp), &mut terms, &zlib_expander());
        assert!(!expanded, "a description search is not a bare token");
        assert_eq!(terms, "fast compression");
    }

    #[test]
    fn empty_alias_table_is_passthrough_not_error() {
        let mut terms = "zlib".to_owned();
        let empty = MapExpander::default();
        let expanded = expand_cpp_bare_token(Some(Language::Cpp), &mut terms, &empty);
        assert!(!expanded, "a miss on an empty table is a pass-through");
        assert_eq!(terms, "zlib");
    }

    #[test]
    fn expansion_is_case_insensitive_on_the_token() {
        let mut terms = "ZLIB".to_owned();
        let expanded = expand_cpp_bare_token(Some(Language::Cpp), &mut terms, &zlib_expander());
        assert!(expanded, "token casing must not defeat the lookup");
        assert_eq!(terms, "github.com/madler/zlib");
    }

    #[test]
    fn presence_counts_distinct_feed_kinds() {
        // Three distinct feed kinds → presence 3; a duplicate kind does not
        // double-count.
        let kinds = ["vcpkg_port", "brew_formula", "conan_recipe", "vcpkg_port"];
        assert_eq!(presence_facet(kinds.iter().copied()), 3);
        // No aliases → presence 0 (an unseen stem).
        assert_eq!(presence_facet(std::iter::empty()), 0);
    }

    #[test]
    fn hyphenated_token_is_not_bare() {
        assert!(!is_bare_token("react-query"));
        assert!(!is_bare_token("boost.asio"));
        assert!(!is_bare_token("a b"));
        assert!(is_bare_token("zlib"));
        assert!(is_bare_token("OpenSSL"));
    }
}
