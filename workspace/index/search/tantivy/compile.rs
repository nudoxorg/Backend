//! Lower a [`super::plan::PackageQueryPlan`] into a tantivy `BooleanQuery`
//! and read the stored packages back.

use heart::PackageId;
use tantivy::{
    IndexReader, Term,
    collector::TopDocs,
    query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, PhraseQuery, TermQuery},
    schema::{Field, IndexRecordOption},
};

use crate::{GlobalPackage, error::SearchError, schema::codec};

use super::{
    FastRankingSignals,
    plan::{Filter, FuzzyField, PackageQueryPlan, ProseField, TextClause, TextMatch},
    schema::{Fields, PackageQueryBoosts},
};

pub(super) fn search(
    reader: &IndexReader,
    fields: &Fields,
    planned: &PackageQueryPlan,
    limit: usize,
) -> Result<Vec<(GlobalPackage, f32)>, SearchError> {
    let searcher = reader.searcher();
    let top_query = compile(fields, planned);
    let top_docs = searcher
        .search(&top_query, &TopDocs::with_limit(limit.max(1)))
        .map_err(SearchError::Tantivy)?;

    top_docs
        .into_iter()
        .map(|(score, address)| {
            let document: tantivy::TantivyDocument =
                searcher.doc(address).map_err(SearchError::Tantivy)?;
            let identifier = stored_text(&document, fields.package_id)?
                .parse::<uuid::Uuid>()
                .map_err(|_| SearchError::StoredIdNotUuid)?;
            let json = stored_text(&document, fields.record)?;
            let mut package: GlobalPackage =
                serde_json::from_str(&json).map_err(|error| SearchError::JsonDecode {
                    domain: "GlobalPackage",
                    source: error,
                })?;
            package.id = codec::package_id_from_uuid(identifier);
            Ok((package, score))
        })
        .collect()
}

pub(super) fn hydrate(
    reader: &IndexReader,
    fields: &Fields,
    identifiers: &[PackageId],
) -> Result<Vec<GlobalPackage>, SearchError> {
    let searcher = reader.searcher();
    let mut records = Vec::with_capacity(identifiers.len());
    for identifier in identifiers {
        let term = Term::from_field_text(fields.package_id, &identifier.to_string());
        let query = TermQuery::new(term, IndexRecordOption::Basic);
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(1))
            .map_err(SearchError::Tantivy)?;
        let Some((_, address)) = top_docs.into_iter().next() else {
            tracing::debug!(package = %identifier, "hydrate miss against replica index");
            continue;
        };
        let document: tantivy::TantivyDocument =
            searcher.doc(address).map_err(SearchError::Tantivy)?;
        let json = stored_text(&document, fields.record)?;
        records.push(
            serde_json::from_str(&json).map_err(|error| SearchError::JsonDecode {
                domain: "GlobalPackage",
                source: error,
            })?,
        );
    }
    Ok(records)
}

pub(super) fn fast_ranking_signals(
    reader: &IndexReader,
    fields: &Fields,
    identifier: PackageId,
) -> Result<Option<FastRankingSignals>, SearchError> {
    let searcher = reader.searcher();
    let term = Term::from_field_text(fields.package_id, &identifier.to_string());
    let query = TermQuery::new(term, IndexRecordOption::Basic);
    let top_docs = searcher
        .search(&query, &TopDocs::with_limit(1))
        .map_err(SearchError::Tantivy)?;
    let Some((_, address)) = top_docs.into_iter().next() else {
        return Ok(None);
    };
    let segment_reader = searcher.segment_reader(address.segment_ord);
    let fast_fields = segment_reader.fast_fields();
    let quality = fast_fields
        .u64("quality_ppm")
        .map_err(SearchError::Tantivy)?;
    let downloads = fast_fields.u64("downloads").map_err(SearchError::Tantivy)?;
    let popularity = fast_fields
        .u64("popularity_pct_ppm")
        .map_err(SearchError::Tantivy)?;
    Ok(Some(FastRankingSignals {
        quality_ppm: quality.first(address.doc_id).unwrap_or(0),
        downloads: downloads.first(address.doc_id).unwrap_or(0),
        popularity_pct_ppm: popularity.first(address.doc_id).unwrap_or(0),
    }))
}

