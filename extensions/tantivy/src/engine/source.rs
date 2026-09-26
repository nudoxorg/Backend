//! Concrete Tantivy projection build and query methods.

use super::*;
use crate::{DocumentState, Error, FieldSelection, Limits, MatchMode, Query, RankedHit, Relevance};
use backend_semantic::EntityId;
use backend_version::CoverageWitness;
use std::{collections::BTreeMap, path::Path};
use tantivy::{
    DocAddress, Index, Searcher, TantivyDocument, Term, doc,
    query::{BooleanQuery, FuzzyTermQuery, Occur, Query as TantivyQuery, TermQuery},
    schema::{Field, IndexRecordOption, Value},
};

impl TantivySource {
    /// Builds the standard in-memory local-service adapter in one admitted step.
    ///
    /// # Errors
    ///
    /// Returns a typed contract or Tantivy construction failure.
    pub fn local_adapter(
        state: &DocumentState,
        limits: Limits,
    ) -> Result<TantivyAdapter, TantivySourceError> {
        Ok(TantivyAdapter::new(Self::build(state, limits)?, limits)?)
    }

    /// Builds and commits a concrete Tantivy projection from complete reconciled state.
    ///
    /// # Errors
    ///
    /// Returns a typed contract or Tantivy construction failure.
    pub fn build(state: &DocumentState, limits: Limits) -> Result<Self, TantivySourceError> {
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        let (schema, raw_token, folded_token, ranking_token, field_name, ordinal) =
            projection_schema();
        let index = Index::create_in_ram(schema);
        Self::populate(
            state,
            limits,
            index,
            raw_token,
            folded_token,
            ranking_token,
            field_name,
            ordinal,
        )
    }

    /// Reopens a committed directory only when its schema and complete state
    /// binding match the supplied authoritative snapshot.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O, schema, binding, coverage, or Tantivy failure.
    pub fn open_in_dir(
        state: &DocumentState,
        limits: Limits,
        directory: impl AsRef<Path>,
    ) -> Result<Self, TantivySourceError> {
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        let persisted = std::fs::read(directory.as_ref().join(BINDING_FILE))?;
        if persisted.as_slice() != projection_fingerprint(state.binding()) {
            return Err(Error::StaleRoot.into());
        }
        let index = Index::open_in_dir(directory)?;
        let (schema, raw_token, folded_token, ranking_token, field_name, ordinal) =
            projection_schema();
        if index.schema() != schema {
            return Err(Error::SchemaDrift.into());
        }
        let reader = index.reader()?;
        let expected_tokens = state
            .iter()
            .flat_map(|(_, fields)| fields.iter())
            .flat_map(|(_, text)| searchable_tokens(&text))
            .count();
        let indexed_tokens =
            usize::try_from(reader.searcher().num_docs()).map_err(|_| Error::SizeLimit)?;
        if indexed_tokens != expected_tokens {
            return Err(Self::corrupt(
                "indexed token count does not match bound state",
            ));
        }
        Ok(Self {
            binding: state.binding(),
            coverage: state.coverage(),
            limits,
            _index: index,
            reader,
            raw_token,
            folded_token,
            ranking_token,
            field_name,
            ordinal,
            documents: state.iter().map(|(document, _)| document).collect(),
        })
    }

    fn corrupt(detail: &'static str) -> TantivySourceError {
        TantivySourceError::Corrupt(detail)
    }

    /// Builds and commits the projection in a durable Tantivy directory.
    ///
    /// The supplied state remains the authority for semantic identities and the
    /// complete root binding; Tantivy stores only the replaceable search projection.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O, contract, or Tantivy construction failure.
    pub fn build_in_dir(
        state: &DocumentState,
        limits: Limits,
        directory: impl AsRef<Path>,
    ) -> Result<Self, TantivySourceError> {
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        let (schema, raw_token, folded_token, ranking_token, field_name, ordinal) =
            projection_schema();
        let index = Index::create_in_dir(directory.as_ref(), schema)?;
        let source = Self::populate(
            state,
            limits,
            index,
            raw_token,
            folded_token,
            ranking_token,
            field_name,
            ordinal,
        )?;
        std::fs::write(
            directory.as_ref().join(BINDING_FILE),
            projection_fingerprint(state.binding()),
        )?;
        Ok(source)
    }

