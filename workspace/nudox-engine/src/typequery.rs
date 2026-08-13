//! The signature-query language behind `SECTION_TYPE`.
//!
//! # What section 1 used to be, and why that was not type search
//!
//! `collect_type_hits` resolved a *kind discriminant* — from a keyword like
//! `"struct"` or from `SearchQuery::kinds` — and returned every symbol of that
//! kind at a fixed relevance. Its own comment conceded the point: "Kind-facet,
//! not text-relevance". It emitted nothing at all unless a kind was present.
//! That is a filter wearing a search's label. It cannot answer any question
//! about a *signature*, which is the only thing "type search" can reasonably
//! mean.
//!
//! # The language: structured facets, not free text
//!
//! The decision, and the reason:
//!
//! ```text
//! return:Result      functions whose return type mentions `Result`
//! param:Path         functions with a parameter mentioning `Path`
//! impl:Display       declarations implementing `Display`
//! field:Vec          records with a field holding a `Vec`
//! throws:IOException methods declaring `IOException`
//! ```
//!
//! Free-text-to-signature ("anything implementing Display whose method takes
//! `&mut self`") is a research project: it needs a parser for seven languages'
//! type grammars, a notion of subtyping to make `&mut self` a *constraint*
//! rather than a token, and a relevance model over structural distance. None of
//! those exist here, and shipping a natural-language box that silently degrades
//! to keyword matching is the affordance-without-engine failure LIMITATIONS.md
//! L41 was filed about.
//!
//! A facet grammar is the opposite trade: it is smaller than what a user might
//! want, and it is *exactly* what it appears to be. Every facet maps to one
//! [`TypePosition`] that the store already indexes, so there is no query this
//! language can express that the index cannot answer, and none it answers
//! approximately.
//!
//! # Conjunction is how compound types are asked for
//!
//! There is deliberately no syntax for `Result<T, io::Error>` as a single
//! value. The store indexes non-relational positions at `Reach::Whole` — every
//! nominal named anywhere in the type expression — so a function returning
//! `Result<T, io::Error>` has postings for *both* `Result` and `Error` in
//! [`TypePosition::Return`]. `return:Result return:Error` therefore selects
//! exactly it, using the index's own semantics rather than a second type
//! grammar layered over them.
//!
//! # What this cannot see, stated here because it is not obvious
//!
//! A facet resolves a type *name* to a declaration, and the index keys on
//! declarations ([`StableRef`]), not on spellings. So a type whose declaration
//! is not in the corpus has no postings at all: `return:Result` finds nothing
//! in a Rust package unless `core` is also loaded, because `Result` is foreign
//! and unlinked. This is a real limit of the index, it is reported honestly
//! (zero rows, not wrong rows), and it is the reason the relevance suite is
//! built over types the corpus actually declares.

use nudox_store::package::TypePosition;

/// One `facet:Type` term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeFacet {
    /// The positions this facet selects. More than one where a single English
    /// word means different structures in different languages — `impl:` is
    /// `impl Trait for T` in Rust and `class C implements I` in Java/C#/Go, and
    /// a user who types `impl:Serialize` means both.
    pub positions: &'static [TypePosition],
    /// The leaf name of the type being asked about, case-folded.
    pub type_name: String,
    /// The facet keyword exactly as the user wrote it, for error messages and
    /// for echoing the parse back in the UI.
    pub keyword: &'static str,
}

/// A parsed signature query: a conjunction of facets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeQuery {
    /// All facets, in the order written. A declaration must satisfy **every**
    /// one to be a hit.
    pub facets: Vec<TypeFacet>,
}

/// Every position a type can be written in, for the `type:`/`sig:` wildcard.
const ANY_POSITION: &[TypePosition] = &[
    TypePosition::ImplementedTrait,
    TypePosition::ImplSelf,
    TypePosition::Supertrait,
    TypePosition::SuperType,
    TypePosition::FieldType,
    TypePosition::Parameter,
    TypePosition::Return,
    TypePosition::Throws,
    TypePosition::AliasTarget,
    TypePosition::ValueType,
];