fn compile(fields: &Fields, planned: &PackageQueryPlan) -> BooleanQuery {
    let mut outer: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();
    match &planned.text {
        TextMatch::All => outer.push((Occur::Must, Box::new(tantivy::query::AllQuery))),
        TextMatch::Scored(clauses) => {
            let mut should: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();
            for clause in clauses {
                let (query, boost) = lower_text(fields, clause);
                should.push((Occur::Should, Box::new(BoostQuery::new(query, boost))));
            }
            outer.push((Occur::Must, Box::new(BooleanQuery::new(should))));
        }
    }
    for filter in &planned.filters {
        outer.push(lower_filter(fields, filter));
    }
    BooleanQuery::new(outer)
}

fn lower_text(fields: &Fields, clause: &TextClause) -> (Box<dyn tantivy::query::Query>, f32) {
    match clause {
        TextClause::ExactName(text) => {
            let term = Term::from_field_text(fields.name_exact, text);
            let query = TermQuery::new(term, IndexRecordOption::Basic);
            (Box::new(query), PackageQueryBoosts::EXACT)
        }
        TextClause::AllTokens { field, tokens } => {
            let (tantivy_field, boost) = prose(*field, fields);
            (
                Box::new(must_conjunction_of_terms(tantivy_field, tokens)),
                boost,
            )
        }
        TextClause::Fuzzy { field, text } => {
            let tantivy_field = match field {
                FuzzyField::NameExact => fields.name_exact,
                FuzzyField::NameTokens => fields.name_tokens,
            };
            let term = Term::from_field_text(tantivy_field, text);
            (
                Box::new(FuzzyTermQuery::new(term, 1, true)),
                PackageQueryBoosts::FUZZY,
            )
        }
        TextClause::AnyKeyword(terms) => {
            let expansion: Vec<(Occur, Box<dyn tantivy::query::Query>)> = terms
                .iter()
                .map(|term_text| {
                    let term = Term::from_field_text(fields.keywords, term_text);
                    let query: Box<dyn tantivy::query::Query> =
                        Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
                    (Occur::Should, query)
                })
                .collect();
            (
                Box::new(BooleanQuery::new(expansion)),
                PackageQueryBoosts::EXPANDED,
            )
        }
    }
}

fn prose(field: ProseField, fields: &Fields) -> (Field, f32) {
    match field {
        ProseField::NameTokens => (fields.name_tokens, PackageQueryBoosts::NAME_TOKENS),
        ProseField::NameNamespace => (fields.name_ns, PackageQueryBoosts::NAME_NS),
        ProseField::Description => (fields.description, PackageQueryBoosts::DESCRIPTION),
        ProseField::Keywords => (fields.keywords, PackageQueryBoosts::KEYWORDS),
    }
}

fn lower_filter(fields: &Fields, filter: &Filter) -> (Occur, Box<dyn tantivy::query::Query>) {
    match filter {
        Filter::Ecosystem(token) => {
            let term = Term::from_field_text(fields.ecosystem, token);
            (
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
            )
        }
        Filter::Namespace(tokens) => (
            Occur::Must,
            Box::new(must_conjunction_of_terms(fields.name_ns, tokens)),
        ),
        Filter::DependsOn(name) => {
            let term = Term::from_field_text(fields.deps, name);
            (
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
            )
        }
        Filter::License(name) => {
            let term = Term::from_field_text(fields.license, name);
            (
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
            )
        }
        Filter::Phrase(tokens) => {
            let make = |field: Field| -> Box<dyn tantivy::query::Query> {
                let offset_terms: Vec<(usize, Term)> = tokens
                    .iter()
                    .enumerate()
                    .map(|(position, token)| (position, Term::from_field_text(field, token)))
                    .collect();
                Box::new(PhraseQuery::new_with_offset(offset_terms))
            };
            let phrase = BooleanQuery::new(vec![
                (Occur::Should, make(fields.description)),
                (Occur::Should, make(fields.keywords)),
            ]);
            (Occur::Must, Box::new(phrase))
        }
    }
}

fn must_conjunction_of_terms(field: Field, tokens: &[String]) -> BooleanQuery {
    let clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = tokens
        .iter()
        .map(|token| {
            let term = Term::from_field_text(field, token);
            let query: Box<dyn tantivy::query::Query> =
                Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs));
            (Occur::Must, query)
        })
        .collect();
    BooleanQuery::new(clauses)
}

fn stored_text(document: &tantivy::TantivyDocument, field: Field) -> Result<String, SearchError> {
    match document.get_first(field) {
        Some(tantivy::schema::OwnedValue::Str(text)) => Ok(text.clone()),
        _ => Err(SearchError::TantivyInternal(
            tantivy::TantivyError::InternalError(
                "stored document is missing a schema-required text field".into(),
            ),
        )),
    }
}