    fn populate(
        state: &DocumentState,
        limits: Limits,
        index: Index,
        raw_token: Field,
        folded_token: Field,
        ranking_token: Field,
        field_name: Field,
        ordinal: Field,
    ) -> Result<Self, TantivySourceError> {
        let mut writer = index.writer(WRITER_MEMORY_BYTES)?;
        let mut documents = Vec::new();
        for (document, fields) in state.iter() {
            let document_ordinal = u64::try_from(documents.len()).map_err(|_| Error::SizeLimit)?;
            documents.push(document);
            for (field, text) in fields {
                for token in searchable_tokens(&text) {
                    writer.add_document(doc!(
                        raw_token => token.searchable.as_str(),
                        folded_token => token.searchable.to_ascii_lowercase(),
                        ranking_token => token.ranking.as_str(),
                        field_name => field.as_str(),
                        ordinal => document_ordinal,
                    ))?;
                }
            }
        }
        writer.commit()?;
        // Join background merges so no thread is still rewriting the index
        // directory once the source is handed out.
        writer.wait_merging_threads()?;
        let reader = index.reader()?;
        Ok(Self {
            binding: state.binding(),
            coverage: state.coverage(),
            limits,
            _index: index,
            reader,
            raw_token,
            folded_token,
            ranking_token,
            field_name,
            ordinal,
            documents,
        })
    }

    /// Executes every clause against Tantivy, reconciles identities globally, then ranks.
    ///
    /// # Errors
    ///
    /// Returns a typed query-admission, index-read, or projection-integrity failure.
    pub fn search(&self, query: &Query) -> Result<Vec<RankedHit>, TantivySourceError> {
        query.validate(self.limits)?;
        if query.terms.is_empty() {
            return Ok(self
                .documents
                .iter()
                .copied()
                .map(|document| RankedHit {
                    document,
                    relevance: Relevance::all_documents(),
                })
                .collect());
        }
        let mut candidates = self.query_clause_candidates(&query.terms[0], query)?;
        for term in query.terms.iter().skip(1) {
            let posting = self.query_clause_candidates(term, query)?;
            let mut intersection = BTreeMap::new();
            for (document, relevance) in candidates {
                if let Some(next) = posting.get(&document) {
                    intersection.insert(document, relevance.combine(*next)?);
                }
            }
            candidates = intersection;
        }
        let mut hits = candidates
            .into_iter()
            .map(|(document, relevance)| RankedHit {
                document,
                relevance,
            })
            .collect::<Vec<_>>();
        hits.sort_unstable_by(|left, right| {
            right
                .relevance
                .cmp(&left.relevance)
                .then_with(|| left.document.cmp(&right.document))
        });
        Ok(hits)
    }

    fn query_clause_candidates(
        &self,
        term: &str,
        query: &Query,
    ) -> Result<BTreeMap<EntityId, Relevance>, TantivySourceError> {
        let engine_query = self.compile_clause(term, query);
        let searcher = self.reader.searcher();
        let count = usize::try_from(searcher.num_docs()).map_err(|_| Error::SizeLimit)?;
        if count == 0 {
            return Ok(BTreeMap::new());
        }
        let matches = searcher.search(
            engine_query.as_ref(),
            &tantivy::collector::TopDocs::with_limit(count).order_by_score(),
        )?;
        let mut ranked = BTreeMap::new();
        for (_, address) in matches {
            let (document, relevance) = self.admit_engine_match(&searcher, address, term)?;
            ranked
                .entry(document)
                .and_modify(|current: &mut Relevance| *current = (*current).max(relevance))
                .or_insert(relevance);
        }
        Ok(ranked)
    }

    fn compile_clause(&self, term: &str, query: &Query) -> Box<dyn TantivyQuery> {
        let token_field = match query.case {
            crate::CaseSensitivity::Sensitive => self.raw_token,
            crate::CaseSensitivity::FoldAscii => self.folded_token,
        };
        let token = Term::from_field_text(token_field, term);
        let token_query: Box<dyn TantivyQuery> = match query.match_mode {
            MatchMode::Exact => Box::new(TermQuery::new(token, IndexRecordOption::Basic)),
            MatchMode::Prefix => Box::new(FuzzyTermQuery::new_prefix(token, 0, true)),
        };
        match &query.fields {
            FieldSelection::All => token_query,
            FieldSelection::Only(field) => Box::new(BooleanQuery::new(vec![
                (Occur::Must, token_query),
                (
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(self.field_name, field),
                        IndexRecordOption::Basic,
                    )),
                ),
            ])),
        }
    }

    fn admit_engine_match(
        &self,
        searcher: &Searcher,
        address: DocAddress,
        term: &str,
    ) -> Result<(EntityId, Relevance), TantivySourceError> {
        let stored: TantivyDocument = searcher.doc(address)?;
        let ordinal = stored
            .get_first(self.ordinal)
            .and_then(|value| value.as_u64())
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(TantivySourceError::Corrupt("document ordinal is missing"))?;
        let document = self
            .documents
            .get(ordinal)
            .copied()
            .ok_or(TantivySourceError::Corrupt(
                "document ordinal is outside the binding",
            ))?;
        let matched = stored
            .get_first(self.ranking_token)
            .and_then(|value| value.as_str())
            .ok_or(TantivySourceError::Corrupt("matched token is missing"))?;
        let field = stored
            .get_first(self.field_name)
            .and_then(|value| value.as_str())
            .ok_or(TantivySourceError::Corrupt("field name is missing"))?;
        Ok((
            document,
            Relevance::new(term.len(), matched.len(), field_weight(field), 1)?,
        ))
    }
}