/// The facet vocabulary.
///
/// A table rather than a `match` so that the *same* list is what parses a query
/// and what the UI offers as completions — a vocabulary that exists twice
/// drifts, and the half users can discover is the half that matters.
///
/// Aliases are generous on purpose: the cost of accepting `returns:` beside
/// `return:` is one row here, and the cost of rejecting it is a user who
/// concludes the feature does not work.
pub const FACETS: &[(&str, &[TypePosition])] = &[
    ("return", &[TypePosition::Return]),
    ("returns", &[TypePosition::Return]),
    ("param", &[TypePosition::Parameter]),
    ("arg", &[TypePosition::Parameter]),
    ("accepts", &[TypePosition::Parameter]),
    ("takes", &[TypePosition::Parameter]),
    ("field", &[TypePosition::FieldType]),
    ("holds", &[TypePosition::FieldType]),
    // `impl` and `extends` both select the two relational positions that mean
    // "this declaration is a kind of that one", because which of the two a
    // language uses is a property of the language, not of the question.
    (
        "impl",
        &[TypePosition::ImplementedTrait, TypePosition::SuperType],
    ),
    (
        "implements",
        &[TypePosition::ImplementedTrait, TypePosition::SuperType],
    ),
    (
        "extends",
        &[TypePosition::Supertrait, TypePosition::SuperType],
    ),
    ("super", &[TypePosition::Supertrait, TypePosition::SuperType]),
    ("for", &[TypePosition::ImplSelf]),
    ("self", &[TypePosition::ImplSelf]),
    ("throws", &[TypePosition::Throws]),
    ("alias", &[TypePosition::AliasTarget]),
    ("value", &[TypePosition::ValueType]),
    ("type", ANY_POSITION),
    ("sig", ANY_POSITION),
];

impl TypeQuery {
    /// Parse `text` into a signature query, or `None` if it contains no facet.
    ///
    /// Returning `None` rather than an empty query is what lets
    /// `collect_type_hits` keep its old kind-keyword behaviour for every query
    /// that is not a signature query — the two are different questions and the
    /// section answers whichever was asked.
    ///
    /// Unknown `word:value` tokens are **ignored, not errors**. A user typing
    /// `http:` mid-thought is searching, not making a mistake, and failing the
    /// whole query for one unrecognised prefix would make the box feel broken
    /// while it is being typed.
    /// A facet's value runs to the **next facet keyword**, not to the next
    /// space.
    ///
    /// Splitting on whitespace was the obvious implementation and it was wrong
    /// for the majority of real type spellings: `param:&mut Path`,
    /// `*const Path` and `return:Result<T, io::Error>` all contain spaces, and
    /// a whitespace tokeniser reduces the first two to the bare word `mut` /
    /// `const` and the third to `Result<T,`. Worse, it fails *quietly* — the
    /// fragment still parses, still resolves, and returns rows for whatever
    /// declaration happens to be named `mut`.
    pub fn parse(text: &str) -> Option<Self> {
        // Each element is (canonical keyword, positions, value fragments).
        let mut pending: Vec<(&'static str, &'static [TypePosition], Vec<&str>)> = Vec::new();

        for token in text.split_whitespace() {
            match token.split_once(':').and_then(|(keyword, rest)| {
                let lower = keyword.to_lowercase();
                FACETS
                    .iter()
                    .find(|(k, _)| *k == lower)
                    .map(|(k, p)| (*k, *p, rest))
            }) {
                // A new facet starts here; everything after it belongs to it
                // until the next one.
                Some((canonical, positions, rest)) => {
                    pending.push((canonical, positions, if rest.is_empty() {
                        Vec::new()
                    } else {
                        vec![rest]
                    }));
                }
                // Continuation of the facet in progress. A token before any
                // facet keyword — free text like `Point` in `Point param:Path`
                // — belongs to the *name* section and is dropped here.
                None => {
                    if let Some((_, _, fragments)) = pending.last_mut() {
                        fragments.push(token);
                    }
                }
            }
        }

        let facets: Vec<TypeFacet> = pending
            .into_iter()
            .filter_map(|(keyword, positions, fragments)| {
                let joined = fragments.join(" ");
                normalize_type_name(&joined).map(|type_name| TypeFacet {
                    positions,
                    type_name,
                    keyword,
                })
            })
            .collect();

        if facets.is_empty() {
            None
        } else {
            Some(Self { facets })
        }
    }
}

