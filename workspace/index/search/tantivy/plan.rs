//! Retrieval plan for a [`StructuredQuery`].
//!
//! The plan names *what* must match. Boosts and tantivy field handles live in
//! the compiler, so a change to scoring does not reshape this value and a
//! change to parsing does not touch the index.

use crate::{runtime::text::tokenizer, search::StructuredQuery};

/// One structured query, as clauses the compiler can lower.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PackageQueryPlan {
    pub(crate) text: TextMatch,
    pub(crate) filters: Vec<Filter>,
}

/// Free-text half of the plan.
///
/// Empty free text matches every document; filters still apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextMatch {
    All,
    Scored(Vec<TextClause>),
}

/// A should-clause inside the free-text block. At least one must match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextClause {
    /// Whole query, lowercased, against `name_exact`.
    ExactName(String),
    /// Every token must occur in `field`.
    AllTokens {
        field: ProseField,
        tokens: Vec<String>,
    },
    /// Edit-distance 1 against one name field. Only emitted for a single
    /// token whose lowercased form is 4..=12 bytes.
    Fuzzy { field: FuzzyField, text: String },
    /// Any expanded synonym hits `keywords`.
    AnyKeyword(Vec<String>),
}

/// Text fields that take a must-conjunction of identifier tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProseField {
    NameTokens,
    NameNamespace,
    Description,
    Keywords,
}

/// Fields a one-token typo may fuzzy-match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FuzzyField {
    NameExact,
    NameTokens,
}

/// A must-filter. Independent of the free-text score.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Filter {
    Ecosystem(String),
    Namespace(Vec<String>),
    DependsOn(String),
    License(String),
    /// Phrase tokens, length ≥ 2. Must occur in description or keywords.
    Phrase(Vec<String>),
}

const PROSE_FIELDS: [ProseField; 4] = [
    ProseField::NameTokens,
    ProseField::NameNamespace,
    ProseField::Description,
    ProseField::Keywords,
];

/// Lower the structured query into a plan. Pure: no index, no boosts.
pub(crate) fn plan(structured: &StructuredQuery) -> PackageQueryPlan {
    let terms_lower = structured.terms.to_ascii_lowercase();
    let filtered_subtokens: Vec<String> = tokenizer::subtokens(&structured.terms)
        .into_iter()
        .filter(|token| token.len() >= 2)
        .collect();

    let text = if structured.terms.is_empty() {
        TextMatch::All
    } else {
        let mut clauses = Vec::new();
        clauses.push(TextClause::ExactName(terms_lower.clone()));
        if !filtered_subtokens.is_empty() {
            for field in PROSE_FIELDS {
                clauses.push(TextClause::AllTokens {
                    field,
                    tokens: filtered_subtokens.clone(),
                });
            }
        }
        let single_token = structured.terms.split_whitespace().count() == 1;
        if single_token && (4..=12).contains(&terms_lower.len()) {
            for field in [FuzzyField::NameExact, FuzzyField::NameTokens] {
                clauses.push(TextClause::Fuzzy {
                    field,
                    text: terms_lower.clone(),
                });
            }
        }
        if !structured.expanded_terms.is_empty() {
            clauses.push(TextClause::AnyKeyword(structured.expanded_terms.clone()));
        }
        TextMatch::Scored(clauses)
    };

    let mut filters = Vec::new();
    if let Some(ecosystem) = structured.ecosystem {
        filters.push(Filter::Ecosystem(ecosystem.as_token().to_owned()));
    }
    if let Some(namespace) = &structured.namespace {
        let namespace_subtokens: Vec<String> = tokenizer::subtokens(namespace)
            .into_iter()
            .filter(|token| token.len() >= 2)
            .collect();
        if !namespace_subtokens.is_empty() {
            filters.push(Filter::Namespace(namespace_subtokens));
        }
    }
    for dependency_name in &structured.deps {
        filters.push(Filter::DependsOn(dependency_name.clone()));
    }
    if let Some(license) = &structured.license {
        filters.push(Filter::License(license.clone()));
    }
    for phrase in &structured.phrases {
        let phrase_subtokens = tokenizer::subtokens(phrase);
        if phrase_subtokens.len() >= 2 {
            filters.push(Filter::Phrase(phrase_subtokens));
        }
    }

    PackageQueryPlan { text, filters }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::structured::StructuredQuery;

    fn kinds(plan: &PackageQueryPlan) -> Vec<&'static str> {
        let TextMatch::Scored(clauses) = &plan.text else {
            return vec!["all"];
        };
        clauses
            .iter()
            .map(|clause| match clause {
                TextClause::ExactName(_) => "exact",
                TextClause::AllTokens { field, .. } => match field {
                    ProseField::NameTokens => "tokens",
                    ProseField::NameNamespace => "ns",
                    ProseField::Description => "desc",
                    ProseField::Keywords => "kw",
                },
                TextClause::Fuzzy { field, .. } => match field {
                    FuzzyField::NameExact => "fuzzy-exact",
                    FuzzyField::NameTokens => "fuzzy-tokens",
                },
                TextClause::AnyKeyword(_) => "syn",
            })
            .collect()
    }

    #[test]
    fn empty_terms_match_everything_and_keep_filters() {
        let structured = StructuredQuery::parse("dep:tokio license:mit", None);
        let planned = plan(&structured);
        assert!(matches!(planned.text, TextMatch::All));
        assert!(planned.filters.iter().any(|filter| matches!(
            filter,
            Filter::DependsOn(name) if name == "tokio"
        )));
        assert!(
            planned
                .filters
                .iter()
                .any(|filter| matches!(filter, Filter::License(name) if name == "mit"))
        );
    }

    #[test]
    fn fuzzy_only_for_one_token_of_four_to_twelve_bytes() {
        let short = plan(&StructuredQuery::parse("ab", None));
        let fit = plan(&StructuredQuery::parse("serde", None));
        let long = plan(&StructuredQuery::parse("abcdefghijklm", None));
        let two = plan(&StructuredQuery::parse("serde json", None));
        for planned in [&short, &long, &two] {
            let TextMatch::Scored(clauses) = &planned.text else {
                panic!("free text must score");
            };
            assert!(
                clauses
                    .iter()
                    .all(|clause| !matches!(clause, TextClause::Fuzzy { .. })),
                "{clauses:?}"
            );
        }
        assert_eq!(kinds(&fit), [
            "exact",
            "tokens",
            "ns",
            "desc",
            "kw",
            "fuzzy-exact",
            "fuzzy-tokens"
        ]);
    }

    #[test]
    fn planning_is_deterministic() {
        let queries = [
            "",
            "serde",
            "dep:tokio lang:rust",
            "\"http client\" scope:org",
            "ab",
            "two words",
        ];
        for query in queries {
            let structured = StructuredQuery::parse(query, None);
            assert_eq!(plan(&structured), plan(&structured), "{query}");
        }
    }
}