/// Reduce a user-written type expression to the leaf name the index keys on.
///
/// The index stores *declarations*, so `&mut std::path::Path`, `std::path::Path`
/// and `Path` all have to arrive at the same lookup. This strips, in order:
/// reference and pointer sigils, generic argument lists, array/slice brackets,
/// and every path qualifier.
///
/// Returns `None` for anything that reduces to nothing (`&`, `<>`, `::`), which
/// is what a half-typed query looks like.
fn normalize_type_name(raw: &str) -> Option<String> {
    let mut s = raw.trim();

    // Reference and pointer sigils, in any order or repetition: `&&mut`, `*const`.
    loop {
        let trimmed = s
            .trim_start_matches('&')
            .trim_start_matches('*')
            .trim_start();
        let trimmed = trimmed
            .strip_prefix("mut ")
            .or_else(|| trimmed.strip_prefix("const "))
            .unwrap_or(trimmed)
            .trim_start();
        // Bare `mut`/`const` with no trailing space (end of token).
        let trimmed = if trimmed == "mut" || trimmed == "const" {
            ""
        } else {
            trimmed
        };
        if trimmed == s {
            break;
        }
        s = trimmed;
    }

    // A Rust lifetime argument, which is not a type and never has a posting:
    // `&'a Path` tokenises to `&'a` + `Path` on the whitespace split above, so
    // what reaches here is `&'a`. Stripping it leaves nothing, and nothing is
    // the right answer — the facet named no type, so it is dropped rather than
    // resolving to a declaration called `'a`.
    if let Some(rest) = s.strip_prefix('\'') {
        s = rest
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .map_or("", |at| &rest[at..])
            .trim_start();
    }

    // Slice / array brackets: `[u8]`, `[u8; 4]`.
    s = s.trim_start_matches('[').trim_end_matches(']');
    if let Some((head, _)) = s.split_once(';') {
        s = head;
    }

    // Generic arguments: keep the head. `Vec<Point>` → `Vec`.
    if let Some((head, _)) = s.split_once('<') {
        s = head;
    }
    // A trailing `>` from an inner fragment the user pasted.
    s = s.trim_end_matches('>');

    // Path qualifiers, in all the spellings the seven producers use.
    //
    // A `char` set rather than a string set: Rust's `::` and C++'s `::` both
    // end in `:`, so splitting on the single characters reaches the same leaf
    // and needs no multi-character pattern (which `str::rsplit` does not accept
    // as a slice anyway).
    if let Some(last) = s.rsplit([':', '.', '/', '\\']).next() {
        s = last;
    }

    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three shapes from the brief parse to the positions they name.
    #[test]
    fn the_documented_facets_select_the_positions_they_name() {
        let q = TypeQuery::parse("return:Result").expect("a facet query must parse");
        assert_eq!(q.facets.len(), 1);
        assert_eq!(q.facets[0].positions, &[TypePosition::Return]);
        assert_eq!(q.facets[0].type_name, "result");

        let q = TypeQuery::parse("param:&Path").expect("a facet query must parse");
        assert_eq!(q.facets[0].positions, &[TypePosition::Parameter]);
        assert_eq!(
            q.facets[0].type_name, "path",
            "the `&` sigil must not reach the index lookup"
        );

        let q = TypeQuery::parse("impl:Display").expect("a facet query must parse");
        assert_eq!(
            q.facets[0].positions,
            &[TypePosition::ImplementedTrait, TypePosition::SuperType],
            "`impl:` must mean both `impl Trait for T` and `class C implements I`, \
             or the same question gets different answers per language"
        );
    }

    /// A compound type is asked for by conjunction, and that is the documented
    /// route — this test is what makes the module docs' claim checkable.
    #[test]
    fn a_compound_return_type_is_expressed_as_a_conjunction() {
        let q = TypeQuery::parse("return:Result return:Error").expect("must parse");
        assert_eq!(q.facets.len(), 2);
        assert_eq!(q.facets[0].type_name, "result");
        assert_eq!(q.facets[1].type_name, "error");
        assert!(
            q.facets.iter().all(|f| f.positions == [TypePosition::Return]),
            "both terms constrain the return position"
        );
    }

    /// Every spelling of a qualified, borrowed, generic type reduces to the
    /// same lookup key.
    ///
    /// The table is the point: these are not variations on one input, they are
    /// the spellings that actually appear across the seven producers, and each
    /// one that failed to reduce would be a silently empty section.
    #[test]
    fn every_spelling_of_one_type_reduces_to_the_same_key() {
        for spelling in [
            "Path",
            "&Path",
            "&'a Path",
            "&mut Path",
            "&&Path",
            "*const Path",
            "*mut Path",
            "std::path::Path",
            "&std::path::Path",
            "java.nio.file.Path",
            "System.IO.Path",
        ] {
            let q = TypeQuery::parse(&format!("param:{spelling}"))
                .unwrap_or_else(|| panic!("{spelling:?} must parse"));
            assert_eq!(
                q.facets[0].type_name, "path",
                "{spelling:?} must reduce to the leaf name the index keys on"
            );
        }
    }

    /// Generic and container spellings keep the head, because the head is what
    /// the relational positions index.
    #[test]
    fn container_spellings_reduce_to_their_head() {
        for (spelling, expected) in [
            ("Vec<Point>", "vec"),
            ("Option<&str>", "option"),
            ("[u8]", "u8"),
            ("[u8; 4]", "u8"),
            ("HashMap<String, Vec<u8>>", "hashmap"),
        ] {
            let q = TypeQuery::parse(&format!("field:{spelling}"))
                .unwrap_or_else(|| panic!("{spelling:?} must parse"));
            assert_eq!(q.facets[0].type_name, expected, "for {spelling:?}");
        }
    }

    /// A query with no facet is not a signature query, and must not become an
    /// empty one — the two drive different code paths in `collect_type_hits`.
    #[test]
    fn a_query_without_a_facet_is_not_a_signature_query() {
        for text in ["Point", "struct", "", "   ", "http", "a:b c:d"] {
            // `a:b c:d` uses no known keyword, so it is also not one.
            assert_eq!(
                TypeQuery::parse(text),
                None,
                "{text:?} names no facet and must not parse as a signature query"
            );
        }
    }

    /// A half-typed facet does not produce a query that matches everything.
    ///
    /// This is the adversarial case that matters most: `return:` with no value
    /// must not resolve to "every type", which is what an empty `type_name`
    /// would do once it reached the index walk.
    #[test]
    fn a_facet_with_no_value_is_dropped_rather_than_matching_everything() {
        assert_eq!(TypeQuery::parse("return:"), None);
        assert_eq!(TypeQuery::parse("param:&"), None);
        assert_eq!(TypeQuery::parse("field:<>"), None);
        // A lifetime is not a type, and a facet naming only one names nothing.
        // Resolving it would look up a declaration called `'a`.
        assert_eq!(TypeQuery::parse("param:&'a"), None);
        assert_eq!(TypeQuery::parse("param:&'static"), None);
        assert_eq!(TypeQuery::parse("param:&mut"), None);
        // …and a valid facet beside a half-typed one keeps working.
        let q = TypeQuery::parse("return: param:Path").expect("must parse the valid half");
        assert_eq!(q.facets.len(), 1);
        assert_eq!(q.facets[0].type_name, "path");
    }

    /// A type spelling containing spaces stays with its facet.
    ///
    /// The regression that motivated the scan-to-next-keyword tokeniser: with a
    /// whitespace split, `param:&mut Path` reduced to the word `mut` and
    /// `return:Result<T, io::Error>` to `Result<T,` — both of which still
    /// *parsed*, still resolved, and returned rows for whatever happened to be
    /// named that. Silent wrongness, not an error.
    #[test]
    fn a_type_spelling_with_spaces_stays_with_its_facet() {
        for (query, expected) in [
            ("param:&mut Path", "path"),
            ("param:*const Path", "path"),
            ("param:&'a Path", "path"),
            ("return:Result<T, io::Error>", "result"),
            ("field:HashMap<String, Vec<u8>>", "hashmap"),
        ] {
            let q = TypeQuery::parse(query).unwrap_or_else(|| panic!("{query:?} must parse"));
            assert_eq!(q.facets.len(), 1, "for {query:?}");
            assert_eq!(q.facets[0].type_name, expected, "for {query:?}");
        }
    }

    /// Two facets separated by a multi-word value each keep their own value.
    #[test]
    fn a_following_facet_ends_the_previous_facets_value() {
        let q = TypeQuery::parse("param:&mut Path return:Result").expect("must parse");
        assert_eq!(q.facets.len(), 2);
        assert_eq!(q.facets[0].type_name, "path");
        assert_eq!(q.facets[0].positions, &[TypePosition::Parameter]);
        assert_eq!(q.facets[1].type_name, "result");
        assert_eq!(q.facets[1].positions, &[TypePosition::Return]);
    }

    /// Free text *before* any facet belongs to the name section, not to a facet.
    #[test]
    fn free_text_before_the_first_facet_is_not_swallowed_by_it() {
        let q = TypeQuery::parse("Point param:Path").expect("must parse");
        assert_eq!(q.facets.len(), 1);
        assert_eq!(
            q.facets[0].type_name, "path",
            "the bare word `Point` precedes every facet and must not become \
             part of one — section 0 is what answers it"
        );
    }

    /// Facet keywords are case-insensitive; type names are folded, not dropped.
    #[test]
    fn facet_keywords_are_case_insensitive() {
        let q = TypeQuery::parse("RETURN:Result").expect("must parse");
        assert_eq!(q.facets[0].keyword, "return");
        assert_eq!(q.facets[0].type_name, "result");
    }

    /// Every facet in the table parses and names at least one position.
    ///
    /// A universally-quantified invariant over the vocabulary: an entry added
    /// with an empty position list would silently match nothing, and the only
    /// symptom would be a completion that never works.
    #[test]
    fn every_advertised_facet_parses_and_selects_a_position() {
        for (keyword, positions) in FACETS {
            assert!(
                !positions.is_empty(),
                "facet {keyword:?} selects no position, so it can never match"
            );
            let q = TypeQuery::parse(&format!("{keyword}:Widget"))
                .unwrap_or_else(|| panic!("advertised facet {keyword:?} must parse"));
            assert_eq!(q.facets.len(), 1, "for {keyword:?}");
            assert_eq!(q.facets[0].positions, *positions, "for {keyword:?}");
            assert_eq!(q.facets[0].type_name, "widget", "for {keyword:?}");
        }
    }

    /// Unicode in a type name survives folding rather than being stripped.
    #[test]
    fn unicode_type_names_survive_normalisation() {
        let q = TypeQuery::parse("return:Größe").expect("must parse");
        assert_eq!(q.facets[0].type_name, "größe");
    }
}
